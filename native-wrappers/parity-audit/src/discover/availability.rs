use std::collections::BTreeSet;

use super::{test_truth, tokens};

// A disjunction of conjunctions. Atoms are syntax predicates, not evaluated
// platform/feature settings; opposite literals make a conjunction impossible.
type Clause = BTreeSet<(String, bool)>;

#[derive(Clone, Eq, PartialEq)]
pub(super) struct Availability(BTreeSet<Clause>);

impl Availability {
    fn always() -> Self {
        Self(BTreeSet::from([Clause::new()]))
    }

    const fn never() -> Self {
        Self(BTreeSet::new())
    }

    pub fn conditions(conditions: &[String]) -> Self {
        conditions.iter().fold(Self::always(), |result, condition| {
            let available = syn::parse_str::<syn::Meta>(condition)
                .map_or_else(|_| Self::always(), |meta| Self::attribute(&meta));
            result.and(&available)
        })
    }

    pub fn covers_all(alternatives: &BTreeSet<Vec<String>>) -> bool {
        Self::always().covered_by(alternatives)
    }

    pub fn covered_by(&self, alternatives: &BTreeSet<Vec<String>>) -> bool {
        self.and(
            &alternatives
                .iter()
                .fold(Self::never(), |result, conditions| {
                    result.or(&Self::conditions(conditions))
                })
                .not(),
        )
        .is_never()
    }

    fn attribute(meta: &syn::Meta) -> Self {
        let syn::Meta::List(list) = meta else {
            return Self::always();
        };
        let Ok(args) = list.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        ) else {
            return Self::always();
        };
        if list.path.is_ident("cfg") && args.len() == 1 {
            return Self::predicate(&args[0]);
        }
        if list.path.is_ident("cfg_attr") && args.len() > 1 {
            let predicate = Self::predicate(&args[0]);
            let attributes = args.iter().skip(1).fold(Self::always(), |result, attr| {
                result.and(&Self::attribute(attr))
            });
            return predicate.not().or(&attributes);
        }
        // ABI metadata and documentation do not restrict binding availability.
        Self::always()
    }

    fn predicate(meta: &syn::Meta) -> Self {
        if let Some(value) = test_truth(meta) {
            return if value { Self::always() } else { Self::never() };
        }
        if let syn::Meta::List(list) = meta
            && let Ok(args) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            )
        {
            if list.path.is_ident("not") && args.len() == 1 {
                return Self::predicate(&args[0]).not();
            }
            if list.path.is_ident("all") {
                return args.iter().fold(Self::always(), |result, arg| {
                    result.and(&Self::predicate(arg))
                });
            }
            if list.path.is_ident("any") {
                return args.iter().fold(Self::never(), |result, arg| {
                    result.or(&Self::predicate(arg))
                });
            }
        }
        Self(BTreeSet::from([Clause::from([(tokens(meta), true)])]))
    }

    pub fn is_never(&self) -> bool {
        self.0.is_empty()
    }

    pub fn and(&self, other: &Self) -> Self {
        let mut result = Self::never();
        for left in &self.0 {
            for right in &other.0 {
                if right
                    .iter()
                    .any(|(atom, positive)| left.contains(&(atom.clone(), !positive)))
                {
                    continue;
                }
                result.insert(left.union(right).cloned().collect());
            }
        }
        result
    }

    fn insert(&mut self, clause: Clause) {
        if !self.0.iter().any(|existing| existing.is_subset(&clause)) {
            self.0.retain(|existing| !clause.is_subset(existing));
            self.0.insert(clause);
        }
    }

    fn or(&self, other: &Self) -> Self {
        let mut result = self.clone();
        for clause in &other.0 {
            result.insert(clause.clone());
        }
        result
    }

    pub fn not(&self) -> Self {
        self.0.iter().fold(Self::always(), |result, clause| {
            let opposite = Self(
                clause
                    .iter()
                    .map(|(atom, positive)| Clause::from([(atom.clone(), !positive)]))
                    .collect(),
            );
            result.and(&opposite)
        })
    }

    pub fn condition(&self) -> String {
        let clauses = self
            .0
            .iter()
            .map(|clause| {
                let atoms = clause
                    .iter()
                    .map(|(atom, positive)| {
                        if *positive {
                            atom.clone()
                        } else {
                            format!("not ({atom})")
                        }
                    })
                    .collect::<Vec<_>>();
                if atoms.len() == 1 {
                    atoms[0].clone()
                } else {
                    format!("all ({})", atoms.join(" , "))
                }
            })
            .collect::<Vec<_>>();
        let predicate = if clauses.len() == 1 {
            clauses[0].clone()
        } else {
            format!("any ({})", clauses.join(" , "))
        };
        format!("cfg ({predicate})")
    }
}
