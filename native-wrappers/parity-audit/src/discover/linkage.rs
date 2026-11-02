use syn::Attribute;

use super::{Availability, attributes};

/// Linker visibility is independent of Rust visibility and reexport reachability.
/// Keep possibly active exports; the declaration retains the original metadata.
pub(super) fn exported(attrs: &[Attribute], conditions: &[String]) -> Result<bool, String> {
    for attr in attrs {
        for (meta, conditions) in attributes::leaves(&attr.meta, conditions)? {
            if !Availability::conditions(&conditions).is_never() && export_metadata(&meta)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn export_metadata(meta: &syn::Meta) -> Result<bool, String> {
    if meta.path().is_ident("no_mangle") || meta.path().is_ident("export_name") {
        return Ok(true);
    }
    if let syn::Meta::List(list) = meta
        && list.path.is_ident("unsafe")
    {
        let nested = list
            .parse_args::<syn::Meta>()
            .map_err(|error| format!("unsupported unsafe linkage attribute: {error}"))?;
        return export_metadata(&nested);
    }
    Ok(false)
}
