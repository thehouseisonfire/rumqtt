use std::collections::BTreeMap;

use super::{Availability, Binding, Namespace, conditions, public};

#[derive(Default)]
pub(super) struct Constructors(BTreeMap<String, Vec<(String, Availability)>>);

impl Constructors {
    pub fn register(&mut self, module: &str, item: &syn::ItemStruct, cfg: &[String]) {
        let restrictions = item.fields.iter().filter_map(|field| {
            if public(&field.vis) {
                return None;
            }
            let cfg = conditions(cfg, &field.attrs)?;
            let mut scope = module.to_owned();
            if let syn::Visibility::Restricted(visibility) = &field.vis {
                for segment in &visibility.path.segments {
                    match segment.ident.to_string().as_str() {
                        "crate" => scope = module.split("::").next().unwrap().into(),
                        "self" => {}
                        "super" => scope = scope.rsplit_once("::").unwrap().0.into(),
                        name => scope = format!("{scope}::{name}"),
                    }
                }
            }
            Some((scope, Availability::conditions(&cfg)))
        });
        self.0
            .entry(format!("{module}::{}", item.ident))
            .or_default()
            .extend(restrictions);
    }

    /// Constructor visibility is the intersection of its fields' visibility.
    /// A conditional private field blocks it only where that field exists.
    /// None represents an export to callers outside the crate.
    pub fn visible(&self, binding: Binding, caller: Option<&str>) -> Option<Binding> {
        if binding.namespace != Namespace::Value {
            return Some(binding);
        }
        let Some(restrictions) = self.0.get(&binding.path) else {
            return Some(binding);
        };
        let original = Availability::conditions(&binding.conditions);
        let mut available = original.clone();
        for (scope, field) in restrictions {
            if !caller.is_some_and(|caller| {
                caller == scope
                    || caller
                        .strip_prefix(scope)
                        .is_some_and(|rest| rest.starts_with("::"))
            }) {
                available = available.and(&field.not());
            }
        }
        if available.is_never() {
            None
        } else if available == original {
            Some(binding)
        } else {
            Some(binding.with_conditions(&[available.condition()]))
        }
    }
}
