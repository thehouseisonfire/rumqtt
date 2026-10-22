//! Keep native protocol dependencies inside the backend and the two storage bridges.
use std::path::{Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use syn::visit::{self, Visit};

fn test_only(attributes: &[syn::Attribute]) -> bool {
    fn requires_test(predicate: &syn::Meta) -> bool {
        match predicate {
            syn::Meta::Path(path) => path.is_ident("test"),
            syn::Meta::List(list) => {
                let predicates = list
                    .parse_args_with(
                        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                    )
                    .unwrap();
                if list.path.is_ident("all") {
                    predicates.iter().any(requires_test)
                } else {
                    list.path.is_ident("any")
                        && !predicates.is_empty()
                        && predicates.iter().all(requires_test)
                }
            }
            syn::Meta::NameValue(_) => false,
        }
    }
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Meta>()
                .is_ok_and(|predicate| requires_test(&predicate))
    })
}

#[derive(Default)]
struct Boundary {
    root: PathBuf,
    file: PathBuf,
    module_dir: PathBuf,
    violations: Vec<String>,
}

impl Boundary {
    fn check_file(&mut self, file: &Path, module_dir: &Path) {
        // Also covers #[path = "backend/..."] modules declared at the crate root.
        if file.starts_with(self.root.join("backend"))
            || ["rust_session_store.rs", "file_store.rs"]
                .iter()
                .any(|bridge| file == self.root.join(bridge))
        {
            return;
        }
        let source = std::fs::read_to_string(file).unwrap();
        let syntax = syn::parse_file(&source).unwrap();
        let previous_file = std::mem::replace(&mut self.file, file.to_owned());
        let previous_dir = std::mem::replace(&mut self.module_dir, module_dir.to_owned());
        self.visit_file(&syntax);
        self.file = previous_file;
        self.module_dir = previous_dir;
    }

    fn check_tokens(&mut self, tokens: TokenStream) {
        for token in tokens {
            match token {
                TokenTree::Group(group) => self.check_tokens(group.stream()),
                TokenTree::Ident(ident) => self.visit_ident(&ident),
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Boundary {
    fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
        if ident == "rumqttc_v4" || ident == "rumqttc_v5" {
            self.violations
                .push(format!("{}: {ident}", self.file.display()));
        }
    }

    fn visit_macro(&mut self, item: &'ast syn::Macro) {
        visit::visit_path(self, &item.path);
        self.check_tokens(item.tokens.clone());
    }

    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attributes: &[syn::Attribute] = match item {
            syn::Item::Const(item) => &item.attrs,
            syn::Item::Enum(item) => &item.attrs,
            syn::Item::ExternCrate(item) => &item.attrs,
            syn::Item::Fn(item) => &item.attrs,
            syn::Item::ForeignMod(item) => &item.attrs,
            syn::Item::Impl(item) => &item.attrs,
            syn::Item::Macro(item) => &item.attrs,
            syn::Item::Mod(item) => &item.attrs,
            syn::Item::Static(item) => &item.attrs,
            syn::Item::Struct(item) => &item.attrs,
            syn::Item::Trait(item) => &item.attrs,
            syn::Item::TraitAlias(item) => &item.attrs,
            syn::Item::Type(item) => &item.attrs,
            syn::Item::Union(item) => &item.attrs,
            syn::Item::Use(item) => &item.attrs,
            _ => &[],
        };
        if !test_only(attributes) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if test_only(&item.attrs) {
            return;
        }
        let directory = self.module_dir.join(item.ident.to_string());
        if let Some((_, items)) = &item.content {
            let previous = std::mem::replace(&mut self.module_dir, directory);
            for item in items {
                self.visit_item(item);
            }
            self.module_dir = previous;
        } else {
            let explicit = item.attrs.iter().find_map(|attribute| {
                if attribute.path().is_ident("path")
                    && let syn::Meta::NameValue(value) = &attribute.meta
                    && let syn::Expr::Lit(value) = &value.value
                    && let syn::Lit::Str(path) = &value.lit
                {
                    return Some(self.file.parent().unwrap().join(path.value()));
                }
                None
            });
            let file = explicit.unwrap_or_else(|| {
                let file = directory.with_extension("rs");
                if file.exists() {
                    file
                } else {
                    directory.join("mod.rs")
                }
            });
            self.check_file(&file, &directory);
        }
    }
}

#[test]
fn production_protocol_references_stay_in_backend_or_storage_bridges() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut boundary = Boundary {
        root: root.clone(),
        ..Boundary::default()
    };
    boundary.check_file(&root.join("lib.rs"), &root);
    assert!(
        boundary.violations.is_empty(),
        "native protocol references outside the backend/storage bridges:\n{}",
        boundary.violations.join("\n")
    );
}

#[test]
fn checker_rejects_imports_aliases_and_macros_but_allows_test_code() {
    let syntax = syn::parse_file(
        r#"
        use rumqttc_v4 as native;
        fn production() { rumqttc_v5::valid_filter("topic"); }
        forward! { (rumqttc_v4::Client) }
        // rumqttc_v5::Client and strings are not dependencies.
        const TEXT: &str = "rumqttc_v5::Client";
        #[cfg(test)] mod tests { use rumqttc_v4::Client; }
        #[cfg(test)] #[path = "missing.rs"] mod external_tests;
        #[cfg(test)] fn test_helper() { rumqttc_v5::valid_filter("topic"); }
        #[cfg(test)] use rumqttc_v4::Client;
        #[cfg(all(test, feature = "optional"))] struct TestClient(rumqttc_v4::Client);
        #[cfg(any(test, feature = "optional"))] type ProductionClient = rumqttc_v4::Client;
    "#,
    )
    .unwrap();
    let mut boundary = Boundary::default();
    boundary.visit_file(&syntax);
    assert_eq!(boundary.violations.len(), 4);
}
