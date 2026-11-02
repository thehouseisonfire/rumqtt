use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use super::availability::Availability;

/// A source path without generic arguments, distinct from inventory-qualified
/// definition identities. Only `crate` denotes the root; a spelling matching
/// the configured crate name still uses ordinary local/extern resolution.
/// Absolute paths start in the extern prelude in these Rust 2024 workspaces.
#[derive(Clone, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct NamePath {
    pub absolute: bool,
    pub segments: Vec<String>,
}

impl NamePath {
    pub fn prefix(&self, end: usize) -> Self {
        Self {
            absolute: self.absolute,
            segments: self.segments[..end].to_vec(),
        }
    }
}

impl From<&syn::Path> for NamePath {
    fn from(path: &syn::Path) -> Self {
        Self {
            absolute: path.leading_colon.is_some(),
            segments: path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect(),
        }
    }
}

impl fmt::Display for NamePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.absolute {
            formatter.write_str("::")?;
        }
        formatter.write_str(&self.segments.join("::"))
    }
}

pub(super) type Bindings = BTreeMap<String, BTreeMap<String, BTreeSet<Binding>>>;

/// Raw identifier syntax does not change the name's namespace identity.
pub(super) fn binding_name(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

pub(super) fn canonical_names(
    names: BTreeMap<String, BTreeSet<Binding>>,
) -> BTreeMap<String, BTreeSet<Binding>> {
    let mut result: BTreeMap<String, BTreeSet<Binding>> = BTreeMap::new();
    for (name, values) in names {
        result
            .entry(binding_name(&name).into())
            .or_default()
            .extend(values);
    }
    result
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum BindingKind {
    Module,
    Enum,
    Variant,
    Declaration,
    Value,
    ExternalCrate,
    SelfCrate,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Namespace {
    Type,
    Value,
    Both,
}

impl Namespace {
    pub const fn includes(self, other: Self) -> bool {
        matches!(self, Self::Both)
            || matches!(
                (self, other),
                (Self::Type, Self::Type) | (Self::Value, Self::Value)
            )
    }
}

/// A resolved path, binding kind and one conjunction under which it is available.
/// Separate alternatives to the same path remain distinct until impl expansion.
#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Binding {
    pub path: String,
    pub kind: BindingKind,
    pub namespace: Namespace,
    pub conditions: Vec<String>,
}

impl Binding {
    pub const fn value(path: String) -> Self {
        Self {
            path,
            kind: BindingKind::Value,
            namespace: Namespace::Value,
            conditions: Vec::new(),
        }
    }
    pub const fn crate_alias(path: String, is_self: bool) -> Self {
        Self {
            path,
            kind: if is_self {
                BindingKind::SelfCrate
            } else {
                BindingKind::ExternalCrate
            },
            namespace: Namespace::Type,
            conditions: Vec::new(),
        }
    }

    pub fn namespace<'a>(&'a self, crate_name: &'a str) -> Option<&'a str> {
        match self.kind {
            BindingKind::Module | BindingKind::Enum => Some(&self.path),
            BindingKind::SelfCrate => Some(crate_name),
            _ => None,
        }
    }

    pub const fn new(path: String) -> Self {
        Self {
            path,
            kind: BindingKind::Declaration,
            namespace: Namespace::Type,
            conditions: Vec::new(),
        }
    }

    pub const fn module(path: String) -> Self {
        Self {
            path,
            kind: BindingKind::Module,
            namespace: Namespace::Type,
            conditions: Vec::new(),
        }
    }

    pub const fn enumeration(path: String) -> Self {
        Self {
            path,
            kind: BindingKind::Enum,
            namespace: Namespace::Type,
            conditions: Vec::new(),
        }
    }

    pub const fn variant(path: String) -> Self {
        Self {
            path,
            kind: BindingKind::Variant,
            namespace: Namespace::Both,
            conditions: Vec::new(),
        }
    }

    pub fn with_conditions(mut self, conditions: &[String]) -> Self {
        self.conditions.extend_from_slice(conditions);
        self.conditions.sort();
        self.conditions.dedup();
        self
    }
}

/// Local declarations and named imports take precedence over glob candidates
/// in each namespace, only on configurations where the strong name exists.
#[derive(Default)]
pub(super) struct Shadowing {
    strong: BTreeMap<String, BTreeMap<String, Vec<(Namespace, Availability)>>>,
    opaque: BTreeSet<(String, String)>,
}

impl Shadowing {
    pub fn insert(
        &mut self,
        module: &str,
        name: &str,
        namespace: Namespace,
        conditions: &[String],
    ) {
        self.strong
            .entry(module.into())
            .or_default()
            .entry(binding_name(name).into())
            .or_default()
            .push((namespace, Availability::conditions(conditions)));
    }

    pub fn opaque(&mut self, module: &str, name: &str) {
        self.opaque
            .insert((module.into(), binding_name(name).into()));
    }

    pub fn check_name(&self, module: &str, name: &str) -> Result<(), String> {
        if self
            .opaque
            .contains(&(module.into(), binding_name(name).into()))
        {
            return Err(format!(
                "cannot resolve import namespace for {module}::{name}: an opaque named import overlaps a local glob candidate; use an explicit local type import or a qualified API type"
            ));
        }
        Ok(())
    }

    pub fn check_path(
        &self,
        module: &str,
        path: &NamePath,
        bindings: &Bindings,
        crate_name: &str,
    ) -> Result<(), String> {
        for (index, name) in path.segments.iter().enumerate() {
            if index == 0 {
                if !path.absolute {
                    self.check_name(module, name)?;
                }
            } else if let Some(parents) = resolve(module, &path.prefix(index), bindings, crate_name)
            {
                for parent in parents {
                    if let Some(scope) = parent.namespace(crate_name) {
                        self.check_name(scope, name)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn unshadowed(&self, module: &str, name: &str, binding: Binding) -> Vec<Binding> {
        let Some(blockers) = self
            .strong
            .get(module)
            .and_then(|names| names.get(binding_name(name)))
        else {
            return vec![binding];
        };
        let available = Availability::conditions(&binding.conditions);
        let mut result = Vec::new();
        for namespace in [Namespace::Type, Namespace::Value] {
            if !binding.namespace.includes(namespace) {
                continue;
            }
            let mut remaining = available.clone();
            for (strong_namespace, strong) in blockers {
                if strong_namespace.includes(namespace) {
                    remaining = remaining.and(&strong.not());
                }
            }
            if remaining.is_never() {
                continue;
            }
            let mut candidate = binding.clone();
            candidate.namespace = namespace;
            if remaining != available {
                candidate = candidate.with_conditions(&[remaining.condition()]);
            }
            result.push(candidate);
        }
        result
    }
}

pub(super) fn resolve(
    module: &str,
    path: &NamePath,
    bindings: &Bindings,
    crate_name: &str,
) -> Option<BTreeSet<Binding>> {
    // Dependency internals are outside discovery. In particular, ::std must
    // not resolve through a local module, import, generic or crate-name match.
    let (first, rest) = path.segments.split_first()?;
    let first = binding_name(first);
    let mut values = if path.absolute {
        bindings
            .get(crate_name)?
            .get(first)?
            .iter()
            .filter(|binding| {
                matches!(
                    binding.kind,
                    BindingKind::ExternalCrate | BindingKind::SelfCrate
                ) && root_alias(&binding.path, crate_name, first)
            })
            .cloned()
            .collect()
    } else if first == "crate" {
        BTreeSet::from([Binding::module(crate_name.into())])
    } else if first == "self" {
        BTreeSet::from([Binding::module(module.into())])
    } else if first == "super" {
        BTreeSet::from([Binding::module(module.rsplit_once("::")?.0.into())])
    } else {
        bindings
            .get(module)?
            .get(first)
            .filter(|values| {
                rest.is_empty()
                    || values.is_empty()
                    || values.iter().any(|value| value.kind != BindingKind::Value)
            })
            .cloned()
            .or_else(|| {
                bindings.get(crate_name)?.get(first).map(|values| {
                    values
                        .iter()
                        .filter(|binding| {
                            matches!(
                                binding.kind,
                                BindingKind::ExternalCrate | BindingKind::SelfCrate
                            ) && root_alias(&binding.path, crate_name, first)
                        })
                        .cloned()
                        .collect()
                })
            })?
    };
    for part in rest {
        let part = binding_name(part);
        if part == "*" || part == "self" {
            continue;
        }
        let mut next = BTreeSet::new();
        for value in &values {
            let Some(scope) = value.namespace(crate_name) else {
                continue;
            };
            if part == "super" {
                if let Some((parent, _)) = scope.rsplit_once("::") {
                    next.insert(Binding::module(parent.into()).with_conditions(&value.conditions));
                }
            } else if let Some(found) = bindings.get(scope).and_then(|b| b.get(part)) {
                next.extend(
                    found
                        .iter()
                        .cloned()
                        .map(|binding| binding.with_conditions(&value.conditions)),
                );
            }
        }
        if next.is_empty() {
            return None;
        }
        values = next;
    }
    Some(values)
}

fn root_alias(path: &str, crate_name: &str, name: &str) -> bool {
    path.rsplit_once("::")
        .is_some_and(|(parent, alias)| parent == crate_name && binding_name(alias) == name)
}
