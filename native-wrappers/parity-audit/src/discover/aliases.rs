use std::collections::{BTreeMap, BTreeSet};

use quote::ToTokens;
use syn::visit_mut::{self, VisitMut};
use syn::{GenericArgument, GenericParam, PathArguments};

use super::availability::Availability;
use super::{Binding, Bindings, NamePath, Namespace, resolve, tokens};

pub(super) struct Alias {
    pub path: String,
    pub module: String,
    pub target: syn::TypePath,
    pub generics: syn::Generics,
    pub conditions: Vec<String>,
}

struct Target {
    path: String,
    arguments: PathArguments,
    generics: syn::Generics,
    conditions: Vec<String>,
}

pub(super) struct ImplType {
    pub path: String,
    pub arguments: PathArguments,
    pub conditions: Vec<String>,
}

pub(super) struct Aliases {
    targets: BTreeMap<String, Vec<Target>>,
}

type ImplTypes = BTreeMap<(String, String), (ImplType, BTreeSet<Vec<String>>)>;

impl Aliases {
    pub fn new(aliases: &[Alias], bindings: &Bindings, crate_name: &str) -> Self {
        let mut targets: BTreeMap<String, Vec<Target>> = BTreeMap::new();
        for alias in aliases {
            let entry = targets.entry(alias.path.clone()).or_default();
            let path = NamePath::from(&alias.target.path);
            if let Some(resolved) = resolve(&alias.module, &path, bindings, crate_name) {
                for binding in resolved {
                    if !binding.namespace.includes(Namespace::Type) {
                        continue;
                    }
                    let binding = binding.with_conditions(&alias.conditions);
                    entry.push(Target {
                        path: binding.path,
                        arguments: alias.target.path.segments.last().unwrap().arguments.clone(),
                        generics: alias.generics.clone(),
                        conditions: binding.conditions,
                    });
                }
            }
        }
        Self { targets }
    }

    pub fn targets(&self, alias: &str) -> impl Iterator<Item = &str> {
        self.targets
            .get(alias)
            .into_iter()
            .flatten()
            .map(|target| target.path.as_str())
    }

    pub fn contains(&self, alias: &str) -> bool {
        self.targets.contains_key(alias)
    }

    pub fn impl_types(
        &self,
        paths: BTreeSet<Binding>,
        arguments: &PathArguments,
        conditions: &[String],
    ) -> Result<Vec<ImplType>, String> {
        let mut result = BTreeMap::new();
        for binding in paths {
            let binding = binding.with_conditions(conditions);
            if !binding.namespace.includes(Namespace::Type) {
                continue;
            }
            if Availability::conditions(&binding.conditions).is_never() {
                continue;
            }
            let mut cycles = BTreeSet::new();
            let resolved = self.expand(
                ImplType {
                    path: binding.path,
                    arguments: arguments.clone(),
                    conditions: binding.conditions,
                },
                &mut BTreeSet::new(),
                &mut cycles,
                &mut result,
            )?;
            if !resolved && !cycles.is_empty() {
                return Err(format!(
                    "type alias cycle without an underlying implementation type: {}",
                    cycles.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        Ok(result
            .into_values()
            .filter_map(|(mut ty, alternatives)| {
                let mut possible = BTreeSet::new();
                for conditions in &alternatives {
                    if !Availability::conditions(conditions).is_never()
                        && !alternatives.iter().any(|other| {
                            other.len() < conditions.len()
                                && other.iter().all(|condition| conditions.contains(condition))
                        })
                    {
                        possible.insert(conditions.clone());
                    }
                }
                if possible.is_empty() {
                    return None;
                }
                let alternatives = possible;
                // Multiple alias branches may lead to the same self type. They
                // make the member available under either branch, not both.
                ty.conditions = if Availability::covers_all(&alternatives) {
                    Vec::new()
                } else if alternatives.len() == 1 {
                    alternatives.into_iter().next().unwrap()
                } else {
                    vec![format!(
                        "any ({})",
                        alternatives
                            .iter()
                            .map(|cfg| format!("[{}]", cfg.join(" && ")))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )]
                };
                Some(ty)
            })
            .collect())
    }

    fn expand(
        &self,
        current: ImplType,
        visiting: &mut BTreeSet<String>,
        cycles: &mut BTreeSet<String>,
        result: &mut ImplTypes,
    ) -> Result<bool, String> {
        let Some(targets) = self.targets.get(&current.path) else {
            let ImplType {
                path,
                arguments,
                conditions,
            } = current;
            result
                .entry((path.clone(), tokens(&arguments)))
                .or_insert_with(|| {
                    (
                        ImplType {
                            path,
                            arguments,
                            conditions: Vec::new(),
                        },
                        BTreeSet::new(),
                    )
                })
                .1
                .insert(conditions);
            return Ok(true);
        };
        if !visiting.insert(current.path.clone()) {
            // Mutually exclusive branches can form a cycle in the symbolic
            // union even though each compiled branch is acyclic. Keep exploring
            // other routes to the underlying type before reporting a cycle.
            cycles.insert(current.path);
            return Ok(false);
        }
        let mut resolved = false;
        for target in targets {
            let mut conditions = current.conditions.clone();
            conditions.extend(target.conditions.iter().cloned());
            conditions.sort();
            conditions.dedup();
            // Inactive alternatives may have a different generic arity. Do not
            // substitute their arguments or follow their symbolic cycles.
            if Availability::conditions(&conditions).is_never() {
                continue;
            }
            resolved |= self.expand(
                ImplType {
                    path: target.path.clone(),
                    arguments: substitute(&target.arguments, &target.generics, &current.arguments)?,
                    conditions,
                },
                visiting,
                cycles,
                result,
            )?;
        }
        visiting.remove(&current.path);
        Ok(resolved)
    }
}

// Substitute syntax parameters, not compiler types. In particular, syn parses
// a bare const argument such as N as a type until its parameter kind is known.
fn substitute(
    target: &PathArguments,
    generics: &syn::Generics,
    supplied: &PathArguments,
) -> Result<PathArguments, String> {
    let mut supplied = match supplied {
        PathArguments::None => Vec::new(),
        PathArguments::AngleBracketed(args) => args.args.iter().cloned().collect(),
        PathArguments::Parenthesized(_) => {
            return Err("unsupported parenthesized type alias arguments".into());
        }
    }
    .into_iter()
    .peekable();
    let mut substitutions = Substitutions::default();
    for parameter in &generics.params {
        let (name, default) = match parameter {
            GenericParam::Type(p) => (
                p.ident.to_string(),
                p.default.clone().map(|(_, ty)| GenericArgument::Type(ty)),
            ),
            GenericParam::Const(p) => (
                p.ident.to_string(),
                p.default
                    .clone()
                    .map(|(_, value)| GenericArgument::Const(value)),
            ),
            GenericParam::Lifetime(p) => (
                p.lifetime.to_string(),
                Some(GenericArgument::Lifetime(syn::parse_quote!('_))),
            ),
        };
        let elided_lifetime = matches!(parameter, GenericParam::Lifetime(_))
            && !matches!(supplied.peek(), Some(GenericArgument::Lifetime(_)));
        let argument = if elided_lifetime {
            None
        } else {
            supplied.next()
        };
        let mut argument = if let Some(argument) = argument {
            argument
        } else {
            let mut default =
                default.ok_or_else(|| format!("missing type alias argument {name}"))?;
            substitutions.visit_generic_argument_mut(&mut default);
            default
        };
        if matches!(parameter, GenericParam::Const(_))
            && let GenericArgument::Type(ty) = argument
        {
            argument = GenericArgument::Const(
                syn::parse2(ty.to_token_stream()).map_err(|e| e.to_string())?,
            );
        }
        substitutions.arguments.insert(name, argument);
    }
    let mut result = target.clone();
    substitutions.visit_path_arguments_mut(&mut result);
    Ok(result)
}

#[derive(Default)]
struct Substitutions {
    arguments: BTreeMap<String, GenericArgument>,
}

impl VisitMut for Substitutions {
    fn visit_generic_argument_mut(&mut self, argument: &mut GenericArgument) {
        if let GenericArgument::Type(syn::Type::Path(ty)) = argument
            && ty.qself.is_none()
            && let Some(name) = ty.path.get_ident()
            && let Some(value) = self.arguments.get(&name.to_string())
        {
            *argument = value.clone();
            return;
        }
        visit_mut::visit_generic_argument_mut(self, argument);
    }

    fn visit_type_mut(&mut self, ty: &mut syn::Type) {
        if let syn::Type::Path(path) = ty
            && path.qself.is_none()
            && let Some(name) = path.path.get_ident()
            && let Some(GenericArgument::Type(value)) = self.arguments.get(&name.to_string())
        {
            *ty = value.clone();
            return;
        }
        visit_mut::visit_type_mut(self, ty);
    }

    fn visit_expr_mut(&mut self, expression: &mut syn::Expr) {
        if let syn::Expr::Path(path) = expression
            && path.qself.is_none()
            && let Some(name) = path.path.get_ident()
            && let Some(GenericArgument::Const(value)) = self.arguments.get(&name.to_string())
        {
            *expression = value.clone();
            return;
        }
        visit_mut::visit_expr_mut(self, expression);
    }

    fn visit_lifetime_mut(&mut self, lifetime: &mut syn::Lifetime) {
        if let Some(GenericArgument::Lifetime(value)) = self.arguments.get(&lifetime.to_string()) {
            *lifetime = value.clone();
        }
    }
}
