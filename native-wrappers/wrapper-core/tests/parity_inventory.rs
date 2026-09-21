use std::collections::BTreeSet;
use std::path::Path;

fn sources(path: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn inventory(root: &Path, protocol: &str) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    let mut files = Vec::new();
    for path in [
        format!("rumqttc-{protocol}/src"),
        "rumqttc-core/src".into(),
        format!("mqttbytes-{protocol}/src"),
    ] {
        sources(&root.join(path), &mut files);
    }
    for file in files {
        let syntax = syn::parse_file(&std::fs::read_to_string(file).unwrap()).unwrap();
        for item in syntax.items {
            match item {
                syn::Item::Impl(item) if item.trait_.is_none() => {
                    let syn::Type::Path(path) = &*item.self_ty else {
                        continue;
                    };
                    let name = path.path.segments.last().unwrap().ident.to_string();
                    for method in item.items {
                        let syn::ImplItem::Fn(method) = method else {
                            continue;
                        };
                        if !matches!(method.vis, syn::Visibility::Public(_)) {
                            continue;
                        }
                        let method_name = method.sig.ident.to_string();
                        let category = match name.as_str() {
                            "MqttOptions" | "NetworkOptions"
                                if method.sig.receiver().is_some_and(|receiver| {
                                    receiver.mutability.is_some()
                                        || matches!(
                                            &receiver.kind,
                                            syn::ReceiverKind::Reference(_, _, Some(_))
                                        )
                                }) =>
                            {
                                "option"
                            }
                            "AsyncClient" | "Client" => "operation",
                            _ => continue,
                        };
                        result.insert(format!("{protocol}.{category}.{method_name}"));
                    }
                }
                syn::Item::Enum(item)
                    if ["Event", "Outgoing", "AuthEvent", "Packet"]
                        .iter()
                        .any(|name| item.ident == name) =>
                {
                    for variant in item.variants {
                        result.insert(format!("{protocol}.event.{}.{}", item.ident, variant.ident));
                    }
                }
                _ => {}
            }
        }
    }
    result
}

#[test]
fn every_native_setter_operation_and_event_has_a_reviewed_parity_entry() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // Published-crate tests do not have the repository's native-client sources.
    if !root.join("TODO15.md").exists() {
        return;
    }
    let actual: BTreeSet<_> = ["v4", "v5"]
        .into_iter()
        .flat_map(|protocol| inventory(&root, protocol))
        .collect();
    let mut reviewed = BTreeSet::new();
    for line in include_str!("../PARITY.md")
        .lines()
        .filter(|line| line.starts_with("| v"))
    {
        let fields: Vec<_> = line.split('|').map(str::trim).collect();
        assert_eq!(fields.len(), 8, "invalid matrix row: {line}");
        assert!(["supported", "not applicable", "intentionally omitted"].contains(&fields[4]));
        assert!(
            !fields[5].is_empty() && !fields[6].is_empty(),
            "reason and test reference required: {line}"
        );
        for protocol in fields[1].split(',') {
            for name in fields[3].split(',') {
                let key = format!("{}.{}.{}", protocol.trim(), fields[2], name.trim());
                assert!(reviewed.insert(key.clone()), "duplicate matrix entry {key}");
            }
        }
    }
    let missing: Vec<_> = actual.difference(&reviewed).collect();
    let stale: Vec<_> = reviewed.difference(&actual).collect();
    assert!(
        missing.is_empty() && stale.is_empty(),
        "unreviewed APIs:\n{missing:#?}\nstale entries:\n{stale:#?}"
    );
}
