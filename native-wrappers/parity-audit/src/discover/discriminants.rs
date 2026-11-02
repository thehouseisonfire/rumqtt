use std::collections::{BTreeMap, BTreeSet};

use super::Availability;

#[derive(Default)]
pub(super) struct Discriminants {
    predecessors: BTreeMap<String, Vec<(String, String)>>,
    owners: BTreeMap<String, String>,
}

impl Discriminants {
    pub fn insert(
        &mut self,
        declaration: &str,
        value: &str,
        previous: &[(String, String, Vec<String>)],
        conditions: &[String],
        implicit: bool,
    ) {
        let mut inputs = Vec::new();
        if implicit {
            let mut remaining = Availability::conditions(conditions);
            for (previous, value, conditions) in previous.iter().rev() {
                let available = Availability::conditions(conditions);
                if !remaining.and(&available).is_never() {
                    inputs.push((previous.clone(), value.clone()));
                }
                remaining = remaining.and(&available.not());
                if remaining.is_never() {
                    break;
                }
            }
        }
        self.predecessors.insert(declaration.into(), inputs.clone());
        self.predecessors.insert(value.into(), inputs);
        self.owners.insert(value.into(), declaration.into());
    }

    pub fn dependencies<'a>(&'a self, id: &str, reachable: &BTreeSet<String>) -> Vec<&'a str> {
        self.predecessors
            .get(id)
            .into_iter()
            .flatten()
            .map(|(declaration, value)| {
                if reachable.contains(declaration) {
                    declaration.as_str()
                } else {
                    value.as_str()
                }
            })
            .collect()
    }

    pub fn remove_redundant(&self, reachable: &mut BTreeSet<String>) {
        let redundant: Vec<_> = self
            .owners
            .iter()
            .filter_map(|(value, owner)| reachable.contains(owner).then_some(value.clone()))
            .collect();
        for value in redundant {
            reachable.remove(&value);
        }
    }

    pub fn enqueue(&self, id: &str, reachable: &mut BTreeSet<String>, pending: &mut Vec<String>) {
        for dependency in self.dependencies(id, reachable) {
            if reachable.insert(dependency.to_owned()) {
                pending.push(dependency.to_owned());
            }
        }
    }
}
