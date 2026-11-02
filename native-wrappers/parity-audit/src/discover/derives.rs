use std::collections::BTreeSet;
use std::path::PathBuf;

use syn::Attribute;

use super::{
    AttributeInput, Availability, Binding, BindingKind, Bindings, ChildOwner, NamePath, Scanner,
    binding_name, cfg_attr_attributes, resolve,
};

pub(super) struct PendingDerive {
    pub owner: String,
    pub module: String,
    pub kind: String,
    pub attribute: Attribute,
    pub paths: Vec<NamePath>,
    pub conditions: Vec<String>,
    pub source: PathBuf,
    pub input: String,
    pub anchor: Option<String>,
}

pub(super) fn paths(meta: &syn::Meta) -> Result<Vec<NamePath>, String> {
    if meta.path().is_ident("derive") {
        let syn::Meta::List(list) = meta else {
            return Err("unsupported derive syntax".into());
        };
        return list
            .parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            )
            .map(|paths| paths.iter().map(NamePath::from).collect())
            .map_err(|error| format!("unsupported derive: {error}"));
    }
    let mut result = Vec::new();
    if meta.path().is_ident("cfg_attr") {
        for attribute in cfg_attr_attributes(meta)? {
            result.extend(paths(&attribute)?);
        }
    }
    Ok(result)
}

impl Scanner<'_> {
    pub(super) fn finish_derives(&mut self, bindings: &Bindings) -> Result<(), String> {
        for mut derive in std::mem::take(&mut self.pending_derives) {
            let module = derive.module;
            let resolved: Vec<_> = derive
                .paths
                .into_iter()
                .map(|path| {
                    let binding = self.derive_binding(
                        &module,
                        &path,
                        bindings,
                        &mut BTreeSet::new(),
                        trusted,
                        &derive.conditions,
                    );
                    (path, binding)
                })
                .collect();
            let crate_inputs: BTreeSet<_> = resolved
                .iter()
                .filter(|(path, binding)| {
                    *binding != Some(true) && !path.absolute && path.segments.len() == 1
                })
                .flat_map(|(path, _)| self.macro_crate_inputs(&path.segments[0]).cloned())
                .collect();
            let custom = resolved.iter().any(|(_, binding)| {
                *binding == Some(false) || binding.is_none() && !crate_inputs.is_empty()
            });
            if custom && derive.anchor.is_none() {
                derive.anchor = Some(self.attribute_anchor(
                    &derive.owner,
                    &derive.kind,
                    &derive.attribute,
                    &derive.conditions,
                    AttributeInput {
                        module: &module,
                        file: &derive.source,
                        tokens: &derive.input,
                    },
                )?);
            }
            let crate_inputs = if derive.anchor.is_some() {
                self.macro_crates.keys().cloned().collect()
            } else {
                crate_inputs
            };
            let ids = derive
                .anchor
                .into_iter()
                .chain(
                    self.children_of(&derive.owner, ChildOwner::declaration(&derive.kind))
                        .iter()
                        .filter(|id| self.declarations[*id].kind == derive.kind)
                        .cloned(),
                )
                .collect::<Vec<_>>();
            for id in ids {
                self.macro_inputs
                    .entry(id.clone())
                    .or_insert_with(|| (module.clone(), BTreeSet::new()))
                    .1
                    .extend(crate_inputs.iter().cloned());
                let entry = self
                    .attribute_inputs
                    .entry(id)
                    .or_insert_with(|| (module.clone(), Vec::new()));
                entry.1.extend(
                    resolved
                        .iter()
                        .filter(|(_, binding)| binding.is_some())
                        .map(|(path, _)| path.clone()),
                );
            }
        }
        Ok(())
    }

    /// None means no local macro binding, so the builtin prelude is available.
    /// Opaque dependency routes require review unless they name a trusted derive.
    pub(super) fn derive_binding(
        &self,
        module: &str,
        path: &NamePath,
        bindings: &Bindings,
        visited: &mut BTreeSet<(String, NamePath)>,
        trusted: fn(&NamePath) -> bool,
        conditions: &[String],
    ) -> Option<bool> {
        let key = (module.into(), path.clone());
        if !visited.insert(key.clone()) {
            return None;
        }
        let result = self.lookup_derive(module, path, bindings, visited, trusted, conditions);
        visited.remove(&key);
        result
    }

    fn lookup_derive(
        &self,
        module: &str,
        path: &NamePath,
        bindings: &Bindings,
        visited: &mut BTreeSet<(String, NamePath)>,
        trusted: fn(&NamePath) -> bool,
        conditions: &[String],
    ) -> Option<bool> {
        let available = Availability::conditions(conditions);
        let shadowed = self.derive_shadowed(module, path, bindings, &available)?;
        if !shadowed && trusted(path) {
            return Some(true);
        }
        let name = binding_name(path.segments.last()?);
        let parents = if path.segments.len() == 1 {
            BTreeSet::from([Binding::module(module.into())])
        } else {
            let parents = resolve(
                module,
                &path.prefix(path.segments.len() - 1),
                bindings,
                &self.surface.crate_name,
            );
            let Some(parents) = parents.filter(|parents| !parents.is_empty()) else {
                return Some(false);
            };
            parents
        };
        let mut found = None;
        for parent in parents {
            let parent = parent.with_conditions(conditions);
            if Availability::conditions(&parent.conditions).is_never() {
                continue;
            }
            if parent.kind == BindingKind::ExternalCrate {
                return Some(false);
            }
            let scope_name = parent.namespace(&self.surface.crate_name)?;
            let scope = self.modules.get(scope_name)?;
            if self
                .macro_definitions
                .contains_key(&(scope_name.into(), name.into()))
                || !scope.macro_anchors.is_empty()
            {
                return Some(false);
            }
            let mut named = Vec::new();
            let mut globs = Vec::new();
            for import in &scope.imports {
                let mut conditions = parent.conditions.clone();
                conditions.extend(import.conditions.iter().cloned());
                if Availability::conditions(&conditions).is_never() {
                    continue;
                }
                if !import.accessible
                    && module != scope_name
                    && !module
                        .strip_prefix(scope_name)
                        .is_some_and(|rest| rest.starts_with("::"))
                {
                    continue;
                }
                let mut target = import.path.clone();
                if import.is_glob() {
                    name.clone_into(target.segments.last_mut().unwrap());
                    globs.push((target, conditions));
                } else if import.name().as_deref() == Some(name) {
                    named.push((target, conditions));
                }
            }
            let named: Vec<_> = named
                .iter()
                .filter_map(|(path, conditions)| {
                    self.derive_binding(scope_name, path, bindings, visited, trusted, conditions)
                })
                .collect();
            let candidates = if named.is_empty() {
                globs
                    .iter()
                    .filter_map(|(path, conditions)| {
                        self.derive_binding(
                            scope_name, path, bindings, visited, trusted, conditions,
                        )
                    })
                    .collect()
            } else {
                named
            };
            if candidates.iter().any(|known| !known) {
                return Some(false);
            }
            if !candidates.is_empty() {
                found = Some(true);
            }
        }
        found
    }

    fn derive_shadowed(
        &self,
        module: &str,
        path: &NamePath,
        bindings: &Bindings,
        available: &Availability,
    ) -> Option<bool> {
        let first = binding_name(path.segments.first()?);
        if path.absolute {
            return Some(false);
        }
        Some(
            bindings[module].get(first).is_some_and(|bindings| {
                bindings.iter().any(|binding| {
                    !available
                        .and(&Availability::conditions(&binding.conditions))
                        .is_never()
                })
            }) || self.modules[module].imports.iter().any(|import| {
                !available
                    .and(&Availability::conditions(&import.conditions))
                    .is_never()
                    && (import.name().as_deref() == Some(first)
                        || import.is_glob()
                            && self.glob_exports_name(
                                module,
                                &import.path,
                                first,
                                bindings,
                                true,
                                &import.conditions,
                            ))
            }),
        )
    }
}

fn trusted(path: &NamePath) -> bool {
    let names: Vec<_> = path
        .segments
        .iter()
        .map(|name| binding_name(name))
        .collect();
    matches!(
        names.as_slice(),
        ["std" | "core", "clone", "Clone"]
            | ["std" | "core", "marker", "Copy"]
            | ["std" | "core", "fmt", "Debug"]
            | ["std" | "core", "default", "Default"]
            | [
                "std" | "core",
                "cmp",
                "PartialEq" | "Eq" | "PartialOrd" | "Ord"
            ]
            | ["std" | "core", "hash", "Hash"]
            | ["serde", "Serialize" | "Deserialize"]
            | ["thiserror", "Error"]
    )
}
