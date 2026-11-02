use std::collections::BTreeSet;

use super::{NamePath, Namespace, binding_name, named_type_path};
use syn::visit::{self, Visit};

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct Reference {
    path: NamePath,
    namespace: Namespace,
}

/// Names in declaration syntax, with separate type and value parameter scopes.
#[derive(Clone, Default)]
pub(super) struct References {
    paths: BTreeSet<Reference>,
    macros: BTreeSet<String>,
    unsupported_qualified: BTreeSet<String>,
    selectors: BTreeSet<NamePath>,
    type_parameters: BTreeSet<String>,
    const_parameters: BTreeSet<String>,
    self_type: Option<NamePath>,
}

impl References {
    pub fn check_supported(&self, declaration: &str) -> Result<(), String> {
        if let Some(expression) = self.unsupported_qualified.first() {
            return Err(format!(
                "unsupported qualified expression dependency `{expression}` in audited declaration {declaration}; \
                 trait-qualified expressions and non-path owners require discovery support"
            ));
        }
        if let Some(mac) = self.macros.first() {
            return Err(format!(
                "unresolved macro dependency `{mac}` in audited declaration {declaration}; \
                 use explicit type and constant syntax so expansion dependencies can be reviewed"
            ));
        }
        Ok(())
    }

    pub fn set_self_type(&mut self, name: &syn::Ident) {
        self.self_type = Some(NamePath {
            absolute: false,
            segments: vec![name.to_string()],
        });
    }

    pub fn iter(&self) -> impl Iterator<Item = (&NamePath, Namespace, bool)> {
        self.paths
            .iter()
            .map(|reference| (&reference.path, reference.namespace, true))
            .chain(
                self.selectors
                    .iter()
                    .map(|path| (path, Namespace::Type, false)),
            )
    }

    pub fn visit_impl_type(&mut self, ty: &syn::Type) {
        if let Some(ty) = named_type_path(ty) {
            // Selecting an impl owner does not expose all sibling enum variants.
            let path = NamePath::from(&ty.path);
            self.self_type = Some(path);
            self.select_type_path(&ty.path);
        } else {
            self.visit_type(ty);
        }
    }

    pub fn select_type_path(&mut self, path: &syn::Path) {
        self.selectors.insert(NamePath::from(path));
        visit::visit_path(self, path);
    }

    pub fn collect(generics: &[&syn::Generics], collect: impl FnOnce(&mut Self)) -> Self {
        let mut references = Self::default();
        for generics in generics {
            for parameter in &generics.params {
                match parameter {
                    syn::GenericParam::Type(parameter) => {
                        references
                            .type_parameters
                            .insert(binding_name(&parameter.ident.to_string()).into());
                    }
                    syn::GenericParam::Const(parameter) => {
                        references
                            .const_parameters
                            .insert(binding_name(&parameter.ident.to_string()).into());
                    }
                    syn::GenericParam::Lifetime(_) => {}
                }
            }
        }
        collect(&mut references);
        references
    }

    fn path(&mut self, path: &syn::Path, mut namespace: Namespace) {
        let mut names = NamePath::from(path);
        if let Some(first) = names.segments.first() {
            let first = binding_name(first);
            // N::BITS selects N in the type namespace; an unqualified
            // expression N uses the value namespace.
            let lookup = if names.segments.len() > 1 {
                Namespace::Type
            } else {
                namespace
            };
            let type_shadowed = first == "Self" || self.type_parameters.contains(first);
            let value_shadowed = first == "Self" || self.const_parameters.contains(first);
            let shadowed = match lookup {
                Namespace::Type => type_shadowed,
                Namespace::Value => value_shadowed,
                Namespace::Both => type_shadowed && value_shadowed,
            };
            if !names.absolute
                && first == "Self"
                && names.segments.len() > 1
                && let Some(owner) = &self.self_type
            {
                names.segments.splice(..1, owner.segments.iter().cloned());
                names.absolute = owner.absolute;
                self.paths.insert(Reference {
                    path: names,
                    namespace,
                });
            } else if names.absolute || !shadowed {
                if !names.absolute && names.segments.len() == 1 && namespace == Namespace::Both {
                    if type_shadowed {
                        namespace = Namespace::Value;
                    }
                    if value_shadowed {
                        namespace = Namespace::Type;
                    }
                }
                self.paths.insert(Reference {
                    path: names,
                    namespace,
                });
            }
        }
        // Arguments retain their own syntax context even if the qualifier is
        // shadowed, e.g. T::Output<LocalAlias> or a const generic expression.
        visit::visit_path(self, path);
    }
}

impl<'ast> Visit<'ast> for References {
    fn visit_generic_argument(&mut self, argument: &'ast syn::GenericArgument) {
        if let syn::GenericArgument::Type(syn::Type::Path(ty)) = argument
            && ty.qself.is_none()
            && let Some(name) = ty.path.get_ident()
        {
            // syn represents a bare const argument (Bytes<N>) as a type path.
            // Without resolving the parameter's declaration, either namespace
            // may select the argument; ordinary type syntax stays type-only.
            // A known type parameter wins this syntactic ambiguity outright,
            // so a same-named value cannot be its selecting dependency.
            if !self
                .type_parameters
                .contains(binding_name(&name.to_string()))
            {
                self.path(&ty.path, Namespace::Both);
            }
        } else {
            visit::visit_generic_argument(self, argument);
        }
    }

    fn visit_type_path(&mut self, ty: &'ast syn::TypePath) {
        if let Some(qself) = &ty.qself {
            self.visit_type(&qself.ty);
        }
        self.path(&ty.path, Namespace::Type);
    }

    fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
        let Some(qself) = &expr.qself else {
            self.path(&expr.path, Namespace::Value);
            return;
        };
        if qself.position == 0
            && expr.path.segments.len() == 1
            && let Some(owner) = named_type_path(&qself.ty)
        {
            // <Owner>::N selects the same value as Owner::N. Preserve that
            // association without exposing Owner as part of the public type or
            // treating N as an unrelated value in the surrounding module.
            let mut path = owner.path.clone();
            path.segments.extend(expr.path.segments.iter().cloned());
            self.path(&path, Namespace::Value);
        } else {
            // Trait impls and non-path owners are outside inherent-constant
            // resolution. Defer rejection until this declaration is reached.
            self.unsupported_qualified.insert(super::tokens(expr));
        }
    }

    fn visit_trait_bound(&mut self, bound: &'ast syn::TraitBound) {
        self.path(&bound.path, Namespace::Type);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        // Other paths remain conservative dependencies.
        self.path(path, Namespace::Both);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // Macro paths have their own namespace and lexical scoping, including
        // macro_use. Ordinary type/value lookup cannot resolve their expansion
        // dependencies safely. Reject only if this declaration is reached.
        self.macros.insert(super::tokens(mac));
    }

    fn visit_attribute(&mut self, _attribute: &'ast syn::Attribute) {
        // Generator inputs have their own full-input supplement anchors.
    }
}
