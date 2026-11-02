use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use quote::{ToTokens, quote};
use syn::visit::Visit;
use syn::{Attribute, Item, UseTree, Visibility};

use crate::model::{Declaration, Surface};

mod aliases;
mod anonymous;
mod attributes;
mod availability;
mod bindings;
mod constructors;
mod derives;
mod discriminants;
mod linkage;
mod macro_references;
mod manifest;
mod references;

use aliases::{Alias, Aliases};
use attributes::{AttributeInput, PendingAttributes};
use availability::Availability;
use bindings::{
    Binding, BindingKind, Bindings, NamePath, Namespace, Shadowing, binding_name, canonical_names,
    resolve,
};
use constructors::Constructors;
use derives::PendingDerive;
use discriminants::Discriminants;
use macro_references::MacroReferences;
use references::References;

fn tokens(value: &impl ToTokens) -> String {
    value.to_token_stream().to_string()
}

fn generics_shape(generics: &syn::Generics) -> String {
    let mut shape = tokens(generics);
    // syn prints the parameter list separately from its where clause.
    if let Some(clause) = &generics.where_clause {
        let _ = write!(shape, " {}", tokens(clause));
    }
    shape
}

const fn public(vis: &Visibility) -> bool {
    matches!(vis, Visibility::Public(_))
}

fn dependency_visibility(vis: &Visibility) -> String {
    match vis {
        Visibility::Public(_) => String::new(),
        Visibility::Inherited => " visibility=private".into(),
        Visibility::Restricted(_) => format!(" visibility={}", tokens(vis)),
    }
}

// Evaluate only `test`; all feature/platform predicates remain symbolic.
fn test_truth(meta: &syn::Meta) -> Option<bool> {
    match meta {
        syn::Meta::Path(path) if path.is_ident("test") => Some(false),
        syn::Meta::List(list) => {
            let args = list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                )
                .ok()?;
            let values: Vec<_> = args.iter().map(test_truth).collect();
            if list.path.is_ident("not") && values.len() == 1 {
                values[0].map(|v| !v)
            } else if list.path.is_ident("all") {
                if values.contains(&Some(false)) {
                    Some(false)
                } else if values.iter().all(|v| *v == Some(true)) {
                    Some(true)
                } else {
                    None
                }
            } else if list.path.is_ident("any") {
                if values.contains(&Some(true)) {
                    Some(true)
                } else if values.iter().all(|v| *v == Some(false)) {
                    Some(false)
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

fn is_declaration_metadata(meta: &syn::Meta) -> bool {
    [
        "repr",
        "non_exhaustive",
        "no_mangle",
        "export_name",
        "link_section",
        "unsafe",
    ]
    .iter()
    .any(|name| meta.path().is_ident(name))
}

fn has_conditional_metadata(meta: &syn::Meta) -> bool {
    if meta.path().is_ident("cfg_attr") {
        // Preserve unsupported syntax for review rather than dropping a
        // possible ABI change. Public generators validate cfg_attr separately.
        return cfg_attr_attributes(meta)
            .map_or(true, |attrs| attrs.iter().any(has_conditional_metadata));
    }
    if meta.path().is_ident("doc") {
        // Keep the existing doc(cfg(...)) availability review input, without
        // mistaking attribute names inside documentation strings for metadata.
        return matches!(meta, syn::Meta::List(list)
            if list.parse_args::<syn::Meta>().is_ok_and(|nested| nested.path().is_ident("cfg")));
    }
    meta.path().is_ident("cfg")
        || meta.path().is_ident("macro_export")
        || meta.path().is_ident("macro_use")
        || is_declaration_metadata(meta)
}

fn has_macro_attribute(meta: &syn::Meta, name: &str) -> Result<bool, String> {
    if meta.path().is_ident(name) {
        return Ok(true);
    }
    let mut found = false;
    if meta.path().is_ident("cfg_attr") {
        for attribute in cfg_attr_attributes(meta)? {
            found |= has_macro_attribute(&attribute, name)?;
        }
    }
    Ok(found)
}

fn macro_use_names(meta: &syn::Meta) -> Result<Option<BTreeSet<String>>, String> {
    if meta.path().is_ident("macro_use") {
        return match meta {
            syn::Meta::Path(_) => Ok(None),
            syn::Meta::List(list) => list
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Ident, syn::Token![,]>::parse_terminated,
                )
                .map(|names| Some(names.iter().map(ToString::to_string).collect()))
                .map_err(|error| format!("unsupported macro_use: {error}")),
            syn::Meta::NameValue(_) => Err("unsupported macro_use syntax".into()),
        };
    }
    let mut names = BTreeSet::new();
    for attribute in cfg_attr_attributes(meta)? {
        if has_macro_attribute(&attribute, "macro_use")? {
            let Some(imported) = macro_use_names(&attribute)? else {
                return Ok(None);
            };
            names.extend(imported);
        }
    }
    Ok(Some(names))
}

fn conditions(parent: &[String], attrs: &[Attribute]) -> Option<Vec<String>> {
    let mut result = parent.to_vec();
    for attr in attrs {
        if attr.path().is_ident("cfg") {
            if let Ok(meta) = attr.parse_args::<syn::Meta>() {
                if test_truth(&meta) == Some(false) {
                    return None;
                }
                if test_truth(&meta) != Some(true) {
                    result.push(tokens(&attr.meta));
                }
            } else {
                // Unsupported predicate syntax is retained for review, never
                // interpreted as an instruction to skip an audited declaration.
                result.push(tokens(&attr.meta));
            }
        } else if attr.path().is_ident("cfg_attr") && has_conditional_metadata(&attr.meta) {
            result.push(tokens(&attr.meta));
        }
    }
    result.sort();
    result.dedup();
    (!Availability::conditions(&result).is_never()).then_some(result)
}

fn attributes(attrs: &[Attribute]) -> String {
    attrs
        .iter()
        .filter(|a| is_declaration_metadata(&a.meta))
        .map(|a| tokens(&a.meta))
        .collect::<Vec<_>>()
        .join(" ")
}

fn cfg_attr_attributes(meta: &syn::Meta) -> Result<Vec<syn::Meta>, String> {
    let syn::Meta::List(list) = meta else {
        return Err("unsupported cfg_attr syntax".into());
    };
    let nested = list
        .parse_args_with(syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
        .map_err(|e| format!("unsupported cfg_attr: {e}"))?;
    if nested.len() < 2 {
        return Err("cfg_attr requires a predicate and at least one attribute".into());
    }
    let mut nested = nested.into_iter();
    if test_truth(&nested.next().unwrap()) == Some(false) {
        return Ok(Vec::new());
    }
    Ok(nested.collect())
}

fn requires_macro_review(meta: &syn::Meta) -> Result<bool, String> {
    if meta.path().is_ident("derive") {
        let syn::Meta::List(list) = meta else {
            return Err("unsupported derive syntax".into());
        };
        let derives = list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            )
            .map_err(|e| format!("unsupported derive: {e}"))?;
        return Ok(derives.iter().any(|path| {
            ![
                "Clone",
                "Copy",
                "Debug",
                "Default",
                "PartialEq",
                "Eq",
                "PartialOrd",
                "Ord",
                "Hash",
                "thiserror :: Error",
                "serde :: Serialize",
                "serde :: Deserialize",
            ]
            .contains(&tokens(path).as_str())
        }));
    }
    if meta.path().is_ident("cfg_attr") {
        let mut unknown = false;
        for attribute in cfg_attr_attributes(meta)? {
            unknown |= requires_macro_review(&attribute)?;
        }
        return Ok(unknown);
    }
    Ok(![
        "doc",
        "cfg",
        "path",
        "repr",
        "non_exhaustive",
        "allow",
        "expect",
        "deny",
        "warn",
        "forbid",
        "must_use",
        "inline",
        "cold",
        "track_caller",
        "unsafe",
        "no_mangle",
        "export_name",
        "link_section",
        "macro_use",
        "deprecated",
        "feature",
    ]
    .iter()
    .any(|name| meta.path().is_ident(name)))
}

fn enum_field_shape(field: &syn::Field) -> Option<String> {
    // Payload fields belong to the variant shape, so their availability must
    // remain attached to each field instead of gating the entire variant.
    let cfg = conditions(&[], &field.attrs)?;
    let mut metadata = attributes(&field.attrs);
    if !cfg.is_empty() {
        let _ = write!(metadata, " [{}]", cfg.join(" && "));
    }
    Some(format!(
        "{}:{}:{}",
        metadata,
        field
            .ident
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        tokens(&field.ty)
    ))
}

fn struct_construction_shape(fields: &syn::Fields) -> String {
    let form = match fields {
        syn::Fields::Named(_) => "named",
        syn::Fields::Unnamed(_) => "tuple",
        syn::Fields::Unit => "unit",
    };
    // Only the existence and availability of private fields affect external
    // construction. Their names, types and count remain implementation details.
    let barriers: BTreeSet<_> = fields
        .iter()
        .filter(|field| !public(&field.vis))
        .filter_map(|field| conditions(&[], &field.attrs))
        .collect();
    let private_fields = if barriers.is_empty() {
        "none".into()
    } else if barriers.contains(&Vec::new()) {
        "unconditional".into()
    } else {
        barriers
            .iter()
            .map(|cfg| format!("[{}]", cfg.join(" && ")))
            .collect::<Vec<_>>()
            .join(" || ")
    };
    format!("form={form} private_fields={private_fields}")
}

#[derive(Default)]
struct Module {
    locals: BTreeMap<String, BTreeSet<Binding>>,
    public: BTreeMap<String, BTreeSet<Binding>>,
    restricted_bindings: BTreeMap<String, BTreeSet<Binding>>,
    imports: Vec<Import>,
    macro_anchors: Vec<String>,
    declarations: Vec<String>,
    extern_crates: Vec<ExternCrate>,
    // Potential textual macro inputs visible at the current source position.
    // Keep conditional alternatives, earlier generators and helper definitions.
    macro_scope: BTreeSet<String>,
    item_macros: BTreeSet<String>,
    macro_order: usize,
}

struct ExternCrate {
    name: String,
    is_self: bool,
    public: bool,
    declaration: String,
    macro_anchors: Vec<String>,
}

impl ExternCrate {
    fn retain(&self, reachable: &mut BTreeSet<String>) {
        reachable.insert(self.declaration.clone());
        reachable.extend(self.macro_anchors.iter().cloned());
    }
}

struct Import {
    path: NamePath,
    alias: Option<String>,
    public: bool,
    accessible: bool,
    declaration: String,
    conditions: Vec<String>,
    macro_anchors: Vec<String>,
}

#[derive(Clone, Copy)]
enum ImportSelection {
    Export,
    Dependency(Namespace),
}

impl ImportSelection {
    const fn namespace(self) -> Namespace {
        match self {
            Self::Export => Namespace::Both,
            Self::Dependency(namespace) => namespace,
        }
    }

    const fn include_private(self) -> bool {
        matches!(self, Self::Dependency(_))
    }
}

impl Import {
    fn is_glob(&self) -> bool {
        self.path.segments.last().is_some_and(|part| part == "*")
    }

    fn may_select(&self, name: &str, include_private: bool, qualifier: bool) -> bool {
        let name = binding_name(name);
        (include_private || self.public || qualifier)
            && (name == "*" || self.is_glob() || self.name().as_deref() == Some(name))
    }

    fn retain(&self, reachable: &mut BTreeSet<String>) {
        reachable.insert(self.declaration.clone());
        reachable.extend(self.macro_anchors.iter().cloned());
    }

    fn items(
        &self,
        targets: &BTreeSet<Binding>,
        scopes: &Bindings,
        crate_name: &str,
    ) -> Vec<(String, BTreeSet<Binding>)> {
        let items = if self.is_glob() {
            targets
                .iter()
                .flat_map(|target| {
                    target
                        .namespace(crate_name)
                        .and_then(|namespace| scopes.get(namespace))
                        .into_iter()
                        .flatten()
                        .map(|(key, values)| {
                            (
                                key.clone(),
                                values
                                    .iter()
                                    .cloned()
                                    .map(|binding| binding.with_conditions(&target.conditions))
                                    .collect::<BTreeSet<_>>(),
                            )
                        })
                })
                .collect()
        } else {
            vec![(self.name().unwrap(), targets.clone())]
        };
        items
            .into_iter()
            .map(|(name, values)| {
                (
                    name,
                    values
                        .into_iter()
                        .map(|binding| binding.with_conditions(&self.conditions))
                        .collect(),
                )
            })
            .collect()
    }

    fn name(&self) -> Option<String> {
        let (last, prefix) = self.path.segments.split_last()?;
        if last == "*" {
            return None;
        }
        let name = self.alias.clone().unwrap_or_else(|| {
            if last == "self" {
                prefix.last().unwrap_or(last).clone()
            } else {
                last.clone()
            }
        });
        Some(binding_name(&name).into())
    }
}

struct PendingImpl {
    module: String,
    ty: NamePath,
    arguments: syn::PathArguments,
    members: Vec<PendingMember>,
    attributes: Vec<Attribute>,
    conditions: Vec<String>,
    source: PathBuf,
    input: String,
    references: References,
}

struct PendingMember {
    name: String,
    kind: &'static str,
    public: bool,
    shape: String,
    conditions: Vec<String>,
    source: PathBuf,
    attributes: Vec<Attribute>,
    input: String,
    references: References,
    macro_scope: Option<BTreeSet<String>>,
    macro_path: Option<NamePath>,
}

struct UnsupportedForeign {
    message: String,
    is_type: bool,
}

struct ModuleDirectory {
    submodules: PathBuf,
    path_attributes: PathBuf,
}

impl ModuleDirectory {
    fn shared(path: PathBuf) -> Self {
        Self {
            submodules: path.clone(),
            path_attributes: path,
        }
    }
}

fn reject_conditional_module_path(meta: &syn::Meta) -> Result<(), String> {
    if meta.path().is_ident("cfg_attr") {
        for attribute in cfg_attr_attributes(meta)? {
            if attribute.path().is_ident("path") {
                return Err(
                    "unsupported conditional module path; use separate #[cfg] modules with literal #[path] attributes"
                        .into(),
                );
            }
            reject_conditional_module_path(&attribute)?;
        }
    }
    Ok(())
}

fn module_path(attrs: &[Attribute]) -> Result<Option<PathBuf>, String> {
    // Even with a literal fallback path, cfg_attr may select a different source
    // file or change an inline module's child directory. Never guess that path.
    for attr in attrs {
        reject_conditional_module_path(&attr.meta)?;
    }
    let Some(attr) = attrs.iter().find(|a| a.path().is_ident("path")) else {
        return Ok(None);
    };
    let syn::Meta::NameValue(value) = &attr.meta else {
        return Err("nonliteral module path".into());
    };
    let syn::Expr::Lit(value) = &value.value else {
        return Err("nonliteral module path".into());
    };
    let syn::Lit::Str(value) = &value.lit else {
        return Err("nonliteral module path".into());
    };
    Ok(Some(value.value().into()))
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum ChildOwner {
    Type,
    Value,
    Module,
    Crate,
}

impl ChildOwner {
    fn declaration(kind: &str) -> Self {
        match kind {
            "function" | "constant" | "static" | "discriminant" => Self::Value,
            "module" => Self::Module,
            "extern_crate" => Self::Crate,
            _ => Self::Type,
        }
    }

    const fn binding(kind: BindingKind) -> Self {
        match kind {
            BindingKind::Module => Self::Module,
            BindingKind::Value => Self::Value,
            BindingKind::ExternalCrate | BindingKind::SelfCrate => Self::Crate,
            BindingKind::Declaration | BindingKind::Enum | BindingKind::Variant => Self::Type,
        }
    }
}

struct Scanner<'a> {
    root: &'a Path,
    surface: &'a Surface,
    modules: BTreeMap<String, Module>,
    enum_exports: Bindings,
    declarations: BTreeMap<String, Declaration>,
    children: BTreeMap<String, BTreeMap<ChildOwner, Vec<String>>>,
    implementations: Vec<PendingImpl>,
    aliases: Vec<Alias>,
    local_types: BTreeSet<String>,
    constants: BTreeMap<String, BTreeSet<String>>,
    constructors: Constructors,
    discriminants: Discriminants,
    shadowing: Shadowing,
    references: BTreeMap<String, (String, References)>,
    macro_inputs: BTreeMap<String, (String, BTreeSet<String>)>,
    macro_definitions: BTreeMap<(String, String), BTreeSet<String>>,
    macro_calls: BTreeMap<String, (String, NamePath)>,
    macro_expansions: BTreeMap<String, (String, MacroReferences)>,
    macro_crates: BTreeMap<String, Option<BTreeSet<String>>>,
    module_macro_imports: BTreeSet<String>,
    pending_derives: Vec<PendingDerive>,
    pending_helpers: Vec<PendingAttributes>,
    anonymous_assertions: Vec<anonymous::Pending>,
    attribute_inputs: BTreeMap<String, (String, Vec<NamePath>)>,
    unsupported_foreign: BTreeMap<String, UnsupportedForeign>,
    // Key by the trait declaration identity, checked after API reachability.
    unsupported_trait_members: BTreeMap<String, String>,
    unsupported_foreign_macros: BTreeMap<String, Vec<String>>,
    exported_macros: BTreeMap<String, String>,
    linkage_exports: BTreeSet<String>,
    stack: BTreeSet<PathBuf>,
}

impl Scanner<'_> {
    fn module_reached(&self, module: &str, sources: &BTreeSet<String>) -> bool {
        sources
            .iter()
            .chain(self.exported_macros.values())
            .any(|source| {
                source == module
                    || source
                        .strip_prefix(module)
                        .is_some_and(|rest| rest.starts_with("::"))
            })
    }

    fn macro_crate_inputs<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a String> {
        let name = name.strip_prefix("r#").unwrap_or(name);
        self.macro_crates.iter().filter_map(move |(id, names)| {
            names
                .as_ref()
                .is_none_or(|names| {
                    names
                        .iter()
                        .any(|candidate| candidate.strip_prefix("r#").unwrap_or(candidate) == name)
                })
                .then_some(id)
        })
    }

    fn children_of(&self, path: &str, owner: ChildOwner) -> &[String] {
        self.children
            .get(path)
            .and_then(|children| children.get(&owner))
            .map_or(&[], Vec::as_slice)
    }

    fn attach(
        &mut self,
        path: &str,
        owner: ChildOwner,
        children: impl IntoIterator<Item = String>,
    ) {
        self.children
            .entry(path.into())
            .or_default()
            .entry(owner)
            .or_default()
            .extend(children);
    }

    fn has_definition(&self, binding: &Binding) -> bool {
        match binding.kind {
            BindingKind::Declaration | BindingKind::Enum => {
                self.local_types.contains(&binding.path)
                    || self.unsupported_foreign.contains_key(&binding.path)
            }
            BindingKind::Value => {
                !self
                    .children_of(&binding.path, ChildOwner::Value)
                    .is_empty()
                    || self.unsupported_foreign.contains_key(&binding.path)
            }
            BindingKind::Module
            | BindingKind::ExternalCrate
            | BindingKind::SelfCrate
            | BindingKind::Variant => true,
        }
    }

    fn reference(
        &mut self,
        id: &str,
        module: &str,
        generics: &[&syn::Generics],
        collect: impl FnOnce(&mut References),
    ) {
        self.references.insert(
            id.into(),
            (module.into(), References::collect(generics, collect)),
        );
    }

    fn macro_input(&mut self, id: &str, module: &str) {
        self.macro_inputs.insert(
            id.into(),
            (module.into(), self.modules[module].macro_scope.clone()),
        );
    }

    fn add(
        &mut self,
        path: &str,
        kind: &str,
        shape: String,
        cfg: &[String],
        file: &Path,
    ) -> Result<String, String> {
        let mut id = format!("{}|{path}#{kind}", self.surface.name);
        if !cfg.is_empty() {
            let _ = write!(id, " [{}]", cfg.join(" && "));
        }
        let declaration = Declaration {
            id: id.clone(),
            surface: self.surface.name.clone(),
            kind: kind.into(),
            shape,
            conditions: cfg.to_vec(),
            source: file
                .strip_prefix(self.root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/"),
        };
        if self.declarations.insert(id.clone(), declaration).is_some() {
            return Err(format!("duplicate declaration {id}"));
        }
        self.attach(path, ChildOwner::declaration(kind), [id.clone()]);
        Ok(id)
    }

    fn file(
        &mut self,
        path: &Path,
        module: &str,
        cfg: &[String],
        directory: &ModuleDirectory,
    ) -> Result<Option<Vec<String>>, String> {
        let canonical = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if !self.stack.insert(canonical.clone()) {
            return Err(format!("module cycle: {}", path.display()));
        }
        let source = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let parsed = syn::parse_file(&source).map_err(|e| format!("{}: {e}", path.display()))?;
        let effective_cfg = conditions(cfg, &parsed.attrs);
        if let Some(cfg) = &effective_cfg {
            for attr in &parsed.attrs {
                reject_conditional_module_path(&attr.meta)
                    .map_err(|e| format!("{module} in {}: {e}", path.display()))?;
            }
            self.items(&parsed.items, path, directory, module, cfg)?;
        }
        self.stack.remove(&canonical);
        Ok(effective_cfg)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Keep the bounded set of supported syn item forms in one exhaustive traversal"
    )]
    fn items(
        &mut self,
        items: &[Item],
        file: &Path,
        directory: &ModuleDirectory,
        module: &str,
        parent_cfg: &[String],
    ) -> Result<(), String> {
        self.modules.entry(module.into()).or_default();
        for item in items {
            let attrs = match item {
                Item::Mod(i) => &i.attrs,
                Item::Use(i) => &i.attrs,
                Item::Struct(i) => &i.attrs,
                Item::Enum(i) => &i.attrs,
                Item::Impl(i) => &i.attrs,
                Item::Fn(i) => &i.attrs,
                Item::Trait(i) => &i.attrs,
                Item::Type(i) => &i.attrs,
                Item::Const(i) => &i.attrs,
                Item::Static(i) => &i.attrs,
                Item::Union(i) => &i.attrs,
                Item::Macro(i) => &i.attrs,
                Item::ForeignMod(i) => &i.attrs,
                Item::ExternCrate(i) => &i.attrs,
                _ => continue,
            };
            let Some(cfg) = conditions(parent_cfg, attrs) else {
                continue;
            };
            if let Item::Const(item) = item
                && item.ident == "_"
            {
                self.anonymous_constant(item, &cfg, module, file)?;
                continue;
            }
            let linkage_export = self.surface.name == "c"
                && matches!(item, Item::Fn(_) | Item::Static(_))
                && linkage::exported(attrs, &cfg)
                    .map_err(|error| format!("{module} in {}: {error}", file.display()))?;
            let hidden_name = match item {
                Item::Struct(i) if !public(&i.vis) => Some((
                    &i.ident,
                    BindingKind::Declaration,
                    if matches!(i.fields, syn::Fields::Named(_)) {
                        Namespace::Type
                    } else {
                        Namespace::Both
                    },
                )),
                Item::Enum(i) if !public(&i.vis) => {
                    Some((&i.ident, BindingKind::Enum, Namespace::Type))
                }
                Item::Trait(i) if !public(&i.vis) => {
                    Some((&i.ident, BindingKind::Declaration, Namespace::Type))
                }
                Item::Union(i) if !public(&i.vis) => {
                    Some((&i.ident, BindingKind::Declaration, Namespace::Type))
                }
                Item::Fn(i) if !public(&i.vis) => {
                    Some((&i.sig.ident, BindingKind::Value, Namespace::Value))
                }
                Item::Static(i) if !public(&i.vis) => {
                    Some((&i.ident, BindingKind::Value, Namespace::Value))
                }
                _ => None,
            };
            if let Some((name, kind, namespace)) = hidden_name {
                // Hidden locals still shadow imports; registering a binding
                // does not expose the private declaration in the inventory.
                let mut binding = Binding::new(format!("{module}::{name}")).with_conditions(&cfg);
                binding.kind = kind;
                binding.namespace = namespace;
                self.local(module, &name.to_string(), binding, false);
            }
            let public_name = match item {
                Item::Struct(i) if public(&i.vis) => Some((&i.ident, "struct")),
                Item::Enum(i) if public(&i.vis) => Some((&i.ident, "enum")),
                Item::Trait(i) if public(&i.vis) => Some((&i.ident, "trait")),
                Item::Fn(i) if public(&i.vis) || linkage_export => Some((&i.sig.ident, "function")),
                Item::Type(i) => Some((&i.ident, "alias")),
                Item::Union(i) if public(&i.vis) => Some((&i.ident, "union")),
                Item::Const(i) => Some((&i.ident, "constant")),
                Item::Static(i) if public(&i.vis) || linkage_export => Some((&i.ident, "static")),
                _ => None,
            };
            if let Some((name, kind)) = public_name {
                if matches!(kind, "struct" | "enum" | "trait" | "union" | "alias") {
                    self.local_types.insert(format!("{module}::{name}"));
                }
                let anchors = self.attribute_macros(
                    &format!("{module}::{name}"),
                    kind,
                    attrs,
                    &cfg,
                    AttributeInput {
                        module,
                        file,
                        tokens: &tokens(item),
                    },
                )?;
                if kind == "constant" {
                    self.constants
                        .entry(format!("{module}::{name}"))
                        .or_default()
                        .extend(anchors);
                } else if linkage_export {
                    self.linkage_exports.extend(anchors);
                }
            }
            match item {
                Item::ExternCrate(i) => {
                    let name = i
                        .rename
                        .as_ref()
                        .map_or(&i.ident, |(_, alias)| alias)
                        .to_string();
                    let path = format!("{module}::{name}");
                    let is_self = i.ident == "self";
                    let binding = Binding::crate_alias(path.clone(), is_self).with_conditions(&cfg);
                    self.local(module, &name, binding.clone(), public(&i.vis));
                    if matches!(i.vis, Visibility::Restricted(_)) {
                        self.modules
                            .get_mut(module)
                            .unwrap()
                            .restricted_bindings
                            .entry(name.clone())
                            .or_default()
                            .insert(binding);
                    }
                    let mut declaration = i.clone();
                    declaration.attrs.retain(|attr| {
                        attr.path().is_ident("macro_use") || is_declaration_metadata(&attr.meta)
                    });
                    let mut imported = Some(BTreeSet::new());
                    for attr in attrs {
                        if has_macro_attribute(&attr.meta, "macro_use")? {
                            let Some(names) = macro_use_names(&attr.meta)? else {
                                imported = None;
                                break;
                            };
                            imported.as_mut().unwrap().extend(names);
                        }
                    }
                    let imported_macros = imported.as_ref().is_none_or(|names| !names.is_empty());
                    let shape = if imported_macros {
                        format!(
                            "{} macro_use_order={}",
                            tokens(&declaration),
                            self.macro_crates.len()
                        )
                    } else {
                        tokens(&declaration)
                    };
                    let id = self.add(&path, "extern_crate", shape, &cfg, file)?;
                    let anchors = self.attribute_macros(
                        &path,
                        "extern_crate",
                        attrs,
                        &cfg,
                        AttributeInput {
                            module,
                            file,
                            tokens: &tokens(item),
                        },
                    )?;
                    let scope = self.modules.get_mut(module).unwrap();
                    if public(&i.vis) {
                        scope.macro_anchors.extend(anchors.iter().cloned());
                    }
                    if imported_macros {
                        self.macro_crates.insert(id.clone(), imported);
                        self.macro_inputs.insert(
                            id.clone(),
                            (module.into(), anchors.iter().cloned().collect()),
                        );
                        scope.macro_scope.insert(id.clone());
                        scope.macro_scope.extend(anchors.iter().cloned());
                        scope.macro_order += 1;
                    }
                    scope.extern_crates.push(ExternCrate {
                        name,
                        is_self,
                        public: public(&i.vis),
                        declaration: id,
                        macro_anchors: anchors,
                    });
                }
                Item::ForeignMod(i) => {
                    // Register public names so ordinary export/root resolution
                    // reaches an explicit error instead of silently omitting
                    // an interface the scanner does not yet model.
                    for item in &i.items {
                        let (name, vis, attrs, is_type) = match item {
                            syn::ForeignItem::Fn(i) => (&i.sig.ident, &i.vis, &i.attrs, false),
                            syn::ForeignItem::Static(i) => (&i.ident, &i.vis, &i.attrs, false),
                            syn::ForeignItem::Type(i) => (&i.ident, &i.vis, &i.attrs, true),
                            syn::ForeignItem::Macro(mac) => {
                                if conditions(&cfg, &mac.attrs).is_some() {
                                    // No output names are known before expansion.
                                    // Defer rejection until this source scope is
                                    // reached, including through macro helpers.
                                    self.unsupported_foreign_macros.entry(module.into()).or_default().push(format!(
                                        "unsupported foreign-item macro {} in {module} ({}); use explicit foreign declarations or add foreign-macro discovery support",
                                        tokens(&mac.mac), file.strip_prefix(self.root).unwrap_or(file).display()
                                    ));
                                }
                                continue;
                            }
                            _ => continue,
                        };
                        if !public(vis) {
                            continue;
                        }
                        let Some(cfg) = conditions(&cfg, attrs) else {
                            continue;
                        };
                        let path = format!("{module}::{name}");
                        self.local(
                            module,
                            &name.to_string(),
                            if is_type {
                                Binding::new(path.clone())
                            } else {
                                Binding::value(path.clone())
                            }
                            .with_conditions(&cfg),
                            true,
                        );
                        self.unsupported_foreign.entry(path.clone())
                            .and_modify(|foreign| foreign.is_type |= is_type)
                            .or_insert_with(|| UnsupportedForeign {
                                message: format!(
                                    "unsupported public foreign declaration {path} in {}; expose a Rust wrapper or add explicit foreign-item discovery support",
                                    file.strip_prefix(self.root).unwrap_or(file).to_string_lossy().replace('\\', "/"),
                                ),
                                is_type,
                            });
                    }
                }
                Item::Mod(i) => {
                    let name = i.ident.to_string();
                    let child = format!("{module}::{name}");
                    let binding = Binding::module(child.clone()).with_conditions(&cfg);
                    self.local(module, &name, binding.clone(), public(&i.vis));
                    if matches!(i.vis, Visibility::Restricted(_)) {
                        self.modules
                            .get_mut(module)
                            .unwrap()
                            .restricted_bindings
                            .entry(name.clone())
                            .or_default()
                            .insert(binding);
                    }
                    let anchors = self.attribute_macros(
                        &child,
                        "module",
                        attrs,
                        &cfg,
                        AttributeInput {
                            module,
                            file,
                            tokens: &tokens(item),
                        },
                    )?;
                    self.modules
                        .entry(child.clone())
                        .or_default()
                        .macro_anchors
                        .extend(anchors);
                    // Child modules inherit textual macros available where the
                    // module is declared, including macros from earlier macro_use.
                    let inherited = self.modules[module].macro_scope.clone();
                    self.modules.get_mut(&child).unwrap().macro_scope = inherited;
                    let explicit = module_path(&i.attrs)
                        .map_err(|e| format!("{child} in {}: {e}", file.display()))?;
                    let module_cfg = if let Some((_, items)) = &i.content {
                        let submodules = explicit.map_or_else(
                            || directory.submodules.join(&name),
                            |path| directory.path_attributes.join(path),
                        );
                        self.items(
                            items,
                            file,
                            &ModuleDirectory::shared(submodules),
                            &child,
                            &cfg,
                        )?;
                        Some(cfg.clone())
                    } else {
                        let (path, child_directory) = if let Some(explicit) = explicit {
                            let path = directory.path_attributes.join(explicit);
                            // A path-attributed file owns its containing directory,
                            // irrespective of the chosen filename or module name.
                            let child_directory =
                                ModuleDirectory::shared(path.parent().unwrap().to_path_buf());
                            (path, child_directory)
                        } else {
                            let submodules = directory.submodules.join(&name);
                            let flat = directory.submodules.join(format!("{name}.rs"));
                            let nested = submodules.join("mod.rs");
                            if flat.exists() && nested.exists() {
                                return Err(format!("ambiguous module {child}"));
                            }
                            if flat.exists() {
                                (
                                    flat,
                                    ModuleDirectory {
                                        submodules,
                                        path_attributes: directory.submodules.clone(),
                                    },
                                )
                            } else {
                                (nested, ModuleDirectory::shared(submodules))
                            }
                        };
                        self.file(&path, &child, &cfg, &child_directory)?
                    };
                    if let Some(module_cfg) = &module_cfg {
                        for attr in attrs {
                            if has_macro_attribute(&attr.meta, "macro_use")? {
                                // Import order can select between same-named
                                // definitions. Review that order as well as the
                                // imported definitions, without private bodies.
                                let order = self.modules[module].macro_order;
                                let id = self.add(
                                    &format!("{module}::@macro:macro_use:{name}"),
                                    "macro",
                                    format!("macro_use {child} at macro position {order}"),
                                    module_cfg,
                                    file,
                                )?;
                                let scope = &self.modules[&child];
                                let mut imported = scope.macro_scope.clone();
                                imported.extend(scope.macro_anchors.iter().cloned());
                                self.macro_inputs
                                    .insert(id.clone(), (child.clone(), imported));
                                self.module_macro_imports.insert(id.clone());
                                let parent = self.modules.get_mut(module).unwrap();
                                parent.macro_scope.insert(id);
                                parent.macro_order += 1;
                                break;
                            }
                        }
                    }
                    if public(&i.vis)
                        && let Some(cfg) = module_cfg
                    {
                        let id =
                            self.add(&child, "module", format!("pub mod {name}"), &cfg, file)?;
                        self.modules.get_mut(&child).unwrap().declarations.push(id);
                    }
                }
                Item::Use(i) => {
                    let mut imports = Vec::new();
                    flatten_use(
                        &i.tree,
                        NamePath {
                            absolute: i.leading_colon.is_some(),
                            segments: Vec::new(),
                        },
                        &mut imports,
                    );
                    // One attribute invocation sees the entire use tree, even
                    // when it binds several names or no concrete names at all.
                    let anchors = self.attribute_macros(
                        &format!(
                            "{module}::@use:{}{}",
                            tokens(&i.leading_colon),
                            tokens(&i.tree)
                        ),
                        "reexport",
                        attrs,
                        &cfg,
                        AttributeInput {
                            module,
                            file,
                            tokens: &tokens(item),
                        },
                    )?;
                    // Like item macros, a public reexport's generator can add
                    // names unrelated to any of the imported bindings.
                    if public(&i.vis) {
                        self.modules
                            .get_mut(module)
                            .unwrap()
                            .macro_anchors
                            .extend(anchors.iter().cloned());
                    }
                    for (path, alias) in imports {
                        let key = format!(
                            "{module}::@use:{path}{}",
                            alias
                                .as_ref()
                                .map(|a| format!(" as {a}"))
                                .unwrap_or_default()
                        );
                        // Imports are retained through exports or API type dependencies.
                        let shape = format!(
                            "use {path}{}{}",
                            alias
                                .as_ref()
                                .map(|a| format!(" as {a}"))
                                .unwrap_or_default(),
                            dependency_visibility(&i.vis),
                        );
                        let id = self.add(&key, "reexport", shape, &cfg, file)?;
                        self.modules.get_mut(module).unwrap().imports.push(Import {
                            path,
                            alias,
                            public: public(&i.vis),
                            accessible: !matches!(i.vis, Visibility::Inherited),
                            declaration: id,
                            conditions: cfg.clone(),
                            macro_anchors: anchors.clone(),
                        });
                    }
                }
                Item::Impl(i) => {
                    let Some(owner) = named_type_path(&i.self_ty) else {
                        if i.trait_.is_some() {
                            let mut generating = false;
                            for attribute in &i.attrs {
                                generating |= requires_macro_review(&attribute.meta)?;
                            }
                            if !generating {
                                // Ordinary trait implementations remain outside
                                // declaration review, including non-path owners.
                                continue;
                            }
                        }
                        return Err(format!(
                            "unsupported {} impl owner `{}` in {}",
                            if i.trait_.is_some() {
                                "attributed trait"
                            } else {
                                "inherent"
                            },
                            tokens(&i.self_ty),
                            file.display()
                        ));
                    };
                    let arguments = owner
                        .path
                        .segments
                        .last()
                        .map(|s| s.arguments.clone())
                        .unwrap_or_default();
                    let ty = NamePath::from(&owner.path);
                    let mut context = generics_shape(&i.generics);
                    if owner
                        .path
                        .segments
                        .iter()
                        .any(|s| !matches!(s.arguments, syn::PathArguments::None))
                    {
                        let _ = write!(context, " for {}", tokens(owner));
                    }
                    let mut members = Vec::new();
                    // A generating outer attribute needs the complete trait
                    // impl input, but ordinary trait members are not standalone
                    // public declarations of the implementing type.
                    let inherent_members = if i.trait_.is_none() {
                        i.items.as_slice()
                    } else {
                        &[]
                    };
                    for member in inherent_members {
                        let (name, kind, shape, attrs) = match member {
                            syn::ImplItem::Fn(method) => (
                                method.sig.ident.to_string(),
                                "method",
                                format!(
                                    "{} {} {}",
                                    attributes(&method.attrs),
                                    context,
                                    tokens(&method.sig)
                                ),
                                &method.attrs,
                            ),
                            syn::ImplItem::Const(value) => {
                                let (name, ty, expression) = (&value.ident, &value.ty, &value.expr);
                                (
                                    name.to_string(),
                                    "associated_constant",
                                    format!(
                                        "{context} {}{}",
                                        tokens(&quote!(const #name: #ty = #expression)),
                                        dependency_visibility(&value.vis)
                                    ),
                                    &value.attrs,
                                )
                            }
                            syn::ImplItem::Macro(value) => (
                                format!(
                                    "@macro:{}!({})",
                                    tokens(&value.mac.path),
                                    tokens(&value.mac.tokens)
                                ),
                                "macro",
                                format!("{context} {}", tokens(&value.mac)),
                                &value.attrs,
                            ),
                            _ => continue,
                        };
                        if let Some(cfg) = conditions(&cfg, attrs) {
                            let generics = match member {
                                syn::ImplItem::Fn(method) => {
                                    vec![&i.generics, &method.sig.generics]
                                }
                                _ => vec![&i.generics],
                            };
                            let references = References::collect(&generics, |references| {
                                references.visit_generics(&i.generics);
                                references.visit_impl_type(&i.self_ty);
                                match member {
                                    syn::ImplItem::Fn(method) => {
                                        references.visit_signature(&method.sig);
                                    }
                                    syn::ImplItem::Const(value) => {
                                        references.visit_type(&value.ty);
                                        references.visit_expr(&value.expr);
                                    }
                                    _ => {}
                                }
                            });
                            members.push(PendingMember {
                                name,
                                kind,
                                public: match member {
                                    syn::ImplItem::Fn(method) => public(&method.vis),
                                    syn::ImplItem::Const(value) => public(&value.vis),
                                    _ => true,
                                },
                                shape,
                                conditions: cfg,
                                source: file.into(),
                                attributes: attrs.clone(),
                                input: tokens(member),
                                references,
                                macro_scope: matches!(member, syn::ImplItem::Macro(_))
                                    .then(|| self.modules[module].macro_scope.clone()),
                                macro_path: match member {
                                    syn::ImplItem::Macro(item) => {
                                        Some(NamePath::from(&item.mac.path))
                                    }
                                    _ => None,
                                },
                            });
                        }
                    }
                    self.implementations.push(PendingImpl {
                        module: module.into(),
                        ty,
                        arguments,
                        members,
                        attributes: i.attrs.clone(),
                        conditions: cfg,
                        source: file.into(),
                        input: tokens(item),
                        references: References::collect(&[&i.generics], |references| {
                            references.visit_generics(&i.generics);
                            references.visit_impl_type(&i.self_ty);
                            if let Some((path, _)) = &i.trait_ {
                                references.select_type_path(path);
                            }
                        }),
                    });
                }
                Item::Struct(i) if public(&i.vis) => {
                    let path = format!("{module}::{}", i.ident);
                    let binding = Binding::new(path.clone()).with_conditions(&cfg);
                    self.local(module, &i.ident.to_string(), binding.clone(), true);
                    if !matches!(i.fields, syn::Fields::Named(_)) {
                        self.constructors.register(module, i, &cfg);
                        let mut constructor = binding;
                        constructor.namespace = Namespace::Value;
                        self.local(module, &i.ident.to_string(), constructor.clone(), false);
                        let scope = self.modules.get_mut(module).unwrap();
                        scope
                            .restricted_bindings
                            .entry(i.ident.to_string())
                            .or_default()
                            .insert(constructor.clone());
                        if let Some(constructor) = self.constructors.visible(constructor, None) {
                            scope
                                .public
                                .entry(i.ident.to_string())
                                .or_default()
                                .insert(constructor);
                        }
                    }
                    let id = self.add(
                        &path,
                        "struct",
                        format!(
                            "{} struct {} {} {}",
                            attributes(&i.attrs),
                            i.ident,
                            generics_shape(&i.generics),
                            struct_construction_shape(&i.fields)
                        ),
                        &cfg,
                        file,
                    )?;
                    self.reference(&id, module, &[&i.generics], |references| {
                        references.set_self_type(&i.ident);
                        references.visit_generics(&i.generics);
                    });
                    // Test-only fields occupy no position in the audited API.
                    // Private and symbolically conditional fields still count.
                    for (index, (field, cfg)) in i
                        .fields
                        .iter()
                        .filter_map(|field| conditions(&cfg, &field.attrs).map(|cfg| (field, cfg)))
                        .enumerate()
                    {
                        if public(&field.vis) {
                            let name = field
                                .ident
                                .as_ref()
                                .map_or_else(|| index.to_string(), ToString::to_string);
                            let id = self.member(
                                &path,
                                &name,
                                "field",
                                format!("{index}: {}", tokens(&field.ty)),
                                &cfg,
                                file,
                            )?;
                            self.reference(&id, module, &[&i.generics], |references| {
                                references.set_self_type(&i.ident);
                                references.visit_type(&field.ty);
                            });
                        }
                    }
                }
                Item::Enum(i) if public(&i.vis) => {
                    let path = format!("{module}::{}", i.ident);
                    self.local(
                        module,
                        &i.ident.to_string(),
                        Binding::enumeration(path.clone()).with_conditions(&cfg),
                        true,
                    );
                    let id = self.add(
                        &path,
                        "enum",
                        format!(
                            "{} enum {} {}",
                            attributes(&i.attrs),
                            i.ident,
                            generics_shape(&i.generics)
                        ),
                        &cfg,
                        file,
                    )?;
                    self.reference(&id, module, &[&i.generics], |references| {
                        references.set_self_type(&i.ident);
                        references.visit_generics(&i.generics);
                    });
                    self.enum_exports.entry(path.clone()).or_default();
                    let mut previous = Vec::new();
                    for (index, (variant, cfg)) in i
                        .variants
                        .iter()
                        .filter_map(|variant| {
                            conditions(&cfg, &variant.attrs).map(|cfg| (variant, cfg))
                        })
                        .enumerate()
                    {
                        self.enum_exports
                            .get_mut(&path)
                            .unwrap()
                            .entry(variant.ident.to_string())
                            .or_default()
                            .insert(
                                Binding::variant(format!("{path}::{}", variant.ident))
                                    .with_conditions(&cfg),
                            );
                        let discriminant = variant
                            .discriminant
                            .as_ref()
                            .map(|(_, e)| tokens(e))
                            .unwrap_or_default();
                        let mut fields = variant
                            .fields
                            .iter()
                            .filter_map(enum_field_shape)
                            .collect::<Vec<_>>()
                            .join(", ");
                        if fields.is_empty() {
                            match &variant.fields {
                                syn::Fields::Named(_) => fields = "named: ".into(),
                                syn::Fields::Unnamed(_) => fields = "unnamed: ".into(),
                                syn::Fields::Unit => {}
                            }
                        }
                        let mut shape =
                            format!("{index}: {}({fields}) = {discriminant}", variant.ident);
                        let metadata = attributes(&variant.attrs);
                        if !metadata.is_empty() {
                            let _ = write!(shape, " {metadata}");
                        }
                        let id = self.member(
                            &path,
                            &variant.ident.to_string(),
                            "variant",
                            shape,
                            &cfg,
                            file,
                        )?;
                        self.reference(&id, module, &[&i.generics], |references| {
                            references.set_self_type(&i.ident);
                            for field in &variant.fields {
                                if conditions(&[], &field.attrs).is_some() {
                                    references.visit_type(&field.ty);
                                }
                            }
                            if let Some((_, discriminant)) = &variant.discriminant {
                                references.visit_expr(discriminant);
                            }
                        });
                        let value = self.add(
                            &format!("{path}::{}", variant.ident),
                            "discriminant",
                            format!(
                                "{index}: {}",
                                variant.discriminant.as_ref().map_or_else(
                                    || "implicit".into(),
                                    |(_, expression)| tokens(expression)
                                )
                            ),
                            &cfg,
                            file,
                        )?;
                        self.reference(&value, module, &[&i.generics], |references| {
                            references.set_self_type(&i.ident);
                            if let Some((_, expression)) = &variant.discriminant {
                                references.visit_expr(expression);
                            }
                        });
                        self.discriminants.insert(
                            &id,
                            &value,
                            &previous,
                            &cfg,
                            variant.discriminant.is_none(),
                        );
                        previous.push((id, value, cfg));
                    }
                }
                Item::Trait(i) if public(&i.vis) => {
                    let path = format!("{module}::{}", i.ident);
                    self.local(
                        module,
                        &i.ident.to_string(),
                        Binding::new(path.clone()).with_conditions(&cfg),
                        true,
                    );
                    let generics = &i.generics;
                    let supertraits = &i.supertraits;
                    let id = self.add(
                        &path,
                        "trait",
                        format!(
                            "{} {} {} {}",
                            tokens(&i.unsafety),
                            i.ident,
                            generics_shape(generics),
                            tokens(supertraits)
                        ),
                        &cfg,
                        file,
                    )?;
                    self.reference(&id, module, &[&i.generics], |references| {
                        references.set_self_type(&i.ident);
                        references.visit_generics(&i.generics);
                        for bound in &i.supertraits {
                            references.visit_type_param_bound(bound);
                        }
                    });
                    for member in &i.items {
                        let attrs: &[Attribute] = match member {
                            syn::TraitItem::Fn(member) => &member.attrs,
                            syn::TraitItem::Type(member) => &member.attrs,
                            syn::TraitItem::Const(member) => &member.attrs,
                            syn::TraitItem::Macro(member) => &member.attrs,
                            _ => &[],
                        };
                        let Some(cfg) = conditions(&cfg, attrs) else {
                            continue;
                        };
                        match member {
                            syn::TraitItem::Fn(method) => {
                                let id = self.member(
                                    &path,
                                    &method.sig.ident.to_string(),
                                    "trait_method",
                                    format!(
                                        "{} default={}",
                                        tokens(&method.sig),
                                        method.default.is_some()
                                    ),
                                    &cfg,
                                    file,
                                )?;
                                self.reference(
                                    &id,
                                    module,
                                    &[&i.generics, &method.sig.generics],
                                    |references| {
                                        references.visit_signature(&method.sig);
                                    },
                                );
                                let anchors = self.attribute_macros(
                                    &format!("{path}::{}", method.sig.ident),
                                    "trait_method",
                                    &method.attrs,
                                    &cfg,
                                    AttributeInput {
                                        module,
                                        file,
                                        tokens: &tokens(member),
                                    },
                                )?;
                                self.attach(&path, ChildOwner::Type, anchors);
                            }
                            syn::TraitItem::Type(ty) => {
                                let id = self.member(
                                    &path,
                                    &ty.ident.to_string(),
                                    "associated_type",
                                    tokens(ty),
                                    &cfg,
                                    file,
                                )?;
                                self.reference(
                                    &id,
                                    module,
                                    &[&i.generics, &ty.generics],
                                    |references| {
                                        references.visit_trait_item_type(ty);
                                    },
                                );
                                let anchors = self.attribute_macros(
                                    &format!("{path}::{}", ty.ident),
                                    "associated_type",
                                    &ty.attrs,
                                    &cfg,
                                    AttributeInput {
                                        module,
                                        file,
                                        tokens: &tokens(member),
                                    },
                                )?;
                                self.attach(&path, ChildOwner::Type, anchors);
                            }
                            _ => {
                                self.unsupported_trait_members
                                    .entry(id.clone())
                                    .or_insert_with(|| {
                                        format!(
                                            "unsupported audited trait member in {path}: {}",
                                            tokens(member)
                                        )
                                    });
                            }
                        }
                    }
                }
                Item::Fn(i) if public(&i.vis) || linkage_export => {
                    let path = format!("{module}::{}", i.sig.ident);
                    self.local(
                        module,
                        &i.sig.ident.to_string(),
                        Binding::value(path.clone()).with_conditions(&cfg),
                        public(&i.vis),
                    );
                    let id = self.add(
                        &path,
                        "function",
                        format!("{} {}", attributes(&i.attrs), tokens(&i.sig)),
                        &cfg,
                        file,
                    )?;
                    self.reference(&id, module, &[&i.sig.generics], |references| {
                        references.visit_signature(&i.sig);
                    });
                    if linkage_export {
                        self.linkage_exports.insert(id);
                    }
                }
                Item::Type(i) => {
                    let path = format!("{module}::{}", i.ident);
                    self.local(
                        module,
                        &i.ident.to_string(),
                        Binding::new(path.clone()).with_conditions(&cfg),
                        public(&i.vis),
                    );
                    if matches!(i.vis, Visibility::Restricted(_)) {
                        self.modules
                            .get_mut(module)
                            .unwrap()
                            .restricted_bindings
                            .entry(i.ident.to_string())
                            .or_default()
                            .insert(Binding::new(path.clone()).with_conditions(&cfg));
                    }
                    if let Some(ty) = named_type_path(&i.ty) {
                        self.aliases.push(Alias {
                            path: path.clone(),
                            module: module.into(),
                            target: ty.clone(),
                            generics: i.generics.clone(),
                            conditions: cfg.clone(),
                        });
                    }
                    let (ident, generics, ty) = (&i.ident, &i.generics, &i.ty);
                    let clause = &i.generics.where_clause;
                    let id = self.add(
                        &path,
                        "alias",
                        format!(
                            "{}{}",
                            tokens(&quote!(type #ident #generics #clause = #ty)),
                            dependency_visibility(&i.vis)
                        ),
                        &cfg,
                        file,
                    )?;
                    self.reference(&id, module, &[&i.generics], |references| {
                        references.set_self_type(&i.ident);
                        references.visit_generics(&i.generics);
                        references.visit_type(&i.ty);
                    });
                }
                Item::Const(i) => {
                    let path = format!("{module}::{}", i.ident);
                    let binding = Binding::value(path.clone()).with_conditions(&cfg);
                    self.local(
                        module,
                        &i.ident.to_string(),
                        binding.clone(),
                        public(&i.vis),
                    );
                    if matches!(i.vis, Visibility::Restricted(_)) {
                        self.modules
                            .get_mut(module)
                            .unwrap()
                            .restricted_bindings
                            .entry(i.ident.to_string())
                            .or_default()
                            .insert(binding);
                    }
                    let (ident, ty, expr) = (&i.ident, &i.ty, &i.expr);
                    let id = self.add(
                        &path,
                        "constant",
                        format!(
                            "{}{}",
                            tokens(&quote!(const #ident: #ty = #expr)),
                            dependency_visibility(&i.vis)
                        ),
                        &cfg,
                        file,
                    )?;
                    self.constants.entry(path).or_default().insert(id.clone());
                    self.reference(&id, module, &[], |references| {
                        references.visit_type(&i.ty);
                        references.visit_expr(&i.expr);
                    });
                }
                Item::Static(i) if public(&i.vis) || linkage_export => {
                    let path = format!("{module}::{}", i.ident);
                    self.local(
                        module,
                        &i.ident.to_string(),
                        Binding::value(path.clone()).with_conditions(&cfg),
                        public(&i.vis),
                    );
                    let (name, mutable, ty, expression) = (&i.ident, &i.mutability, &i.ty, &i.expr);
                    let mut shape = tokens(&quote!(static #mutable #name: #ty = #expression));
                    let metadata = attributes(&i.attrs);
                    if !metadata.is_empty() {
                        shape = format!("{metadata} {shape}");
                    }
                    let id = self.add(&path, "static", shape, &cfg, file)?;
                    self.reference(&id, module, &[], |references| {
                        references.visit_type(&i.ty);
                        references.visit_expr(&i.expr);
                    });
                    if linkage_export {
                        self.linkage_exports.insert(id);
                    }
                }
                Item::Union(i) if public(&i.vis) => {
                    let path = format!("{module}::{}", i.ident);
                    self.local(
                        module,
                        &i.ident.to_string(),
                        Binding::new(path.clone()).with_conditions(&cfg),
                        true,
                    );
                    let id = self.add(
                        &path,
                        "union",
                        format!(
                            "{} union {} {}",
                            attributes(&i.attrs),
                            i.ident,
                            generics_shape(&i.generics)
                        ),
                        &cfg,
                        file,
                    )?;
                    self.reference(&id, module, &[&i.generics], |references| {
                        references.set_self_type(&i.ident);
                        references.visit_generics(&i.generics);
                    });
                    for (index, (field, cfg)) in i
                        .fields
                        .named
                        .iter()
                        .filter_map(|field| conditions(&cfg, &field.attrs).map(|cfg| (field, cfg)))
                        .enumerate()
                    {
                        if public(&field.vis) {
                            let id = self.member(
                                &path,
                                &field.ident.as_ref().unwrap().to_string(),
                                "field",
                                format!("{index}: {}", tokens(&field.ty)),
                                &cfg,
                                file,
                            )?;
                            self.reference(&id, module, &[&i.generics], |references| {
                                references.set_self_type(&i.ident);
                                references.visit_type(&field.ty);
                            });
                        }
                    }
                }
                Item::Macro(i) => {
                    let parsed_enum = syn::parse2::<syn::ItemEnum>(i.mac.tokens.clone()).ok();
                    let name = i.ident.as_ref().map_or_else(
                        || {
                            // An invocation identity is deliberately derived from its tokens: changing
                            // an unresolved invocation must invalidate its reviewed supplement.
                            parsed_enum.as_ref().map_or_else(
                                || format!("{}!({})", tokens(&i.mac.path), tokens(&i.mac.tokens)),
                                |e| format!("{}:{}", tokens(&i.mac.path), e.ident),
                            )
                        },
                        ToString::to_string,
                    );
                    let path = format!("{module}::@macro:{name}");
                    let mut attributed = false;
                    for attr in attrs {
                        attributed |= requires_macro_review(&attr.meta)
                            .map_err(|e| format!("{path}: {e}"))?;
                    }
                    let shape = if attributed {
                        // An outer generator can inspect every attribute and
                        // the unnormalized input, or replace the whole item.
                        tokens(i)
                    } else {
                        // Parsing may shorten the identity, but every invocation
                        // token remains an opaque input to the generator.
                        tokens(&i.mac)
                    };
                    let id = self.add(&path, "macro", shape, &cfg, file)?;
                    self.macro_input(&id, module);
                    if attributed {
                        for attribute in attrs {
                            self.attribute_providers(&id, module, &attribute.meta)?;
                        }
                    }
                    let scope = self.modules.get_mut(module).unwrap();
                    // An earlier invocation can itself introduce textual macros.
                    scope.macro_scope.insert(id.clone());
                    scope.item_macros.insert(id.clone());
                    scope.macro_order += 1;
                    let mut exported = false;
                    for attribute in attrs {
                        exported |= has_macro_attribute(&attribute.meta, "macro_export")?;
                    }
                    if let Some(name) = &i.ident {
                        let name = binding_name(&name.to_string()).to_owned();
                        self.macro_expansions.insert(
                            id.clone(),
                            (module.into(), MacroReferences::collect(&i.mac)),
                        );
                        self.macro_definitions
                            .entry((module.into(), name.clone()))
                            .or_default()
                            .insert(id.clone());
                        if exported {
                            self.macro_definitions
                                .entry((self.surface.crate_name.clone(), name))
                                .or_default()
                                .insert(id.clone());
                        }
                    } else {
                        self.macro_calls
                            .insert(id.clone(), (module.into(), NamePath::from(&i.mac.path)));
                    }
                    if exported {
                        self.exported_macros.insert(id, module.into());
                    }
                    self.attach(module, ChildOwner::Module, [format!("@macro:{path}")]);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn member(
        &mut self,
        owner: &str,
        name: &str,
        kind: &str,
        shape: String,
        cfg: &[String],
        file: &Path,
    ) -> Result<String, String> {
        let path = format!("{owner}::{name}");
        let id = self.add(&path, kind, shape, cfg, file)?;
        self.attach(owner, ChildOwner::Type, [id.clone()]);
        Ok(id)
    }

    fn attribute_macros(
        &mut self,
        owner: &str,
        declaration_kind: &str,
        attrs: &[Attribute],
        cfg: &[String],
        input: AttributeInput<'_>,
    ) -> Result<Vec<String>, String> {
        let mut helpers = false;
        if matches!(declaration_kind, "struct" | "enum") {
            for attr in attrs {
                helpers |= attributes::has_helper(&attr.meta)?;
            }
        }
        if helpers {
            self.pending_helpers.push(PendingAttributes {
                owner: owner.into(),
                module: input.module.into(),
                kind: declaration_kind.into(),
                attributes: attrs.to_vec(),
                conditions: cfg.to_vec(),
                source: input.file.into(),
                input: input.tokens.into(),
            });
            return Ok(Vec::new());
        }
        self.register_attributes(owner, declaration_kind, attrs, cfg, input, &BTreeSet::new())
    }

    fn register_attributes(
        &mut self,
        owner: &str,
        declaration_kind: &str,
        attrs: &[Attribute],
        cfg: &[String],
        input: AttributeInput<'_>,
        helpers: &BTreeSet<Vec<String>>,
    ) -> Result<Vec<String>, String> {
        let mut anchors = Vec::new();
        for attr in attrs {
            let anchor = if attributes::requires_review(&attr.meta, cfg, helpers)
                .map_err(|e| format!("{owner}: {e}"))?
            {
                Some(self.attribute_anchor(owner, declaration_kind, attr, cfg, input)?)
            } else {
                None
            };
            let paths = derives::paths(&attr.meta)?;
            if !paths.is_empty() {
                self.pending_derives.push(PendingDerive {
                    owner: owner.into(),
                    module: input.module.into(),
                    kind: declaration_kind.into(),
                    attribute: attr.clone(),
                    paths,
                    conditions: cfg.to_vec(),
                    source: input.file.into(),
                    input: input.tokens.into(),
                    anchor: anchor.clone(),
                });
            }
            anchors.extend(anchor);
        }
        Ok(anchors)
    }

    fn attribute_anchor(
        &mut self,
        owner: &str,
        declaration_kind: &str,
        attr: &Attribute,
        cfg: &[String],
        input: AttributeInput<'_>,
    ) -> Result<String, String> {
        let path = format!(
            "{owner}::@macro:attribute:{declaration_kind}:{}",
            tokens(&attr.meta)
        );
        // Generators can inspect private fields, helper attributes and bodies.
        let id = self.add(
            &path,
            "macro",
            format!("{} on {}", tokens(&attr.meta), input.tokens),
            cfg,
            input.file,
        )?;
        let owner_kind = ChildOwner::declaration(declaration_kind);
        if owner_kind != ChildOwner::Type {
            self.children
                .get_mut(&path)
                .unwrap()
                .get_mut(&ChildOwner::Type)
                .unwrap()
                .retain(|child| child != &id);
            self.attach(&path, owner_kind, [id.clone()]);
        }
        self.attach(owner, owner_kind, [id.clone()]);
        if declaration_kind == "constant" {
            self.constants
                .entry(owner.into())
                .or_default()
                .insert(id.clone());
        }
        self.attribute_providers(&id, input.module, &attr.meta)?;
        Ok(id)
    }

    fn attribute_providers(
        &mut self,
        id: &str,
        module: &str,
        meta: &syn::Meta,
    ) -> Result<(), String> {
        let paths = attributes::provider_paths(meta)?;
        if !paths.is_empty() {
            let entry = self
                .attribute_inputs
                .entry(id.into())
                .or_insert_with(|| (module.into(), Vec::new()));
            entry.1.extend(paths);
            self.macro_inputs
                .entry(id.into())
                .or_insert_with(|| (module.into(), BTreeSet::new()));
        }
        Ok(())
    }

    fn local(&mut self, module: &str, name: &str, binding: Binding, visible: bool) {
        let name = binding_name(name);
        let m = self.modules.get_mut(module).unwrap();
        m.locals
            .entry(name.into())
            .or_default()
            .insert(binding.clone());
        if visible {
            m.public.entry(name.into()).or_default().insert(binding);
        }
    }

    /// Retain the import declarations along an export path, even when its
    /// ultimate target belongs to an external dependency and cannot be resolved.
    fn retain_reexports(
        &self,
        module: &str,
        path: &NamePath,
        bindings: &Bindings,
        reachable: &mut BTreeSet<String>,
        source_modules: &mut BTreeSet<String>,
    ) {
        self.retain_imports(
            module,
            path,
            bindings,
            reachable,
            source_modules,
            ImportSelection::Export,
        );
    }

    fn retain_extern_crates(
        &self,
        module: &str,
        name: &str,
        reachable: &mut BTreeSet<String>,
        source_modules: &mut BTreeSet<String>,
    ) -> bool {
        let mut local = false;
        for item in &self.modules[module].extern_crates {
            if binding_name(name) == binding_name(&item.name) || name == "*" && item.public {
                item.retain(reachable);
                source_modules.insert(module.into());
                local |= item.is_self;
            }
        }
        local
    }

    fn normalize_import_path(
        &self,
        module: &mut String,
        path: &mut NamePath,
        reachable: &mut BTreeSet<String>,
        source_modules: &mut BTreeSet<String>,
    ) -> bool {
        if !path.absolute {
            return true;
        }
        let Some(name) = path.segments.first() else {
            return false;
        };
        if !self.retain_extern_crates(&self.surface.crate_name, name, reachable, source_modules) {
            return false;
        }
        module.clone_from(&self.surface.crate_name);
        path.absolute = false;
        path.segments[0] = "crate".into();
        true
    }

    /// Macro names have their own namespace. Resolve module qualifiers with
    /// ordinary bindings, then follow selecting imports in the macro namespace.
    /// Keep all possible routes, including opaque dependency macro imports.
    fn retain_macro_imports(
        &self,
        module: &str,
        path: &NamePath,
        bindings: &Bindings,
        reachable: &mut BTreeSet<String>,
        source_modules: &mut BTreeSet<String>,
    ) {
        let mut pending = vec![(module.to_owned(), path.clone())];
        let mut visited = BTreeSet::new();
        while let Some((mut module, mut path)) = pending.pop() {
            if !self.normalize_import_path(&mut module, &mut path, reachable, source_modules)
                || !visited.insert((module.clone(), path.clone()))
            {
                continue;
            }
            let Some(last) = path.segments.last() else {
                continue;
            };
            let name = binding_name(last).to_owned();
            let parents = if path.segments.len() == 1 {
                BTreeSet::from([Binding::module(module.clone())])
            } else {
                let prefix = path.prefix(path.segments.len() - 1);
                self.retain_imports(
                    &module,
                    &prefix,
                    bindings,
                    reachable,
                    source_modules,
                    ImportSelection::Dependency(Namespace::Type),
                );
                resolve(&module, &prefix, bindings, &self.surface.crate_name).unwrap_or_default()
            };
            for parent in parents {
                let Some(scope_name) = parent.namespace(&self.surface.crate_name) else {
                    continue;
                };
                let Some(scope) = self.modules.get(scope_name) else {
                    continue;
                };
                if let Some(definitions) = self
                    .macro_definitions
                    .get(&(scope_name.into(), name.clone()))
                {
                    reachable.extend(definitions.iter().cloned());
                    source_modules.insert(scope_name.into());
                }
                if !scope.macro_anchors.is_empty() {
                    reachable.extend(scope.macro_anchors.iter().cloned());
                    source_modules.insert(scope_name.into());
                }
                for import in &scope.imports {
                    if import.is_glob() || import.name().as_ref() == Some(&name) {
                        import.retain(reachable);
                        source_modules.insert(scope_name.into());
                        let mut target = import.path.clone();
                        if import.is_glob() {
                            target.segments.last_mut().unwrap().clone_from(&name);
                        }
                        pending.push((scope_name.into(), target));
                    }
                }
            }
        }
    }

    fn retain_imports(
        &self,
        module: &str,
        path: &NamePath,
        bindings: &Bindings,
        reachable: &mut BTreeSet<String>,
        source_modules: &mut BTreeSet<String>,
        selection: ImportSelection,
    ) {
        let mut pending = vec![(module.to_owned(), path.clone())];
        let mut visited = BTreeSet::new();
        while let Some((mut module, mut path)) = pending.pop() {
            if !self.normalize_import_path(&mut module, &mut path, reachable, source_modules)
                || !visited.insert((module.clone(), path.clone()))
            {
                continue;
            }
            for (index, name) in path.segments.iter().enumerate() {
                let name = binding_name(name);
                if matches!(name, "crate" | "self" | "super") {
                    continue;
                }
                let parents = if index == 0 {
                    let shadowed = bindings[&module].get(name).is_some_and(|values| {
                        path.segments.len() == 1
                            || values.is_empty()
                            || values.iter().any(|value| value.kind != BindingKind::Value)
                    });
                    if module != self.surface.crate_name && !shadowed {
                        self.retain_extern_crates(
                            &self.surface.crate_name,
                            name,
                            reachable,
                            source_modules,
                        );
                    }
                    BTreeSet::from([Binding::module(module.clone())])
                } else {
                    resolve(
                        &module,
                        &path.prefix(index),
                        bindings,
                        &self.surface.crate_name,
                    )
                    .unwrap_or_default()
                };
                for mut parent in parents {
                    if parent.kind == BindingKind::SelfCrate {
                        parent.path.clone_from(&self.surface.crate_name);
                        parent.kind = BindingKind::Module;
                    }
                    if parent.kind == BindingKind::Enum {
                        self.retain_enum_metadata(&parent.path, reachable, source_modules);
                    }
                    if parent.kind != BindingKind::Module {
                        continue;
                    }
                    let scope = &self.modules[&parent.path];
                    self.retain_extern_crates(&parent.path, name, reachable, source_modules);
                    if !scope.macro_anchors.is_empty() {
                        reachable.extend(scope.macro_anchors.iter().cloned());
                        source_modules.insert(parent.path.clone());
                    }
                    for import in &scope.imports {
                        let glob = import.is_glob();
                        // A private alias used as a qualifier can determine a
                        // public enum glob's source and platform availability.
                        let qualifier = index + 1 < path.segments.len();
                        let required = if qualifier {
                            Namespace::Type
                        } else {
                            selection.namespace()
                        };
                        if !import.may_select(name, selection.include_private(), qualifier)
                            || !self.import_selects_namespace(
                                &parent.path,
                                import,
                                name,
                                required,
                                bindings,
                            )
                        {
                            continue;
                        }
                        if glob
                            && name != "*"
                            && !self.glob_exports_name(
                                &parent.path,
                                &import.path,
                                name,
                                bindings,
                                selection.include_private(),
                                &import.conditions,
                            )
                        {
                            continue;
                        }
                        import.retain(reachable);
                        source_modules.insert(parent.path.clone());
                        let mut target = import.path.clone();
                        // Selecting one name through a glob must not expose all
                        // unrelated reexports in the glob's private source.
                        if glob && name != "*" {
                            name.clone_into(target.segments.last_mut().unwrap());
                        }
                        pending.push((parent.path.clone(), target));
                    }
                }
            }
        }
    }

    fn constant_dependencies(
        &self,
        module: &str,
        path: &NamePath,
        bindings: &Bindings,
        aliases: &Aliases,
        conditions: &[String],
        source_modules: &mut BTreeSet<String>,
    ) -> BTreeSet<String> {
        let available = Availability::conditions(conditions);
        let mut dependencies = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for binding in resolve(module, path, bindings, &self.surface.crate_name)
            .into_iter()
            .flatten()
            .filter(|binding| {
                binding.namespace.includes(Namespace::Value)
                    && !available
                        .and(&Availability::conditions(&binding.conditions))
                        .is_never()
            })
        {
            if binding.kind == BindingKind::Variant {
                dependencies.extend(
                    self.children_of(&binding.path, ChildOwner::Type)
                        .iter()
                        .cloned(),
                );
                let (owner, _) = binding.path.rsplit_once("::").unwrap();
                self.retain_enum_metadata(owner, &mut dependencies, source_modules);
            } else {
                paths.insert(binding.path);
            }
        }
        if path.segments.len() > 1 {
            let name = binding_name(path.segments.last().unwrap());
            let mut owners: Vec<_> = resolve(
                module,
                &path.prefix(path.segments.len() - 1),
                bindings,
                &self.surface.crate_name,
            )
            .into_iter()
            .flatten()
            .filter(|binding| {
                binding.namespace.includes(Namespace::Type)
                    && !available
                        .and(&Availability::conditions(&binding.conditions))
                        .is_never()
            })
            .map(|binding| binding.path)
            .collect();
            let mut visited = BTreeSet::new();
            while let Some(owner) = owners.pop() {
                if visited.insert(owner.clone()) {
                    paths.insert(format!("{owner}::{name}"));
                    owners.extend(aliases.targets(&owner).map(str::to_owned));
                }
            }
        }
        dependencies.extend(
            paths
                .iter()
                .filter_map(|path| self.constants.get(path))
                .flatten()
                .filter(|id| {
                    !available
                        .and(&Availability::conditions(
                            &self.declarations[*id].conditions,
                        ))
                        .is_never()
                })
                .cloned(),
        );
        dependencies
    }

    fn retain_type_dependencies(
        &self,
        bindings: &Bindings,
        aliases: &Aliases,
        reachable: &mut BTreeSet<String>,
        source_modules: &mut BTreeSet<String>,
    ) -> Result<(), String> {
        let mut pending: Vec<_> = reachable.iter().cloned().collect();
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id.clone()) {
                continue;
            }
            self.discriminants.enqueue(&id, reachable, &mut pending);
            let Some((module, references)) = self.references.get(&id) else {
                continue;
            };
            references.check_supported(&id)?;
            for (path, namespace, expose) in references.iter() {
                self.shadowing
                    .check_path(module, path, bindings, &self.surface.crate_name)?;
                // A path such as Alias::Item or Alias::MAX depends on Alias,
                // even though the associated name is outside local resolution.
                for end in 1..=path.segments.len() {
                    let Some(targets) = resolve(
                        module,
                        &path.prefix(end),
                        bindings,
                        &self.surface.crate_name,
                    ) else {
                        continue;
                    };
                    let required = if end == path.segments.len() {
                        namespace
                    } else {
                        Namespace::Type
                    };
                    for target in targets {
                        if required != Namespace::Both && !target.namespace.includes(required) {
                            continue;
                        }
                        if let Some(foreign) = self.unsupported_foreign.get(&target.path)
                            && foreign.is_type
                        {
                            return Err(foreign.message.clone());
                        }
                        if !target.namespace.includes(Namespace::Type)
                            || !self.local_types.contains(&target.path)
                        {
                            continue;
                        }
                        let expose_members = expose && namespace.includes(Namespace::Type);
                        let (scope, _) = target.path.rsplit_once("::").unwrap();
                        source_modules.insert(scope.into());
                        let attribute_prefix =
                            format!("{}|{}::@macro:attribute:", self.surface.name, target.path);
                        for dependency in self.children_of(&target.path, ChildOwner::Type) {
                            if !expose_members
                                && self.declarations.get(dependency).is_none_or(|declaration| {
                                    declaration.kind != "alias"
                                        && !dependency
                                            .starts_with(&format!("{attribute_prefix}alias:"))
                                })
                            {
                                continue;
                            }
                            if reachable.insert(dependency.clone()) {
                                pending.push(dependency.clone());
                            }
                        }
                    }
                }
                // Every API type reference depends on its selecting imports,
                // including unresolved external types with no local alias.
                let mut dependencies = BTreeSet::new();
                if namespace.includes(Namespace::Value) {
                    dependencies.extend(self.constant_dependencies(
                        module,
                        path,
                        bindings,
                        aliases,
                        &self.declarations[&id].conditions,
                        source_modules,
                    ));
                }
                self.retain_imports(
                    module,
                    path,
                    bindings,
                    &mut dependencies,
                    source_modules,
                    ImportSelection::Dependency(namespace),
                );
                for dependency in dependencies {
                    if let Some((scope, _)) = self.references.get(&dependency) {
                        source_modules.insert(scope.clone());
                    }
                    if reachable.insert(dependency.clone()) {
                        pending.push(dependency);
                    }
                }
            }
        }
        self.discriminants.remove_redundant(reachable);
        Ok(())
    }

    fn import_selects_namespace(
        &self,
        module: &str,
        import: &Import,
        name: &str,
        required: Namespace,
        bindings: &Bindings,
    ) -> bool {
        if required == Namespace::Both || !import.macro_anchors.is_empty() {
            return true;
        }
        let Some(targets) = resolve(module, &import.path, bindings, &self.surface.crate_name)
            .filter(|targets| !targets.is_empty())
        else {
            return true;
        };
        if import.is_glob() {
            self.unshadowed_glob_candidates(
                module,
                name,
                Some(&targets),
                bindings,
                &import.conditions,
                required,
            )
        } else {
            targets
                .iter()
                .any(|target| target.namespace.includes(required))
        }
    }

    fn unshadowed_glob_candidates(
        &self,
        module: &str,
        name: &str,
        targets: Option<&BTreeSet<Binding>>,
        bindings: &Bindings,
        conditions: &[String],
        required: Namespace,
    ) -> bool {
        let mut opaque = Binding::new(String::new()).with_conditions(conditions);
        opaque.namespace = Namespace::Both;
        let candidates = targets.map_or_else(
            || vec![opaque.clone()],
            |targets| {
                targets
                    .iter()
                    .flat_map(|target| {
                        target
                            .namespace(&self.surface.crate_name)
                            .and_then(|scope| bindings.get(scope))
                            .and_then(|names| names.get(name))
                            .filter(|values| !values.is_empty())
                            .map_or_else(
                                || vec![opaque.clone().with_conditions(&target.conditions)],
                                |values| {
                                    values
                                        .iter()
                                        .cloned()
                                        .map(|value| {
                                            value
                                                .with_conditions(&target.conditions)
                                                .with_conditions(conditions)
                                        })
                                        .collect()
                                },
                            )
                    })
                    .collect()
            },
        );
        candidates
            .into_iter()
            .flat_map(|candidate| self.shadowing.unshadowed(module, name, candidate))
            .any(|candidate| required == Namespace::Both || candidate.namespace.includes(required))
    }

    fn glob_exports_name(
        &self,
        module: &str,
        path: &NamePath,
        name: &str,
        bindings: &Bindings,
        include_private: bool,
        conditions: &[String],
    ) -> bool {
        let targets = resolve(module, path, bindings, &self.surface.crate_name)
            .filter(|targets| !targets.is_empty());
        if !self.unshadowed_glob_candidates(
            module,
            name,
            targets.as_ref(),
            bindings,
            conditions,
            Namespace::Both,
        ) {
            return false;
        }
        let Some(targets) = targets else {
            // Dependency internals and generated names are outside resolution.
            // Their import declaration must remain an explicit review input.
            return true;
        };
        let mut pending: Vec<_> = targets.into_iter().collect();
        let mut visited = BTreeSet::new();
        while let Some(mut target) = pending.pop() {
            if target.kind == BindingKind::ExternalCrate {
                return true;
            }
            if target.kind == BindingKind::SelfCrate {
                target.path.clone_from(&self.surface.crate_name);
                target.kind = BindingKind::Module;
            }
            if !matches!(target.kind, BindingKind::Module | BindingKind::Enum)
                || !visited.insert((target.path.clone(), target.kind))
            {
                continue;
            }
            if self.modules.iter().any(|(module, scope)| {
                !scope.macro_anchors.is_empty()
                    && (target.path == *module
                        || target
                            .path
                            .strip_prefix(module)
                            .is_some_and(|suffix| suffix.starts_with("::")))
            }) {
                return true;
            }
            if target.kind == BindingKind::Enum {
                if self.enum_exports[&target.path].contains_key(name)
                    || self
                        .children_of(&target.path, ChildOwner::Type)
                        .iter()
                        .any(|id| self.declarations[id].kind == "macro")
                {
                    return true;
                }
                continue;
            }
            let scope = &self.modules[&target.path];
            if scope.public.contains_key(name)
                || include_private
                    && scope
                        .restricted_bindings
                        .keys()
                        .any(|key| binding_name(key) == binding_name(name))
                || self
                    .children_of(&target.path, ChildOwner::Module)
                    .iter()
                    .any(|id| id.starts_with("@macro:"))
            {
                return true;
            }
            for import in scope
                .imports
                .iter()
                .filter(|import| import.public || include_private && import.accessible)
            {
                if import.name().as_deref() == Some(name) {
                    return true;
                }
                if import.path.segments.last().is_some_and(|part| part == "*") {
                    let Some(targets) = resolve(
                        &target.path,
                        &import.path,
                        bindings,
                        &self.surface.crate_name,
                    )
                    .filter(|targets| !targets.is_empty()) else {
                        return true;
                    };
                    pending.extend(targets);
                }
            }
        }
        false
    }

    fn retain_enum_metadata(
        &self,
        owner: &str,
        reachable: &mut BTreeSet<String>,
        source_modules: &mut BTreeSet<String>,
    ) {
        // Constructors expose the owning type's representation, generators and
        // methods, without making unselected variants public. Empty enum globs
        // still require reviewing attributes that may generate variants.
        reachable.extend(
            self.children_of(owner, ChildOwner::Type)
                .iter()
                .filter(|id| self.declarations[*id].kind != "variant")
                .cloned(),
        );
        if let Some((module, _)) = owner.rsplit_once("::") {
            source_modules.insert(module.to_owned());
        }
    }

    fn resolve_imports(&self, prioritize: bool) -> (Bindings, Bindings) {
        let mut bindings: Bindings = self
            .modules
            .iter()
            .map(|(name, m)| {
                let mut locals = canonical_names(m.locals.clone());
                // Explicit imports shadow extern-prelude names even while an
                // external target remains opaque to local name resolution.
                for import in &m.imports {
                    if let Some(name) = import.name() {
                        locals.entry(name).or_default();
                    }
                }
                (name.clone(), locals)
            })
            .collect();
        let mut exports: Bindings = self
            .modules
            .iter()
            .map(|(name, m)| (name.clone(), canonical_names(m.public.clone())))
            .collect();
        // Enum variants form an export scope of their own, without treating
        // the enum as a module or exposing unrelated definitions beside it.
        let enums: Bindings = self
            .enum_exports
            .iter()
            .map(|(path, names)| (path.clone(), canonical_names(names.clone())))
            .collect();
        bindings.extend(enums.clone());
        exports.extend(enums);
        // Restricted aliases are usable by internal glob imports without
        // becoming public exports of the module that defines them.
        let mut importable = exports.clone();
        for (module, scope) in &self.modules {
            for (name, aliases) in &scope.restricted_bindings {
                importable
                    .get_mut(module)
                    .unwrap()
                    .entry(binding_name(name).into())
                    .or_default()
                    .extend(aliases.iter().cloned());
            }
        }
        // Local import/reexport graphs reach a fixed point without compiler name resolution.
        loop {
            let previous = (bindings.clone(), exports.clone(), importable.clone());
            for (module, m) in &self.modules {
                for import in &m.imports {
                    let Some(targets) =
                        resolve(module, &import.path, &bindings, &self.surface.crate_name)
                    else {
                        continue;
                    };
                    let items = import.items(&targets, &importable, &self.surface.crate_name);
                    let public_items = import.items(&targets, &exports, &self.surface.crate_name);
                    for (name, values) in items {
                        let values = values
                            .into_iter()
                            .filter_map(|binding| self.constructors.visible(binding, Some(module)))
                            .collect();
                        let values =
                            self.import_candidates(module, import, &name, values, prioritize);
                        bindings
                            .get_mut(module)
                            .unwrap()
                            .entry(name.clone())
                            .or_default()
                            .extend(values.iter().cloned());
                        if import.accessible {
                            importable
                                .get_mut(module)
                                .unwrap()
                                .entry(name)
                                .or_default()
                                .extend(values);
                        }
                    }
                    if import.public {
                        for (name, values) in public_items {
                            let values = values
                                .into_iter()
                                .filter_map(|binding| self.constructors.visible(binding, None))
                                .collect();
                            let values =
                                self.import_candidates(module, import, &name, values, prioritize);
                            exports
                                .get_mut(module)
                                .unwrap()
                                .entry(name)
                                .or_default()
                                .extend(values);
                        }
                    }
                }
            }
            if previous == (bindings.clone(), exports.clone(), importable.clone()) {
                break;
            }
        }
        (bindings, exports)
    }

    fn import_candidates(
        &self,
        module: &str,
        import: &Import,
        name: &str,
        values: BTreeSet<Binding>,
        prioritize: bool,
    ) -> BTreeSet<Binding> {
        if prioritize && import.path.segments.last().is_some_and(|part| part == "*") {
            values
                .into_iter()
                .flat_map(|binding| self.shadowing.unshadowed(module, name, binding))
                .collect()
        } else {
            values
        }
    }

    fn import_shadowing(&self, bindings: &Bindings) -> Shadowing {
        let mut shadowing = Shadowing::default();
        for (module, scope) in &self.modules {
            for (name, values) in &scope.locals {
                for binding in values {
                    shadowing.insert(module, name, binding.namespace, &binding.conditions);
                }
            }
            for import in &scope.imports {
                let Some(name) = import.name() else {
                    continue;
                };
                let targets = resolve(module, &import.path, bindings, &self.surface.crate_name)
                    .unwrap_or_default();
                if targets.is_empty() {
                    // Dependency imports may bind a type, a value, or both.
                    // Guessing would hide a local type or invent its members.
                    if scope
                        .imports
                        .iter()
                        .filter(|import| import.is_glob())
                        .any(|glob| {
                            resolve(module, &glob.path, bindings, &self.surface.crate_name)
                                .is_some_and(|targets| {
                                    glob.items(&targets, bindings, &self.surface.crate_name)
                                        .iter()
                                        .any(|(candidate, values)| {
                                            candidate == &name
                                                && values.iter().any(|value| {
                                                    !Availability::conditions(&value.conditions)
                                                        .and(&Availability::conditions(
                                                            &import.conditions,
                                                        ))
                                                        .is_never()
                                                })
                                        })
                                })
                        })
                    {
                        shadowing.opaque(module, &name);
                    }
                    continue;
                }
                for target in targets
                    .into_iter()
                    .filter_map(|binding| self.constructors.visible(binding, Some(module)))
                {
                    // Namespace availability belongs to each resolved branch,
                    // including predicates inherited through reexport chains.
                    let target = target.with_conditions(&import.conditions);
                    shadowing.insert(module, &name, target.namespace, &target.conditions);
                }
            }
        }
        shadowing
    }

    #[expect(
        clippy::too_many_lines,
        reason = "Attach implementation members before selecting reachable declarations"
    )]
    fn finish(mut self) -> Result<Vec<Declaration>, String> {
        // Discover the namespaces of named imports before applying their
        // precedence. Rebuild the graph so shadowed glob owners cannot linger.
        let (candidates, _) = self.resolve_imports(false);
        self.shadowing = self.import_shadowing(&candidates);
        let (bindings, exports) = self.resolve_imports(true);
        let aliases = Aliases::new(&self.aliases, &bindings, &self.surface.crate_name);
        let mut attributed_impls: BTreeMap<(String, String), usize> = BTreeMap::new();
        for implementation in std::mem::take(&mut self.implementations) {
            let mut macro_attributes = Vec::new();
            for attribute in &implementation.attributes {
                if requires_macro_review(&attribute.meta)? {
                    macro_attributes.push(attribute.clone());
                }
            }
            let Some(owners) = resolve(
                &implementation.module,
                &implementation.ty,
                &bindings,
                &self.surface.crate_name,
            ) else {
                if !macro_attributes.is_empty() {
                    return Err(format!(
                        "unsupported attributed impl owner `{}` in {}: cannot resolve a local implementation type",
                        implementation.ty,
                        implementation.source.display()
                    ));
                }
                continue;
            };
            // A module attribute can rewrite an impl of a type defined outside
            // that module. Keep those anchors reachable through the owner, even
            // when explicit roots select only the type or the impl is empty.
            let mut module_anchors = Vec::new();
            let mut ancestor = Some(implementation.module.as_str());
            while let Some(module) = ancestor {
                module_anchors.extend(self.modules[module].macro_anchors.iter().cloned());
                ancestor = module.rsplit_once("::").map(|(parent, _)| parent);
            }
            let active_owner = owners.iter().any(|owner| {
                owner.namespace.includes(Namespace::Type)
                    && !Availability::conditions(&owner.conditions)
                        .and(&Availability::conditions(&implementation.conditions))
                        .is_never()
            });
            let owners = aliases.impl_types(
                owners,
                &implementation.arguments,
                &implementation.conditions,
            )?;
            if active_owner && owners.is_empty() && !macro_attributes.is_empty() {
                return Err(format!(
                    "unsupported attributed impl owner `{}` in {}: no resolved local implementation type",
                    implementation.ty,
                    implementation.source.display()
                ));
            }
            for owner in owners {
                self.attach(
                    &owner.path,
                    ChildOwner::Type,
                    module_anchors.iter().cloned(),
                );
                if !macro_attributes.is_empty() {
                    let specialized_owner = format!("{}{}", owner.path, tokens(&owner.arguments));
                    let index = attributed_impls
                        .entry((specialized_owner.clone(), implementation.module.clone()))
                        .or_default();
                    // Distinct blocks on one self type need independent anchors,
                    // including empty blocks. Count only attributed blocks in
                    // this owner/module scope; the full input remains the shape.
                    let impl_path = format!(
                        "{specialized_owner}::@impl:{}:{index}",
                        implementation.module
                    );
                    *index += 1;
                    let mut cfg = implementation.conditions.clone();
                    cfg.extend(owner.conditions.iter().cloned());
                    cfg.sort();
                    cfg.dedup();
                    let anchors = self.attribute_macros(
                        &impl_path,
                        "impl",
                        &macro_attributes,
                        &cfg,
                        AttributeInput {
                            module: &implementation.module,
                            file: &implementation.source,
                            tokens: &implementation.input,
                        },
                    )?;
                    for anchor in &anchors {
                        self.references.insert(
                            anchor.clone(),
                            (
                                implementation.module.clone(),
                                implementation.references.clone(),
                            ),
                        );
                    }
                    self.attach(&owner.path, ChildOwner::Type, anchors);
                }
                for member in &implementation.members {
                    let member_path = format!(
                        "{}{}::{}",
                        owner.path,
                        tokens(&owner.arguments),
                        member.name
                    );
                    let mut cfg = member.conditions.clone();
                    cfg.extend(owner.conditions.iter().cloned());
                    cfg.sort();
                    cfg.dedup();
                    let id = self.add(
                        &member_path,
                        member.kind,
                        member.shape.clone(),
                        &cfg,
                        &member.source,
                    )?;
                    self.references.insert(
                        id.clone(),
                        (implementation.module.clone(), member.references.clone()),
                    );
                    if let Some(scope) = &member.macro_scope {
                        self.macro_inputs
                            .insert(id.clone(), (implementation.module.clone(), scope.clone()));
                    }
                    if let Some(path) = &member.macro_path {
                        self.macro_calls
                            .insert(id.clone(), (implementation.module.clone(), path.clone()));
                    }
                    // Resolve exports and roots through the unspecialized owner,
                    // but keep each self-type's members and macro anchors distinct.
                    if member.public {
                        self.attach(&owner.path, ChildOwner::Type, [id.clone()]);
                    }
                    let anchors = self.attribute_macros(
                        &member_path,
                        member.kind,
                        &member.attributes,
                        &cfg,
                        AttributeInput {
                            module: &implementation.module,
                            file: &member.source,
                            tokens: &member.input,
                        },
                    )?;
                    if member.kind == "associated_constant" {
                        self.constants
                            .entry(format!("{}::{}", owner.path, binding_name(&member.name)))
                            .or_default()
                            .extend(std::iter::once(id).chain(anchors.iter().cloned()));
                    }
                    for anchor in &anchors {
                        self.references.insert(
                            anchor.clone(),
                            (implementation.module.clone(), member.references.clone()),
                        );
                    }
                    // A generator on a private member can introduce public
                    // declarations. Only the unexpanded member itself is private.
                    self.attach(&owner.path, ChildOwner::Type, anchors);
                }
            }
        }
        self.finish_helper_attributes(&bindings)?;
        self.finish_derives(&bindings)?;
        self.finish_anonymous_constants(&bindings)?;
        let mut reachable: BTreeSet<_> = self
            .exported_macros
            .keys()
            .cloned()
            .chain(self.linkage_exports.iter().cloned())
            .collect();
        let mut visited = BTreeSet::new();
        let mut modules = BTreeSet::new();
        let mut macro_modules = BTreeSet::new();
        let mut source_modules = BTreeSet::new();
        for id in &self.linkage_exports {
            if let Some((module, _)) = self.references.get(id) {
                source_modules.insert(module.clone());
            }
        }
        let mut pending = if self.surface.roots.is_empty() {
            vec![Binding::module(self.surface.crate_name.clone())]
        } else {
            self.surface
                .roots
                .iter()
                .flat_map(|path| {
                    if let Some((parent, name)) = path.rsplit_once("::")
                        && let Some(locals) =
                            self.modules.get(parent).and_then(|m| m.locals.get(name))
                    {
                        return locals
                            .iter()
                            .filter(|binding| self.has_definition(binding))
                            .cloned()
                            .collect::<Vec<_>>();
                    }
                    vec![if self.modules.contains_key(path) {
                        Binding::module(path.clone())
                    } else {
                        Binding::new(path.clone())
                    }]
                })
                .collect()
        };
        while let Some(binding) = pending.pop() {
            let path = binding.path;
            // One spelling can denote both a module and a value. Visiting the
            // module must not suppress a separately exported declaration, and a
            // public value must not make a private module's contents reachable.
            if !visited.insert((path.clone(), binding.kind)) {
                continue;
            }
            if binding.kind == BindingKind::Module {
                if !modules.insert(path.clone()) {
                    continue;
                }
                reachable.extend(self.modules[&path].declarations.iter().cloned());
                for item in &self.modules[&path].extern_crates {
                    if item.public {
                        item.retain(&mut reachable);
                    }
                }
                for (name, values) in &exports[&path] {
                    self.shadowing.check_name(&path, name)?;
                    pending.extend(values.iter().cloned());
                }
                for import in &self.modules[&path].imports {
                    if import.public {
                        import.retain(&mut reachable);
                        self.retain_reexports(
                            &path,
                            &import.path,
                            &bindings,
                            &mut reachable,
                            &mut source_modules,
                        );
                        // A private module can export only generated names. Its
                        // macros still need review even when syn sees no concrete
                        // target definition for the public reexport.
                        if import.path.segments.len() > 1
                            && let Some(parents) = resolve(
                                &path,
                                &import.path.prefix(import.path.segments.len() - 1),
                                &bindings,
                                &self.surface.crate_name,
                            )
                        {
                            macro_modules.extend(
                                parents
                                    .into_iter()
                                    .filter(|binding| binding.kind == BindingKind::Module)
                                    .map(|binding| binding.path)
                                    .filter(|p| self.modules.contains_key(p)),
                            );
                        }
                    }
                }
                // Item macros in an exported module need an explicit review even when
                // their generated names are invisible to syn.
                pending.extend(
                    self.children_of(&path, ChildOwner::Module)
                        .iter()
                        .filter_map(|s| {
                            s.strip_prefix("@macro:")
                                .map(|path| Binding::new(path.into()))
                        }),
                );
            } else {
                if let Some(error) = self.unsupported_foreign.get(&path) {
                    return Err(error.message.clone());
                }
                if binding.kind == BindingKind::Variant {
                    let (owner, _) = path.rsplit_once("::").unwrap();
                    self.retain_enum_metadata(owner, &mut reachable, &mut source_modules);
                }
                let owner = ChildOwner::binding(binding.kind);
                let children = self.children_of(&path, owner);
                if !children.is_empty() {
                    reachable.extend(children.iter().cloned());
                } else if !aliases.contains(&path) && !self.modules.contains_key(&path) {
                    return Err(format!("unresolved audited root {path}"));
                }
                // Record the declaration's scope, rather than inferring it
                // from nested member or attribute-anchor identifier strings.
                if let Some((module, _)) = path.rsplit_once("::") {
                    source_modules.insert(module.to_owned());
                }
                if owner == ChildOwner::Type {
                    pending.extend(aliases.targets(&path).map(|path| Binding::new(path.into())));
                }
            }
        }
        self.retain_type_dependencies(&bindings, &aliases, &mut reachable, &mut source_modules)?;
        // Macro generators can reside in private modules reexported by the root.
        source_modules.extend(macro_modules);
        source_modules.extend(modules);
        // Seed the item macros in selected source scopes, then retain their
        // textual definition dependencies before collecting module generators.
        for module in &source_modules {
            if let Some(scope) = self.modules.get(module) {
                reachable.extend(scope.item_macros.iter().cloned());
            }
        }
        let mut pending: Vec<_> = reachable
            .iter()
            .map(|id| {
                let caller = self.macro_inputs.get(id).map_or_else(
                    || self.surface.crate_name.clone(),
                    |(module, _)| module.clone(),
                );
                (id.clone(), caller)
            })
            .collect();
        let mut visited = BTreeSet::new();
        loop {
            while let Some((id, caller)) = pending.pop() {
                if !visited.insert((id.clone(), caller.clone())) {
                    continue;
                }
                let mut dependencies = BTreeSet::new();
                if let Some((module, inputs)) = self.macro_inputs.get(&id) {
                    source_modules.insert(module.clone());
                    dependencies.extend(inputs.iter().cloned());
                }
                if let Some((module, path)) = self.macro_calls.get(&id) {
                    if !path.absolute && path.segments.len() == 1 {
                        // Opaque generators may invoke additional macros from the
                        // caller's prelude, so retain the whole imported namespace.
                        dependencies.extend(self.macro_crates.keys().cloned());
                    }
                    self.retain_macro_imports(
                        module,
                        path,
                        &bindings,
                        &mut dependencies,
                        &mut source_modules,
                    );
                }
                if let Some((module, paths)) = self.attribute_inputs.get(&id) {
                    if self.declarations[&id].kind == "macro" {
                        dependencies.extend(self.macro_crates.keys().cloned());
                    }
                    for path in paths {
                        self.retain_macro_imports(
                            module,
                            path,
                            &bindings,
                            &mut dependencies,
                            &mut source_modules,
                        );
                    }
                }
                if let Some((module, references)) = self.macro_expansions.get(&id) {
                    for path in references.paths().map_err(|error| {
                        format!("unsupported macro expansion dependency in {id}: {error}")
                    })? {
                        if !path.absolute && path.segments.len() == 1 {
                            dependencies.extend(self.macro_crates.keys().cloned());
                        }
                        self.retain_macro_imports(
                            module,
                            path,
                            &bindings,
                            &mut dependencies,
                            &mut source_modules,
                        );
                        if module != &caller {
                            self.retain_macro_imports(
                                &caller,
                                path,
                                &bindings,
                                &mut dependencies,
                                &mut source_modules,
                            );
                        }
                    }
                }
                let context = self
                    .macro_calls
                    .get(&id)
                    .map_or(&caller, |(module, _)| module);
                for dependency in dependencies {
                    reachable.insert(dependency.clone());
                    pending.push((dependency, context.clone()));
                }
            }
            // Provider imports can reach new modules, whose attributes can select
            // more providers. Finish only after that closure stops adding anchors.
            for (module, metadata) in &self.modules {
                if self.module_reached(module, &source_modules) {
                    for id in &metadata.macro_anchors {
                        if reachable.insert(id.clone()) {
                            let caller = self.macro_inputs.get(id).map_or_else(
                                || self.surface.crate_name.clone(),
                                |(module, _)| module.clone(),
                            );
                            pending.push((id.clone(), caller));
                        }
                    }
                }
            }
            if pending.is_empty() {
                break;
            }
        }
        // Explicit roots select definition paths, but publicly exported module
        // ancestors still contribute externally usable paths to those types.
        let mut exported_modules = BTreeSet::new();
        let mut pending = vec![self.surface.crate_name.clone()];
        while let Some(module) = pending.pop() {
            if exported_modules.insert(module.clone()) {
                pending.extend(
                    exports[&module]
                        .values()
                        .flatten()
                        .filter(|binding| binding.kind == BindingKind::Module)
                        .map(|binding| binding.path.clone()),
                );
            }
        }
        // A module attribute can rewrite every descendant, including a selected
        // root or a reexport from a private module. Empty exported modules also
        // need review because their attributes may generate the entire API.
        for (module, metadata) in &self.modules {
            if self.module_reached(module, &source_modules) {
                reachable.extend(metadata.macro_anchors.iter().cloned());
                if exported_modules.contains(module) {
                    reachable.extend(metadata.declarations.iter().cloned());
                }
            }
        }
        for module in &source_modules {
            if let Some(error) = self
                .unsupported_foreign_macros
                .get(module)
                .and_then(|errors| errors.first())
            {
                return Err(error.clone());
            }
        }
        for id in &reachable {
            if let Some(error) = self.unsupported_trait_members.get(id) {
                return Err(error.clone());
            }
        }
        Ok(reachable
            .into_iter()
            .filter_map(|id| self.declarations.remove(&id))
            .collect())
    }
}

fn flatten_use(tree: &UseTree, mut prefix: NamePath, out: &mut Vec<(NamePath, Option<String>)>) {
    match tree {
        UseTree::Path(p) => {
            prefix.segments.push(p.ident.to_string());
            flatten_use(&p.tree, prefix, out);
        }
        UseTree::Name(n) => {
            prefix.segments.push(n.ident.to_string());
            out.push((prefix, None));
        }
        UseTree::Rename(n) => {
            prefix.segments.push(n.ident.to_string());
            out.push((prefix, Some(n.rename.to_string())));
        }
        UseTree::Glob(_) => {
            prefix.segments.push("*".into());
            out.push((prefix, None));
        }
        UseTree::Group(g) => {
            for tree in &g.items {
                flatten_use(tree, prefix.clone(), out);
            }
        }
    }
}

fn named_type_path(ty: &syn::Type) -> Option<&syn::TypePath> {
    match ty {
        syn::Type::Path(path) if path.qself.is_none() => Some(path),
        syn::Type::Paren(ty) => named_type_path(&ty.elem),
        syn::Type::Group(ty) => named_type_path(&ty.elem),
        _ => None,
    }
}

pub fn discover(root: &Path, surface: &Surface) -> Result<Vec<Declaration>, String> {
    let manifest = root.join(&surface.path).join("Cargo.toml");
    if !manifest.is_file() {
        return Err(format!("missing required manifest {}", manifest.display()));
    }
    let source = manifest::library_source(&manifest)?;
    let mut scanner = Scanner {
        root,
        surface,
        modules: BTreeMap::new(),
        enum_exports: BTreeMap::new(),
        declarations: BTreeMap::new(),
        children: BTreeMap::new(),
        implementations: Vec::new(),
        aliases: Vec::new(),
        local_types: BTreeSet::new(),
        constants: BTreeMap::new(),
        constructors: Constructors::default(),
        discriminants: Discriminants::default(),
        shadowing: Shadowing::default(),
        references: BTreeMap::new(),
        macro_inputs: BTreeMap::new(),
        macro_definitions: BTreeMap::new(),
        macro_calls: BTreeMap::new(),
        macro_expansions: BTreeMap::new(),
        macro_crates: BTreeMap::new(),
        module_macro_imports: BTreeSet::new(),
        pending_derives: Vec::new(),
        pending_helpers: Vec::new(),
        anonymous_assertions: Vec::new(),
        attribute_inputs: BTreeMap::new(),
        unsupported_foreign: BTreeMap::new(),
        unsupported_trait_members: BTreeMap::new(),
        unsupported_foreign_macros: BTreeMap::new(),
        exported_macros: BTreeMap::new(),
        linkage_exports: BTreeSet::new(),
        stack: BTreeSet::new(),
    };
    let _ = scanner.file(
        &source,
        &surface.crate_name,
        &[],
        &ModuleDirectory::shared(source.parent().unwrap().to_path_buf()),
    )?;
    scanner.finish()
}
