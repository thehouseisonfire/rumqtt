use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use syn::parse::Parser;
use syn::visit::{self, Visit};
use syn::{Attribute, Expr, Item, Token};

use super::{
    Availability, BindingKind, Bindings, NamePath, Namespace, Scanner, attributes, binding_name,
    conditions, has_macro_attribute, requires_macro_review,
};

pub(super) struct Pending {
    module: String,
    file: PathBuf,
    assertions: Vec<Assertion>,
    scope: BTreeSet<String>,
}

struct Assertion {
    path: NamePath,
    conditions: Vec<String>,
}

#[derive(Default)]
struct Inspect {
    conditions: Vec<String>,
    assertions: Vec<Assertion>,
    error: Option<String>,
}

impl Inspect {
    fn scoped(&mut self, attrs: &[Attribute], inspect: impl FnOnce(&mut Self)) {
        if self.error.is_some() {
            return;
        }
        let Some(cfg) = conditions(&self.conditions, attrs) else {
            return;
        };
        for attribute in attrs {
            let generating = attributes::leaves(&attribute.meta, &cfg).and_then(|leaves| {
                for (meta, conditions) in leaves {
                    if Availability::conditions(&conditions).is_never() {
                        continue;
                    }
                    let mut linkage = false;
                    for name in ["unsafe", "no_mangle", "export_name", "link_section"] {
                        linkage |= has_macro_attribute(&meta, name)?;
                    }
                    if requires_macro_review(&meta)? || linkage {
                        return Ok(true);
                    }
                }
                Ok(false)
            });
            match generating {
                Ok(false) => {}
                Ok(true) => {
                    self.error = Some(
                        "generating or linkage attributes require block-scope discovery support"
                            .into(),
                    );
                    return;
                }
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            }
        }
        let previous = std::mem::replace(&mut self.conditions, cfg);
        inspect(self);
        self.conditions = previous;
    }

    fn inspect_macro(&mut self, mac: &syn::Macro) {
        if self.error.is_some() {
            return;
        }
        let path = NamePath::from(&mac.path);
        let Some(last) = path.segments.last() else {
            self.error = Some("unsupported empty macro path".into());
            return;
        };
        let name = binding_name(last);
        let literal = matches!(
            name,
            "cfg"
                | "stringify"
                | "concat"
                | "env"
                | "option_env"
                | "line"
                | "column"
                | "file"
                | "module_path"
        );
        let assertion = matches!(
            name,
            "assert"
                | "assert_eq"
                | "assert_ne"
                | "debug_assert"
                | "debug_assert_eq"
                | "debug_assert_ne"
                | "panic"
        );
        if !literal && !assertion && name != "matches" {
            self.error = Some(format!(
                "macro `{path}!` requires block-scope expansion discovery support"
            ));
            return;
        }
        self.assertions.push(Assertion {
            path: path.clone(),
            conditions: self.conditions.clone(),
        });
        if literal {
            // These compiler macros produce literals or predicates, not Rust
            // declarations. stringify! deliberately does not expand its input.
            return;
        }
        if name == "matches" {
            self.inspect_matches(mac);
        } else {
            match syn::punctuated::Punctuated::<Expr, Token![,]>::parse_terminated
                .parse2(mac.tokens.clone())
            {
                Ok(arguments) => {
                    for argument in arguments {
                        self.visit_expr(&argument);
                    }
                }
                Err(error) => {
                    self.error = Some(format!("unsupported assertion arguments: {error}"));
                }
            }
        }
    }

    fn inspect_matches(&mut self, mac: &syn::Macro) {
        let parsed = mac.parse_body_with(|input: syn::parse::ParseStream<'_>| {
            let expression: Expr = input.parse()?;
            input.parse::<Token![,]>()?;
            let pattern = input.call(syn::Pat::parse_multi_with_leading_vert)?;
            let guard = if input.peek(Token![if]) {
                input.parse::<Token![if]>()?;
                Some(input.parse::<Expr>()?)
            } else {
                None
            };
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
            Ok((expression, pattern, guard))
        });
        match parsed {
            Ok((expression, pattern, guard)) => {
                self.visit_expr(&expression);
                self.visit_pat(&pattern);
                if let Some(guard) = guard {
                    self.visit_expr(&guard);
                }
            }
            Err(error) => self.error = Some(format!("unsupported matches! arguments: {error}")),
        }
    }
}

impl<'ast> Visit<'ast> for Inspect {
    fn visit_item(&mut self, item: &'ast Item) {
        let attrs: &[Attribute] = match item {
            Item::Const(i) => &i.attrs,
            Item::Enum(i) => &i.attrs,
            Item::ExternCrate(i) => &i.attrs,
            Item::Fn(i) => &i.attrs,
            Item::ForeignMod(i) => &i.attrs,
            Item::Impl(i) => &i.attrs,
            Item::Macro(i) => &i.attrs,
            Item::Mod(i) => &i.attrs,
            Item::Static(i) => &i.attrs,
            Item::Struct(i) => &i.attrs,
            Item::Trait(i) => &i.attrs,
            Item::TraitAlias(i) => &i.attrs,
            Item::Type(i) => &i.attrs,
            Item::Union(i) => &i.attrs,
            Item::Use(i) => &i.attrs,
            _ => &[],
        };
        self.scoped(attrs, |this| match item {
            // Local values and helper functions may support pure assertions;
            // their initializers, types and bodies must still be inspected.
            Item::Const(_) | Item::Static(_) | Item::Fn(_) => visit::visit_item(this, item),
            _ => this.error = Some("nested declarations, imports or implementations require block-scope discovery support; move them to module scope".into()),
        });
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        self.scoped(expr_attributes(expr), |this| {
            if matches!(expr, Expr::Verbatim(_)) {
                this.error = Some("unsupported opaque expression".into());
            } else {
                visit::visit_expr(this, expr);
            }
        });
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        self.scoped(&local.attrs, |this| visit::visit_local(this, local));
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        self.scoped(&arm.attrs, |this| visit::visit_arm(this, arm));
    }

    fn visit_field_value(&mut self, field: &'ast syn::FieldValue) {
        self.scoped(&field.attrs, |this| visit::visit_field_value(this, field));
    }

    fn visit_stmt_macro(&mut self, mac: &'ast syn::StmtMacro) {
        self.scoped(&mac.attrs, |this| this.inspect_macro(&mac.mac));
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.inspect_macro(mac);
    }
}

impl Scanner<'_> {
    pub(super) fn anonymous_constant(
        &mut self,
        item: &syn::ItemConst,
        cfg: &[String],
        module: &str,
        file: &Path,
    ) -> Result<(), String> {
        let mut inspect = Inspect {
            conditions: cfg.to_vec(),
            ..Inspect::default()
        };
        // The outer item also needs attribute validation, without registering
        // an inventory identity for the repeatable anonymous name.
        inspect.scoped(&item.attrs, |inspect| {
            inspect.visit_type(&item.ty);
            inspect.visit_expr(&item.expr);
        });
        if let Some(error) = inspect.error {
            return Err(format!(
                "unsupported anonymous constant in {module} ({}): {error}",
                file.display()
            ));
        }
        if inspect.assertions.is_empty() {
            return Ok(());
        }
        self.anonymous_assertions.push(Pending {
            module: module.into(),
            file: file.into(),
            assertions: inspect.assertions,
            scope: self.modules[module].macro_scope.clone(),
        });
        Ok(())
    }

    pub(super) fn finish_anonymous_constants(&self, bindings: &Bindings) -> Result<(), String> {
        let definitions: BTreeSet<_> = self.macro_definitions.values().flatten().collect();
        // Opaque generators can introduce path-based imports before their
        // source position, but only in scopes their expansion can affect.
        let opaque: Vec<_> = self
            .declarations
            .iter()
            .filter(|(id, declaration)| {
                declaration.kind == "macro"
                    && !self.module_macro_imports.contains(*id)
                    && (!definitions.contains(*id) || self.attribute_inputs.contains_key(*id))
            })
            .map(|(id, declaration)| {
                let module = self
                    .macro_inputs
                    .get(id)
                    .map_or(self.surface.crate_name.as_str(), |(module, _)| {
                        module.as_str()
                    });
                (module, declaration.conditions.as_slice())
            })
            .collect();
        for pending in &self.anonymous_assertions {
            for assertion in &pending.assertions {
                if !self.standard_assertion(pending, assertion, bindings, &opaque) {
                    return Err(format!(
                        "unsupported anonymous constant in {} ({}): macro `{}!` is not proven to be an unshadowed standard assertion or literal macro",
                        pending.module,
                        pending.file.display(),
                        assertion.path
                    ));
                }
            }
        }
        Ok(())
    }

    fn standard_assertion(
        &self,
        pending: &Pending,
        assertion: &Assertion,
        bindings: &Bindings,
        opaque: &[(&str, &[String])],
    ) -> bool {
        let path = &assertion.path;
        let name = binding_name(path.segments.last().unwrap());
        let available = Availability::conditions(&assertion.conditions);
        let active = |conditions: &[String]| {
            !available
                .and(&Availability::conditions(conditions))
                .is_never()
        };
        let blocked = |absolute: bool| {
            opaque.iter().any(|(module, conditions)| {
                let relevant = if absolute {
                    *module == self.surface.crate_name
                } else {
                    *module == pending.module
                        || pending
                            .module
                            .strip_prefix(module)
                            .is_some_and(|suffix| suffix.starts_with("::"))
                };
                relevant && active(conditions)
            })
        };
        let imports = &self.modules[&pending.module].imports;
        let selects = |name: &str| {
            imports.iter().any(|import| {
                active(&import.conditions)
                    && (import.is_glob() || import.name().as_deref() == Some(name))
            })
        };
        if path.segments.len() == 1 && !path.absolute {
            if selects(name) || blocked(false) {
                return false;
            }
            if self.macro_crates.iter().any(|(id, names)| {
                active(&self.declarations[id].conditions)
                    && names.as_ref().is_none_or(|names| {
                        names
                            .iter()
                            .any(|candidate| binding_name(candidate) == name)
                    })
            }) {
                return false;
            }
            for id in &pending.scope {
                if !active(&self.declarations[id].conditions) {
                    continue;
                }
                if self.module_macro_imports.contains(id) {
                    return false;
                }
                if self
                    .macro_definitions
                    .iter()
                    .any(|((_, defined), ids)| defined == name && ids.contains(id))
                {
                    return false;
                }
            }
            return !self
                .macro_definitions
                .get(&(self.surface.crate_name.clone(), name.into()))
                .is_some_and(|ids| {
                    ids.iter().any(|id| {
                        self.exported_macros.contains_key(id)
                            && active(&self.declarations[id].conditions)
                    })
                });
        }
        let first = binding_name(&path.segments[0]);
        if path.segments.len() != 2 || !matches!(first, "std" | "core") {
            return false;
        }
        if !path.absolute
            && (selects(first)
                || bindings[&pending.module].get(first).is_some_and(|values| {
                    values.iter().any(|binding| {
                        binding.namespace.includes(Namespace::Type) && active(&binding.conditions)
                    })
                }))
        {
            return false;
        }
        let aliased = bindings[&self.surface.crate_name]
            .get(first)
            .is_some_and(|values| {
                values.iter().any(|binding| {
                    matches!(
                        binding.kind,
                        BindingKind::ExternalCrate | BindingKind::SelfCrate
                    ) && active(&binding.conditions)
                })
            });
        !aliased && !blocked(path.absolute)
    }
}

fn expr_attributes(expr: &Expr) -> &[Attribute] {
    match expr {
        Expr::Array(e) => &e.attrs,
        Expr::Assign(e) => &e.attrs,
        Expr::Async(e) => &e.attrs,
        Expr::Await(e) => &e.attrs,
        Expr::Binary(e) => &e.attrs,
        Expr::Block(e) => &e.attrs,
        Expr::Break(e) => &e.attrs,
        Expr::Call(e) => &e.attrs,
        Expr::Cast(e) => &e.attrs,
        Expr::Closure(e) => &e.attrs,
        Expr::Const(e) => &e.attrs,
        Expr::Continue(e) => &e.attrs,
        Expr::Field(e) => &e.attrs,
        Expr::ForLoop(e) => &e.attrs,
        Expr::Group(e) => &e.attrs,
        Expr::If(e) => &e.attrs,
        Expr::Index(e) => &e.attrs,
        Expr::Infer(e) => &e.attrs,
        Expr::Let(e) => &e.attrs,
        Expr::Lit(e) => &e.attrs,
        Expr::Loop(e) => &e.attrs,
        Expr::Macro(e) => &e.attrs,
        Expr::Match(e) => &e.attrs,
        Expr::MethodCall(e) => &e.attrs,
        Expr::Paren(e) => &e.attrs,
        Expr::Path(e) => &e.attrs,
        Expr::Range(e) => &e.attrs,
        Expr::RawAddr(e) => &e.attrs,
        Expr::Reference(e) => &e.attrs,
        Expr::Repeat(e) => &e.attrs,
        Expr::Return(e) => &e.attrs,
        Expr::Struct(e) => &e.attrs,
        Expr::Try(e) => &e.attrs,
        Expr::TryBlock(e) => &e.attrs,
        Expr::Tuple(e) => &e.attrs,
        Expr::Unary(e) => &e.attrs,
        Expr::Unsafe(e) => &e.attrs,
        Expr::While(e) => &e.attrs,
        Expr::Yield(e) => &e.attrs,
        _ => &[],
    }
}
