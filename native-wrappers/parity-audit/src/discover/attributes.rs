use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{
    Availability, Bindings, NamePath, Scanner, binding_name, cfg_attr_attributes, derives,
    requires_macro_review, tokens,
};

#[derive(Clone, Copy)]
pub(super) struct AttributeInput<'a> {
    pub module: &'a str,
    pub file: &'a Path,
    pub tokens: &'a str,
}

pub(super) struct PendingAttributes {
    pub owner: String,
    pub module: String,
    pub kind: String,
    pub attributes: Vec<syn::Attribute>,
    pub conditions: Vec<String>,
    pub source: PathBuf,
    pub input: String,
}

fn helper(meta: &syn::Meta) -> bool {
    meta.path().get_ident().is_some_and(|ident| {
        ["error", "from", "source", "backtrace"].contains(&binding_name(&ident.to_string()))
    })
}

pub(super) fn has_helper(meta: &syn::Meta) -> Result<bool, String> {
    if meta.path().is_ident("cfg_attr") {
        for attribute in cfg_attr_attributes(meta)? {
            if has_helper(&attribute)? {
                return Ok(true);
            }
        }
    }
    Ok(helper(meta))
}

// Each leaf keeps its effective availability. A conditional derive only
// registers helpers while that derive is present, including nested cfg_attr.
pub(super) fn leaves(
    meta: &syn::Meta,
    conditions: &[String],
) -> Result<Vec<(syn::Meta, Vec<String>)>, String> {
    if !meta.path().is_ident("cfg_attr") {
        return Ok(vec![(meta.clone(), conditions.to_vec())]);
    }
    let attributes = cfg_attr_attributes(meta)?;
    if attributes.is_empty() {
        return Ok(Vec::new());
    }
    let syn::Meta::List(list) = meta else {
        unreachable!()
    };
    let arguments = list
        .parse_args_with(syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
        .map_err(|error| format!("unsupported cfg_attr: {error}"))?;
    let mut conditions = conditions.to_vec();
    conditions.push(format!("cfg ({})", tokens(arguments.first().unwrap())));
    let mut result = Vec::new();
    for attribute in attributes {
        result.extend(leaves(&attribute, &conditions)?);
    }
    Ok(result)
}

pub(super) fn requires_review(
    meta: &syn::Meta,
    conditions: &[String],
    helpers: &BTreeSet<Vec<String>>,
) -> Result<bool, String> {
    if helpers.is_empty() {
        return requires_macro_review(meta);
    }
    for (attribute, conditions) in leaves(meta, conditions)? {
        if helper(&attribute) && Availability::conditions(&conditions).covered_by(helpers) {
            continue;
        }
        if requires_macro_review(&attribute)? {
            return Ok(true);
        }
    }
    Ok(false)
}

impl Scanner<'_> {
    pub(super) fn finish_helper_attributes(&mut self, bindings: &Bindings) -> Result<(), String> {
        for item in std::mem::take(&mut self.pending_helpers) {
            let mut helpers = BTreeSet::new();
            let mut inputs = Vec::new();
            for attribute in &item.attributes {
                inputs.extend(leaves(&attribute.meta, &item.conditions)?);
            }
            for (_, helper_conditions) in inputs.iter().filter(|(meta, _)| helper(meta)) {
                for (leaf, derive_conditions) in &inputs {
                    let mut conditions = helper_conditions.clone();
                    conditions.extend(derive_conditions.iter().cloned());
                    if Availability::conditions(&conditions).is_never() {
                        continue;
                    }
                    for path in derives::paths(leaf)? {
                        if self.derive_binding(
                            &item.module,
                            &path,
                            bindings,
                            &mut BTreeSet::new(),
                            |path| {
                                path.segments
                                    .iter()
                                    .map(|name| binding_name(name))
                                    .eq(["thiserror", "Error"])
                            },
                            &conditions,
                        ) == Some(true)
                        {
                            helpers.insert(conditions.clone());
                        }
                    }
                }
            }
            self.register_attributes(
                &item.owner,
                &item.kind,
                &item.attributes,
                &item.conditions,
                AttributeInput {
                    module: &item.module,
                    file: &item.source,
                    tokens: &item.input,
                },
                &helpers,
            )?;
        }
        Ok(())
    }
}

/// Providers read by a generating attribute, including nested `cfg_attr` leaves.
/// The anchor already accounts for the complete input; provider paths also
/// depend on their selecting imports in the source module.
pub(super) fn provider_paths(meta: &syn::Meta) -> Result<Vec<NamePath>, String> {
    if meta.path().is_ident("derive") {
        return derives::paths(meta);
    }
    if meta.path().is_ident("cfg_attr") {
        let mut paths = Vec::new();
        for attribute in cfg_attr_attributes(meta)? {
            paths.extend(provider_paths(&attribute)?);
        }
        return Ok(paths);
    }
    // macro_export changes a definition's public scope and needs an anchor,
    // but its spelling selects a compiler attribute rather than a provider.
    if meta.path().is_ident("macro_export") || !requires_macro_review(meta)? {
        return Ok(Vec::new());
    }
    Ok(vec![NamePath::from(meta.path())])
}
