use std::fs;
use std::path::Path;

use rumqttc_parity_audit::model::{Decision, Disposition, Evidence, Ledger, Supplement, Surface};
use rumqttc_parity_audit::{discover_sources, inventory, validate};
use tempfile::TempDir;

fn fixture() -> (TempDir, Ledger) {
    let directory = tempfile::tempdir().unwrap();
    let surfaces = ["native-v4", "native-v5", "core", "c"].into_iter().map(|name| {
        fs::create_dir_all(directory.path().join(name).join("src")).unwrap();
        fs::write(directory.path().join(name).join("Cargo.toml"), "[package]\nname='fixture'\nversion='0.1.0'\n").unwrap();
        fs::write(directory.path().join(name).join("src/lib.rs"),
            "pub struct Options { pub value: u16 }\nimpl Options { pub const fn set(&mut self, value: u16) { self.value = value; } }\n").unwrap();
        Surface { name: name.into(), crate_name: name.replace('-', "_"), path: name.into(), roots: Vec::new() }
    }).collect();
    fs::write(
        directory.path().join("evidence.rs"),
        "#[test] fn retained_values() {}\n",
    )
    .unwrap();
    let mut ledger = Ledger {
        schema_version: 1,
        scope: "fixture exports".into(),
        exclusions: vec!["unrelated internals".into()],
        surfaces,
        supplements: Vec::new(),
        declarations: Vec::new(),
        decisions: Vec::new(),
        capabilities: Vec::new(),
    };
    review(directory.path(), &mut ledger);
    (directory, ledger)
}

fn review(root: &Path, ledger: &mut Ledger) {
    ledger.declarations = inventory(root, ledger).unwrap();
    ledger.decisions = ledger
        .declarations
        .iter()
        .enumerate()
        .map(|(index, d)| Decision {
            id: format!("fixture-{index}"),
            boundary: match d.surface.as_str() {
                "core" => "core-to-c",
                "c" => "c-contract",
                _ => "native-to-core",
            }
            .into(),
            sources: vec![d.id.clone()],
            targets: Vec::new(),
            disposition: Disposition::IntentionallyExcluded,
            rationale: "Fixture-only declaration used to exercise review enforcement.".into(),
            restrictions: "No runtime contract.".into(),
            evidence: Vec::new(),
            validation_limits: "Synthetic source tree.".into(),
        })
        .collect();
}

fn replace(root: &Path, surface: &str, before: &str, after: &str) {
    let path = root.join(surface).join("src/lib.rs");
    let source = fs::read_to_string(&path).unwrap();
    assert!(source.contains(before));
    fs::write(path, source.replace(before, after)).unwrap();
}

#[test]
fn checkout_needs_no_planning_files_and_every_surface_does_work() {
    let (directory, ledger) = fixture();
    assert!(!directory.path().join("TODO15.md").exists());
    let actual = inventory(directory.path(), &ledger).unwrap();
    for surface in &ledger.surfaces {
        assert!(actual.iter().any(|d| d.surface == surface.name));
    }
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn absent_partial_and_empty_source_trees_fail_instead_of_skipping() {
    let (directory, ledger) = fixture();
    let missing = tempfile::tempdir().unwrap();
    assert!(
        inventory(missing.path(), &ledger)
            .unwrap_err()
            .contains("missing required manifest")
    );
    fs::remove_file(directory.path().join("native-v5/src/lib.rs")).unwrap();
    assert!(inventory(directory.path(), &ledger).is_err());
    fs::write(directory.path().join("native-v5/src/lib.rs"), "").unwrap();
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("empty inventory")
    );
}

#[test]
fn additions_removals_renames_and_type_changes_require_review() {
    for (before, after, expected) in [
        (
            "pub value: u16",
            "pub value: u16, pub extra: bool",
            "unreviewed declaration",
        ),
        ("pub value: u16", "pub renamed: u16", "stale declaration"),
        ("pub value: u16", "pub value: u32", "changed declaration"),
        (
            "pub const fn set",
            "pub const fn renamed",
            "stale declaration",
        ),
        ("value: u16) {", "value: u32) {", "changed declaration"),
        ("pub const fn set", "const fn set", "stale declaration"),
    ] {
        let (directory, ledger) = fixture();
        replace(directory.path(), "native-v4", before, after);
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            validate(directory.path(), &ledger, &actual)
                .unwrap_err()
                .contains(expected)
        );
    }
    for addition in [
        "impl Options { pub fn new_builder_policy(self, yes: bool) -> Self { self } }",
        "pub enum Event { Value(u16) }",
    ] {
        let (directory, ledger) = fixture();
        replace(
            directory.path(),
            "native-v4",
            "pub struct Options",
            &format!("{addition}\npub struct Options"),
        );
        assert!(
            validate(
                directory.path(),
                &ledger,
                &inventory(directory.path(), &ledger).unwrap()
            )
            .unwrap_err()
            .contains("unreviewed declaration")
        );
    }
}

#[test]
fn nested_paths_reexports_same_names_and_async_methods_are_distinct() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("native-v4/src/lib.rs"),
        r#"
        #[path = "nested.rs"] mod implementation;
        pub use implementation::Options as Builder;
        pub mod another { pub struct Options; impl Options { pub async fn set(&self) {} } }
    "#,
    )
    .unwrap();
    fs::write(
        directory.path().join("native-v4/src/nested.rs"),
        r"
        pub struct Options { pub value: u16 }
        impl Options { pub const fn set(&mut self, value: u16) {} }
        pub struct PrivateToModule;
    ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        actual
            .iter()
            .any(|d| d.id.contains("another::Options::set#method") && d.shape.contains("async"))
    );
    assert!(
        actual
            .iter()
            .any(|d| d.id.contains("implementation::Options::set#method")
                && d.shape.contains("const"))
    );
    assert!(
        actual
            .iter()
            .any(|d| d.kind == "reexport" && d.shape.ends_with("as Builder"))
    );
    assert!(!actual.iter().any(|d| d.id.contains("PrivateToModule")));
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn disabled_branches_and_variant_payloads_are_audited_but_test_helpers_are_not() {
    let (directory, mut ledger) = fixture();
    replace(
        directory.path(),
        "native-v5",
        "pub struct Options",
        r#"
        #[cfg(feature = "disabled")] pub enum Event { Value(u16) }
        #[cfg(not(feature = "disabled"))] pub enum Event { Absent }
        #[cfg(all(test, feature = "unused"))] mod tests { pub struct Helper; }
        pub struct Options
    "#,
    );
    review(directory.path(), &mut ledger);
    assert!(!ledger.declarations.iter().any(|d| d.id.contains("Helper")));
    assert_eq!(
        ledger
            .declarations
            .iter()
            .filter(|d| d.kind == "enum" && d.surface == "native-v5")
            .count(),
        2
    );
    replace(directory.path(), "native-v5", "Value(u16)", "Value(u32)");
    assert!(
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap()
        )
        .unwrap_err()
        .contains("changed declaration")
    );
}

#[test]
fn macro_supplements_require_exact_reviewed_anchors() {
    let (directory, mut ledger) = fixture();
    replace(
        directory.path(),
        "core",
        "pub struct Options",
        "macro_rules! generate { () => { pub struct Generated; }; } generate!(); pub struct Options",
    );
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
    let raw = discover_sources(directory.path(), &ledger).unwrap();
    ledger.supplements = raw
        .iter()
        .filter(|d| d.kind == "macro")
        .map(|d| Supplement {
            anchor: d.id.clone(),
            expected_shape: d.shape.clone(),
            rationale: "Fixture generator; generated output reviewed below.".into(),
            declarations: Vec::new(),
        })
        .collect();
    let mut generated = raw
        .iter()
        .find(|d| d.surface == "core" && d.kind == "struct")
        .unwrap()
        .clone();
    generated.id = "core|core::Generated#struct".into();
    generated.shape = "struct Generated".into();
    ledger.supplements[0].declarations.push(generated.clone());
    review(directory.path(), &mut ledger);
    assert!(ledger.declarations.iter().any(|d| d.id == generated.id));
    replace(
        directory.path(),
        "core",
        "pub struct Generated",
        "pub struct Changed",
    );
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("changed or invalid supplement")
    );
}

#[test]
fn duplicates_dangling_targets_and_missing_field_decisions_fail() {
    let (directory, ledger) = fixture();
    let actual = inventory(directory.path(), &ledger).unwrap();
    let mut broken = ledger.clone();
    broken.declarations.push(broken.declarations[0].clone());
    assert!(
        validate(directory.path(), &broken, &actual)
            .unwrap_err()
            .contains("duplicate declaration")
    );
    let mut broken = ledger.clone();
    broken.decisions.push(broken.decisions[0].clone());
    assert!(
        validate(directory.path(), &broken, &actual)
            .unwrap_err()
            .contains("duplicate decision")
    );
    let mut broken = ledger.clone();
    broken.decisions[0]
        .targets
        .push("core|missing#struct".into());
    assert!(
        validate(directory.path(), &broken, &actual)
            .unwrap_err()
            .contains("dangling target")
    );
    let mut broken = ledger;
    let field = actual
        .iter()
        .find(|d| d.surface == "core" && d.kind == "field")
        .unwrap();
    broken.decisions.retain(|d| !d.sources.contains(&field.id));
    assert!(
        validate(directory.path(), &broken, &actual)
            .unwrap_err()
            .contains("missing boundary decision")
    );
}

#[test]
fn supported_mappings_need_real_targets_and_named_evidence() {
    let (directory, mut ledger) = fixture();
    let source = ledger
        .declarations
        .iter()
        .find(|d| d.surface == "core" && d.kind == "field")
        .unwrap()
        .id
        .clone();
    let target = ledger
        .declarations
        .iter()
        .find(|d| d.surface == "c" && d.kind == "field")
        .unwrap()
        .id
        .clone();
    let decision = ledger
        .decisions
        .iter_mut()
        .find(|d| d.sources.contains(&source))
        .unwrap();
    decision.disposition = Disposition::Supported;
    decision.targets = vec![target];
    decision.evidence = vec![Evidence {
        path: "evidence.rs".into(),
        symbol: "retained_values".into(),
    }];
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
    fs::write(directory.path().join("evidence.rs"), "fn renamed() {}\n").unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("missing evidence symbol")
    );
}

#[test]
fn conversion_body_changes_do_not_claim_declaration_changes() {
    let (directory, ledger) = fixture();
    replace(
        directory.path(),
        "c",
        "self.value = value",
        "self.value = 0",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn local_type_and_module_aliases_preserve_underlying_member_discovery() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        mod hidden { pub struct Options { pub field: u16 } }
        pub type Alias = hidden::Options;
        pub use hidden::{self as exposed};
    ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("hidden::Options::field#field"))
    );
    replace(directory.path(), "core", "pub field: u16", "pub field: u32");
    assert!(
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap()
        )
        .unwrap_err()
        .contains("changed declaration")
    );
}

#[test]
fn conditional_aliases_retain_every_target_and_are_order_independent() {
    let (directory, mut ledger) = fixture();
    let branches = r#"
        #[cfg(feature = "first")] pub type Alias = hidden::First;
        #[cfg(not(feature = "first"))] pub type Alias = hidden::Second;
    "#;
    fs::write(
        directory.path().join("core/src/lib.rs"),
        format!(
            r"
        mod hidden {{
            pub struct First {{ pub first: u16 }}
            pub struct Second {{ pub second: u16 }}
            impl First {{ pub fn first_method(&self) {{}} }}
            impl Second {{ pub fn second_method(&self) {{}} }}
        }}
        {branches}
    "
        ),
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    for name in [
        "First::first#field",
        "First::first_method#method",
        "Second::second#field",
        "Second::second_method#method",
    ] {
        assert!(
            ledger.declarations.iter().any(|d| d.id.contains(name)),
            "missing {name}"
        );
    }
    replace(
        directory.path(),
        "core",
        branches,
        r#"
        #[cfg(not(feature = "first"))] pub type Alias = hidden::Second;
        #[cfg(feature = "first")] pub type Alias = hidden::First;
    "#,
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
    for field in ["first", "second"] {
        let before = format!("pub {field}: u16");
        let after = format!("pub {field}: u32");
        replace(directory.path(), "core", &before, &after);
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            validate(directory.path(), &ledger, &actual)
                .unwrap_err()
                .contains("changed declaration")
        );
        replace(directory.path(), "core", &after, &before);
    }
}

#[test]
fn private_alias_chains_attach_methods_to_the_public_type() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        pub struct Options;
        type Alias = intermediate::Inner;
        pub type Public = Alias;
        mod intermediate { pub(super) type Inner = super::Options; }
        impl Alias { pub fn added() {} }
        type Unused = Options;
    ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::Options::added#method")
    );
    let aliases: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "alias")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(
        aliases,
        [
            "core|core::Alias#alias",
            "core|core::Public#alias",
            "core|core::intermediate::Inner#alias",
        ]
    );
    replace(directory.path(), "core", "fn added()", "fn renamed()");
    let actual = inventory(directory.path(), &ledger).unwrap();
    let error = validate(directory.path(), &ledger, &actual).unwrap_err();
    assert!(error.contains("unreviewed declaration: core|core::Options::renamed#method"));
    assert!(error.contains("stale declaration: core|core::Options::added#method"));
    review(directory.path(), &mut ledger);
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn conditional_private_aliases_preserve_method_availability() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r#"
        pub struct First;
        pub struct Second;
        #[cfg(feature = "first")] type Alias = First;
        #[cfg(not(feature = "first"))] type Alias = Second;
        impl Alias { pub fn callback(&self) {} }
    "#,
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let methods: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "method")
        .collect();
    assert_eq!(methods.len(), 2);
    assert!(methods.iter().all(|d| d.conditions.len() == 1));
    replace(
        directory.path(),
        "core",
        "feature = \"first\"",
        "feature = \"second\"",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("unreviewed declaration")
    );
}

#[test]
fn conditional_alias_branches_to_the_same_type_union_availability() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r#"
        pub struct Options;
        #[cfg(feature = "first")] type Alias = Options;
        #[cfg(not(feature = "first"))] type Alias = Options;
        #[cfg(feature = "first")] impl Alias { pub fn new() {} }
        #[cfg(not(feature = "first"))] impl Alias { pub fn new() {} }
        "#,
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let methods: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "method")
        .collect();
    assert_eq!(methods.len(), 2);
    assert!(
        methods
            .iter()
            .all(|d| d.id.contains("core::Options::new#method"))
    );
    assert!(
        methods
            .iter()
            .all(|d| d.conditions.len() == 1 && d.conditions[0].starts_with("cfg ("))
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn private_alias_specializations_keep_type_const_and_lifetime_arguments() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        pub struct Options<'a, T, const N: usize>(&'a T);
        type Generic<'a, T, const N: usize = 2> = Options<'a, T, N>;
        type First = Generic<'static, u16>;
        type Second = Generic<'static, u32, 3>;
        impl First { pub fn new() {} }
        impl Second { pub fn new() {} }
    ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let methods: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "method")
        .collect();
    assert_eq!(methods.len(), 2);
    assert!(
        methods
            .iter()
            .any(|d| d.id.contains("Options< 'static , u16 , 2 >::new"))
    );
    assert!(
        methods
            .iter()
            .any(|d| d.id.contains("Options< 'static , u32 , 3 >::new"))
    );
    replace(directory.path(), "core", "u32, 3", "u64, 3");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("unreviewed declaration")
    );
}

#[test]
fn generic_alias_defaults_and_nested_arguments_keep_specializations_distinct() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        pub struct Options<T, U>(T, U);
        type Alias<T, U = T, const N: usize = 2> = Options<(T, U), [u8; N]>;
        impl Alias<u16> { pub fn new() {} }
        impl Alias<u32, u64, 3> { pub fn new() {} }
        ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let methods: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "method")
        .collect();
    assert_eq!(methods.len(), 2);
    assert!(
        methods
            .iter()
            .any(|d| d.id.contains("Options< (u16 , u16) , [u8 ; 2] >::new"))
    );
    assert!(
        methods
            .iter()
            .any(|d| d.id.contains("Options< (u32 , u64) , [u8 ; 3] >::new"))
    );
    replace(directory.path(), "core", "usize = 2", "usize = 4");
    let actual = inventory(directory.path(), &ledger).unwrap();
    let error = validate(directory.path(), &ledger, &actual).unwrap_err();
    assert!(error.contains("unreviewed declaration"));
    assert!(!error.contains("(u32 , u64)"));
}

#[test]
fn alias_cycles_report_an_error_instead_of_losing_impl_members() {
    let (directory, ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub struct Options; type A = B; type B = A; impl A { pub fn method() {} }",
    )
    .unwrap();
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("type alias cycle")
    );
}

#[test]
fn mutually_exclusive_alias_chains_do_not_fail_on_a_symbolic_cycle() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r#"
        pub struct Options;
        #[cfg(feature = "first")] type A = Options;
        #[cfg(not(feature = "first"))] type A = B;
        #[cfg(feature = "first")] type B = A;
        #[cfg(not(feature = "first"))] type B = Options;
        impl A { pub fn added() {} }
        "#,
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("core::Options::added#method"))
    );
    replace(directory.path(), "core", "fn added()", "fn renamed()");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("unreviewed declaration")
    );
}

#[test]
fn struct_constructor_form_and_private_field_barriers_require_review() {
    for (before, after) in [
        ("pub struct Options;", "pub struct Options {}"),
        ("pub struct Options {}", "pub struct Options();"),
        (
            "pub struct Options { pub value: u16 }",
            "pub struct Options { pub value: u16, private: bool }",
        ),
        (
            "pub struct Options(pub u16);",
            "pub struct Options(pub u16, bool);",
        ),
        (
            "pub struct Options { pub value: u16 }",
            "pub struct Options { pub value: u16, pub(crate) private: bool }",
        ),
        (
            "pub struct Options { #[cfg(unix)] private: bool }",
            "pub struct Options { #[cfg(windows)] private: bool }",
        ),
        (
            "pub struct Options { #[cfg_attr(feature = \"optional\", cfg(unix))] private: bool }",
            "pub struct Options { #[cfg_attr(feature = \"optional\", cfg(windows))] private: bool }",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), before).unwrap();
        review(directory.path(), &mut ledger);
        replace(directory.path(), "core", before, after);
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            validate(directory.path(), &ledger, &actual)
                .unwrap_err()
                .contains("changed declaration requires review: core|core::Options#struct")
        );
        review(directory.path(), &mut ledger);
        replace(directory.path(), "core", after, before);
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(validate(directory.path(), &ledger, &actual).is_err());
    }
}

#[test]
fn struct_shapes_ignore_private_implementation_details_and_test_fields() {
    for (before, after) in [
        (
            "pub struct Options { pub value: u16 }",
            "pub struct Options { pub value: u16, #[cfg(test)] helper: bool }",
        ),
        (
            "pub struct Options { private: bool }",
            "pub struct Options { renamed: u32, another: bool }",
        ),
        (
            "pub struct Options { #[cfg(unix)] private: bool }",
            "pub struct Options { #[cfg(unix)] renamed: u32 }",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), before).unwrap();
        review(directory.path(), &mut ledger);
        replace(directory.path(), "core", before, after);
        let actual = inventory(directory.path(), &ledger).unwrap();
        validate(directory.path(), &ledger, &actual).unwrap();
    }
}

#[test]
fn trait_default_presence_requires_review_but_body_contents_do_not() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub trait Authenticator { fn failure(&self) {} }",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    replace(
        directory.path(),
        "core",
        "fn failure(&self) {}",
        "fn failure(&self) { let _ = 1; }",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
    replace(
        directory.path(),
        "core",
        "fn failure(&self) { let _ = 1; }",
        "fn failure(&self);",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(validate(directory.path(), &ledger, &actual).unwrap_err().contains("changed declaration requires review: core|core::Authenticator::failure#trait_method"));
    review(directory.path(), &mut ledger);
    replace(
        directory.path(),
        "core",
        "fn failure(&self);",
        "fn failure(&self) {}",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(validate(directory.path(), &ledger, &actual).is_err());
}

#[test]
fn associated_constants_and_macros_cannot_hide_inside_audited_impls() {
    let (directory, ledger) = fixture();
    replace(
        directory.path(),
        "core",
        "impl Options {",
        "impl Options { pub const POLICY: bool = true;",
    );
    assert!(
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap()
        )
        .unwrap_err()
        .contains("unreviewed declaration")
    );
    replace(
        directory.path(),
        "core",
        "impl Options {",
        "impl Options { generate_methods!();",
    );
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
}

#[test]
fn unknown_attribute_and_derive_macros_require_explicit_supplements() {
    for attribute in ["#[derive(Getters)]", "#[builder]"] {
        let (directory, ledger) = fixture();
        replace(
            directory.path(),
            "core",
            "pub struct Options",
            &format!("{attribute} pub struct Options"),
        );
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
    }
    for attribute in [
        "#[generated_methods]",
        "#[cfg_attr(feature = \"unused\", generated_methods)]",
    ] {
        let (directory, ledger) = fixture();
        replace(
            directory.path(),
            "core",
            "pub const fn set",
            &format!("{attribute} pub const fn set"),
        );
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
    }
}

#[test]
fn procedural_attributes_on_aliases_and_trait_members_require_supplements() {
    for (source, declaration, owner) in [
        ("pub type Alias = u16;", "pub type Alias", "core::Alias"),
        (
            "pub trait Auth { fn callback(&self); }",
            "fn callback",
            "core::Auth::callback",
        ),
        (
            "pub trait Auth { type Output; }",
            "type Output",
            "core::Auth::Output",
        ),
        (
            "pub union Bits { pub value: u16 }",
            "pub union Bits",
            "core::Bits",
        ),
        (
            "pub const VALUE: u16 = 0;",
            "pub const VALUE",
            "core::VALUE",
        ),
        (
            "pub static VALUE: u16 = 0;",
            "pub static VALUE",
            "core::VALUE",
        ),
    ] {
        for attribute in [
            "#[rewrite]",
            "#[cfg_attr(feature = \"new\", doc(hidden), rewrite)]",
        ] {
            let (directory, mut ledger) = fixture();
            fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
            review(directory.path(), &mut ledger);
            replace(
                directory.path(),
                "core",
                declaration,
                &format!("{attribute} {declaration}"),
            );
            let raw = discover_sources(directory.path(), &ledger).unwrap();
            assert!(
                raw.iter().any(|d| d.kind == "macro"
                    && d.id
                        .starts_with(&format!("core|{owner}::@macro:attribute:"))),
                "missing generator anchor for {source}"
            );
            assert!(
                inventory(directory.path(), &ledger)
                    .unwrap_err()
                    .contains("unreviewed macro")
            );
        }
    }
}

#[test]
fn alias_and_trait_member_supplements_retain_full_macro_inputs() {
    for (source, before, after) in [
        ("#[rewrite] pub type Alias = u16;", "u16", "u32"),
        (
            "pub trait Auth { #[rewrite] fn callback(&self) { let _ = 1; } }",
            "let _ = 1",
            "let _ = 2",
        ),
        (
            "pub trait Auth { #[rewrite] type Output: Sized; }",
            "Sized",
            "Clone",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
        let raw = discover_sources(directory.path(), &ledger).unwrap();
        let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
        assert_eq!(anchors.len(), 1);
        ledger.supplements = anchors
            .iter()
            .map(|d| Supplement {
                anchor: d.id.clone(),
                expected_shape: d.shape.clone(),
                rationale: "Review the complete alias or trait-member macro input.".into(),
                declarations: Vec::new(),
            })
            .collect();
        review(directory.path(), &mut ledger);
        let actual = inventory(directory.path(), &ledger).unwrap();
        validate(directory.path(), &ledger, &actual).unwrap();
        replace(directory.path(), "core", before, after);
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("changed or invalid supplement")
        );
    }
}

#[test]
fn attribute_anchors_distinguish_type_and_value_namespaces() {
    let (directory, ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        #[rewrite] pub type Choice = u16;
        #[rewrite] pub const Choice: u16 = 0;
        pub trait Auth {
            #[rewrite] type Output;
            #[rewrite] fn Output(&self);
        }
        ",
    )
    .unwrap();
    let raw = discover_sources(directory.path(), &ledger).unwrap();
    let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
    assert_eq!(anchors.len(), 4);
    for kind in ["alias", "constant", "associated_type", "trait_method"] {
        assert!(
            anchors
                .iter()
                .any(|d| d.id.contains(&format!("@macro:attribute:{kind}:rewrite")))
        );
    }
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
}

struct ModuleTree {
    files: &'static [(&'static str, &'static str)],
    child: &'static str,
}

const MODULE_TREES: &[ModuleTree] = &[
    ModuleTree {
        files: &[
            ("lib.rs", "#[path = \"custom.rs\"] pub mod adapted;"),
            ("custom.rs", "pub mod child;"),
        ],
        child: "child.rs",
    },
    ModuleTree {
        files: &[
            ("lib.rs", "#[path = \"other/custom.rs\"] pub mod adapted;"),
            ("other/custom.rs", "pub mod child;"),
        ],
        child: "other/child.rs",
    },
    ModuleTree {
        files: &[
            ("lib.rs", "pub mod ordinary;"),
            ("ordinary.rs", "pub mod child;"),
        ],
        child: "ordinary/child.rs",
    },
    ModuleTree {
        files: &[
            ("lib.rs", "pub mod ordinary;"),
            ("ordinary/mod.rs", "pub mod child;"),
        ],
        child: "ordinary/child.rs",
    },
    ModuleTree {
        files: &[
            ("lib.rs", "pub mod outer;"),
            ("outer.rs", "pub mod lib;"),
            ("outer/lib.rs", "pub mod child;"),
        ],
        child: "outer/lib/child.rs",
    },
    ModuleTree {
        files: &[
            ("lib.rs", "#[path = \"custom.rs\"] pub mod adapted;"),
            ("custom.rs", "pub mod inner { pub mod child; }"),
        ],
        child: "inner/child.rs",
    },
    ModuleTree {
        files: &[(
            "lib.rs",
            "#[path = \"picked\"] pub mod inline { pub mod child; }",
        )],
        child: "picked/child.rs",
    },
    ModuleTree {
        files: &[
            ("lib.rs", "pub mod ordinary;"),
            (
                "ordinary.rs",
                "#[path = \"picked\"] pub mod inline { pub mod child; }",
            ),
        ],
        child: "picked/child.rs",
    },
    ModuleTree {
        files: &[
            ("lib.rs", "pub mod ordinary;"),
            (
                "ordinary.rs",
                "#[path = \"other/custom.rs\"] pub mod adapted;",
            ),
            ("other/custom.rs", "pub mod child;"),
        ],
        child: "other/child.rs",
    },
    ModuleTree {
        files: &[
            ("lib.rs", "pub mod ordinary;"),
            (
                "ordinary.rs",
                "pub mod inline { #[path = \"custom.rs\"] pub mod adapted; }",
            ),
            ("ordinary/inline/custom.rs", "pub mod child;"),
        ],
        child: "ordinary/inline/child.rs",
    },
    ModuleTree {
        files: &[
            ("lib.rs", "#[path = \"custom.rs\"] pub mod adapted;"),
            ("custom.rs", "#[path = \"other/next.rs\"] pub mod next;"),
            ("other/next.rs", "pub mod child;"),
        ],
        child: "other/child.rs",
    },
];

#[test]
fn module_directories_follow_rustc_for_ordinary_inline_and_path_modules() {
    for tree in MODULE_TREES {
        let (directory, mut ledger) = fixture();
        let src = directory.path().join("core/src");
        for &(path, source) in tree.files {
            let path = src.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, source).unwrap();
        }
        let child = src.join(tree.child);
        fs::create_dir_all(child.parent().unwrap()).unwrap();
        fs::write(&child, "pub struct Actual { pub value: u16 }").unwrap();
        let compiled =
            std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .args(["--edition=2024", "--crate-type=lib", "--crate-name=fixture"])
                .arg(src.join("lib.rs"))
                .arg("-o")
                .arg(directory.path().join("fixture.rlib"))
                .output()
                .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        review(directory.path(), &mut ledger);
        let field = ledger
            .declarations
            .iter()
            .find(|d| d.id.contains("::Actual::value#field"))
            .unwrap();
        assert_eq!(field.source, format!("core/src/{}", tree.child));
        fs::write(&child, "pub struct Actual { pub value: u32 }").unwrap();
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            validate(directory.path(), &ledger, &actual)
                .unwrap_err()
                .contains("changed declaration")
        );
    }
}

#[test]
fn path_modules_audit_the_real_child_even_when_a_decoy_exists() {
    let (directory, mut ledger) = fixture();
    let src = directory.path().join("core/src");
    fs::write(
        src.join("lib.rs"),
        "#[path = \"custom.rs\"] pub mod adapted;",
    )
    .unwrap();
    fs::write(src.join("custom.rs"), "pub mod child;").unwrap();
    fs::write(src.join("child.rs"), "pub struct Actual { pub value: u16 }").unwrap();
    fs::create_dir_all(src.join("custom")).unwrap();
    fs::write(
        src.join("custom/child.rs"),
        "pub struct Decoy { pub other: bool }",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("::Actual::value#field") && d.source == "core/src/child.rs")
    );
    assert!(!ledger.declarations.iter().any(|d| d.id.contains("Decoy")));
    fs::write(src.join("child.rs"), "pub struct Actual { pub value: u32 }").unwrap();
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("changed declaration")
    );
}

#[test]
fn grouped_self_imports_resolve_inherent_owners_and_preserve_renames() {
    for (import, binding) in [
        ("use crate::types::inner::{self};", "inner"),
        ("use crate::{types::inner::{self}};", "inner"),
        ("use self::types::inner::{self};", "inner"),
        ("use crate::types::inner::{self as renamed};", "renamed"),
        ("pub use crate::types::inner::{self};", "inner"),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!(
                r"
            mod types {{ pub mod inner {{ pub struct Options; }} }}
            pub use types::inner::Options;
            {import}
            impl {binding}::Options {{}}
            "
            ),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        replace(
            directory.path(),
            "core",
            &format!("impl {binding}::Options {{}}"),
            &format!("impl {binding}::Options {{ pub fn added() {{}} }}"),
        );
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            actual
                .iter()
                .any(|d| d.id == "core|core::types::inner::Options::added#method")
        );
        let error = validate(directory.path(), &ledger, &actual).unwrap_err();
        assert!(error.contains("unreviewed declaration"));
        assert!(error.contains("missing boundary decision"));
        review(directory.path(), &mut ledger);
        validate(directory.path(), &ledger, &actual).unwrap();
    }
}

#[test]
fn cfg_attr_checks_every_nested_attribute_for_api_generators() {
    for attribute in [
        "#[cfg_attr(feature = \"new\", derive(Getters), doc(hidden))]",
        "#[cfg_attr(feature = \"new\", doc(hidden), derive(Getters))]",
        "#[cfg_attr(feature = \"new\", allow(dead_code), generated_methods)]",
        "#[cfg_attr(feature = \"new\", derive(Clone, Getters), allow(dead_code))]",
        "#[cfg_attr(feature = \"new\", doc(hidden), cfg_attr(unix, allow(dead_code), derive(Getters)))]",
        "#[cfg_attr(feature = \"new\", generated_methods(\"doc (\", \"allow (\"))]",
    ] {
        for target in ["pub struct Options", "pub const fn set", "impl Options"] {
            let (directory, ledger) = fixture();
            replace(
                directory.path(),
                "core",
                target,
                &format!("{attribute} {target}"),
            );
            let raw = discover_sources(directory.path(), &ledger).unwrap();
            assert!(
                raw.iter().any(|d| d.surface == "core" && d.kind == "macro"),
                "missing anchor for {attribute} on {target}"
            );
            assert!(
                inventory(directory.path(), &ledger)
                    .unwrap_err()
                    .contains("unreviewed macro"),
                "missed {attribute} on {target}"
            );
        }
    }
}

#[test]
fn benign_nested_cfg_attr_attributes_need_no_macro_supplement() {
    for attribute in [
        "#[cfg_attr(feature = \"new\", derive(Clone), doc(hidden), allow(dead_code))]",
        "#[cfg_attr(feature = \"new\", cfg_attr(unix, doc(hidden), allow(dead_code)), must_use)]",
        "#[cfg_attr(test, generated_methods)]",
    ] {
        let (directory, ledger) = fixture();
        replace(
            directory.path(),
            "core",
            "pub struct Options",
            &format!("{attribute} pub struct Options"),
        );
        let actual = inventory(directory.path(), &ledger).unwrap();
        validate(directory.path(), &ledger, &actual).unwrap();
    }
}

#[test]
fn malformed_nested_generators_cannot_hide_behind_documentation() {
    for attribute in [
        "#[cfg_attr(feature = \"new\", doc(hidden), derive(Getters = invalid))]",
        "#[cfg_attr(feature = \"new\", doc(hidden), cfg_attr(unix, allow(dead_code), derive(Getters = invalid)))]",
    ] {
        let (directory, ledger) = fixture();
        replace(
            directory.path(),
            "core",
            "pub struct Options",
            &format!("{attribute} pub struct Options"),
        );
        assert!(
            discover_sources(directory.path(), &ledger)
                .unwrap_err()
                .contains("unsupported derive")
        );
    }
}

#[test]
fn attributed_empty_impls_require_supplements_on_the_resolved_owner() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        mod hidden {
            pub struct Options<T>(T);
            type Alias = Options<u16>;
            #[generate_methods] impl Alias {}
            #[generate_methods] impl Options<u32> {}
            pub struct Unreachable;
            #[generate_methods] impl Unreachable {}
        }
        pub use hidden::Options;
        ",
    )
    .unwrap();
    // Explicit roots must retain block anchors even without ordinary members.
    ledger
        .surfaces
        .iter_mut()
        .find(|s| s.name == "core")
        .unwrap()
        .roots = vec!["core::hidden::Options".into()];
    let raw = discover_sources(directory.path(), &ledger).unwrap();
    let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
    assert_eq!(anchors.len(), 2);
    assert!(
        anchors
            .iter()
            .any(|d| d.id.contains("Options< u16 >") && d.shape.contains("impl Alias"))
    );
    assert!(
        anchors
            .iter()
            .any(|d| d.id.contains("Options< u32 >") && d.shape.contains("impl Options < u32 >"))
    );
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
}

#[test]
fn separate_attributed_impl_blocks_retain_distinct_full_input_anchors() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        pub struct Options;
        #[generate_methods] impl Options { fn first() { let _ = 1; } }
        #[generate_methods] impl Options { fn second() {} }
        ",
    )
    .unwrap();
    let raw = discover_sources(directory.path(), &ledger).unwrap();
    let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
    assert_eq!(anchors.len(), 2);
    assert_ne!(anchors[0].id, anchors[1].id);
    ledger.supplements = anchors
        .iter()
        .map(|d| Supplement {
            anchor: d.id.clone(),
            expected_shape: d.shape.clone(),
            rationale: "Review each generator's complete impl input.".into(),
            declarations: Vec::new(),
        })
        .collect();
    review(directory.path(), &mut ledger);
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
    replace(
        directory.path(),
        "core",
        "pub struct Options;",
        "pub struct Options; impl Options { fn unrelated() {} }",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
    replace(directory.path(), "core", "let _ = 1", "let _ = 2");
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("changed or invalid supplement")
    );
}

#[test]
fn procedural_macro_input_changes_invalidate_reviewed_expansion() {
    let (directory, mut ledger) = fixture();
    replace(
        directory.path(),
        "core",
        "pub struct Options",
        "#[derive(Getters)] pub struct Options",
    );
    replace(
        directory.path(),
        "core",
        "pub value: u16",
        "private: bool, pub value: u16",
    );
    let raw = discover_sources(directory.path(), &ledger).unwrap();
    ledger.supplements = raw
        .iter()
        .filter(|d| d.kind == "macro")
        .map(|d| Supplement {
            anchor: d.id.clone(),
            expected_shape: d.shape.clone(),
            rationale: "Review the getter generator input, including private fields.".into(),
            declarations: Vec::new(),
        })
        .collect();
    review(directory.path(), &mut ledger);
    replace(
        directory.path(),
        "core",
        "private: bool",
        "renamed_private: bool",
    );
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("changed or invalid supplement")
    );
}

#[test]
fn a_new_core_field_needs_both_a_shape_review_and_a_c_mapping_decision() {
    let (directory, mut ledger) = fixture();
    replace(
        directory.path(),
        "core",
        "pub value: u16",
        "pub value: u16, pub recovery_pending: bool",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    let error = validate(directory.path(), &ledger, &actual).unwrap_err();
    assert!(error.contains("unreviewed declaration"));
    assert!(error.contains("missing boundary decision"));
    ledger.declarations.clone_from(&actual);
    let error = validate(directory.path(), &ledger, &actual).unwrap_err();
    assert!(!error.contains("unreviewed declaration"));
    assert!(error.contains("missing boundary decision"));
}

#[test]
fn cli_reports_work_and_never_accepts_or_rewrites_changed_declarations() {
    let (directory, ledger) = fixture();
    let ledger_path = directory.path().join(rumqttc_parity_audit::LEDGER);
    fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    let original = serde_json::to_string_pretty(&ledger).unwrap();
    fs::write(&ledger_path, &original).unwrap();
    fs::write(
        directory.path().join(rumqttc_parity_audit::MARKDOWN),
        format!("Narrative\n{}\n", rumqttc_parity_audit::render(&ledger)),
    )
    .unwrap();
    let invoke = |command: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
            .args([command, "--root"])
            .arg(directory.path())
            .output()
            .unwrap()
    };
    let output = invoke("check");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    for surface in ["native-v4", "native-v5", "core", "c"] {
        assert!(stdout.contains(&format!("{surface}: 3 declarations checked")));
    }
    assert!(stdout.contains("12 reviewed boundary decisions checked"));
    replace(directory.path(), "core", "pub value: u16", "pub value: u32");
    let output = invoke("inventory");
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout).unwrap().contains("u32"));
    assert!(!invoke("check").status.success());
    assert_eq!(fs::read_to_string(ledger_path).unwrap(), original);
    fs::write(
        directory.path().join(rumqttc_parity_audit::MARKDOWN),
        "stale tables",
    )
    .unwrap();
    assert!(rumqttc_parity_audit::check_markdown(directory.path(), &ledger).is_err());
}

#[test]
fn impl_where_bounds_and_specialized_self_types_are_review_inputs() {
    for (before, after, method) in [
        ("T: Copy", "T: Clone", "policy"),
        ("Options<u16>", "Options<u32>", "specialized"),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            r"
            pub struct Options<T> where T: Copy { pub value: T }
            impl<T> Options<T> where T: Copy { pub fn policy(&self) {} }
            impl Options<u16> { pub fn specialized(&self) {} }
        ",
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        replace(directory.path(), "core", before, after);
        let error = validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap(),
        )
        .unwrap_err();
        assert!(error.contains(method));
        assert!(error.contains(if method == "specialized" {
            "unreviewed declaration"
        } else {
            "changed declaration requires review"
        }));
    }
}

#[test]
fn specialized_methods_and_constants_have_independent_review_decisions() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        mod implementation {
            pub struct Options<T, const N: usize>(pub T);
            impl Options<u16, 1> {
                pub fn new() -> Self { Self(0) }
                pub const SIZE: usize = 1;
            }
        }
        pub use implementation::Options as Builder;
        impl Builder<u32, 2> {
            pub fn new() -> Self { Self(0) }
            pub const SIZE: usize = 2;
        }
        impl Builder<u16, 2> {
            pub fn new() -> Self { Self(0) }
            pub const SIZE: usize = 1;
        }
        ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let methods: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "method")
        .collect();
    assert_eq!(methods.len(), 3);
    assert_ne!(methods[0].id, methods[1].id);
    assert!(methods.iter().any(|d| d.id.contains("< u16 , 1 >")));
    assert!(methods.iter().any(|d| d.id.contains("< u32 , 2 >")));
    assert!(methods.iter().any(|d| d.id.contains("< u16 , 2 >")));
    assert_eq!(
        ledger
            .declarations
            .iter()
            .filter(|d| d.surface == "core" && d.kind == "associated_constant")
            .count(),
        3
    );
    replace(
        directory.path(),
        "core",
        "const SIZE: usize = 2",
        "const SIZE: usize = 3",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    let error = validate(directory.path(), &ledger, &actual).unwrap_err();
    assert!(error.contains("changed declaration requires review"));
    assert!(error.contains("< u32 , 2 >::SIZE#associated_constant"));
    assert!(!error.contains("< u16 , 1 >"));
    review(directory.path(), &mut ledger);
    replace(
        directory.path(),
        "core",
        "Builder<u32, 2>",
        "Builder<u64, 2>",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    let error = validate(directory.path(), &ledger, &actual).unwrap_err();
    assert!(error.contains("unreviewed declaration"));
    assert!(error.contains("stale declaration"));
    review(directory.path(), &mut ledger);
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn specialized_method_macro_anchors_remain_distinct_and_reachable() {
    let (directory, ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        pub struct Options<T>(T);
        impl Options<u16> { #[generated] pub fn new() -> Self { Self(0) } }
        impl Options<u32> { #[generated] pub fn new() -> Self { Self(0) } }
        ",
    )
    .unwrap();
    let raw = discover_sources(directory.path(), &ledger).unwrap();
    let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
    assert_eq!(anchors.len(), 2);
    assert_ne!(anchors[0].id, anchors[1].id);
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
}

#[test]
fn enum_payload_conditions_and_variant_attributes_require_review() {
    for payload in ["{ method: String }", "(String)"] {
        for (before, after) in [
            ("String", "#[cfg(feature = \"auth\")] String"),
            ("String", "#[cfg(unix)] String"),
            (
                "String",
                "#[cfg_attr(feature = \"auth\", cfg(unix))] String",
            ),
            ("Started", "#[non_exhaustive] Started"),
            (
                "Started",
                "#[cfg_attr(feature = \"auth\", non_exhaustive)] Started",
            ),
        ] {
            let (directory, mut ledger) = fixture();
            // Named field attributes precede the field name, rather than its type.
            let (before, after) = if payload.starts_with('{') && before == "String" {
                ("method: String", after.replace("String", "method: String"))
            } else {
                (before, after.into())
            };
            fs::write(
                directory.path().join("core/src/lib.rs"),
                format!("pub enum AuthEvent {{ Started {payload} }}"),
            )
            .unwrap();
            review(directory.path(), &mut ledger);
            replace(directory.path(), "core", before, &after);
            let actual = inventory(directory.path(), &ledger).unwrap();
            let error = validate(directory.path(), &ledger, &actual).unwrap_err();
            assert!(error.contains("core|core::AuthEvent::Started#variant"));
            assert!(
                error.contains(if before == "Started" && after.contains("cfg_attr") {
                    "unreviewed declaration"
                } else {
                    "changed declaration requires review"
                })
            );
            review(directory.path(), &mut ledger);
            replace(directory.path(), "core", &after, before);
            let actual = inventory(directory.path(), &ledger).unwrap();
            assert!(validate(directory.path(), &ledger, &actual).is_err());
        }
    }
}

#[test]
fn enum_test_only_fields_and_documentation_do_not_change_public_shapes() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub enum Event { Named { value: u16 }, Tuple(u16), Unit }",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    replace(
        directory.path(),
        "core",
        "value: u16",
        "#[doc = \"Field\"] value: u16, #[cfg(test)] extra: bool",
    );
    replace(
        directory.path(),
        "core",
        "Tuple(u16)",
        "Tuple(u16, #[cfg(test)] bool)",
    );
    replace(
        directory.path(),
        "core",
        "Unit",
        "#[doc = \"Variant\"] Unit",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn reexporting_a_private_module_with_only_generated_names_requires_supplements() {
    for export in ["pub use generated::Generated;", "pub use generated::*;"] {
        let (directory, ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!(
                r"
            mod generated {{
                macro_rules! make {{ () => {{ pub struct Generated; }}; }}
                make!();
            }}
            {export}
        "
            ),
        )
        .unwrap();
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
    }
}

#[test]
fn unit_tuple_and_named_empty_variants_have_distinct_shapes() {
    let (directory, mut ledger) = fixture();
    replace(
        directory.path(),
        "native-v4",
        "pub struct Options",
        "pub enum Event { Empty } pub struct Options",
    );
    review(directory.path(), &mut ledger);
    replace(directory.path(), "native-v4", "Empty }", "Empty() }");
    assert!(
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap()
        )
        .unwrap_err()
        .contains("changed declaration")
    );
    replace(directory.path(), "native-v4", "Empty() }", "Empty {} }");
    assert!(
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap()
        )
        .unwrap_err()
        .contains("changed declaration")
    );
}

#[test]
fn conditional_import_owner_swaps_require_review() {
    for (imports, owner) in [
        (
            "#[cfg(unix)] use A as Choice; #[cfg(not(unix))] use B as Choice;",
            "Choice",
        ),
        (
            "#[cfg(unix)] use crate::{A as Choice}; #[cfg(not(unix))] use crate::{B as Choice};
             type Alias = Choice;",
            "Alias",
        ),
        (
            "mod aliases {
                #[cfg(unix)] pub use crate::A as Choice;
                #[cfg(not(unix))] pub use crate::B as Choice;
             }
             use aliases::Choice;",
            "Choice",
        ),
        (
            "mod first { pub use crate::A as Choice; }
             mod second { pub use crate::B as Choice; }
             #[cfg(unix)] use first as selected;
             #[cfg(not(unix))] use second as selected;",
            "selected::Choice",
        ),
        (
            "mod first { pub use crate::A as Choice; }
             mod second { pub use crate::B as Choice; }
             #[cfg(unix)] use first::*;
             #[cfg(not(unix))] use second::*;",
            "Choice",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!(
                "pub struct A; pub struct B; {imports}
                 impl {owner} {{ pub fn run() {{}} pub const LIMIT: usize = 1; }}"
            ),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        let members: Vec<_> = ledger
            .declarations
            .iter()
            .filter(|d| {
                d.surface == "core" && matches!(d.kind.as_str(), "method" | "associated_constant")
            })
            .collect();
        assert_eq!(members.len(), 4, "{imports}");
        for member in members {
            let predicate = if member.id.contains("::A::") {
                "cfg (unix)"
            } else {
                assert!(member.id.contains("::B::"));
                "cfg (not (unix))"
            };
            assert_eq!(member.conditions, [predicate], "{imports}");
        }
        // Private imports do not get inventory entries of their own. Swapping
        // their owners must still invalidate the reviewed member availability.
        replace(
            directory.path(),
            "core",
            "A as Choice",
            "Placeholder as Choice",
        );
        replace(directory.path(), "core", "B as Choice", "A as Choice");
        replace(
            directory.path(),
            "core",
            "Placeholder as Choice",
            "B as Choice",
        );
        let actual = inventory(directory.path(), &ledger).unwrap();
        let error = validate(directory.path(), &ledger, &actual).unwrap_err();
        assert!(error.contains("unreviewed declaration"), "{imports}");
        assert!(error.contains("stale declaration"), "{imports}");
    }
}

#[test]
fn import_alias_impl_and_member_predicates_are_combined() {
    for imports in [
        "#[cfg(unix)] use A as First;
         #[cfg(feature = \"route\")] use First as Choice;",
        "mod types { #[cfg(feature = \"route\")] pub use crate::A as Choice; }
         #[cfg(unix)] use types as selected;
         use selected::*;",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!(
                "pub struct A; {imports}
                 #[cfg(feature = \"alias\")] type Alias = Choice;
                 #[cfg(feature = \"implementation\")] impl Alias {{
                     #[cfg(feature = \"member\")] pub fn run() {{}}
                 }}"
            ),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        let method = ledger
            .declarations
            .iter()
            .find(|d| d.surface == "core" && d.kind == "method")
            .unwrap();
        assert_eq!(
            method.conditions,
            [
                "cfg (feature = \"alias\")",
                "cfg (feature = \"implementation\")",
                "cfg (feature = \"member\")",
                "cfg (feature = \"route\")",
                "cfg (unix)",
            ]
        );
        replace(
            directory.path(),
            "core",
            "feature = \"route\"",
            "feature = \"changed\"",
        );
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(validate(directory.path(), &ledger, &actual).is_err());
    }
}

#[test]
fn conditional_imports_to_one_owner_union_availability_without_order_dependence() {
    let (directory, mut ledger) = fixture();
    let imports = "#[cfg(unix)] use Options as Choice; #[cfg(windows)] use Options as Choice;";
    fs::write(
        directory.path().join("core/src/lib.rs"),
        format!("pub struct Options; {imports} impl Choice {{ pub fn run() {{}} }}"),
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let methods: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "method")
        .collect();
    assert_eq!(methods.len(), 1);
    assert_eq!(
        methods[0].conditions,
        ["any ([cfg (unix)], [cfg (windows)])"]
    );
    replace(
        directory.path(),
        "core",
        imports,
        "#[cfg(windows)] use Options as Choice; #[cfg(unix)] use Options as Choice;",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn conditional_impl_owners_preserve_predicates_on_generator_anchors() {
    let (directory, ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub struct A; pub struct B;
         #[cfg(unix)] use A as Choice;
         #[cfg(not(unix))] use B as Choice;
         #[generate_api] impl Choice { #[generate_member] pub fn run() {} }",
    )
    .unwrap();
    let raw = discover_sources(directory.path(), &ledger).unwrap();
    let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
    assert_eq!(anchors.len(), 4);
    for anchor in anchors {
        let predicate = if anchor.id.contains("::A::") {
            "cfg (unix)"
        } else {
            assert!(anchor.id.contains("::B::"));
            "cfg (not (unix))"
        };
        assert_eq!(anchor.conditions, [predicate]);
    }
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
}

#[test]
fn procedural_attributes_on_public_modules_require_supplements_even_when_empty() {
    for body in ["", "pub struct Options;"] {
        for attribute in [
            "#[generate_api]",
            "#[cfg_attr(feature = \"new\", doc(hidden), generate_api)]",
        ] {
            let (directory, mut ledger) = fixture();
            fs::write(
                directory.path().join("core/src/lib.rs"),
                format!("pub struct Sentinel; #[cfg(unix)] pub mod api {{ {body} }}"),
            )
            .unwrap();
            review(directory.path(), &mut ledger);
            replace(
                directory.path(),
                "core",
                "pub mod api",
                &format!("{attribute} pub mod api"),
            );
            let raw = discover_sources(directory.path(), &ledger).unwrap();
            let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
            assert_eq!(anchors.len(), 1);
            assert!(
                anchors[0]
                    .id
                    .starts_with("core|core::api::@macro:attribute:module:")
            );
            assert_eq!(anchors[0].conditions, ["cfg (unix)"]);
            assert!(
                inventory(directory.path(), &ledger)
                    .unwrap_err()
                    .contains("unreviewed macro")
            );
            ledger.supplements = vec![Supplement {
                anchor: anchors[0].id.clone(),
                expected_shape: anchors[0].shape.clone(),
                rationale: "Review the module generator's output.".into(),
                declarations: Vec::new(),
            }];
            review(directory.path(), &mut ledger);
            let actual = inventory(directory.path(), &ledger).unwrap();
            validate(directory.path(), &ledger, &actual).unwrap();
        }
    }
}

#[test]
fn module_attribute_reviews_cover_full_inputs_through_exports_and_explicit_roots() {
    for (visibility, contents, export, roots) in [
        ("pub", "", "", Vec::new()),
        ("pub", "", "pub use generated as api;", Vec::new()),
        (
            "",
            "pub struct Options;",
            "pub use generated::Options;",
            Vec::new(),
        ),
        ("", "", "pub use generated::Generated;", Vec::new()),
        ("", "", "pub use generated::*;", Vec::new()),
        (
            "",
            "pub mod nested { pub struct Options; }",
            "",
            vec!["core::generated::nested::Options".into()],
        ),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!(
                "pub struct Sentinel;
                 #[generate_api] {visibility} mod generated {{
                     fn helper() {{ let _ = 1; }} {contents}
                 }}
                 {export}"
            ),
        )
        .unwrap();
        ledger
            .surfaces
            .iter_mut()
            .find(|s| s.name == "core")
            .unwrap()
            .roots = roots;
        let raw = discover_sources(directory.path(), &ledger).unwrap();
        let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
        assert_eq!(anchors.len(), 1, "{visibility} {contents} {export}");
        assert!(anchors[0].shape.contains("fn helper () { let _ = 1 ; }"));
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
        ledger.supplements = vec![Supplement {
            anchor: anchors[0].id.clone(),
            expected_shape: anchors[0].shape.clone(),
            rationale: "Review all module input, including private helpers.".into(),
            declarations: Vec::new(),
        }];
        review(directory.path(), &mut ledger);
        let actual = inventory(directory.path(), &ledger).unwrap();
        validate(directory.path(), &ledger, &actual).unwrap();
        replace(directory.path(), "core", "let _ = 1", "let _ = 2");
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("changed or invalid supplement")
        );
    }
}

#[test]
fn unreachable_and_test_only_module_generators_do_not_require_supplements() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub struct Options;
         #[generate_api] mod hidden { pub struct Unreachable; }
         #[cfg(test)] #[generate_api] pub mod tests {}
         #[cfg_attr(test, generate_api)] pub mod api {}",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(!ledger.declarations.iter().any(|d| d.kind == "macro"));
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn path_attributes_on_modules_are_benign_but_generators_still_require_review() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "#[cfg_attr(unix, doc(hidden))] #[path = \"api.rs\"] pub mod api;",
    )
    .unwrap();
    fs::write(
        directory.path().join("core/src/api.rs"),
        "pub struct Options;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(!ledger.declarations.iter().any(|d| d.kind == "macro"));
    replace(
        directory.path(),
        "core",
        "pub mod api",
        "#[generate_api] pub mod api",
    );
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
}

#[test]
fn import_predicates_include_parent_modules_and_cfg_attr_but_exclude_tests() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r#"
        pub struct A;
        #[cfg(feature = "parent")] mod owners {
            #[cfg_attr(feature = "route", cfg(unix))] use crate::A as Choice;
            #[cfg(test)] use crate::Missing as Choice;
            impl Choice { pub fn run() {} }
        }
        "#,
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let method = ledger
        .declarations
        .iter()
        .find(|d| d.surface == "core" && d.kind == "method")
        .unwrap();
    assert_eq!(
        method.conditions,
        [
            "cfg (feature = \"parent\")",
            "cfg_attr (feature = \"route\" , cfg (unix))",
        ]
    );
    replace(directory.path(), "core", "cfg(unix)", "cfg(windows)");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(validate(directory.path(), &ledger, &actual).is_err());
}

#[test]
fn conditional_import_cycles_reach_a_stable_inventory() {
    let (directory, mut ledger) = fixture();
    let source = r"
        pub struct A;
        pub struct B;
        mod first {
            #[cfg(unix)] pub use crate::A as Choice;
            #[cfg(not(unix))] pub use crate::second::Choice;
        }
        mod second {
            #[cfg(not(unix))] pub use crate::B as Choice;
            #[cfg(unix)] pub use crate::first::Choice;
        }
        use first::Choice;
        impl Choice { pub fn run() {} }
    ";
    fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
    review(directory.path(), &mut ledger);
    let methods: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "method")
        .collect();
    assert_eq!(methods.len(), 2);
    assert!(methods.iter().all(|d| !d.conditions.is_empty()));
    // Reverse module traversal order without changing either binding graph.
    let first = source.find("        mod first").unwrap();
    let second = source.find("        mod second").unwrap();
    let end = source.find("        use first::Choice").unwrap();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        format!(
            "{}{}{}{}",
            &source[..first],
            &source[second..end],
            &source[first..second],
            &source[end..]
        ),
    )
    .unwrap();
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn selected_modules_retain_their_own_and_ancestor_generator_anchors() {
    for roots in [Vec::new(), vec!["core::hidden::generated".into()]] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            "#[outer_generator] mod hidden {
                 #[inner_generator] pub mod generated {}
             }
             pub use hidden::generated as api;",
        )
        .unwrap();
        ledger
            .surfaces
            .iter_mut()
            .find(|s| s.name == "core")
            .unwrap()
            .roots = roots;
        let raw = discover_sources(directory.path(), &ledger).unwrap();
        let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
        assert_eq!(anchors.len(), 2);
        assert!(anchors.iter().any(|d| {
            d.id.contains("hidden::@macro:attribute:module:outer_generator")
        }));
        assert!(anchors.iter().any(|d| {
            d.id.contains("generated::@macro:attribute:module:inner_generator")
        }));
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
    }
}

#[test]
fn module_generators_on_impl_sources_remain_reachable_through_selected_owners() {
    for members in ["", "pub fn run() {}"] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!(
                "pub struct Options;
                 #[generate_api] pub mod implementation {{ impl crate::Options {{ {members} }} }}
                 pub struct Unreachable;
                 #[unrelated_generator] pub mod unrelated {{ impl crate::Unreachable {{}} }}"
            ),
        )
        .unwrap();
        ledger
            .surfaces
            .iter_mut()
            .find(|s| s.name == "core")
            .unwrap()
            .roots = vec!["core::Options".into()];
        let raw = discover_sources(directory.path(), &ledger).unwrap();
        let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
        assert_eq!(anchors.len(), 1);
        assert!(
            anchors[0]
                .id
                .contains("implementation::@macro:attribute:module:generate_api")
        );
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
    }
}

#[test]
fn conditional_module_paths_cannot_hide_api_changes_behind_a_default_file() {
    let (directory, mut ledger) = fixture();
    let src = directory.path().join("core/src");
    fs::write(src.join("lib.rs"), "pub mod selected;").unwrap();
    fs::write(
        src.join("selected.rs"),
        "pub struct Options { pub value: u16 }",
    )
    .unwrap();
    fs::write(src.join("unix.rs"), "pub struct Options { pub value: u32 }").unwrap();
    review(directory.path(), &mut ledger);
    let ledger_path = directory.path().join(rumqttc_parity_audit::LEDGER);
    fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    let original = serde_json::to_string_pretty(&ledger).unwrap();
    fs::write(&ledger_path, &original).unwrap();
    fs::write(
        directory.path().join(rumqttc_parity_audit::MARKDOWN),
        rumqttc_parity_audit::render(&ledger),
    )
    .unwrap();
    let invoke = |command: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
            .args([command, "--root"])
            .arg(directory.path())
            .output()
            .unwrap()
    };
    assert!(invoke("check").status.success());
    fs::write(
        src.join("lib.rs"),
        "#[cfg_attr(unix, path = \"unix.rs\")] pub mod selected;",
    )
    .unwrap();
    let compiled =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args(["--edition=2024", "--crate-type=lib", "--crate-name=fixture"])
            .arg(src.join("lib.rs"))
            .arg("-o")
            .arg(directory.path().join("fixture.rlib"))
            .output()
            .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    // Both files exist and the fixture compiles, but discovery must not accept
    // the reviewed default file when a different file may supply the API.
    for command in ["inventory", "check"] {
        let output = invoke(command);
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("unsupported conditional module path"),
            "{error}"
        );
        assert!(error.contains("core::selected"), "{error}");
        assert!(error.contains("lib.rs"), "{error}");
    }
    assert_eq!(fs::read_to_string(ledger_path).unwrap(), original);
}

#[test]
fn nested_and_mixed_conditional_module_paths_are_rejected() {
    for attribute in [
        "#[cfg_attr(unix, path = \"unix.rs\")]",
        "#[cfg_attr(feature = \"platform\", doc(hidden), path = \"unix.rs\")]",
        "#[cfg_attr(unix, path = \"unix.rs\", doc(hidden))]",
        "#[cfg_attr(unix, cfg_attr(feature = \"platform\", path = \"unix.rs\"))]",
        "#[cfg_attr(not(test), path = \"unix.rs\")]",
        "#[path = \"selected.rs\"] #[cfg_attr(unix, path = \"unix.rs\")]",
        "#[cfg_attr(unix, path = \"unix.rs\")] #[path = \"selected.rs\"]",
    ] {
        for module in [
            "pub mod selected;",
            "pub mod selected { pub struct Options; }",
        ] {
            let (directory, ledger) = fixture();
            let src = directory.path().join("core/src");
            fs::write(src.join("lib.rs"), format!("{attribute} {module}")).unwrap();
            fs::write(src.join("selected.rs"), "pub struct Options;").unwrap();
            fs::write(src.join("unix.rs"), "pub struct Different;").unwrap();
            let error = discover_sources(directory.path(), &ledger).unwrap_err();
            assert!(
                error.contains("unsupported conditional module path"),
                "{attribute}: {error}"
            );
            assert!(error.contains("core::selected"), "{error}");
        }
    }
}

#[test]
fn conditional_inner_module_paths_are_rejected_before_child_discovery() {
    for source in [
        "pub mod selected { #![cfg_attr(unix, path = \"alternate\")] pub mod child; }",
        "pub mod selected;",
    ] {
        let (directory, ledger) = fixture();
        let src = directory.path().join("core/src");
        fs::write(src.join("lib.rs"), source).unwrap();
        fs::write(
            src.join("selected.rs"),
            "#![cfg_attr(unix, path = \"alternate\")] pub mod child;",
        )
        .unwrap();
        fs::create_dir_all(src.join("selected")).unwrap();
        fs::write(src.join("selected/child.rs"), "pub struct Default;").unwrap();
        let error = discover_sources(directory.path(), &ledger).unwrap_err();
        assert!(
            error.contains("unsupported conditional module path"),
            "{error}"
        );
        assert!(error.contains("core::selected"), "{error}");
    }
}

#[test]
fn inactive_conditional_paths_and_path_text_in_documentation_are_benign() {
    for attribute in [
        "#[cfg_attr(test, path = \"missing.rs\")]",
        "#[cfg_attr(all(test, unix), path = \"missing.rs\")]",
        "#[cfg_attr(unix, cfg_attr(test, path = \"missing.rs\"))]",
        "#[cfg_attr(test, cfg_attr(unix, path = \"missing.rs\"))]",
        "#[cfg_attr(unix, doc = \"path = missing.rs\")]",
    ] {
        let (directory, mut ledger) = fixture();
        let src = directory.path().join("core/src");
        fs::write(src.join("lib.rs"), "pub mod selected;").unwrap();
        fs::write(src.join("selected.rs"), "pub struct Options;").unwrap();
        review(directory.path(), &mut ledger);
        fs::write(src.join("lib.rs"), format!("{attribute} pub mod selected;")).unwrap();
        let actual = inventory(directory.path(), &ledger).unwrap();
        validate(directory.path(), &ledger, &actual).unwrap();
    }
    let (directory, ledger) = fixture();
    replace(
        directory.path(),
        "core",
        "pub struct Options",
        "#[cfg(test)] #[cfg_attr(unix, path = \"missing.rs\")] mod tests; pub struct Options",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn explicit_cfg_module_paths_review_both_source_alternatives() {
    let (directory, mut ledger) = fixture();
    let src = directory.path().join("core/src");
    fs::write(
        src.join("lib.rs"),
        "#[cfg(unix)] #[path = \"unix.rs\"] pub mod selected;
         #[cfg(not(unix))] #[path = \"selected.rs\"] pub mod selected;",
    )
    .unwrap();
    for name in ["unix.rs", "selected.rs"] {
        fs::write(src.join(name), "pub struct Options { pub value: u16 }").unwrap();
    }
    review(directory.path(), &mut ledger);
    let fields: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "field")
        .collect();
    assert_eq!(fields.len(), 2);
    assert!(
        fields
            .iter()
            .any(|d| d.source == "core/src/unix.rs" && d.conditions == ["cfg (unix)"])
    );
    assert!(
        fields
            .iter()
            .any(|d| d.source == "core/src/selected.rs" && d.conditions == ["cfg (not (unix))"])
    );
    for name in ["unix.rs", "selected.rs"] {
        fs::write(src.join(name), "pub struct Options { pub value: u32 }").unwrap();
        let actual = inventory(directory.path(), &ledger).unwrap();
        let error = validate(directory.path(), &ledger, &actual).unwrap_err();
        assert!(error.contains("changed declaration"), "{error}");
        fs::write(src.join(name), "pub struct Options { pub value: u16 }").unwrap();
    }
}

#[test]
fn test_only_members_do_not_shift_public_declaration_indexes() {
    for (before, after, public_before, public_after) in [
        (
            "pub struct Options { pub first: u16, pub second: u32 }",
            "pub struct Options {
                 TEST_ONLY helper_before: bool, pub first: u16,
                 TEST_ONLY pub helper_between: bool, pub second: u32
             }",
            "u16",
            "u64",
        ),
        (
            "pub struct Options(pub u16, pub u32);",
            "pub struct Options(TEST_ONLY bool, pub u16, TEST_ONLY pub bool, pub u32);",
            "u16",
            "u64",
        ),
        (
            "pub enum Options { First, Second }",
            "pub enum Options { TEST_ONLY HelperBefore, First, TEST_ONLY HelperBetween, Second }",
            "First",
            "Renamed",
        ),
        (
            "pub union Options { pub first: u16, pub second: u32 }",
            "pub union Options {
                 TEST_ONLY helper_before: bool, pub first: u16,
                 TEST_ONLY pub helper_between: bool, pub second: u32
             }",
            "u16",
            "u64",
        ),
    ] {
        for predicate in [
            "test",
            "all(test, unix)",
            "not(not(test))",
            "all(feature = \"helper\", test)",
        ] {
            let (directory, mut ledger) = fixture();
            fs::write(directory.path().join("core/src/lib.rs"), before).unwrap();
            review(directory.path(), &mut ledger);
            let after = after.replace("TEST_ONLY", &format!("#[cfg({predicate})]"));
            fs::write(directory.path().join("core/src/lib.rs"), &after).unwrap();
            let actual = inventory(directory.path(), &ledger).unwrap();
            assert_eq!(actual, ledger.declarations, "{after}");
            validate(directory.path(), &ledger, &actual).unwrap();
            // Ignoring helpers must not hide a real public member change.
            replace(directory.path(), "core", public_before, public_after);
            let actual = inventory(directory.path(), &ledger).unwrap();
            assert!(
                validate(directory.path(), &ledger, &actual).is_err(),
                "{after}"
            );
        }
    }
}

#[test]
fn private_fields_still_count_after_test_only_fields_are_excluded() {
    for (before, after, member) in [
        (
            "pub struct Options { private: bool, pub value: u16 }",
            "pub struct Options {
                 #[cfg(test)] helper_before: bool, private: bool,
                 #[cfg(test)] pub helper_between: bool, pub value: u16
             }",
            "value",
        ),
        (
            "pub struct Options(bool, pub u16);",
            "pub struct Options(#[cfg(test)] bool, bool, #[cfg(test)] pub bool, pub u16);",
            "1",
        ),
        (
            "pub union Options { private: bool, pub value: u16 }",
            "pub union Options {
                 #[cfg(test)] helper_before: bool, private: bool,
                 #[cfg(test)] pub helper_between: bool, pub value: u16
             }",
            "value",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), before).unwrap();
        review(directory.path(), &mut ledger);
        let field = ledger
            .declarations
            .iter()
            .find(|d| d.id == format!("core|core::Options::{member}#field"))
            .unwrap();
        assert_eq!(field.shape, "1: u16");
        fs::write(directory.path().join("core/src/lib.rs"), after).unwrap();
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert_eq!(actual, ledger.declarations, "{after}");
        validate(directory.path(), &ledger, &actual).unwrap();
    }
}

#[test]
fn symbolic_feature_and_platform_members_still_count_toward_declaration_indexes() {
    for (source, member) in [
        (
            "pub struct Options { CONDITIONAL pub conditional: bool, pub value: u16 }",
            "value#field",
        ),
        (
            "pub struct Options(CONDITIONAL pub bool, pub u16);",
            "1#field",
        ),
        (
            "pub enum Options { CONDITIONAL Conditional, Value }",
            "Value#variant",
        ),
        (
            "pub union Options { CONDITIONAL pub conditional: bool, pub value: u16 }",
            "value#field",
        ),
    ] {
        for predicate in ["unix", "windows", "feature = \"optional\""] {
            let (directory, mut ledger) = fixture();
            let source = source.replace("CONDITIONAL", &format!("#[cfg({predicate})]"));
            fs::write(directory.path().join("core/src/lib.rs"), &source).unwrap();
            review(directory.path(), &mut ledger);
            let indexed = ledger
                .declarations
                .iter()
                .find(|d| d.id == format!("core|core::Options::{member}"))
                .unwrap();
            assert!(indexed.shape.starts_with("1: "), "{source}");
            let conditional: Vec<_> = ledger
                .declarations
                .iter()
                .filter(|d| d.surface == "core" && !d.conditions.is_empty())
                .collect();
            assert_eq!(conditional.len(), 1, "{source}");
            assert_eq!(conditional[0].conditions, [format!("cfg ({predicate})")]);
            let actual = inventory(directory.path(), &ledger).unwrap();
            validate(directory.path(), &ledger, &actual).unwrap();
        }
    }
}

#[test]
fn values_sharing_module_names_require_review_through_exports_and_roots() {
    for (declaration, kind) in [
        ("pub fn readiness() {}", "function"),
        ("pub const readiness: bool = true;", "constant"),
        ("pub static readiness: bool = true;", "static"),
    ] {
        for (container, export, prefix) in [
            ("", "", "core"),
            ("hidden", "pub use hidden::readiness;", "core::hidden"),
            ("hidden", "pub use hidden::*;", "core::hidden"),
        ] {
            for before_module in [false, true] {
                let (directory, mut ledger) = fixture();
                let module = "pub mod readiness { pub struct Nested; }";
                let contents = if before_module {
                    format!("ADDITION {module}")
                } else {
                    format!("{module} ADDITION")
                };
                let source = if container.is_empty() {
                    contents
                } else {
                    format!("mod {container} {{ {contents} }} {export}")
                };
                fs::write(
                    directory.path().join("core/src/lib.rs"),
                    source.replace("ADDITION", ""),
                )
                .unwrap();
                review(directory.path(), &mut ledger);
                fs::write(
                    directory.path().join("core/src/lib.rs"),
                    source.replace("ADDITION", declaration),
                )
                .unwrap();
                let actual = inventory(directory.path(), &ledger).unwrap();
                let id = format!("core|{prefix}::readiness#{kind}");
                assert!(actual.iter().any(|d| d.id == id), "{source}: {declaration}");
                assert!(
                    actual
                        .iter()
                        .any(|d| d.id == format!("core|{prefix}::readiness::Nested#struct"))
                );
                let error = validate(directory.path(), &ledger, &actual).unwrap_err();
                assert!(
                    error.contains(&format!("unreviewed declaration: {id}")),
                    "{error}"
                );
                assert!(
                    error.contains(&format!("missing boundary decision: {id}")),
                    "{error}"
                );
                ledger
                    .surfaces
                    .iter_mut()
                    .find(|s| s.name == "core")
                    .unwrap()
                    .roots = vec![format!("{prefix}::readiness")];
                let rooted = inventory(directory.path(), &ledger).unwrap();
                assert!(rooted.iter().any(|d| d.id == id));
                assert!(
                    rooted
                        .iter()
                        .any(|d| d.id == format!("core|{prefix}::readiness::Nested#struct"))
                );
            }
        }
    }
}

#[test]
fn public_values_do_not_expose_same_named_private_modules() {
    for source in [
        "#[module_generator] mod readiness { pub struct Hidden; } pub fn readiness() {}",
        "pub fn readiness() {} #[module_generator] mod readiness { pub struct Hidden; }",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
        review(directory.path(), &mut ledger);
        let actual = inventory(directory.path(), &ledger).unwrap();
        let core: Vec<_> = actual.iter().filter(|d| d.surface == "core").collect();
        assert_eq!(core.len(), 1, "{source}");
        assert_eq!(core[0].id, "core|core::readiness#function");
        validate(directory.path(), &ledger, &actual).unwrap();
    }
}

#[test]
fn same_named_module_and_function_attributes_retain_both_generator_anchors() {
    let (directory, ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "#[module_generator] pub mod readiness {} #[function_generator] pub fn readiness() {}",
    )
    .unwrap();
    let raw = discover_sources(directory.path(), &ledger).unwrap();
    let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
    assert_eq!(anchors.len(), 2);
    assert!(
        anchors
            .iter()
            .any(|d| d.id.contains("attribute:module:module_generator"))
    );
    assert!(
        anchors
            .iter()
            .any(|d| d.id.contains("attribute:function:function_generator"))
    );
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
}

#[test]
fn c_evidence_requires_complete_code_identifiers() {
    let (directory, mut ledger) = fixture();
    ledger.decisions[0].evidence = vec![Evidence {
        path: "evidence.c".into(),
        symbol: "main".into(),
    }];
    let actual = inventory(directory.path(), &ledger).unwrap();
    fs::write(
        directory.path().join("evidence.c"),
        "int main(void) { return 0; }",
    )
    .unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
    for source in [
        "int renamed_main(void) { return 0; }",
        "int main_suffix(void) { return 0; }",
        "int domain(void) { return 0; }",
        "int main2(void) { return 0; }",
        "/* int main(void) {} */ int renamed_main(void) { return 0; }",
        "// int main(void) {}\nint renamed_main(void) { return 0; }",
        "const char *text = \"main(\"; int renamed_main(void) { return 0; }",
        "const char *text = \"escaped \\\" main(\"; int renamed_main(void) { return 0; }",
        "int character = 'main('; int renamed_main(void) { return 0; }",
        "int renamed_\\\nmain(void) { return 0; }",
        "// continued comment \\\nint main(void) {}\nint renamed_main(void) { return 0; }",
        "int renamed_\\\r\nmain(void) { return 0; }",
        "int main; int renamed_main(void) { return 0; }",
        "int main/**/suffix(void) { return 0; }",
    ] {
        fs::write(directory.path().join("evidence.c"), source).unwrap();
        let error = validate(directory.path(), &ledger, &actual).unwrap_err();
        assert!(
            error.contains("missing evidence symbol main"),
            "{source}: {error}"
        );
    }
}

#[test]
fn c_evidence_accepts_whitespace_comments_and_line_splices_between_tokens() {
    let (directory, mut ledger) = fixture();
    ledger.decisions[0].evidence = vec![Evidence {
        path: "evidence.c".into(),
        symbol: "main".into(),
    }];
    let actual = inventory(directory.path(), &ledger).unwrap();
    for source in [
        "int main(void) { return 0; }",
        "int main   (void) { return 0; }",
        "int main\t(void) { return 0; }",
        "int main\r\n(void) { return 0; }",
        "int main/* entry point */(void) { return 0; }",
        "int main // entry point\n(void) { return 0; }",
        "int ma\\\nin(void) { return 0; }",
        "const char *text = \"not a /* comment */\"; int main(void) { return 0; }",
    ] {
        fs::write(directory.path().join("evidence.c"), source).unwrap();
        validate(directory.path(), &ledger, &actual)
            .unwrap_or_else(|error| panic!("{source}: {error}"));
    }
}

#[test]
fn function_attributes_do_not_expose_same_named_private_module_generators() {
    let (directory, ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "#[module_generator] mod readiness { pub struct Hidden; }
         #[function_generator] pub fn readiness() {}",
    )
    .unwrap();
    let raw = discover_sources(directory.path(), &ledger).unwrap();
    let anchors: Vec<_> = raw.iter().filter(|d| d.kind == "macro").collect();
    assert_eq!(anchors.len(), 1);
    assert!(
        anchors[0]
            .id
            .contains("attribute:function:function_generator")
    );
    assert!(!raw.iter().any(|d| d.id.contains("Hidden")));
}

#[test]
fn intermediate_external_reexports_require_review_without_exposing_unrelated_imports() {
    for source in [
        "mod hidden {
             pub use std::time::Duration as Value;
             pub use std::io::Error as Unrelated;
         }
         pub use hidden::Value;",
        "mod original {
             pub use std::time::Duration as Actual;
             pub use std::io::Error as Unrelated;
         }
         mod hidden { pub use crate::original::Actual as Value; }
         pub use hidden::Value;",
        "mod hidden {
             pub mod original { pub use std::time::Duration as Value; }
             pub use original as selected;
         }
         pub use hidden::selected::Value;",
        "mod hidden {
             pub use std::time::Duration as Value;
             pub use std::io::Error as Unrelated;
         }
         mod relay { pub use crate::hidden::*; }
         pub use relay::Value;",
        "mod hidden { pub use std::time::Duration as Value; } pub use hidden::*;",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
        review(directory.path(), &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.surface == "core" && d.shape.contains("std::time::Duration")),
            "{source}"
        );
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.id.contains("Unrelated")),
            "{source}"
        );
        replace(directory.path(), "core", "Duration", "Instant");
        let actual = inventory(directory.path(), &ledger).unwrap();
        let error = validate(directory.path(), &ledger, &actual).unwrap_err();
        assert!(
            error.contains("unreviewed declaration"),
            "{source}: {error}"
        );
        assert!(error.contains("stale declaration"), "{source}: {error}");
    }
}

#[test]
fn intermediate_reexport_predicates_and_symbolic_cycles_remain_review_inputs() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "mod first {
             #[cfg(unix)] pub use std::time::Duration as Value;
             #[cfg(not(unix))] pub use crate::second::Value;
             #[cfg(test)] pub use crate::Missing as Value;
         }
         mod second {
             #[cfg(not(unix))] pub use std::time::Instant as Value;
             #[cfg(unix)] pub use crate::first::Value;
         }
         pub use first::Value;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let core: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core")
        .collect();
    assert_eq!(core.len(), 5);
    assert_eq!(core.iter().filter(|d| !d.conditions.is_empty()).count(), 4);
    assert!(!core.iter().any(|d| d.id.contains("Missing")));
    replace(directory.path(), "core", "unix", "windows");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("unreviewed declaration")
    );
}

#[test]
fn local_enum_glob_reexports_review_payloads_and_new_variants() {
    for export in [
        "pub use hidden::E::*;",
        "use hidden::E as Choice; pub use Choice::*;",
        "mod relay { pub use crate::hidden::E as Choice; } pub use relay::Choice::*;",
        "mod relay { pub use crate::hidden::E::*; } pub use relay::*;",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!("mod hidden {{ pub enum E {{ A(u16) }} pub struct Unrelated; }} {export}"),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == "core|core::hidden::E::A#variant"),
            "{export}"
        );
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == "core|core::hidden::E#enum"),
            "{export}"
        );
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.id.contains("Unrelated"))
        );
        replace(directory.path(), "core", "A(u16)", "A(u32)");
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            validate(directory.path(), &ledger, &actual)
                .unwrap_err()
                .contains("changed declaration"),
            "{export}"
        );
        replace(directory.path(), "core", "A(u32)", "A(u16), B");
        let actual = inventory(directory.path(), &ledger).unwrap();
        let error = validate(directory.path(), &ledger, &actual).unwrap_err();
        assert!(
            error.contains("unreviewed declaration: core|core::hidden::E::B#variant"),
            "{export}: {error}"
        );
    }
}

#[test]
fn named_variant_reexports_retain_enum_metadata_and_only_selected_variants() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "mod hidden {
             #[repr(u8)] pub enum E { A(u16), B }
             impl E { pub fn value(&self) -> u16 { 0 } }
         }
         pub use hidden::E::A;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::E::A#variant")
    );
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::E::value#method")
    );
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::E::B#variant")
    );
    replace(directory.path(), "core", "repr(u8)", "repr(u16)");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("changed declaration")
    );
}

#[test]
fn enum_globs_preserve_variant_conditions_and_skip_test_only_helpers() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "mod hidden {
             pub enum E {
                 #[cfg(feature = \"first\")] A(u16),
                 #[cfg(not(feature = \"first\"))] B { value: u32 },
             }
         }
         pub use hidden::E::*;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let variants: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "variant")
        .collect();
    assert_eq!(variants.len(), 2);
    assert!(variants.iter().all(|d| d.conditions.len() == 1));
    replace(
        directory.path(),
        "core",
        "pub enum E {",
        "pub enum E { #[cfg(test)] Helper,",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_eq!(actual, ledger.declarations);
    replace(directory.path(), "core", "\"first\"", "\"second\"");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("unreviewed declaration")
    );
}

#[test]
fn enum_glob_exports_require_review_of_generating_attributes() {
    for contents in ["A(u16)", "", "#[cfg(test)] Helper"] {
        for export in [
            "pub use hidden::E::*;",
            "mod relay { pub use crate::hidden::E::*; } pub use relay::Generated;",
        ] {
            let (directory, ledger) = fixture();
            fs::write(
                directory.path().join("core/src/lib.rs"),
                format!(
                    "mod hidden {{ #[generate_variants] pub enum E {{ {contents} }} }} {export}"
                ),
            )
            .unwrap();
            let raw = discover_sources(directory.path(), &ledger).unwrap();
            assert!(raw.iter().any(|d| d.kind == "macro" && d.id.contains("attribute:enum:generate_variants")), "{contents}: {export}");
            assert!(
                inventory(directory.path(), &ledger)
                    .unwrap_err()
                    .contains("unreviewed macro")
            );
        }
    }
}

#[test]
fn unrelated_private_module_globs_do_not_become_reexport_dependencies() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "mod other { pub use std::io::Error as Unrelated; }
         mod another { pub use std::time::Instant as Unrelated; }
         mod hidden {
             pub use std::time::Duration as Value;
             pub use crate::other::*;
         }
         pub use hidden::Value;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.shape.starts_with("use crate::other::*"))
    );
    replace(
        directory.path(),
        "core",
        "use crate::other::*",
        "use crate::another::*",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_eq!(actual, ledger.declarations);
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn private_conditional_enum_qualifiers_remain_review_inputs() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "mod hidden { pub enum First { A(u16) } pub enum Second { A(u32) } }
         #[cfg(unix)] use hidden::First as Choice;
         #[cfg(not(unix))] use hidden::Second as Choice;
         pub use Choice::*;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let qualifiers: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| {
            d.surface == "core"
                && d.kind == "reexport"
                && d.shape.ends_with("as Choice visibility=private")
        })
        .collect();
    assert_eq!(qualifiers.len(), 2);
    assert!(qualifiers.iter().all(|d| d.conditions.len() == 1));
    replace(
        directory.path(),
        "core",
        "First as Choice",
        "Placeholder as Choice",
    );
    replace(
        directory.path(),
        "core",
        "Second as Choice",
        "First as Choice",
    );
    replace(
        directory.path(),
        "core",
        "Placeholder as Choice",
        "Second as Choice",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    let error = validate(directory.path(), &ledger, &actual).unwrap_err();
    assert!(error.contains("unreviewed declaration"), "{error}");
    assert!(error.contains("stale declaration"), "{error}");
}

#[test]
fn enum_globs_do_not_export_values_sharing_the_enum_name() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "mod hidden {
             pub enum E { A }
             pub fn E(value: u16) {}
         }
         pub use hidden::E::*;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::E::A#variant")
    );
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::E#function")
    );
    replace(
        directory.path(),
        "core",
        "fn E(value: u16)",
        "fn E(value: u32)",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_eq!(actual, ledger.declarations);
    validate(directory.path(), &ledger, &actual).unwrap();
}

fn review_fixture_macro_inputs(root: &Path, ledger: &mut Ledger) {
    ledger.supplements = discover_sources(root, ledger)
        .unwrap()
        .into_iter()
        .filter(|d| d.kind == "macro")
        .map(|d| Supplement {
            anchor: d.id,
            expected_shape: d.shape,
            rationale: "Reviewed fixture macro input.".into(),
            declarations: Vec::new(),
        })
        .collect();
    review(root, ledger);
}

#[test]
fn generating_outer_attributes_invalidate_reviewed_macro_inputs() {
    for (source, target) in [
        ("macro_rules! make { () => {}; } make!();", "make!();"),
        ("macro_rules! make { () => {}; } make![];", "make![];"),
        ("macro_rules! make { () => {}; } make! {}", "make! {}"),
        (
            "macro_rules! make { () => {}; } make!();",
            "macro_rules! make",
        ),
        (
            "macro_rules! codes { ($($input:tt)*) => {}; } codes!(pub enum Generated { A });",
            "codes!(pub enum Generated { A });",
        ),
    ] {
        for attribute in [
            "#[generate_extra]",
            "#[cfg_attr(feature = \"new\", generate_extra)]",
            "#[cfg_attr(unix, doc(hidden), cfg_attr(feature = \"new\", allow(dead_code), generate_extra(\"mode\")))]",
        ] {
            let (directory, mut ledger) = fixture();
            fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
            review_fixture_macro_inputs(directory.path(), &mut ledger);
            replace(
                directory.path(),
                "core",
                target,
                &format!("{attribute} {target}"),
            );
            let error = inventory(directory.path(), &ledger).unwrap_err();
            assert!(
                error.contains("changed or invalid supplement"),
                "{attribute} on {target}: {error}"
            );
        }
    }
}

#[test]
fn attributed_macro_supplements_retain_all_attributes_and_full_inputs() {
    for (source, before, after) in [
        (
            "macro_rules! make { () => {}; } #[generate_extra(\"first\")] make!();",
            "\"first\"",
            "\"second\"",
        ),
        (
            "macro_rules! make { () => {}; } #[first] #[second] make!();",
            "#[first] #[second]",
            "#[second] #[first]",
        ),
        (
            "macro_rules! make { () => {}; } #[generate_extra] #[doc = \"first\"] make!();",
            "\"first\"",
            "\"second\"",
        ),
        (
            "#[generate_extra] macro_rules! make { () => {}; } make!();",
            "() => {}",
            "() => { pub fn extra() {} }",
        ),
        (
            "macro_rules! codes { ($($input:tt)*) => {}; } #[generate_extra] codes!(#[doc = \"first\"] pub enum Generated { A });",
            "\"first\"",
            "\"second\"",
        ),
        (
            "macro_rules! make { () => {}; } #[generate_extra] make!();",
            "#[generate_extra]",
            "",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
        review_fixture_macro_inputs(directory.path(), &mut ledger);
        replace(directory.path(), "core", before, after);
        let error = inventory(directory.path(), &ledger).unwrap_err();
        assert!(
            error.contains("changed or invalid supplement"),
            "{source}: {error}"
        );
    }
}

#[test]
fn benign_and_inactive_macro_attributes_preserve_reviewed_shapes() {
    for target in ["macro_rules! make", "make!();"] {
        for attribute in [
            "#[doc = \"Documentation\"]",
            "#[allow(unused_macros)]",
            "#[cfg_attr(unix, doc(hidden), allow(unused_macros))]",
            "#[cfg_attr(test, generate_extra)]",
            "#[cfg_attr(unix, cfg_attr(test, generate_extra))]",
        ] {
            let (directory, mut ledger) = fixture();
            fs::write(
                directory.path().join("core/src/lib.rs"),
                "macro_rules! make { () => {}; } make!();",
            )
            .unwrap();
            review_fixture_macro_inputs(directory.path(), &mut ledger);
            replace(
                directory.path(),
                "core",
                target,
                &format!("{attribute} {target}"),
            );
            let actual = inventory(directory.path(), &ledger).unwrap();
            assert_eq!(actual, ledger.declarations, "{attribute} on {target}");
            validate(directory.path(), &ledger, &actual).unwrap();
        }
    }
}

#[test]
fn enum_macro_supplements_keep_documentation_in_their_full_input() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "macro_rules! codes { ($($input:tt)*) => {}; } codes!(pub enum Generated { A });",
    )
    .unwrap();
    review_fixture_macro_inputs(directory.path(), &mut ledger);
    replace(
        directory.path(),
        "core",
        "pub enum Generated { A }",
        "#[doc = \"Enum\"] pub enum Generated { #[doc = \"Variant\"] A }",
    );
    let error = inventory(directory.path(), &ledger).unwrap_err();
    assert!(error.contains("changed or invalid supplement"), "{error}");
}

#[test]
fn malformed_macro_attributes_are_not_hidden_by_neighboring_generators() {
    for attribute in [
        "#[cfg_attr(feature = \"new\", doc(hidden), derive(Getters = invalid))]",
        "#[generate_extra] #[cfg_attr(unix, doc(hidden), derive(Getters = invalid))]",
    ] {
        let (directory, ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!("macro_rules! make {{ () => {{}}; }} {attribute} make!();"),
        )
        .unwrap();
        let error = discover_sources(directory.path(), &ledger).unwrap_err();
        assert!(error.contains("unsupported derive"), "{attribute}: {error}");
    }
}

fn build_fixture_generator(root: &Path, rustc: &std::ffi::OsStr) -> std::path::PathBuf {
    let generator_source = root.join("fixture_generator.rs");
    fs::write(
        &generator_source,
        r#"
        extern crate proc_macro;
        #[proc_macro_attribute]
        pub fn generate_extra(_: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream {
            format!("{item} pub fn extra() {{}}").parse().unwrap()
        }
        "#,
    )
    .unwrap();
    let generator_library = root.join(format!(
        "{}fixture_generator{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX,
    ));
    let compiled = std::process::Command::new(rustc)
        .args([
            "--edition=2024",
            "--crate-type=proc-macro",
            "--crate-name=fixture_generator",
        ])
        .arg(&generator_source)
        .arg("-o")
        .arg(&generator_library)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    generator_library
}

#[test]
fn a_real_outer_macro_attribute_cannot_add_api_under_an_unchanged_supplement() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let generator_library = build_fixture_generator(root, &rustc);
    fs::write(
        root.join("core/src/lib.rs"),
        "macro_rules! make { () => {}; } make!();",
    )
    .unwrap();
    review_fixture_macro_inputs(root, &mut ledger);
    let ledger_path = root.join(rumqttc_parity_audit::LEDGER);
    fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    let original = serde_json::to_string_pretty(&ledger).unwrap();
    fs::write(&ledger_path, &original).unwrap();
    fs::write(
        root.join(rumqttc_parity_audit::MARKDOWN),
        rumqttc_parity_audit::render(&ledger),
    )
    .unwrap();
    let check = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
            .args(["check", "--root"])
            .arg(root)
            .output()
            .unwrap()
    };
    let baseline = check();
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    let compile_library = || {
        let compiled = std::process::Command::new(&rustc)
            .args(["--edition=2024", "--crate-type=lib", "--crate-name=fixture"])
            .arg(root.join("core/src/lib.rs"))
            .arg("--extern")
            .arg(format!("fixture_generator={}", generator_library.display()))
            .arg("-o")
            .arg(root.join("libfixture.rlib"))
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
    };
    fs::write(root.join("consumer.rs"), "fn main() { fixture::extra(); }").unwrap();
    let compile_consumer = || {
        std::process::Command::new(&rustc)
            .arg("--edition=2024")
            .arg(root.join("consumer.rs"))
            .arg("--extern")
            .arg(format!(
                "fixture={}",
                root.join("libfixture.rlib").display()
            ))
            .arg("-L")
            .arg(root)
            .arg("-o")
            .arg(root.join("consumer"))
            .output()
            .unwrap()
    };
    compile_library();
    let absent = compile_consumer();
    assert!(!absent.status.success());
    assert!(String::from_utf8_lossy(&absent.stderr).contains("cannot find function `extra`"));
    replace(
        root,
        "core",
        "make!();",
        "#[fixture_generator::generate_extra] make!();",
    );
    compile_library();
    let added = compile_consumer();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let output = check();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("changed or invalid supplement"));
    assert_eq!(fs::read_to_string(ledger_path).unwrap(), original);
}

#[test]
fn private_alias_types_used_by_public_declarations_require_review() {
    for declaration in [
        "pub struct Options { pub value: Value }",
        "pub struct Options(pub Value);",
        "pub enum Options { Value(Value) }",
        "pub union Options { pub value: Value }",
        "pub fn value(input: Value) -> Value { input }",
        "pub struct Options; impl Options { pub fn value(&self) -> Value { 0 } }",
        "pub struct Options; impl Options { pub const VALUE: Value = 0; }",
        "pub trait Options { fn value(&self) -> Value; }",
        "pub trait Options { type Output: Into<Value>; }",
        "pub type Options = Option<Value>;",
        "pub struct Options<T = Value>(pub T);",
        "pub struct Options<T>(pub T) where T: Into<Value>;",
        "pub const VALUE: Value = 0;",
        "pub static VALUE: Value = 0;",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!("type Value = u16; {declaration}"),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        replace(
            directory.path(),
            "core",
            "type Value = u16",
            "type Value = u32",
        );
        let actual = inventory(directory.path(), &ledger).unwrap();
        let error = validate(directory.path(), &ledger, &actual).expect_err(declaration);
        assert!(
            error.contains("changed declaration requires review: core|core::Value#alias"),
            "{declaration}: {error}"
        );
    }
}

#[test]
fn private_alias_dependencies_are_recursive_and_retain_non_path_types() {
    for target in [
        "(Leaf, [Leaf; 2])",
        "&'static Leaf",
        "fn(Leaf) -> Leaf",
        "Box<dyn Fn(Leaf) -> Leaf + Send + Sync>",
        "Box<dyn Iterator<Item = Leaf>>",
        "<Leaf as Iterator>::Item",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!(
                "type Leaf = u16; type Value = {target}; pub fn value() -> Option<Value> {{ None }}"
            ),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        replace(directory.path(), "core", "Leaf = u16", "Leaf = u32");
        let error = validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap(),
        )
        .unwrap_err();
        assert!(
            error.contains("changed declaration requires review: core|core::Leaf#alias"),
            "{target}: {error}"
        );
    }
}

#[test]
fn alias_dependencies_follow_private_imports_and_conditional_alternatives() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        mod hidden {
            pub(crate) type A = u16;
            pub(crate) type B = u32;
            type Unused = bool;
        }
        #[cfg(unix)] use hidden::A as Choice;
        #[cfg(not(unix))] use hidden::B as Choice;
        #[cfg(unix)] type Value = Option<Choice>;
        #[cfg(not(unix))] type Value = Result<Choice, ()>;
        #[cfg(test)] type TestOnly = u8;
        pub fn value() -> Value { todo!() }
        ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let aliases: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "alias")
        .collect();
    assert_eq!(aliases.len(), 4);
    assert!(
        aliases
            .iter()
            .filter(|d| d.id.contains("::Value#"))
            .all(|d| !d.conditions.is_empty())
    );
    replace(
        directory.path(),
        "core",
        "hidden::A as Choice",
        "hidden::B as Choice",
    );
    replace(
        directory.path(),
        "core",
        "not(unix))] use hidden::B",
        "not(unix))] use hidden::A",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_ne!(ledger.declarations, actual);
    validate(directory.path(), &ledger, &actual).unwrap_err();
}

#[test]
fn alias_targets_retain_external_import_inputs_without_dependency_discovery() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "use std::time::Duration as Inner; type Value = Option<Inner>; pub fn value() -> Value { None }",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    replace(directory.path(), "core", "time::Duration", "time::Instant");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_ne!(ledger.declarations, actual);
    validate(directory.path(), &ledger, &actual).unwrap_err();
}

#[test]
fn alias_dependencies_exclude_private_bodies_fields_tests_and_shadowed_names() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        type Value = u16;
        type Hidden = u16;
        type TestOnly = u16;
        type SelfType = u16;
        pub struct Options<Value> { pub value: Value, hidden: Hidden, #[cfg(test)] pub helper: TestOnly }
        impl<Value> Options<Value> {
            pub fn value(&self) -> Option<Value> { let _: Option<SelfType> = None; None }
            fn hidden(&self) -> Hidden { 0 }
        }
        pub trait Policy<Value> { fn value(&self) -> Option<Value>; }
        pub fn value<Value>(value: Value) -> Value { value }
        pub fn bounds<Value: Iterator>() -> Option<Value::Item> { None }
        pub fn external() -> Option<::std::time::Duration> { None }
        mod hidden {
            type Selected = u16;
            type Ignored = u16;
            pub enum E { Selected(Selected), Ignored(Ignored) }
        }
        pub use hidden::E::Selected;
        ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let aliases: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "alias")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(aliases, ["core|core::hidden::Selected#alias"]);
    replace(directory.path(), "core", "= u16", "= u32");
    replace(
        directory.path(),
        "core",
        "type Selected = u32",
        "type Selected = u16",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_eq!(ledger.declarations, actual);
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn generators_on_referenced_private_aliases_require_full_input_supplements() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "#[generate] type Value = u16; #[unrelated] type Unused = u8; pub fn value() -> Value { 0 }",
    )
    .unwrap();
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
    review_fixture_macro_inputs(directory.path(), &mut ledger);
    assert_eq!(ledger.supplements.len(), 1);
    replace(directory.path(), "core", "Value = u16", "Value = u32");
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("changed or invalid supplement")
    );
}

#[test]
fn a_compiling_private_alias_change_invalidates_the_unchanged_ledger() {
    use std::process::Command;

    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let source = root.join("core/src/lib.rs");
    fs::write(&source, "type Value = u16; pub struct Options { pub value: Value } pub fn value(input: Value) -> Value { input }").unwrap();
    review(root, &mut ledger);
    fs::create_dir_all(root.join("native-wrappers/wrapper-core")).unwrap();
    let ledger_path = root.join(rumqttc_parity_audit::LEDGER);
    let reviewed = serde_json::to_vec_pretty(&ledger).unwrap();
    fs::write(&ledger_path, &reviewed).unwrap();
    fs::write(
        root.join(rumqttc_parity_audit::MARKDOWN),
        rumqttc_parity_audit::render(&ledger),
    )
    .unwrap();
    let check = || {
        Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
            .arg("check")
            .arg("--root")
            .arg(root)
            .output()
            .unwrap()
    };
    assert!(check().status.success());
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let library = root.join("libfixture.rlib");
    let compile = || {
        let result = Command::new(&rustc)
            .args(["--edition=2024", "--crate-type=lib", "--crate-name=fixture"])
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    compile();
    let consumer = root.join("consumer.rs");
    fs::write(&consumer, "fn main() { let x = fixture::Options { value: 1u16 }; let _: u16 = fixture::value(x.value); }").unwrap();
    let consume = || {
        Command::new(&rustc)
            .arg("--edition=2024")
            .arg(&consumer)
            .arg("--extern")
            .arg(format!("fixture={}", library.display()))
            .arg("-o")
            .arg(root.join("consumer"))
            .output()
            .unwrap()
    };
    assert!(consume().status.success());
    replace(root, "core", "Value = u16", "Value = u32");
    compile();
    let result = consume();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("mismatched types"));
    let result = check();
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("changed declaration requires review: core|core::Value#alias")
    );
    assert_eq!(fs::read(&ledger_path).unwrap(), reviewed);
}

#[test]
fn restricted_aliases_follow_internal_globs_without_becoming_public_exports() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        mod hidden {
            pub(crate) type Value = u16;
            pub(crate) type Unused = bool;
            type Private = u8;
        }
        mod intermediate { pub(crate) use crate::hidden::*; }
        pub use intermediate::*;
        pub struct Options { pub value: Value }
        ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let aliases: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "alias")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(aliases, ["core|core::hidden::Value#alias"]);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::intermediate::@use:crate::hidden::*#reexport")
    );
    replace(directory.path(), "core", "Value = u16", "Value = u32");
    let error = validate(
        directory.path(),
        &ledger,
        &inventory(directory.path(), &ledger).unwrap(),
    )
    .unwrap_err();
    assert!(error.contains("changed declaration requires review: core|core::hidden::Value#alias"));
}

#[test]
fn retained_type_dependencies_do_not_hide_removal_of_public_alias_exports() {
    for (source, before, after) in [
        (
            "pub type Value = u16; pub fn value() -> Value { 0 }",
            "pub type Value",
            "type Value",
        ),
        (
            "mod hidden { pub type Value = u16; } pub use hidden::Value; pub fn value() -> Value { 0 }",
            "pub use hidden::Value",
            "use hidden::Value",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
        review(directory.path(), &mut ledger);
        replace(directory.path(), "core", before, after);
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert_ne!(ledger.declarations, actual);
        assert!(
            validate(directory.path(), &ledger, &actual)
                .unwrap_err()
                .contains("changed declaration")
        );
    }
}

#[test]
fn direct_private_imports_in_public_declarations_require_review() {
    for declaration in [
        "pub struct Options { pub value: Value }",
        "pub struct Options(pub Value);",
        "pub enum Options { Value(Value) }",
        "pub union Options { pub value: Value }",
        "pub fn value(input: Value) -> Value { input }",
        "pub struct Options; impl Options { pub fn value(&self) -> Value { todo!() } }",
        "pub struct Options; impl Options { pub const VALUE: Option<Value> = None; }",
        "pub trait Options { fn value(&self) -> Value; }",
        "pub trait Options { type Output: Into<Value>; }",
        "pub struct Options<T = Value>(pub T);",
        "pub struct Options<T>(pub T) where T: Into<Value>;",
        "pub const VALUE: Option<Value> = None;",
        "pub static VALUE: Option<Value> = None;",
    ] {
        let (directory, mut ledger) = fixture();
        if declaration.contains("Options") {
            ledger
                .surfaces
                .iter_mut()
                .find(|surface| surface.name == "core")
                .unwrap()
                .roots = vec!["core::Options".into()];
        }
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!("use std::num::NonZeroU16 as Value; {declaration}"),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        replace(
            directory.path(),
            "core",
            "num::NonZeroU16",
            "num::NonZeroU32",
        );
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert_ne!(ledger.declarations, actual, "{declaration}");
        validate(directory.path(), &ledger, &actual).unwrap_err();
    }
}

#[test]
fn procedural_attributes_on_public_reexports_require_supplements() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub use std::num::NonZeroU16 as Value;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    replace(
        directory.path(),
        "core",
        "pub use",
        "#[generate_extra] pub use",
    );
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
}

#[test]
fn direct_import_dependencies_retain_conditions_and_qualified_routes() {
    for (imports, public_api) in [
        (
            "#[cfg(unix)] use std::num::NonZeroU16 as Value; #[cfg(not(unix))] use std::num::NonZeroU32 as Value;",
            "pub fn value(input: Value) -> Value { input }",
        ),
        (
            "mod types { pub use std::num::NonZeroU16 as Value; } use types as selected;",
            "pub fn value(input: selected::Value) -> selected::Value { input }",
        ),
        (
            "mod types { pub use std::num::NonZeroU16 as Value; pub use std::time::Duration as Unrelated; } use types::*;",
            "pub struct Options { pub value: Value }",
        ),
        (
            "use std::num::{NonZeroU16 as Value, NonZeroU32 as Unused};",
            "pub struct Options { pub value: Value }",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!("{imports} {public_api}"),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.id.contains(" as Unrelated") || d.id.contains(" as Unused"))
        );
        replace(directory.path(), "core", "NonZeroU16", "NonZeroU64");
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert_ne!(ledger.declarations, actual, "{imports}");
        validate(directory.path(), &ledger, &actual).unwrap_err();
    }
}

#[test]
fn direct_imports_ignore_unrelated_private_and_shadowed_type_uses() {
    let (directory, mut ledger) = fixture();
    fs::write(directory.path().join("core/src/lib.rs"), r"
        use std::num::NonZeroU16 as Value;
        use std::num::NonZeroU16 as Hidden;
        use std::num::NonZeroU16 as Helper;
        pub struct Options<Value> { pub value: Value, hidden: Hidden, #[cfg(test)] pub helper: Helper }
        impl<Value> Options<Value> {
            pub fn value(&self) -> Option<Value> { let _: Option<Hidden> = None; None }
            fn hidden(&self) -> Option<Hidden> { None }
        }
        pub fn value<Value>(input: Value) -> Value { input }
        pub trait Policy<Value> { fn value(&self) -> Value; }
        #[cfg(test)] pub fn helper() -> Helper { todo!() }
    ").unwrap();
    review(directory.path(), &mut ledger);
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.surface == "core" && d.kind == "reexport")
    );
    replace(directory.path(), "core", "NonZeroU16", "NonZeroU32");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_eq!(ledger.declarations, actual);
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn reexport_generators_retain_one_full_input_anchor_through_reachable_imports() {
    for source in [
        "#[expand] pub use std::num::NonZeroU16 as Value;",
        "#[expand] pub use std::num::{NonZeroU16 as Value, NonZeroU32};",
        "#[expand] pub use std::num::*;",
        "#[expand] pub use std::num::NonZeroU16 as _;",
        "#[expand] pub use {};",
        "mod hidden { #[expand] pub use std::num::NonZeroU16 as Value; } pub use hidden::Value;",
        "mod hidden { #[expand] pub use std::num::NonZeroU16 as Value; } pub use hidden::extra;",
        "mod hidden { #[expand] pub use std::num::NonZeroU16 as Value; } pub fn value() -> hidden::Generated { todo!() }",
        "mod hidden { #[expand] pub use std::num::{NonZeroU16 as Value, NonZeroU32}; } pub use hidden::*;",
        "#[expand] use std::num::NonZeroU16 as Value; pub fn value(input: Value) -> Value { input }",
        "mod hidden { #[expand] pub use std::num::NonZeroU16 as Value; } pub fn value(input: hidden::Value) -> hidden::Value { input }",
        "#[cfg(unix)] #[cfg_attr(unix, doc(hidden), expand)] pub use std::num::NonZeroU16;",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("unreviewed macro"),
            "{source}"
        );
        review_fixture_macro_inputs(directory.path(), &mut ledger);
        assert_eq!(ledger.supplements.len(), 1, "{source}");
        let anchor = ledger
            .declarations
            .iter()
            .find(|d| d.kind == "macro")
            .unwrap();
        assert!(anchor.shape.contains("use "), "{source}: {}", anchor.shape);
        assert!(anchor.shape.contains("expand"));
        assert!(anchor.shape.contains(';'));
        if source.contains("#[cfg(unix)]") {
            assert_eq!(anchor.conditions, ["cfg (unix)"]);
        }
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn reviewed_reexport_generators_reject_changes_to_any_part_of_the_input() {
    for (before, after) in [
        ("mode = 1", "mode = 2"),
        ("doc = \"before\"", "doc = \"after\""),
        ("allow(unused_imports)", "deny(unused_imports)"),
        ("NonZeroU32", "NonZeroU64"),
        ("pub use", "pub(crate) use"),
        ("#[expand(mode = 1)]", ""),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"),
            "#[doc = \"before\"] #[expand(mode = 1)] #[allow(unused_imports)] pub use std::num::{NonZeroU16 as Value, NonZeroU32}; pub fn value(input: Value) -> Value { input }").unwrap();
        review_fixture_macro_inputs(directory.path(), &mut ledger);
        replace(directory.path(), "core", before, after);
        let error = inventory(directory.path(), &ledger).unwrap_err();
        assert!(error.contains("supplement"), "{before}: {error}");
    }
}

#[test]
fn benign_test_only_and_unreachable_reexport_attributes_need_no_supplements() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub use std::num::NonZeroU16 as Value;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    replace(
        directory.path(),
        "core",
        "pub use",
        "#[doc(hidden)] #[allow(unused_imports)] #[cfg_attr(test, expand)] pub use",
    );
    let path = directory.path().join("core/src/lib.rs");
    let source = fs::read_to_string(&path).unwrap();
    fs::write(path, format!("{source}\n#[expand] use std::num::NonZeroU32 as Unused;\nmod hidden {{ #[expand] pub use std::num::NonZeroU64; }}\n#[cfg(test)] #[expand] pub use std::num::NonZeroU8;")).unwrap();
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_eq!(ledger.declarations, actual);
    validate(directory.path(), &ledger, &actual).unwrap();
}

#[test]
fn a_compiling_direct_import_change_is_rejected_by_the_unchanged_ledger() {
    use std::process::Command;

    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "use std::num::NonZeroU16 as Value; pub struct Options { pub value: Value } pub fn value(input: Value) -> Value { input }").unwrap();
    review(root, &mut ledger);
    let ledger_path = root.join(rumqttc_parity_audit::LEDGER);
    fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    let original = serde_json::to_vec_pretty(&ledger).unwrap();
    fs::write(&ledger_path, &original).unwrap();
    fs::write(
        root.join(rumqttc_parity_audit::MARKDOWN),
        rumqttc_parity_audit::render(&ledger),
    )
    .unwrap();
    let check = || {
        Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
            .args(["check", "--root"])
            .arg(root)
            .output()
            .unwrap()
    };
    assert!(check().status.success());
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let library = root.join("libfixture.rlib");
    let compile_library = || {
        let result = Command::new(&rustc)
            .args(["--edition=2024", "--crate-type=lib", "--crate-name=fixture"])
            .arg(root.join("core/src/lib.rs"))
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    let consumer = root.join("consumer.rs");
    fs::write(&consumer, "fn main() { let x = fixture::Options { value: std::num::NonZeroU16::new(1).unwrap() }; let _: std::num::NonZeroU16 = fixture::value(x.value); }").unwrap();
    let compile_consumer = || {
        Command::new(&rustc)
            .arg("--edition=2024")
            .arg(&consumer)
            .arg("--extern")
            .arg(format!("fixture={}", library.display()))
            .arg("-o")
            .arg(root.join("consumer"))
            .output()
            .unwrap()
    };
    compile_library();
    assert!(compile_consumer().status.success());
    replace(root, "core", "NonZeroU16", "NonZeroU32");
    compile_library();
    let broken = compile_consumer();
    assert!(!broken.status.success());
    assert!(String::from_utf8_lossy(&broken.stderr).contains("mismatched types"));
    let source = fs::read_to_string(&consumer).unwrap();
    fs::write(&consumer, source.replace("NonZeroU16", "NonZeroU32")).unwrap();
    assert!(compile_consumer().status.success());
    let checked = check();
    assert!(!checked.status.success());
    assert!(String::from_utf8_lossy(&checked.stderr).contains(
        "unreviewed declaration: core|core::@use:std::num::NonZeroU32 as Value#reexport"
    ));
    assert_eq!(fs::read(&ledger_path).unwrap(), original);
}

#[test]
fn a_real_reexport_attribute_cannot_add_public_api_without_a_supplement() {
    use std::process::Command;

    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let generator = build_fixture_generator(root, &rustc);
    fs::write(
        root.join("core/src/lib.rs"),
        "pub use std::num::NonZeroU16 as Value;",
    )
    .unwrap();
    review(root, &mut ledger);
    let ledger_path = root.join(rumqttc_parity_audit::LEDGER);
    fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    let original = serde_json::to_vec_pretty(&ledger).unwrap();
    fs::write(&ledger_path, &original).unwrap();
    fs::write(
        root.join(rumqttc_parity_audit::MARKDOWN),
        rumqttc_parity_audit::render(&ledger),
    )
    .unwrap();
    let check = || {
        Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
            .args(["check", "--root"])
            .arg(root)
            .output()
            .unwrap()
    };
    assert!(check().status.success());
    let library = root.join("libfixture.rlib");
    let compile_library = || {
        let result = Command::new(&rustc)
            .args(["--edition=2024", "--crate-type=lib", "--crate-name=fixture"])
            .arg(root.join("core/src/lib.rs"))
            .arg("--extern")
            .arg(format!("fixture_generator={}", generator.display()))
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    fs::write(root.join("consumer.rs"), "fn main() { fixture::extra(); }").unwrap();
    let compile_consumer = || {
        Command::new(&rustc)
            .arg("--edition=2024")
            .arg(root.join("consumer.rs"))
            .arg("--extern")
            .arg(format!("fixture={}", library.display()))
            .arg("-L")
            .arg(root)
            .arg("-o")
            .arg(root.join("consumer"))
            .output()
            .unwrap()
    };
    compile_library();
    let absent = compile_consumer();
    assert!(!absent.status.success());
    assert!(String::from_utf8_lossy(&absent.stderr).contains("cannot find function `extra`"));
    replace(
        root,
        "core",
        "pub use",
        "#[fixture_generator::generate_extra] pub use",
    );
    compile_library();
    let added = compile_consumer();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let checked = check();
    assert!(!checked.status.success());
    assert!(
        String::from_utf8_lossy(&checked.stderr)
            .contains("unreviewed macro; explicit supplement required")
    );
    assert_eq!(fs::read(&ledger_path).unwrap(), original);
}

#[test]
fn absolute_import_markers_are_preserved_when_a_local_module_shadows_the_crate() {
    let (directory, mut ledger) = fixture();
    fs::write(directory.path().join("core/src/lib.rs"),
        "mod std { pub mod time { pub type Duration = u32; } } use ::std::time::Duration; pub struct Options { pub value: Duration }").unwrap();
    review(directory.path(), &mut ledger);
    replace(directory.path(), "core", "use ::std", "use std");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_ne!(ledger.declarations, actual);
    validate(directory.path(), &ledger, &actual).unwrap_err();
}

#[test]
fn public_module_visibility_changes_require_review_even_with_a_root_reexport() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub mod api { pub struct Options; } pub use api::Options;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    replace(directory.path(), "core", "pub mod api", "mod api");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_ne!(ledger.declarations, actual);
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("stale declaration: core|core::api#module")
    );
}

#[test]
fn absolute_grouped_glob_and_renamed_imports_do_not_resolve_local_shadows() {
    for import in [
        "use ::std::time::Duration as Value;",
        "use ::std::time::{Duration as Value};",
        "use ::std::{time::Duration as Value};",
        "use ::std::time::{self as times}; type Value = times::Duration;",
        "use ::std::time::*; type Value = Duration;",
        "pub use ::std::time::Duration as Value;",
        "#[cfg(unix)] use ::std::time::Duration as Value;",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), format!(
            "mod std {{ pub mod time {{ pub type Duration = u32; }} }} {import} pub struct Options {{ pub value: Value }}"
        )).unwrap();
        review(directory.path(), &mut ledger);
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.id.contains("core::std::")),
            "{import}"
        );
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.shape.starts_with("use ::std::")),
            "{import}"
        );
        replace(directory.path(), "core", "Duration = u32", "Duration = u64");
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert_eq!(ledger.declarations, actual, "{import}");
        validate(directory.path(), &ledger, &actual).unwrap();
        replace(directory.path(), "core", "use ::std", "use std");
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            actual
                .iter()
                .any(|d| d.id.contains("core::std::time::Duration#alias")),
            "{import}"
        );
        validate(directory.path(), &ledger, &actual).unwrap_err();
    }
}

#[test]
fn absolute_alias_targets_and_api_paths_skip_shadows_but_visit_generic_arguments() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        #[unrelated] mod std { pub mod time { pub type Duration = u32; } }
        type Local = u16;
        type Value = ::std::vec::Vec<Local>;
        pub struct Options { pub value: Value }
        pub fn duration(input: ::std::time::Duration) -> ::std::time::Duration { input }
    ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::Local#alias")
    );
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("core::std::"))
    );
    replace(directory.path(), "core", "Duration = u32", "Duration = u64");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_eq!(ledger.declarations, actual);
    replace(directory.path(), "core", "Local = u16", "Local = u32");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("changed declaration")
    );
}

#[test]
fn absolute_imports_can_coexist_with_same_named_relative_imports() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        mod std { pub mod time { pub type Duration = u32; } }
        #[cfg(unix)] use ::std::time::Duration as Value;
        #[cfg(not(unix))] use std::time::Duration as Value;
        pub struct Options { pub value: Value }
    ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let imports: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "reexport")
        .collect();
    assert_eq!(imports.len(), 2);
    assert_ne!(imports[0].shape, imports[1].shape);
    assert!(imports.iter().all(|d| d.conditions.len() == 1));
    replace(directory.path(), "core", "cfg(unix)", "cfg(windows)");
    validate(
        directory.path(),
        &ledger,
        &inventory(directory.path(), &ledger).unwrap(),
    )
    .unwrap_err();
}

#[test]
fn public_module_metadata_covers_empty_nested_reexported_and_selected_paths() {
    for (source, visibility, roots, expected) in [
        (
            "pub mod api {} pub struct Sentinel;",
            "pub mod api",
            vec![],
            "core::api",
        ),
        (
            "pub mod outer { pub mod api { pub struct Options; } } pub use outer::api::Options;",
            "pub mod outer",
            vec![],
            "core::outer",
        ),
        (
            "mod hidden { pub mod api { pub struct Options; } } pub use hidden::api as exported; pub use exported::Options;",
            "pub mod api",
            vec![],
            "core::hidden::api",
        ),
        (
            "pub mod api { pub struct Options; } pub use api::Options;",
            "pub mod api",
            vec!["core::api::Options".into()],
            "core::api",
        ),
        (
            "pub mod api { pub struct Options; }",
            "pub mod api",
            vec!["core::api".into()],
            "core::api",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        ledger
            .surfaces
            .iter_mut()
            .find(|s| s.name == "core")
            .unwrap()
            .roots = roots;
        fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
        review(directory.path(), &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == format!("core|{expected}#module")),
            "{source}"
        );
        replace(
            directory.path(),
            "core",
            visibility,
            &visibility.replacen("pub ", "pub(crate) ", 1),
        );
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            validate(directory.path(), &ledger, &actual)
                .unwrap_err()
                .contains("stale declaration"),
            "{source}"
        );
    }
}

#[test]
fn conditional_module_visibility_is_retained_independently_of_reexported_children() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        #[cfg(unix)] pub mod api { pub struct Options; }
        #[cfg(not(unix))] mod api { pub struct Options; }
        pub use api::Options;
    ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let modules: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.kind == "module")
        .collect();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].conditions, ["cfg (unix)"]);
    replace(
        directory.path(),
        "core",
        "#[cfg(unix)] pub mod",
        "#[cfg(unix)] mod",
    );
    replace(
        directory.path(),
        "core",
        "#[cfg(not(unix))] mod",
        "#[cfg(not(unix))] pub mod",
    );
    validate(
        directory.path(),
        &ledger,
        &inventory(directory.path(), &ledger).unwrap(),
    )
    .unwrap_err();
}

#[test]
fn module_metadata_ignores_private_bodies_test_modules_and_unexported_namespaces() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        pub mod api { pub struct Options; fn helper() { let _ = 1; } }
        mod hidden { pub mod internal { pub struct Kept; } }
        pub use hidden::internal::Kept;
        #[cfg(test)] pub mod test_only { pub struct Helper; }
    ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let modules: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.kind == "module")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(modules, ["core|core::api#module"]);
    replace(directory.path(), "core", "let _ = 1", "let _ = 2");
    replace(
        directory.path(),
        "core",
        "mod hidden",
        "pub(crate) mod hidden",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_eq!(ledger.declarations, actual);
}

#[test]
fn file_module_metadata_uses_inner_cfg_and_excludes_test_only_modules() {
    let (directory, mut ledger) = fixture();
    let src = directory.path().join("core/src");
    fs::write(
        src.join("lib.rs"),
        "pub struct Sentinel; pub mod api; pub mod test_only;",
    )
    .unwrap();
    fs::write(src.join("api.rs"), "#![cfg(unix)] pub struct Options;").unwrap();
    fs::write(src.join("test_only.rs"), "#![cfg(test)] pub struct Helper;").unwrap();
    review(directory.path(), &mut ledger);
    let modules: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.kind == "module")
        .collect();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].conditions, ["cfg (unix)"]);
    assert_eq!(modules[0].source, "core/src/lib.rs");
    fs::write(
        src.join("test_only.rs"),
        "#![cfg(test)] pub struct Changed;",
    )
    .unwrap();
    assert_eq!(
        ledger.declarations,
        inventory(directory.path(), &ledger).unwrap()
    );
}

fn verify_compiling_path_change(
    source: &str,
    before: &str,
    after: &str,
    consumer: &str,
    updated_consumer: &str,
    consumer_error: &str,
    audit_error: &str,
) {
    use std::process::Command;

    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let source_path = root.join("core/src/lib.rs");
    fs::write(&source_path, source).unwrap();
    review(root, &mut ledger);
    let ledger_path = root.join(rumqttc_parity_audit::LEDGER);
    fs::create_dir_all(ledger_path.parent().unwrap()).unwrap();
    let original = serde_json::to_vec_pretty(&ledger).unwrap();
    fs::write(&ledger_path, &original).unwrap();
    fs::write(
        root.join(rumqttc_parity_audit::MARKDOWN),
        rumqttc_parity_audit::render(&ledger),
    )
    .unwrap();
    let check = || {
        Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
            .args(["check", "--root"])
            .arg(root)
            .output()
            .unwrap()
    };
    assert!(check().status.success());
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let library = root.join("libfixture.rlib");
    let compile_library = || {
        let result = Command::new(&rustc)
            .args(["--edition=2024", "--crate-type=lib", "--crate-name=fixture"])
            .arg(&source_path)
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    let consumer_path = root.join("consumer.rs");
    fs::write(&consumer_path, consumer).unwrap();
    let compile_consumer = || {
        Command::new(&rustc)
            .arg("--edition=2024")
            .arg(&consumer_path)
            .arg("--extern")
            .arg(format!("fixture={}", library.display()))
            .arg("-o")
            .arg(root.join("consumer"))
            .output()
            .unwrap()
    };
    compile_library();
    let baseline = compile_consumer();
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    replace(root, "core", before, after);
    compile_library();
    let broken = compile_consumer();
    assert!(!broken.status.success());
    assert!(String::from_utf8_lossy(&broken.stderr).contains(consumer_error));
    fs::write(&consumer_path, updated_consumer).unwrap();
    let updated = compile_consumer();
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let checked = check();
    assert!(!checked.status.success());
    assert!(
        String::from_utf8_lossy(&checked.stderr).contains(audit_error),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    assert_eq!(fs::read(ledger_path).unwrap(), original);
}

#[test]
fn compiling_absolute_import_and_module_visibility_changes_fail_the_unchanged_ledger() {
    verify_compiling_path_change(
        "mod std { pub mod time { pub type Duration = u32; } } use ::std::time::Duration; pub struct Options { pub value: Duration }",
        "use ::std",
        "use std",
        "fn main() { let _ = fixture::Options { value: std::time::Duration::from_secs(1) }; }",
        "fn main() { let _ = fixture::Options { value: 1u32 }; }",
        "mismatched types",
        "stale declaration: core|core::@use:::std::time::Duration#reexport",
    );
    verify_compiling_path_change(
        "pub mod api { pub struct Options; } pub use api::Options;",
        "pub mod api",
        "mod api",
        "fn main() { let _ = fixture::api::Options; let _ = fixture::Options; }",
        "fn main() { let _ = fixture::Options; }",
        "module `api` is private",
        "stale declaration: core|core::api#module",
    );
}

#[test]
fn public_foreign_functions_and_statics_are_rejected_instead_of_skipped() {
    for declaration in ["pub fn new_operation();", "pub static NEW_VALUE: u16;"] {
        let (directory, ledger) = fixture();
        let path = directory.path().join("core/src/lib.rs");
        let source = fs::read_to_string(&path).unwrap();
        fs::write(
            path,
            format!("{source}\nunsafe extern \"C\" {{ {declaration} }}"),
        )
        .unwrap();
        let error = inventory(directory.path(), &ledger).unwrap_err();
        assert!(
            error.contains("unsupported public foreign declaration"),
            "{declaration}: {error}"
        );
    }
}

#[test]
fn public_static_export_name_changes_require_review() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "#[unsafe(export_name = \"old_name\")] pub static VALUE: u32 = 1;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    replace(directory.path(), "core", "old_name", "new_name");
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("changed declaration requires review: core|core::VALUE#static")
    );
}

#[test]
fn foreign_rejection_follows_exports_module_paths_and_explicit_roots() {
    for declaration in [
        "pub fn operation();",
        "pub static VALUE: u16;",
        "pub static mut VALUE: u16;",
        "pub type Opaque;",
    ] {
        let name = if declaration.contains("operation") {
            "operation"
        } else if declaration.contains("Opaque") {
            "Opaque"
        } else {
            "VALUE"
        };
        for (visibility, export) in [
            ("", format!("pub use hidden::{name};")),
            ("", "pub use hidden::*;".into()),
            ("pub", "pub use hidden as api;".into()),
        ] {
            let (directory, ledger) = fixture();
            fs::write(directory.path().join("core/src/lib.rs"), format!(
                    "{visibility} mod hidden {{ #[cfg(unix)] unsafe extern \"C\" {{ {declaration} }} }} {export}"
            )).unwrap();
            let error = inventory(directory.path(), &ledger).unwrap_err();
            assert!(
                error.contains(&format!(
                    "unsupported public foreign declaration core::hidden::{name}"
                )),
                "{error}"
            );
            assert!(error.contains("core/src/lib.rs"));
        }
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!("mod hidden {{ unsafe extern \"C\" {{ {declaration} }} }}"),
        )
        .unwrap();
        ledger
            .surfaces
            .iter_mut()
            .find(|s| s.name == "core")
            .unwrap()
            .roots = vec![format!("core::hidden::{name}")];
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("unsupported public foreign declaration")
        );
    }
}

#[test]
fn private_test_only_and_unreachable_foreign_declarations_do_not_require_review() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r#"
        pub struct Options;
        unsafe extern "C" {
            fn private_operation();
            static PRIVATE_VALUE: u16;
            pub(crate) fn restricted_operation();
            #[cfg(test)] pub fn test_operation();
            #[cfg(test)] pub static TEST_VALUE: u16;
        }
        #[cfg(test)] unsafe extern "C" { pub fn test_block(); }
        mod hidden { unsafe extern "C" { pub fn hidden_operation(); } }
    "#,
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let actual = inventory(directory.path(), &ledger).unwrap();
    validate(directory.path(), &ledger, &actual).unwrap();
    assert_eq!(actual.iter().filter(|d| d.surface == "core").count(), 1);
    replace(directory.path(), "core", "mod hidden", "pub mod hidden");
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("core::hidden::hidden_operation")
    );
}

#[test]
fn foreign_names_outside_selected_roots_do_not_hide_or_contaminate_other_namespaces() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r#"
        pub struct Selected;
        unsafe extern "C" { pub fn unselected(); }
        mod hidden {
            pub mod readiness { pub struct Value; }
            unsafe extern "C" { pub fn readiness(); }
        }
        pub use hidden::readiness::Value;
    "#,
    )
    .unwrap();
    ledger
        .surfaces
        .iter_mut()
        .find(|s| s.name == "core")
        .unwrap()
        .roots = vec![
        "core::Selected".into(),
        "core::hidden::readiness::Value".into(),
    ];
    review(directory.path(), &mut ledger);
    validate(
        directory.path(),
        &ledger,
        &inventory(directory.path(), &ledger).unwrap(),
    )
    .unwrap();
    ledger
        .surfaces
        .iter_mut()
        .find(|s| s.name == "core")
        .unwrap()
        .roots = vec!["core::unselected".into()];
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("core::unselected")
    );
}

#[test]
fn public_static_abi_attributes_require_review_through_reexports() {
    for (attribute, replacement) in [
        (
            "#[unsafe(export_name = \"old\")]",
            "#[unsafe(export_name = \"new\")]",
        ),
        ("#[unsafe(no_mangle)]", ""),
        ("", "#[unsafe(no_mangle)]"),
        (
            "#[unsafe(link_section = \".old\")]",
            "#[unsafe(link_section = \".new\")]",
        ),
        ("#[export_name = \"old\"]", "#[export_name = \"new\"]"),
        ("#[no_mangle]", ""),
        (
            "#[cfg_attr(unix, unsafe(export_name = \"old\"))]",
            "#[cfg_attr(unix, unsafe(export_name = \"new\"))]",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!(
                "mod hidden {{ {attribute} pub static VALUE: u16 = 1; }} pub use hidden::VALUE;"
            ),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!(
                "mod hidden {{ {replacement} pub static VALUE: u16 = 1; }} pub use hidden::VALUE;"
            ),
        )
        .unwrap();
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert_ne!(ledger.declarations, actual, "{attribute} -> {replacement}");
        validate(directory.path(), &ledger, &actual).unwrap_err();
    }
}

#[test]
fn benign_static_attributes_preserve_existing_shapes() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub static VALUE: u16 = 1;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    let declaration = ledger
        .declarations
        .iter()
        .find(|d| d.kind == "static")
        .unwrap();
    assert_eq!(declaration.shape, "static VALUE : u16 = 1");
    replace(
        directory.path(),
        "core",
        "pub static",
        "#[doc = \"Documentation\"] #[allow(non_upper_case_globals)] pub static",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert_eq!(ledger.declarations, actual);
    validate(directory.path(), &ledger, &actual).unwrap();
}

fn save_ffi_fixture_review(root: &Path, ledger: &Ledger) -> Vec<u8> {
    let path = root.join(rumqttc_parity_audit::LEDGER);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = serde_json::to_vec_pretty(ledger).unwrap();
    fs::write(path, &original).unwrap();
    fs::write(
        root.join(rumqttc_parity_audit::MARKDOWN),
        rumqttc_parity_audit::render(ledger),
    )
    .unwrap();
    original
}

#[test]
fn compiling_and_linking_public_foreign_additions_fail_the_unchanged_audit() {
    use std::process::Command;

    for (declaration, consumer) in [
        (
            "pub fn new_operation() -> u16;",
            "fn main() { assert_eq!(unsafe { fixture::new_operation() }, 7); }",
        ),
        (
            "pub static NEW_VALUE: u16;",
            "fn main() { assert_eq!(unsafe { fixture::NEW_VALUE }, 7); }",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let source = root.join("core/src/lib.rs");
        fs::write(&source, "pub struct Options;").unwrap();
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        let check = || {
            Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
                .args(["check", "--root"])
                .arg(root)
                .output()
                .unwrap()
        };
        assert!(check().status.success());
        fs::write(
            &source,
            format!("pub struct Options; unsafe extern \"C\" {{ {declaration} }}"),
        )
        .unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let library = root.join("libfixture.rlib");
        let built = Command::new(&rustc)
            .args(["--edition=2024", "--crate-type=lib", "--crate-name=fixture"])
            .arg(&source)
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let provider = root.join("provider.rs");
        fs::write(&provider, "#[unsafe(no_mangle)] pub extern \"C\" fn new_operation() -> u16 { 7 } #[unsafe(no_mangle)] pub static NEW_VALUE: u16 = 7;").unwrap();
        let object = root.join("provider.o");
        let built = Command::new(&rustc)
            .args(["--edition=2024", "--crate-type=lib", "--emit=obj"])
            .arg(provider)
            .arg("-o")
            .arg(&object)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let consumer_source = root.join("consumer.rs");
        fs::write(&consumer_source, consumer).unwrap();
        let executable = root.join(format!("consumer{}", std::env::consts::EXE_SUFFIX));
        let linked = Command::new(&rustc)
            .arg("--edition=2024")
            .arg(consumer_source)
            .arg("--extern")
            .arg(format!("fixture={}", library.display()))
            .arg("-C")
            .arg(format!("link-arg={}", object.display()))
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            linked.status.success(),
            "{}",
            String::from_utf8_lossy(&linked.stderr)
        );
        assert!(Command::new(executable).output().unwrap().status.success());
        let checked = check();
        assert!(!checked.status.success());
        assert!(
            String::from_utf8_lossy(&checked.stderr)
                .contains("unsupported public foreign declaration")
        );
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn a_real_static_export_rename_breaks_linkage_and_fails_the_unchanged_audit() {
    use std::process::Command;

    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let source = root.join("core/src/lib.rs");
    fs::write(
        &source,
        "#[unsafe(export_name = \"old_name\")] pub static VALUE: u32 = 1;",
    )
    .unwrap();
    review(root, &mut ledger);
    let original = save_ffi_fixture_review(root, &ledger);
    let check = || {
        Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
            .args(["check", "--root"])
            .arg(root)
            .output()
            .unwrap()
    };
    assert!(check().status.success());
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let object = root.join("export.o");
    let build = || {
        let result = Command::new(&rustc)
            .args(["--edition=2024", "--crate-type=lib", "--emit=obj"])
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    let consumer = root.join("consumer.rs");
    fs::write(&consumer, "unsafe extern \"C\" { #[link_name = \"old_name\"] static EXPORTED: u32; } fn main() { assert_eq!(unsafe { EXPORTED }, 1); }").unwrap();
    let executable = root.join(format!("consumer{}", std::env::consts::EXE_SUFFIX));
    let link = || {
        Command::new(&rustc)
            .arg("--edition=2024")
            .arg(&consumer)
            .arg("-C")
            .arg(format!("link-arg={}", object.display()))
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap()
    };
    build();
    let baseline = link();
    assert!(
        baseline.status.success(),
        "{}",
        String::from_utf8_lossy(&baseline.stderr)
    );
    assert!(Command::new(&executable).output().unwrap().status.success());
    replace(root, "core", "old_name", "new_name");
    build();
    let broken = link();
    assert!(!broken.status.success());
    assert!(String::from_utf8_lossy(&broken.stderr).contains("old_name"));
    let checked = check();
    assert!(!checked.status.success());
    assert!(
        String::from_utf8_lossy(&checked.stderr)
            .contains("changed declaration requires review: core|core::VALUE#static")
    );
    let updated_consumer = fs::read_to_string(&consumer)
        .unwrap()
        .replace("old_name", "new_name");
    fs::write(&consumer, updated_consumer).unwrap();
    let updated = link();
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    assert!(Command::new(executable).output().unwrap().status.success());
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn public_foreign_types_used_by_audited_signatures_are_rejected() {
    let (directory, ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r#"
        mod hidden { unsafe extern "C" { pub type Opaque; } }
        type Alias = hidden::Opaque;
        pub fn value() -> *mut Alias { todo!() }
    "#,
    )
    .unwrap();
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unsupported public foreign declaration core::hidden::Opaque")
    );
}

#[test]
fn conditional_link_sections_require_review_when_added_or_changed() {
    for declaration in ["pub static VALUE: u32 = 1;", "pub fn value() {}"] {
        let (directory, mut ledger) = fixture();
        let path = directory.path().join("core/src/lib.rs");
        fs::write(&path, declaration).unwrap();
        review(directory.path(), &mut ledger);
        fs::write(&path, format!("#[cfg_attr(target_os = \"linux\", unsafe(link_section = \".custom\"))] {declaration}")).unwrap();
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert_ne!(ledger.declarations, actual, "{declaration}");
        validate(directory.path(), &ledger, &actual).unwrap_err();
        review(directory.path(), &mut ledger);
        replace(directory.path(), "core", ".custom", ".changed");
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap(),
        )
        .unwrap_err();
    }
}

#[test]
fn nested_conditional_link_sections_retain_metadata_and_all_predicates() {
    for attribute in [
        "#[cfg_attr(unix, unsafe(link_section = \".custom\"))]",
        "#[cfg_attr(unix, cfg_attr(feature = \"section\", unsafe(link_section = \".custom\")))]",
        "#[cfg_attr(unix, doc(hidden), allow(dead_code), unsafe(link_section = \".custom\"))]",
        "#[cfg_attr(unix, doc = \"Helper\", cfg_attr(feature = \"section\", unsafe(link_section = \".custom\")))]",
        "#[cfg_attr(unix, link_section = \".custom\")]",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), format!(
            "#[cfg(feature = \"outer\")] mod hidden {{ {attribute} pub static VALUE: u32 = 1; }} pub use hidden::VALUE;"
        )).unwrap();
        review(directory.path(), &mut ledger);
        let declaration = ledger
            .declarations
            .iter()
            .find(|d| d.kind == "static")
            .unwrap();
        assert!(
            declaration
                .conditions
                .contains(&"cfg (feature = \"outer\")".into())
        );
        assert!(
            declaration
                .conditions
                .iter()
                .any(|cfg| cfg.contains("cfg_attr (unix") && cfg.contains(".custom")),
            "{attribute}"
        );
        replace(directory.path(), "core", ".custom", ".changed");
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap(),
        )
        .unwrap_err();
        replace(directory.path(), "core", ".changed", ".custom");
        replace(
            directory.path(),
            "core",
            "cfg_attr(unix",
            "cfg_attr(windows",
        );
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap(),
        )
        .unwrap_err();
    }
}

#[test]
fn inactive_sections_and_attribute_names_in_doc_strings_are_not_review_inputs() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub static VALUE: u32 = 1;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    for attribute in [
        "#[cfg_attr(test, unsafe(link_section = \".test\"))]",
        "#[cfg_attr(all(unix, test), unsafe(link_section = \".test\"))]",
        "#[cfg_attr(unix, cfg_attr(test, unsafe(link_section = \".test\")))]",
        "#[cfg_attr(test, cfg_attr(unix, unsafe(link_section = \".test\")))]",
        "#[cfg_attr(unix, doc = \"link_section export_name no_mangle repr (C) cfg (unix)\")]",
        "#[cfg_attr(link_section, allow(dead_code))]",
    ] {
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!("{attribute} pub static VALUE: u32 = 1;"),
        )
        .unwrap();
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert_eq!(ledger.declarations, actual, "{attribute}");
        validate(directory.path(), &ledger, &actual).unwrap();
    }
}

#[test]
fn conditional_metadata_still_retains_other_abi_and_availability_attributes() {
    for (attribute, declaration) in [
        (
            "#[cfg_attr(unix, unsafe(no_mangle))]",
            "pub static VALUE: u32 = 1;",
        ),
        (
            "#[cfg_attr(unix, unsafe(export_name = \"exported\"))]",
            "pub static VALUE: u32 = 1;",
        ),
        ("#[cfg_attr(unix, repr(C))]", "pub struct Options;"),
        ("#[cfg_attr(unix, non_exhaustive)]", "pub struct Options;"),
        ("#[cfg_attr(docsrs, doc(cfg(unix)))]", "pub struct Options;"),
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), declaration).unwrap();
        review(directory.path(), &mut ledger);
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!("{attribute} {declaration}"),
        )
        .unwrap();
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            actual
                .iter()
                .any(|d| d.surface == "core" && !d.conditions.is_empty()),
            "{attribute}"
        );
        validate(directory.path(), &ledger, &actual).unwrap_err();
    }
}

#[test]
fn compiled_conditional_sections_change_emission_and_invalidate_the_reviewed_ledger() {
    use std::process::Command;

    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let source = root.join("core/src/lib.rs");
    fs::write(&source, "pub static VALUE: u32 = 1;").unwrap();
    review(root, &mut ledger);
    save_ffi_fixture_review(root, &ledger);
    let check = || {
        Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
            .args(["check", "--root"])
            .arg(root)
            .output()
            .unwrap()
    };
    assert!(check().status.success());
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let ir = root.join("fixture.ll");
    let compile = || {
        let built = Command::new(&rustc)
            .args([
                "--edition=2024",
                "--crate-type=lib",
                "--crate-name=fixture",
                "--emit=llvm-ir",
            ])
            .arg(&source)
            .arg("-o")
            .arg(&ir)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        fs::read_to_string(&ir).unwrap()
    };
    let (old, new) = if cfg!(target_vendor = "apple") {
        ("__DATA,__parity_old", "__DATA,__parity_new")
    } else {
        (".parity_old", ".parity_new")
    };
    assert!(!compile().contains(&format!("section \"{old}\"")));
    fs::write(
        &source,
        format!("#[cfg_attr(all(), unsafe(link_section = \"{old}\"))] pub static VALUE: u32 = 1;"),
    )
    .unwrap();
    assert!(compile().contains(&format!("section \"{old}\"")));
    let added = check();
    assert!(!added.status.success());
    assert!(String::from_utf8_lossy(&added.stderr).contains("unreviewed declaration"));
    review(root, &mut ledger);
    let original = save_ffi_fixture_review(root, &ledger);
    assert!(check().status.success());
    replace(root, "core", old, new);
    let emission = compile();
    assert!(emission.contains(&format!("section \"{new}\"")));
    assert!(!emission.contains(&format!("section \"{old}\"")));
    let changed = check();
    assert!(!changed.status.success());
    assert!(String::from_utf8_lossy(&changed.stderr).contains("unreviewed declaration"));
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn initializer_only_alias_dependencies_require_review() {
    for declaration in [
        "pub const BITS: u32 = Number::BITS;",
        "pub static BITS: u32 = Number::BITS;",
        "pub struct Options; impl Options { pub const BITS: u32 = Number::BITS; }",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!("type Number = u16; {declaration}"),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        replace(directory.path(), "core", "Number = u16", "Number = u32");
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            validate(directory.path(), &ledger, &actual)
                .unwrap_err()
                .contains("core|core::Number#alias"),
            "{declaration}"
        );
    }
}

#[test]
fn exported_macros_in_private_modules_require_supplements() {
    let (directory, ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub struct Options; mod hidden { #[macro_export] macro_rules! make { () => {}; } }",
    )
    .unwrap();
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
}

#[test]
fn public_extern_crate_reexports_require_review() {
    let (directory, ledger) = fixture();
    replace(
        directory.path(),
        "core",
        "pub struct Options",
        "pub extern crate std as support; pub struct Options",
    );
    let actual = inventory(directory.path(), &ledger).unwrap();
    assert!(
        validate(directory.path(), &ledger, &actual)
            .unwrap_err()
            .contains("unreviewed declaration")
    );
}

#[test]
fn initializer_dependencies_keep_selecting_imports_and_skip_unrelated_private_items() {
    for declaration in [
        "pub const BITS: u32 = Number::BITS;",
        "pub static BITS: u32 = Number::BITS;",
        "pub struct Options; impl Options { pub const BITS: u32 = Number::BITS; }",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), format!(
            "#[cfg(unix)] use std::primitive::u16 as Number; #[cfg(not(unix))] use std::primitive::u32 as Number; type Unused = u8; const PRIVATE: u32 = Unused::BITS; #[cfg(test)] pub const TEST: u32 = Unused::BITS; {declaration}"
        )).unwrap();
        review(directory.path(), &mut ledger);
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.id.contains("Unused#alias"))
        );
        assert_eq!(
            ledger
                .declarations
                .iter()
                .filter(|d| d.surface == "core" && d.kind == "reexport")
                .count(),
            2
        );
        replace(directory.path(), "core", "primitive::u16", "primitive::u64");
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap(),
        )
        .unwrap_err();
    }
}

#[test]
fn exported_macro_reviews_keep_conditions_and_ignore_unexported_siblings() {
    for attribute in [
        "#[macro_export]",
        "#[cfg_attr(unix, macro_export)]",
        "#[cfg_attr(unix, cfg_attr(feature = \"export\", macro_export))]",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), format!(
            "pub struct Options; #[cfg(feature = \"outer\")] mod hidden {{ {attribute} macro_rules! make {{ () => {{}}; }} macro_rules! private {{ () => {{}}; }} }}"
        )).unwrap();
        ledger
            .surfaces
            .iter_mut()
            .find(|s| s.name == "core")
            .unwrap()
            .roots = vec!["core::Options".into()];
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
        review_fixture_macro_inputs(directory.path(), &mut ledger);
        assert_eq!(ledger.supplements.len(), 1);
        let anchor = ledger
            .declarations
            .iter()
            .find(|d| d.kind == "macro")
            .unwrap();
        assert!(
            anchor
                .conditions
                .contains(&"cfg (feature = \"outer\")".into())
        );
        assert!(anchor.shape.contains("macro_export"));
        if attribute.contains("cfg_attr") {
            assert!(
                anchor
                    .conditions
                    .iter()
                    .any(|cfg| cfg.contains("cfg_attr (unix"))
            );
        }
        replace(
            directory.path(),
            "core",
            "macro_rules! private { () => {}; }",
            "macro_rules! private { () => { struct Hidden; }; }",
        );
        assert_eq!(
            ledger.declarations,
            inventory(directory.path(), &ledger).unwrap()
        );
        replace(
            directory.path(),
            "core",
            "macro_rules! make { () => {}; }",
            "macro_rules! make { () => { pub struct Generated; }; }",
        );
        assert!(
            inventory(directory.path(), &ledger)
                .unwrap_err()
                .contains("supplement")
        );
    }
}

#[test]
fn exported_macros_keep_module_generators_but_ignore_test_only_exports() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        pub struct Options;
        #[rewrite] mod hidden { #[macro_export] macro_rules! make { () => {}; } }
        #[cfg(test)] mod tests { #[macro_export] macro_rules! test_only { () => {}; } }
        mod inactive { #[cfg_attr(test, macro_export)] macro_rules! private { () => {}; } }
    ",
    )
    .unwrap();
    review_fixture_macro_inputs(directory.path(), &mut ledger);
    assert_eq!(ledger.supplements.len(), 2);
    assert!(
        ledger
            .supplements
            .iter()
            .any(|s| s.anchor.contains("attribute:module:rewrite"))
    );
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("test_only") || d.id.contains("inactive"))
    );
    replace(
        directory.path(),
        "core",
        "#[macro_export]",
        "#[doc(hidden)] #[macro_export]",
    );
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("supplement")
    );
}

#[test]
fn extern_crate_aliases_are_dependencies_through_nested_qualified_and_glob_imports() {
    for source in [
        "extern crate std as support; pub fn value(x: support::time::Duration) -> support::time::Duration { x }",
        "extern crate std as support; pub mod api { pub fn value(x: support::time::Duration) -> support::time::Duration { x } }",
        "extern crate std as support; mod api { use support::time::Duration; pub fn value(x: Duration) -> Duration { x } } pub use api::value;",
        "extern crate std as support; mod api { use support::time::*; pub fn value(x: Duration) -> Duration { x } } pub use api::value;",
        "extern crate std as support; pub fn value(x: ::support::time::Duration) -> ::support::time::Duration { x }",
        "mod dep { pub(crate) extern crate std as support; } use dep::*; pub fn value(x: support::time::Duration) -> support::time::Duration { x }",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), source).unwrap();
        review(directory.path(), &mut ledger);
        assert_eq!(
            ledger
                .declarations
                .iter()
                .filter(|d| d.kind == "extern_crate")
                .count(),
            1,
            "{source}"
        );
        replace(
            directory.path(),
            "core",
            "extern crate std",
            "extern crate alloc",
        );
        let actual = inventory(directory.path(), &ledger).unwrap();
        assert!(
            validate(directory.path(), &ledger, &actual)
                .unwrap_err()
                .contains("changed declaration"),
            "{source}"
        );
    }
}

#[test]
fn unused_shadowed_and_test_only_extern_crates_are_excluded() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        extern crate alloc as support;
        #[cfg(test)] pub extern crate std as test_only;
        mod hidden { pub extern crate std as unrelated; }
        pub mod api {
            use std as support;
            pub fn value(x: support::time::Duration) -> support::time::Duration { x }
        }
        pub fn generic<support>(value: support) -> support { value }
        pub fn body() { let _: Option<support::string::String> = None; }
    ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(!ledger.declarations.iter().any(|d| d.kind == "extern_crate"));
    replace(
        directory.path(),
        "core",
        "extern crate alloc",
        "extern crate std",
    );
    assert_eq!(
        ledger.declarations,
        inventory(directory.path(), &ledger).unwrap()
    );
}

#[test]
fn self_crate_aliases_resolve_impls_and_public_names_without_expanding_dependencies() {
    for qualifier in ["support", "::support"] {
        let (directory, mut ledger) = fixture();
        fs::write(directory.path().join("core/src/lib.rs"), format!(
            "extern crate self as support; pub struct Options; mod implementation {{ impl {qualifier}::Options {{ pub fn added() {{}} }} }} pub use support::*;"
        )).unwrap();
        review(directory.path(), &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == "core|core::Options::added#method")
        );
        assert!(ledger.declarations.iter().any(|d| d.kind == "extern_crate"));
        replace(directory.path(), "core", "fn added", "fn changed");
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap(),
        )
        .unwrap_err();
    }
}

#[test]
fn extern_crate_visibility_generators_and_same_named_values_are_separate() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "pub extern crate std as support; pub use support as exported;",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    replace(directory.path(), "core", "pub extern crate", "extern crate");
    validate(
        directory.path(),
        &ledger,
        &inventory(directory.path(), &ledger).unwrap(),
    )
    .unwrap_err();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "#[rewrite] extern crate std as support; pub fn support() {}",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.kind == "extern_crate" || d.kind == "macro")
    );
    fs::write(
        directory.path().join("core/src/lib.rs"),
        "#[rewrite] pub extern crate std as support;",
    )
    .unwrap();
    assert!(
        inventory(directory.path(), &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
}

fn compile_review_library(root: &Path, externs: &[(&str, std::path::PathBuf)]) {
    compile_review_library_with_cfg(root, externs, &[]);
}

fn compile_review_library_with_cfg(
    root: &Path,
    externs: &[(&str, std::path::PathBuf)],
    predicates: &[&str],
) {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut command = std::process::Command::new(rustc);
    command
        .args(["--edition=2024", "--crate-type=lib", "--crate-name=fixture"])
        .arg(root.join("core/src/lib.rs"))
        .arg("-L")
        .arg(root)
        .arg("-o")
        .arg(root.join("libfixture.rlib"));
    for predicate in predicates {
        command.arg("--cfg").arg(predicate);
    }
    for (name, path) in externs {
        command
            .arg("--extern")
            .arg(format!("{name}={}", path.display()));
    }
    let built = command.output().unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
}

fn compile_review_consumer(root: &Path, source: &str) -> std::process::Output {
    fs::write(root.join("consumer.rs"), source).unwrap();
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    std::process::Command::new(rustc)
        .arg("--edition=2024")
        .arg(root.join("consumer.rs"))
        .arg("--extern")
        .arg(format!(
            "fixture={}",
            root.join("libfixture.rlib").display()
        ))
        .arg("-L")
        .arg(root)
        .arg("-o")
        .arg(root.join(format!("consumer{}", std::env::consts::EXE_SUFFIX)))
        .output()
        .unwrap()
}

fn check_review_fixture(root: &Path) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
        .args(["check", "--root"])
        .arg(root)
        .output()
        .unwrap()
}

#[test]
fn compiled_initializer_value_changes_invalidate_the_unchanged_ledger() {
    for (declaration, expression) in [
        ("pub const BITS: u32 = Number::BITS;", "fixture::BITS"),
        ("pub static BITS: u32 = Number::BITS;", "fixture::BITS"),
        (
            "pub struct Options; impl Options { pub const BITS: u32 = Number::BITS; }",
            "fixture::Options::BITS",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!("type Number = u16; {declaration}"),
        )
        .unwrap();
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success());
        let consumer = format!("fn main() {{ println!(\"{{}}\", {expression}); }}");
        for expected in ["16", "32"] {
            compile_review_library(root, &[]);
            let built = compile_review_consumer(root, &consumer);
            assert!(
                built.status.success(),
                "{}",
                String::from_utf8_lossy(&built.stderr)
            );
            let output = std::process::Command::new(
                root.join(format!("consumer{}", std::env::consts::EXE_SUFFIX)),
            )
            .output()
            .unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
            if expected == "16" {
                replace(root, "core", "Number = u16", "Number = u32");
            }
        }
        let checked = check_review_fixture(root);
        assert!(!checked.status.success());
        assert!(
            String::from_utf8_lossy(&checked.stderr)
                .contains("changed declaration requires review: core|core::Number#alias")
        );
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn compiled_exported_macros_and_crate_reexports_require_new_review() {
    for (addition, consumer, expected) in [
        (
            "mod hidden { #[macro_export] macro_rules! make { () => { 7u16 }; } }",
            "fn main() { assert_eq!(fixture::make!(), 7); }",
            "unreviewed macro",
        ),
        (
            "pub extern crate std as support;",
            "fn main() { assert_eq!(fixture::support::time::Duration::from_secs(1).as_secs(), 1); }",
            "unreviewed declaration",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), "pub struct Options;").unwrap();
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success());
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        fs::write(
            root.join("core/src/lib.rs"),
            format!("pub struct Options; {addition}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        let built = compile_review_consumer(root, consumer);
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        assert!(
            std::process::Command::new(
                root.join(format!("consumer{}", std::env::consts::EXE_SUFFIX))
            )
            .output()
            .unwrap()
            .status
            .success()
        );
        let checked = check_review_fixture(root);
        assert!(!checked.status.success());
        assert!(String::from_utf8_lossy(&checked.stderr).contains(expected));
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn compiled_private_crate_alias_retargeting_breaks_the_original_consumer_and_review() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let mut externs = Vec::new();
    for (name, ty) in [("v16", "u16"), ("v32", "u32")] {
        let source = root.join(format!("{name}.rs"));
        fs::write(&source, format!("pub type Value = {ty};")).unwrap();
        let library = root.join(format!("lib{name}.rlib"));
        let built = std::process::Command::new(&rustc)
            .args(["--edition=2024", "--crate-type=lib", "--crate-name", name])
            .arg(source)
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        externs.push((name, library));
    }
    fs::write(
        root.join("core/src/lib.rs"),
        "extern crate v16 as support; pub fn value(x: support::Value) -> support::Value { x }",
    )
    .unwrap();
    review(root, &mut ledger);
    let original = save_ffi_fixture_review(root, &ledger);
    assert!(check_review_fixture(root).status.success());
    compile_review_library(root, &externs);
    let consumer = "fn main() { let _: u16 = fixture::value(1u16); }";
    assert!(compile_review_consumer(root, consumer).status.success());
    replace(root, "core", "extern crate v16", "extern crate v32");
    compile_review_library(root, &externs);
    let broken = compile_review_consumer(root, consumer);
    assert!(!broken.status.success());
    assert!(String::from_utf8_lossy(&broken.stderr).contains("mismatched types"));
    assert!(
        compile_review_consumer(root, "fn main() { let _: u32 = fixture::value(1u32); }")
            .status
            .success()
    );
    let checked = check_review_fixture(root);
    assert!(!checked.status.success());
    assert!(
        String::from_utf8_lossy(&checked.stderr)
            .contains("changed declaration requires review: core|core::support#extern_crate")
    );
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn compiled_self_crate_aliases_preserve_implementation_discovery() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "extern crate self as support; pub struct Options; impl ::support::Options { pub fn added() -> u16 { 7 } }").unwrap();
    review(root, &mut ledger);
    save_ffi_fixture_review(root, &ledger);
    assert!(check_review_fixture(root).status.success());
    compile_review_library(root, &[]);
    let built = compile_review_consumer(
        root,
        "fn main() { assert_eq!(fixture::Options::added(), 7); }",
    );
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    replace(root, "core", "fn added", "fn renamed");
    assert!(!check_review_fixture(root).status.success());
}

#[test]
fn extern_crate_conditions_and_value_namespaces_remain_independent() {
    let (directory, mut ledger) = fixture();
    fs::write(
        directory.path().join("core/src/lib.rs"),
        r"
        #[cfg(unix)] extern crate std as support;
        #[cfg(not(unix))] extern crate alloc as support;
        pub mod api {
            pub fn support() {}
            pub fn value(x: support::time::Duration) -> support::time::Duration { x }
        }
    ",
    )
    .unwrap();
    review(directory.path(), &mut ledger);
    assert_eq!(
        ledger
            .declarations
            .iter()
            .filter(|d| d.kind == "extern_crate")
            .count(),
        2
    );
    replace(
        directory.path(),
        "core",
        "extern crate std",
        "extern crate core",
    );
    validate(
        directory.path(),
        &ledger,
        &inventory(directory.path(), &ledger).unwrap(),
    )
    .unwrap_err();
}

#[test]
fn opaque_import_aliases_keep_selecting_globs() {
    for imports in [
        "use std::time as selected; use selected::*;",
        "mod hidden { pub use std::time as selected; pub use selected::*; } use hidden::*;",
    ] {
        let (directory, mut ledger) = fixture();
        fs::write(
            directory.path().join("core/src/lib.rs"),
            format!("{imports} pub fn value(input: Duration) -> Duration {{ input }}"),
        )
        .unwrap();
        review(directory.path(), &mut ledger);
        assert!(ledger.declarations.iter().any(|d| {
            d.surface == "core" && d.kind == "reexport" && d.shape.contains("selected::*")
        }));
        replace(directory.path(), "core", "std::time", "other::time");
        validate(
            directory.path(),
            &ledger,
            &inventory(directory.path(), &ledger).unwrap(),
        )
        .unwrap_err();
    }
}

#[test]
fn compiled_private_module_return_types_require_recursive_member_review() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "mod hidden { pub struct Value { pub n: u16 } } pub fn value() -> hidden::Value { hidden::Value { n: 0 } }").unwrap();
    review(root, &mut ledger);
    let original = save_ffi_fixture_review(root, &ledger);
    compile_review_library(root, &[]);
    let consumer = "fn main() { let _: u16 = fixture::value().n; }";
    assert!(compile_review_consumer(root, consumer).status.success());
    replace(root, "core", "n: u16", "n: u32");
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    let checked = check_review_fixture(root);
    assert!(!checked.status.success());
    assert!(String::from_utf8_lossy(&checked.stderr).contains("core::hidden::Value::n#field"));
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn compiled_explicit_imports_shadow_glob_implementation_owners() {
    for imports in [
        "use left::*; use right::Options;",
        "use right::Options; use left::*;",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), format!("pub mod left {{ pub struct Options; impl Options {{ pub fn same() {{}} }} }} pub mod right {{ pub struct Options; }} {imports} impl Options {{ pub fn same() {{}} }}")).unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        assert_eq!(
            ledger
                .declarations
                .iter()
                .filter(|d| d.kind == "method" && d.id.contains("Options::same#method"))
                .count(),
            2
        );
        assert!(
            compile_review_consumer(
                root,
                "fn main() { fixture::left::Options::same(); fixture::right::Options::same(); }"
            )
            .status
            .success()
        );
    }
}

#[test]
fn local_type_dependencies_follow_fields_variants_methods_callbacks_and_cycles() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), r"
        mod hidden {
            pub struct Value { pub leaf: Leaf, pub callback: fn() -> Box<dyn View>, secret: Private }
            pub enum Leaf { End, More(Box<Value>), Bits(Payload) }
            pub union Payload { pub raw: u16 }
            pub trait View { fn read(&self) -> Payload; }
            impl Leaf { pub fn next(&self) -> Option<Box<Value>> { None } }
            pub struct Private { pub hidden: u32 }
            pub struct Unrelated;
        }
        pub fn value() -> hidden::Value { unimplemented!() }
    ").unwrap();
    compile_review_library(root, &[]);
    review(root, &mut ledger);
    for member in [
        "Value#struct",
        "Value::leaf#field",
        "Leaf#enum",
        "Leaf::Bits#variant",
        "Leaf::next#method",
        "Payload#union",
        "Payload::raw#field",
        "View#trait",
        "View::read#trait_method",
    ] {
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == format!("core|core::hidden::{member}")),
            "{member}"
        );
    }
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("::Private") || d.id.contains("::Unrelated"))
    );
    replace(root, "core", "hidden: u32", "hidden: u64");
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    replace(root, "core", "raw: u16", "raw: u32");
    validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
}

#[test]
fn referenced_local_types_retain_generators_conditions_and_recursive_aliases() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        #[rewrite_module] mod hidden {
            #[cfg(feature = "value")] #[rewrite_type] pub struct Value { pub n: Number }
            type Number = u16;
            #[cfg(feature = "value")] #[rewrite_impl] impl Value {}
            #[cfg(test)] pub struct TestOnly { pub unused: u64 }
        }
        pub fn value() -> Option<hidden::Value> { None }
    "#,
    )
    .unwrap();
    review_fixture_macro_inputs(root, &mut ledger);
    assert_eq!(ledger.supplements.len(), 3);
    let value = ledger
        .declarations
        .iter()
        .find(|d| d.kind == "struct" && d.id.contains("hidden::Value#"))
        .unwrap();
    assert_eq!(value.conditions, ["cfg (feature = \"value\")"]);
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("TestOnly"))
    );
    replace(root, "core", "Number = u16", "Number = u32");
    assert!(
        inventory(root, &ledger)
            .unwrap_err()
            .contains("changed or invalid supplement")
    );
}

#[test]
fn local_and_explicit_names_shadow_globs_in_their_own_namespaces() {
    for (selection, expected) in [
        (
            "use left::*; pub struct Options;",
            "core::Options::same#method",
        ),
        (
            "use left::*; pub fn Options() {}",
            "core::left::Options::same#method",
        ),
        (
            "mod middle { pub use crate::left::*; pub use crate::right::Options; } use middle::*;",
            "core::right::Options::same#method",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), format!("pub mod left {{ pub struct Options {{ pub value: u16 }} }} pub mod right {{ pub struct Options; }} {selection} impl Options {{ pub fn same() {{}} }}")).unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        let methods: Vec<_> = ledger
            .declarations
            .iter()
            .filter(|d| d.surface == "core" && d.kind == "method")
            .collect();
        assert_eq!(methods.len(), 1, "{selection}");
        assert_eq!(methods[0].id, format!("core|{expected}"));
    }
}

#[test]
fn conditional_explicit_imports_leave_only_the_globs_unshadowed_configurations() {
    for attribute in [
        "#[cfg(unix)]",
        "#[cfg_attr(feature = \"pick\", cfg(unix))]",
        "#[cfg_attr(feature = \"pick\", cfg_attr(feature = \"inner\", cfg(unix)))]",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), format!("pub mod left {{ pub struct Options; }} pub mod right {{ pub struct Options; }} use left::*; {attribute} use right::Options; impl Options {{ pub fn selected() {{}} }}")).unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        let methods: Vec<_> = ledger
            .declarations
            .iter()
            .filter(|d| d.surface == "core" && d.kind == "method")
            .collect();
        assert_eq!(methods.len(), 2, "{attribute}");
        let fallback = methods
            .iter()
            .find(|d| d.id.contains("left::Options"))
            .unwrap();
        assert!(
            fallback
                .conditions
                .iter()
                .any(|condition| condition.contains("not (unix)")),
            "{attribute}: {:?}",
            fallback.conditions
        );
        let selected = methods
            .iter()
            .find(|d| d.id.contains("right::Options"))
            .unwrap();
        assert_ne!(selected.conditions, Vec::<String>::new());
        replace(root, "core", "unix", "windows");
        validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
    }
}

#[test]
fn shadowed_globs_are_not_dependencies_of_an_explicitly_selected_type() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "mod left { pub struct Options; } mod right { pub struct Options; } use left::*; use right::Options; pub fn value(input: Options) -> Options { input }").unwrap();
    compile_review_library(root, &[]);
    review(root, &mut ledger);
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("left::Options") || d.shape.contains("left::*"))
    );
    replace(
        root,
        "core",
        "mod left { pub struct Options; }",
        "mod left { pub struct Unrelated; }",
    );
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn opaque_named_imports_cannot_silently_choose_a_namespace_over_local_globs() {
    for (import, ty) in [
        ("std::time::Duration", "Options"),
        ("std::mem::drop", "Options"),
    ] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), format!("mod left {{ pub struct Options {{ pub n: u16 }} }} use left::*; use {import} as Options; pub fn value(input: {ty}) -> {ty} {{ input }}")).unwrap();
        compile_review_library(root, &[]);
        assert!(
            inventory(root, &ledger)
                .unwrap_err()
                .contains("cannot resolve import namespace")
        );
        replace(
            root,
            "core",
            "input: Options) -> Options",
            "input: left::Options) -> left::Options",
        );
        compile_review_library(root, &[]);
        assert!(inventory(root, &ledger).is_ok());
    }
}

#[test]
fn opaque_import_namespace_checks_ignore_disjoint_and_unreachable_candidates() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        mod left { pub struct Options { pub n: u16 } }
        #[cfg(not(unix))] use left::*;
        #[cfg(unix)] use std::time::Duration as Options;
        mod unused {
            pub use crate::left::*;
            use std::mem::drop as Options;
            pub fn value(input: Options) -> Options { input }
        }
        pub fn value(input: Options) -> Options { input }
    ",
    )
    .unwrap();
    compile_review_library(root, &[]);
    review(root, &mut ledger);
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("unused::"))
    );
}

#[test]
fn private_local_types_shadow_globs_without_exposing_their_implementations() {
    for declaration in [
        "struct Options;",
        "pub(crate) struct Options;",
        "enum Options { A }",
        "union Options { n: u16 }",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), format!("pub mod left {{ pub struct Options; impl Options {{ pub fn same() {{}} }} }} use left::*; {declaration} impl Options {{ pub fn same() {{}} }}")).unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        let methods: Vec<_> = ledger
            .declarations
            .iter()
            .filter(|d| d.surface == "core" && d.kind == "method")
            .collect();
        assert_eq!(methods.len(), 1, "{declaration}");
        assert_eq!(methods[0].id, "core|core::left::Options::same#method");
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.id == "core|core::Options::same#method")
        );
    }
}

#[test]
fn compiled_local_crate_name_collisions_cannot_hide_public_methods() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    ledger
        .surfaces
        .iter_mut()
        .find(|surface| surface.name == "core")
        .unwrap()
        .crate_name = "fixture".into();
    fs::write(
        root.join("core/src/lib.rs"),
        "pub mod fixture { pub struct Options; } impl fixture::Options { pub fn policy(&self) {} }",
    )
    .unwrap();
    compile_review_library(root, &[]);
    let consumer = "fn main() { fixture::fixture::Options.policy(); }";
    assert!(compile_review_consumer(root, consumer).status.success());
    review(root, &mut ledger);
    let original = save_ffi_fixture_review(root, &ledger);
    assert!(check_review_fixture(root).status.success());
    replace(root, "core", "fn policy", "fn renamed_policy");
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    let checked = check_review_fixture(root);
    assert!(!checked.status.success());
    assert!(
        String::from_utf8_lossy(&checked.stderr)
            .contains("fixture::fixture::Options::policy#method")
    );
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn compiled_public_functions_do_not_expose_same_named_private_types() {
    for explicit_root in [false, true] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            "struct Options {} impl Options { pub fn internal() {} } pub fn Options() {}",
        )
        .unwrap();
        if explicit_root {
            ledger
                .surfaces
                .iter_mut()
                .find(|surface| surface.name == "core")
                .unwrap()
                .roots = vec!["core::Options".into()];
        }
        compile_review_library(root, &[]);
        let consumer = "fn main() { fixture::Options(); }";
        assert!(compile_review_consumer(root, consumer).status.success());
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.surface == "core" && d.kind == "method")
        );
        replace(root, "core", "fn internal", "fn renamed_internal");
        compile_review_library(root, &[]);
        assert!(compile_review_consumer(root, consumer).status.success());
        assert!(check_review_fixture(root).status.success());
        assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
        replace(
            root,
            "core",
            "pub fn Options()",
            "pub fn Options(value: u16)",
        );
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn crate_name_spellings_resolve_nested_locals_and_explicit_self_aliases() {
    for (source, method) in [
        (
            "pub mod api { pub mod fixture { pub struct Options; } impl fixture::Options { pub fn policy() {} } }",
            "fixture::api::fixture::Options::policy#method",
        ),
        (
            "extern crate self as fixture; pub struct Options; impl fixture::Options { pub fn policy() {} }",
            "fixture::Options::policy#method",
        ),
        (
            "pub struct Options; type fixture = Options; impl fixture { pub fn policy() {} }",
            "fixture::Options::policy#method",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        ledger
            .surfaces
            .iter_mut()
            .find(|surface| surface.name == "core")
            .unwrap()
            .crate_name = "fixture".into();
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == format!("core|{method}")),
            "{source}"
        );
        replace(root, "core", "fn policy", "fn renamed_policy");
        validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
    }
}

#[test]
fn crate_name_imports_retain_predicates_and_inventory_roots_remain_qualified() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let surface = ledger
        .surfaces
        .iter_mut()
        .find(|surface| surface.name == "core")
        .unwrap();
    surface.crate_name = "fixture".into();
    surface.roots = vec!["fixture::left::Options".into()];
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        pub mod left { pub struct Options; }
        pub mod right { pub struct Options; }
        #[cfg(unix)] use left as fixture;
        #[cfg(not(unix))] use right as fixture;
        impl fixture::Options { pub fn policy() {} }
    ",
    )
    .unwrap();
    compile_review_library(root, &[]);
    review(root, &mut ledger);
    let methods: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.surface == "core" && d.kind == "method")
        .collect();
    assert_eq!(methods.len(), 1);
    assert!(
        methods[0]
            .id
            .contains("fixture::left::Options::policy#method")
    );
    assert_eq!(methods[0].conditions, ["cfg (unix)"]);
    assert_eq!(
        ledger
            .declarations
            .iter()
            .filter(|d| d.surface == "core" && d.kind == "reexport")
            .count(),
        2
    );
    replace(root, "core", "use left as fixture", "use right as fixture");
    validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
}

#[test]
fn value_exports_do_not_follow_private_aliases_or_associated_members() {
    for declaration in [
        "pub const Options: u16 = 7;",
        "pub static Options: u16 = 7;",
        "pub fn Options() {}",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), format!("mod hidden {{ pub struct Actual {{ pub n: u16 }} impl Actual {{ pub const INTERNAL: u16 = 7; pub fn internal() {{}} }} }} type Options = hidden::Actual; {declaration}")).unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        assert_eq!(
            ledger
                .declarations
                .iter()
                .filter(|d| d.surface == "core")
                .count(),
            1,
            "{declaration}"
        );
        replace(root, "core", "pub n: u16", "pub n: u32");
        replace(root, "core", "fn internal", "fn renamed_internal");
        assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    }
}

#[test]
fn type_and_value_generator_anchors_follow_their_declarations() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "struct Options {} #[rewrite_impl] impl Options { #[rewrite_method] pub fn internal() {} } pub fn Options() {}").unwrap();
    review(root, &mut ledger);
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.surface == "core" && d.kind == "macro")
    );
    replace(
        root,
        "core",
        "pub fn Options()",
        "#[rewrite_function] pub fn Options()",
    );
    assert!(
        inventory(root, &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
    review_fixture_macro_inputs(root, &mut ledger);
    assert_eq!(ledger.supplements.len(), 1);
    assert!(
        ledger.supplements[0]
            .anchor
            .contains("attribute:function:rewrite_function")
    );
    replace(root, "core", "fn internal", "fn renamed_internal");
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn signature_types_do_not_expose_same_named_value_generators() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "mod hidden { pub struct Options { pub n: u16 } #[rewrite_function] pub fn Options() {} impl Options { pub fn policy(&self) {} } } pub fn value() -> hidden::Options { hidden::Options { n: 0 } }").unwrap();
    review(root, &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::Options::policy#method")
    );
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::Options#function" || d.kind == "macro")
    );
    replace(root, "core", "pub n: u16", "pub n: u32");
    validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
}

#[test]
fn value_namespace_constructors_keep_their_types_public_members() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "mod hidden { pub struct Options(pub u16); impl Options { pub fn value(&self) -> u16 { self.0 } } } pub use hidden::*; pub type Options = u32;").unwrap();
    compile_review_library(root, &[]);
    let consumer = "fn main() { let value = fixture::Options(7); let _: fixture::Options = 0; let _: u16 = value.value(); }";
    assert!(compile_review_consumer(root, consumer).status.success());
    review(root, &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::Options::value#method")
    );
    replace(root, "core", "pub u16", "pub u32");
    replace(root, "core", "-> u16", "-> u32");
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
}

#[test]
fn compiled_conditional_import_namespaces_keep_the_public_fallback_type() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        mod first {
            #[cfg(feature = "x")] pub struct Options { pub n: u16 }
            #[cfg(not(feature = "x"))] pub fn Options() {}
        }
        mod fallback {
            pub struct Options { pub n: u16 }
            impl Options { pub fn policy(&self) {} }
        }
        use fallback::*;
        use first::Options;
        pub fn value() -> Options {
            #[cfg(feature = "x")] { first::Options { n: 1 } }
            #[cfg(not(feature = "x"))] { fallback::Options { n: 2 } }
        }
    "#,
    )
    .unwrap();
    let consumer = "fn main() { let _: u16 = fixture::value().n; }";
    for predicates in [vec![], vec!["feature=\"x\""]] {
        compile_review_library_with_cfg(root, &[], &predicates);
        assert!(compile_review_consumer(root, consumer).status.success());
    }
    review(root, &mut ledger);
    let original = save_ffi_fixture_review(root, &ledger);
    replace(
        root,
        "core",
        "mod fallback {\n            pub struct Options { pub n: u16 }",
        "mod fallback {\n            pub struct Options { pub n: u32 }",
    );
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    compile_review_library_with_cfg(root, &[], &["feature=\"x\""]);
    assert!(compile_review_consumer(root, consumer).status.success());
    let checked = check_review_fixture(root);
    assert!(!checked.status.success());
    assert!(String::from_utf8_lossy(&checked.stderr).contains("core::fallback::Options::n#field"));
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn compiled_unreachable_and_test_only_trait_members_do_not_fail_discovery() {
    for declaration in [
        "mod hidden { pub trait Internal { const VALUE: u8; } }",
        "mod hidden { pub trait Internal { #[cfg(test)] const VALUE: u8; } }",
        "pub trait Visible { #[cfg(test)] const VALUE: u8; }",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!("pub struct Options; {declaration}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success(), "{declaration}");
    }
}

#[test]
fn named_import_blockers_keep_reexport_target_and_import_conditions() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        mod definitions {
            #[cfg(feature = "x")] pub struct Options;
            #[cfg(not(feature = "x"))] pub fn Options() {}
        }
        mod first { pub use crate::definitions::Options; }
        mod fallback { pub struct Options { pub n: u16 } }
        use fallback::*;
        #[cfg_attr(not(feature = "named"), cfg(any()))] use first::Options;
        impl Options { pub fn policy(&self) {} }
        pub fn value(input: Options) -> Options { input }
    "#,
    )
    .unwrap();
    for predicates in [
        vec![],
        vec!["feature=\"x\""],
        vec!["feature=\"named\""],
        vec!["feature=\"x\"", "feature=\"named\""],
    ] {
        compile_review_library_with_cfg(root, &[], &predicates);
    }
    review(root, &mut ledger);
    let fallback = ledger
        .declarations
        .iter()
        .find(|d| d.id.contains("core::fallback::Options::policy#method"))
        .unwrap();
    assert!(
        fallback
            .conditions
            .iter()
            .any(|cfg| cfg.contains("not (feature = \"x\")")
                && cfg.contains("not (feature = \"named\")"))
    );
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::fallback::Options::n#field")
    );
    replace(root, "core", "feature = \"x\"", "feature = \"other\"");
    validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
}

#[test]
fn conditional_targets_in_both_namespaces_keep_fallback_type_members() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        mod first {
            #[cfg(feature = "x")] pub struct Options;
            #[cfg(not(feature = "x"))] pub fn Options() {}
        }
        mod fallback { pub struct Options { pub n: u16 } }
        use first::Options;
        use fallback::*;
        impl Options { pub fn policy(&self) {} }
        pub fn value(input: Options) -> Options { input }
    "#,
    )
    .unwrap();
    compile_review_library(root, &[]);
    compile_review_library_with_cfg(root, &[], &["feature=\"x\""]);
    review(root, &mut ledger);
    let fallback = ledger
        .declarations
        .iter()
        .find(|d| d.id.contains("core::fallback::Options::policy#method"))
        .unwrap();
    assert_eq!(fallback.conditions, ["cfg (not (feature = \"x\"))"]);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::fallback::Options::n#field")
    );
}

#[test]
fn unsupported_trait_members_fail_only_when_the_trait_is_reached() {
    for (declaration, exposure, explicit_root) in [
        (
            "mod hidden { pub trait Internal { const VALUE: u8; } }",
            "pub use hidden::Internal;",
            false,
        ),
        (
            "mod hidden { pub trait Internal { const VALUE: u8; } }",
            "pub fn value<T: hidden::Internal>(input: T) -> T { input }",
            false,
        ),
        (
            "mod hidden { pub trait Internal { const VALUE: u8; } }",
            "pub trait Visible: hidden::Internal {}",
            false,
        ),
        (
            "mod hidden { pub trait Internal { const VALUE: u8; } }",
            "pub trait Visible { type Output: hidden::Internal; }",
            false,
        ),
        (
            "mod hidden { pub trait Internal { const VALUE: u8; } }",
            "",
            true,
        ),
        (
            "mod hidden { macro_rules! members { () => { const VALUE: u8; }; } pub trait Internal { members!(); } }",
            "pub use hidden::Internal;",
            false,
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!("pub struct Options; {declaration}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        replace(
            root,
            "core",
            "pub struct Options;",
            &format!("pub struct Options; {exposure}"),
        );
        if explicit_root {
            ledger
                .surfaces
                .iter_mut()
                .find(|surface| surface.name == "core")
                .unwrap()
                .roots = vec!["core::hidden::Internal".into()];
        }
        compile_review_library(root, &[]);
        let error = inventory(root, &ledger).unwrap_err();
        assert!(
            error.contains("unsupported audited trait member in core::hidden::Internal"),
            "{error}"
        );
    }
}

#[test]
fn test_only_trait_items_preserve_the_reviewed_inventory() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        "pub trait Policy { fn run(&self); }",
    )
    .unwrap();
    review(root, &mut ledger);
    let original = save_ffi_fixture_review(root, &ledger);
    replace(
        root,
        "core",
        "pub trait Policy {",
        r#"pub trait Policy {
        #[cfg(test)] const VALUE: u8;
        #[cfg_attr(all(), cfg(test))] missing!();
        #[cfg(all(test, feature = "x"))] type Hidden;
        #[cfg_attr(all(), cfg(test))] #[generate] fn generated(&self);
    "#,
    );
    compile_review_library(root, &[]);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    assert!(check_review_fixture(root).status.success());
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn feature_gated_unsupported_trait_members_remain_review_errors() {
    for member in [
        "#[cfg(feature = \"x\")] const VALUE: u8;",
        "#[cfg_attr(feature = \"x\", cfg(test))] const VALUE: u8;",
    ] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!("pub trait Policy {{ {member} }}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        compile_review_library_with_cfg(root, &[], &["feature=\"x\""]);
        let error = inventory(root, &ledger).unwrap_err();
        assert!(
            error.contains("unsupported audited trait member in core::Policy")
                && error.contains("VALUE")
        );
    }
}

#[test]
fn compiled_array_length_constant_changes_invalidate_the_unchanged_ledger() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        "mod limits { pub const LEN: usize = 1; } pub struct S { pub data: [u8; limits::LEN] }",
    )
    .unwrap();
    compile_review_library(root, &[]);
    let consumer = "fn check(s: fixture::S) { let _: [u8; 1] = s.data; } fn main() {}";
    assert!(compile_review_consumer(root, consumer).status.success());
    review(root, &mut ledger);
    let original = save_ffi_fixture_review(root, &ledger);
    replace(root, "core", "LEN: usize = 1", "LEN: usize = 2");
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    let checked = check_review_fixture(root);
    assert!(!checked.status.success());
    assert!(String::from_utf8_lossy(&checked.stderr).contains("core::limits::LEN#constant"));
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn compiled_const_generics_do_not_shadow_type_aliases() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        "type N = u16; pub struct S<const N: usize> { pub v: [N; N] }",
    )
    .unwrap();
    compile_review_library(root, &[]);
    let consumer = "fn check(s: fixture::S<1>) { let _: [u16; 1] = s.v; } fn main() {}";
    assert!(compile_review_consumer(root, consumer).status.success());
    review(root, &mut ledger);
    let original = save_ffi_fixture_review(root, &ledger);
    replace(root, "core", "type N = u16", "type N = u32");
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    let checked = check_review_fixture(root);
    assert!(!checked.status.success());
    assert!(String::from_utf8_lossy(&checked.stderr).contains("core::N#alias"));
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn compiled_test_only_cfg_attr_fields_and_modules_preserve_the_inventory() {
    for addition in [
        "pub struct S(#[cfg_attr(all(), cfg(test))] bool, pub u16);",
        "pub struct S(pub u16); #[cfg_attr(all(), cfg(test))] pub mod missing;",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), "pub struct S(pub u16);").unwrap();
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        fs::write(root.join("core/src/lib.rs"), addition).unwrap();
        compile_review_library(root, &[]);
        assert!(
            compile_review_consumer(root, "fn main() { let _: u16 = fixture::S(7).0; }")
                .status
                .success()
        );
        assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
        assert!(check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn constant_dependencies_recurse_through_private_imports_aliases_and_self() {
    for (source, before, after, expected, consumer) in [
        (
            "type Number = u8; const BASE: usize = Number::BITS as usize / 8; const LEN: usize = BASE; const UNUSED: usize = 9; pub struct S { pub data: [u8; LEN] }",
            "Number = u8",
            "Number = u16",
            "core::Number#alias",
            "fn check(s: fixture::S) { let _: [u8; 1] = s.data; } fn main() {}",
        ),
        (
            "mod limits { pub(crate) const LEN: usize = 1; pub(crate) const UNUSED: usize = 9; } use limits::*; pub struct S { pub data: [u8; LEN] }",
            "LEN: usize = 1",
            "LEN: usize = 2",
            "core::limits::LEN#constant",
            "fn check(s: fixture::S) { let _: [u8; 1] = s.data; } fn main() {}",
        ),
        (
            "struct Limits {} type Choice = Limits; impl Limits { const BASE: usize = 1; const LEN: usize = Self::BASE; const UNUSED: usize = 9; } pub struct S { pub data: [u8; Choice::LEN] }",
            "BASE: usize = 1",
            "BASE: usize = 2",
            "core::Limits::BASE#associated_constant",
            "fn check(s: fixture::S) { let _: [u8; 1] = s.data; } fn main() {}",
        ),
        (
            "pub struct S { pub data: [u8; Self::LEN] } impl S { const LEN: usize = 1; const UNUSED: usize = 9; }",
            "LEN: usize = 1",
            "LEN: usize = 2",
            "core::S::LEN#associated_constant",
            "fn check(s: fixture::S) { let _: [u8; 1] = s.data; } fn main() {}",
        ),
        (
            "pub struct S; impl S { const LEN: usize = 1; const UNUSED: usize = 9; pub fn bytes(&self) -> [u8; Self::LEN] { [0; Self::LEN] } }",
            "LEN: usize = 1",
            "LEN: usize = 2",
            "core::S::LEN#associated_constant",
            "fn check(s: fixture::S) { let _: [u8; 1] = s.bytes(); } fn main() {}",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[]);
        assert!(compile_review_consumer(root, consumer).status.success());
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == format!("core|{expected}")),
            "{source}"
        );
        assert!(
            !ledger.declarations.iter().any(|d| d.id.contains("UNUSED")),
            "{source}"
        );
        replace(root, "core", "UNUSED: usize = 9", "UNUSED: usize = 10");
        assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
        replace(root, "core", before, after);
        compile_review_library(root, &[]);
        assert!(
            !compile_review_consumer(root, consumer).status.success(),
            "{source}"
        );
        let checked = check_review_fixture(root);
        assert!(!checked.status.success(), "{source}");
        assert!(String::from_utf8_lossy(&checked.stderr).contains(expected));
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn type_and_const_generic_shadowing_select_only_their_namespace_imports() {
    for (generics, fields, changed, unused, consumer) in [
        (
            "N: Iterator",
            "pub v: [N::Item; N]",
            "values",
            "types",
            "fn check(s: fixture::S<std::vec::IntoIter<u8>>) { let _: [u8; 1] = s.v; } fn main() {}",
        ),
        (
            "const N: usize",
            "pub v: [N; N], pub bits: [u8; N::BITS as usize]",
            "types",
            "values",
            "fn check(s: fixture::S<1>) { let _: [u16; 1] = s.v; } fn main() {}",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), format!("mod types {{ pub type N = u16; }} mod values {{ pub const N: usize = 1; }} use types::N; use values::N; pub struct S<{generics}> {{ {fields} }}")).unwrap();
        compile_review_library(root, &[]);
        assert!(compile_review_consumer(root, consumer).status.success());
        review(root, &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id.contains(&format!("core::{changed}::N#")))
        );
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.id.contains(&format!("core::{unused}::"))
                    || d.shape.contains(&format!("use {unused}::N")))
        );
        if unused == "types" {
            replace(root, "core", "N = u16", "N = u32");
        } else {
            replace(root, "core", "N: usize = 1", "N: usize = 2");
        }
        assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
        if changed == "types" {
            replace(root, "core", "N = u16", "N = u32");
        } else {
            replace(root, "core", "N: usize = 1", "N: usize = 2");
        }
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
    }
}

#[test]
fn referenced_private_constant_generators_require_review_but_unused_ones_do_not() {
    for declaration in [
        "#[generate] const LEN: usize = 1; pub struct S(pub [u8; LEN]);",
        "pub struct S(pub [u8; Self::LEN]); impl S { #[generate] const LEN: usize = 1; }",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!("#[unused_generator] const UNUSED: usize = 9; {declaration}"),
        )
        .unwrap();
        assert!(
            inventory(root, &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
        review_fixture_macro_inputs(root, &mut ledger);
        assert_eq!(ledger.supplements.len(), 1);
        assert!(!ledger.declarations.iter().any(|d| d.id.contains("UNUSED")));
        replace(root, "core", "LEN: usize = 1", "LEN: usize = 2");
        assert!(inventory(root, &ledger).unwrap_err().contains("supplement"));
    }
}

#[test]
fn nested_test_only_cfg_attr_members_and_inner_modules_are_excluded_early() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let original_source = "pub struct S(pub u16); pub struct Named { pub n: u16 } pub enum E { A(u16), B } pub union U { pub n: u16 }";
    fs::write(root.join("core/src/lib.rs"), original_source).unwrap();
    review(root, &mut ledger);
    let inactive = "#[cfg_attr(all(), cfg_attr(not(test), cfg(test)))]";
    fs::write(root.join("core/src/lib.rs"), format!(
        "pub struct S({inactive} bool, pub u16); pub struct Named {{ {inactive} helper: bool, pub n: u16 }} pub enum E {{ {inactive} Helper, A(u16), B }} pub union U {{ {inactive} helper: u8, pub n: u16 }} {inactive} #[generate] pub fn ignored() {{}} {inactive} #[cfg_attr(unix, path = \"missing.rs\")] pub mod absent; pub mod inner;"
    )).unwrap();
    fs::write(
        root.join("core/src/inner.rs"),
        format!("#![cfg_attr(all(), cfg(test))] pub mod missing; {inactive} pub struct Ignored;"),
    )
    .unwrap();
    compile_review_library(root, &[]);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn cfg_attr_test_filtering_preserves_symbolic_feature_alternatives() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        pub struct S(#[cfg_attr(feature = "helper", cfg(test))] bool, pub u16);
        pub enum E { #[cfg_attr(feature = "helper", cfg(test))] Hidden, Visible }
        #[cfg_attr(any(), cfg(test))] pub struct Kept;
    "#,
    )
    .unwrap();
    compile_review_library(root, &[]);
    compile_review_library_with_cfg(root, &[], &["feature=\"helper\""]);
    review(root, &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::S::1#field")
    );
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("core::E::Hidden#variant"))
    );
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::Kept#struct")
    );
    replace(root, "core", "feature = \"helper\"", "feature = \"other\"");
    validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
}

#[test]
fn enum_discriminants_used_as_array_lengths_remain_value_dependencies() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "mod hidden { pub enum Size { Len = 1, Unused = 9 } } pub struct S { pub data: [u8; hidden::Size::Len as usize] }").unwrap();
    compile_review_library(root, &[]);
    let consumer = "fn check(s: fixture::S) { let _: [u8; 1] = s.data; } fn main() {}";
    assert!(compile_review_consumer(root, consumer).status.success());
    review(root, &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::Size::Len#variant")
    );
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::Size::Unused#variant")
    );
    replace(root, "core", "Unused = 9", "Unused = 10");
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    replace(root, "core", "Len = 1", "Len = 2");
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
}

#[test]
fn anonymous_constant_assertions_do_not_create_duplicate_declarations() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        "pub struct S; const _: () = assert!(true); const _: usize = 1; const _: usize = 2;",
    )
    .unwrap();
    compile_review_library(root, &[]);
    review(root, &mut ledger);
    assert_eq!(
        ledger
            .declarations
            .iter()
            .filter(|d| d.surface == "core")
            .count(),
        1
    );
    replace(root, "core", "const _: usize = 2", "const _: usize = 3");
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn compiled_bare_const_arguments_retain_constants_and_selecting_imports() {
    for (dependency, exposed, expected) in [
        (
            "const N: usize = 1;",
            "pub struct S { pub bytes: Bytes<N> }",
            "core::N#constant",
        ),
        (
            "mod hidden { pub(crate) const LEN: usize = 1; } use hidden::LEN as N;",
            "pub struct S { pub bytes: Bytes<N> }",
            "core::hidden::LEN#constant",
        ),
        (
            "const N: usize = 1; type Value = Bytes<N>;",
            "pub struct S { pub bytes: Value }",
            "core::N#constant",
        ),
        (
            "const N: usize = 1;",
            "pub fn bytes() -> Bytes<N> { Bytes([0; N]) }",
            "core::N#constant",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!("{dependency} pub struct Bytes<const SIZE: usize>(pub [u8; SIZE]); {exposed}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        let consumer = if exposed.contains("pub fn") {
            "fn main() { let _: [u8; 1] = fixture::bytes().0; }"
        } else {
            "fn check(s: fixture::S) { let _: [u8; 1] = s.bytes.0; } fn main() {}"
        };
        assert!(compile_review_consumer(root, consumer).status.success());
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == format!("core|{expected}"))
        );
        replace(root, "core", "usize = 1", "usize = 2");
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        let checked = check_review_fixture(root);
        assert!(!checked.status.success());
        assert!(String::from_utf8_lossy(&checked.stderr).contains(expected));
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn compiled_macros_in_audited_types_and_constant_expressions_fail_explicitly() {
    for exposed in [
        "pub struct S { pub data: [u8; len!()] }",
        "pub struct S(pub bytes!());",
        "pub struct S<T = bytes!()>(pub T);",
        "type Value = [u8; len!()]; pub struct S(pub Value);",
        "const N: usize = len!(); pub struct S(pub [u8; N]);",
        "const N: usize = len!(); pub struct Bytes<const SIZE: usize>(pub [u8; SIZE]); pub struct S(pub Bytes<N>);",
        "pub struct S; impl S { const N: usize = len!(); pub fn bytes(&self) -> [u8; Self::N] { [0; Self::N] } }",
        "pub struct S; impl S { pub const N: usize = len!(); }",
        "pub enum E { N = len!() }",
        "pub const N: usize = len!();",
        "pub const NAME: &str = stringify!(S);",
        "pub static DATA: [u8; 1] = [0; len!()];",
        "pub fn bytes() -> [u8; len!()] { [0; 1] }",
    ] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        let source = format!(
            "#[macro_use] mod hidden {{ macro_rules! len {{ () => {{ 1 }}; }} macro_rules! bytes {{ () => {{ [u8; 1] }}; }} }} {exposed}"
        );
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[]);
        let error = inventory(root, &ledger).unwrap_err();
        assert!(
            error.contains("unresolved macro dependency")
                && error.contains("audited declaration core|core::"),
            "{error}"
        );
    }
}

#[test]
fn unreachable_expression_macros_do_not_require_dependency_resolution() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        "pub struct S; pub fn run() {}",
    )
    .unwrap();
    review(root, &mut ledger);
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        #[macro_use] mod hidden {
            macro_rules! len { () => { 1 }; }
            pub struct Unused(pub [u8; len!()]);
        }
        const UNUSED: usize = len!();
        pub struct S;
        impl S { const UNUSED: usize = len!(); }
        pub fn run() { let _ = len!(); }
        fn private() { let _ = len!(); }
        #[cfg_attr(all(), cfg(test))] pub struct Test(pub [u8; missing!()]);
    ",
    )
    .unwrap();
    compile_review_library(root, &[]);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn bare_generic_arguments_respect_known_parameter_scopes() {
    for source in [
        "const N: usize = 1; pub struct Boxed<T>(pub T); pub struct S<N>(pub Boxed<N>);",
        "const N: usize = 1; pub struct Bytes<const SIZE: usize>(pub [u8; SIZE]); pub struct S<const N: usize>(pub Bytes<N>);",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.id == "core|core::N#constant")
        );
        replace(root, "core", "usize = 1", "usize = 2");
        assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    }
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        "type N = u16; pub struct Boxed<T>(pub T); pub struct S<const N: usize>(pub Boxed<N>);",
    )
    .unwrap();
    compile_review_library(root, &[]);
    review(root, &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::N#alias")
    );
    replace(root, "core", "N = u16", "N = u32");
    compile_review_library(root, &[]);
    validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
}

#[test]
fn compiled_private_macro_mutations_cannot_pass_check() {
    let (directory, ledger) = fixture();
    let root = directory.path();
    let original = save_ffi_fixture_review(root, &ledger);
    for length in [1, 2] {
        fs::write(root.join("core/src/lib.rs"), format!(
            "#[macro_use] mod hidden {{ macro_rules! len {{ () => {{ {length} }}; }} }} pub struct S(pub [u8; len!()]);"
        )).unwrap();
        compile_review_library(root, &[]);
        let checked = check_review_fixture(root);
        assert!(!checked.status.success());
        let error = String::from_utf8_lossy(&checked.stderr);
        assert!(
            error.contains("unresolved macro dependency `len ! ()`"),
            "{error}"
        );
    }
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn compiled_supplemented_invocations_retain_private_macro_use_definitions() {
    for (source, file, expected) in [
        (
            "#[macro_use] mod hidden { macro_rules! make { () => { pub struct Options { pub value: u16 } }; } } make!();",
            None,
            "core::hidden::@macro:make",
        ),
        (
            "#[macro_use] mod hidden; make!();",
            Some(
                "#[macro_use] mod inner { macro_rules! make { () => { pub struct Options { pub value: u16 } }; } }",
            ),
            "core::hidden::inner::@macro:make",
        ),
        (
            "#[macro_use] mod hidden { macro_rules! helper { () => { pub struct Options { pub value: u16 } }; } macro_rules! make { () => { helper!(); }; } } make!();",
            None,
            "core::hidden::@macro:helper",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        // Review the actual generated declarations, as well as the generators.
        fs::write(
            root.join("core/src/lib.rs"),
            "pub struct Options { pub value: u16 }",
        )
        .unwrap();
        let generated: Vec<_> = discover_sources(root, &ledger)
            .unwrap()
            .into_iter()
            .filter(|d| d.surface == "core")
            .collect();
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        if let Some(file) = file {
            fs::write(root.join("core/src/hidden.rs"), file).unwrap();
        }
        compile_review_library(root, &[]);
        let consumer = "fn check(o: fixture::Options) { let _: u16 = o.value; } fn main() {}";
        assert!(compile_review_consumer(root, consumer).status.success());
        assert!(
            inventory(root, &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
        review_fixture_macro_inputs(root, &mut ledger);
        assert!(
            ledger
                .supplements
                .iter()
                .any(|s| s.anchor.contains(expected))
        );
        ledger
            .supplements
            .iter_mut()
            .find(|s| s.anchor == "core|core::@macro:make!()#macro")
            .unwrap()
            .declarations = generated;
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success());
        let path = root.join(if file.is_some() {
            "core/src/hidden.rs"
        } else {
            "core/src/lib.rs"
        });
        let original_source = fs::read_to_string(&path).unwrap();
        fs::write(
            path,
            original_source.replace("pub value: u16", "pub value: u32"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        let checked = check_review_fixture(root);
        assert!(!checked.status.success());
        let error = String::from_utf8_lossy(&checked.stderr);
        assert!(
            error.contains("supplement") && error.contains(expected),
            "{error}"
        );
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn inherited_and_associated_item_macros_keep_their_lexical_inputs() {
    for exposed in [
        "pub mod api { make!(); }",
        "pub struct Options; impl Options { make!(); }",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), format!(
            "#[macro_use] mod hidden {{ macro_rules! make {{ () => {{ pub const VALUE: u16 = 0; }}; }} }} {exposed}"
        )).unwrap();
        compile_review_library(root, &[]);
        review_fixture_macro_inputs(root, &mut ledger);
        assert!(
            ledger
                .supplements
                .iter()
                .any(|s| s.anchor == "core|core::hidden::@macro:make#macro")
        );
        replace(root, "core", "VALUE: u16", "VALUE: u32");
        compile_review_library(root, &[]);
        assert!(inventory(root, &ledger).unwrap_err().contains("supplement"));
    }
}

#[test]
fn macro_use_import_order_remains_a_review_input() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let left =
        "#[macro_use] mod left { macro_rules! make { () => { pub struct Options(pub u16); }; } }";
    let right =
        "#[macro_use] mod right { macro_rules! make { () => { pub struct Options(pub u32); }; } }";
    fs::write(
        root.join("core/src/lib.rs"),
        format!("{left} {right} make!();"),
    )
    .unwrap();
    compile_review_library(root, &[]);
    let consumer = "fn check(o: fixture::Options) { let _: u32 = o.0; } fn main() {}";
    assert!(compile_review_consumer(root, consumer).status.success());
    review_fixture_macro_inputs(root, &mut ledger);
    fs::write(
        root.join("core/src/lib.rs"),
        format!("{right} {left} make!();"),
    )
    .unwrap();
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    assert!(inventory(root, &ledger).unwrap_err().contains("supplement"));
}

#[test]
fn macro_use_keeps_invocations_that_generate_imported_macros() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), r"
        macro_rules! emit { ($ty:ty) => { macro_rules! make { () => { pub struct Options(pub $ty); }; } }; }
        #[macro_use] mod hidden { emit!(u16); }
        make!();
    ").unwrap();
    compile_review_library(root, &[]);
    let consumer = "fn check(o: fixture::Options) { let _: u16 = o.0; } fn main() {}";
    assert!(compile_review_consumer(root, consumer).status.success());
    review_fixture_macro_inputs(root, &mut ledger);
    assert!(
        ledger
            .supplements
            .iter()
            .any(|s| s.anchor.contains("hidden::@macro:emit!(u16)"))
    );
    replace(root, "core", "emit!(u16)", "emit!(u32)");
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    assert!(inventory(root, &ledger).unwrap_err().contains("supplement"));
}

#[test]
fn macro_dependencies_exclude_unimported_modules_and_known_test_only_inputs() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let source = r"
        #[cfg_attr(all(), macro_use)] mod hidden {
            macro_rules! make { () => { pub struct Options; }; }
        }
        make!();
    ";
    fs::write(root.join("core/src/lib.rs"), source).unwrap();
    compile_review_library(root, &[]);
    review_fixture_macro_inputs(root, &mut ledger);
    fs::write(root.join("core/src/lib.rs"), source.replace(
        "macro_rules! make",
        "#[cfg_attr(all(), cfg(test))] macro_rules! test_only { () => {}; } struct Helper; fn helper() {} macro_rules! make"
    ) + r"
        mod unrelated { macro_rules! make { () => { pub struct Unused; }; } }
        #[cfg_attr(all(), cfg(test))] #[macro_use] mod missing;
    ").unwrap();
    compile_review_library(root, &[]);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn conditional_macro_use_dependencies_keep_predicates_and_module_generators() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        #[cfg(feature = "generated")] #[macro_use] #[rewrite] mod hidden {
            macro_rules! make { () => { pub struct Options; }; }
        }
        #[cfg(feature = "generated")] make!();
        pub struct Existing;
    "#,
    )
    .unwrap();
    review_fixture_macro_inputs(root, &mut ledger);
    for name in [
        "@macro:macro_use:hidden",
        "hidden::@macro:make",
        "hidden::@macro:attribute:module:rewrite",
    ] {
        let anchor = ledger
            .declarations
            .iter()
            .find(|d| d.id.contains(name))
            .unwrap();
        assert!(
            anchor
                .conditions
                .contains(&"cfg (feature = \"generated\")".into())
        );
    }
    replace(
        root,
        "core",
        "mod hidden {",
        "mod hidden { struct PrivateInput;",
    );
    assert!(inventory(root, &ledger).unwrap_err().contains("supplement"));
}

#[test]
fn compiled_ordinary_macro_imports_retain_definitions_and_selecting_routes() {
    for route in [
        "use hidden::make; make!();",
        "use hidden::make as build; build!();",
        "use hidden::*; make!();",
        "use hidden as factory; factory::make!();",
        "mod middle { pub(crate) use crate::hidden::make as build; } use middle::build; build!();",
        "hidden::make!();",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!(
                r"
            mod hidden {{
                macro_rules! make {{ () => {{ pub struct Options {{ pub value: u16 }} }}; }}
                pub(crate) use make;
                macro_rules! unused {{ () => {{ pub struct Unused; }}; }}
            }}
            {route}
        "
            ),
        )
        .unwrap();
        compile_review_library(root, &[]);
        let consumer = "fn check(o: fixture::Options) { let _: u16 = o.value; } fn main() {}";
        assert!(compile_review_consumer(root, consumer).status.success());
        review_fixture_macro_inputs(root, &mut ledger);
        assert!(
            ledger
                .supplements
                .iter()
                .any(|s| s.anchor == "core|core::hidden::@macro:make#macro")
        );
        assert!(
            !ledger
                .supplements
                .iter()
                .any(|s| s.anchor.contains("@macro:unused"))
        );
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id.contains("hidden::@use:make#reexport"))
        );
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success());
        replace(root, "core", "pub value: u16", "pub value: u32");
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        let checked = check_review_fixture(root);
        assert!(!checked.status.success());
        assert!(String::from_utf8_lossy(&checked.stderr).contains("core::hidden::@macro:make"));
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn macro_import_retargeting_and_conditions_require_review() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), r#"
        mod left { macro_rules! make { () => { pub struct Options(pub u16); }; } pub(crate) use make; }
        mod right { macro_rules! make { () => { pub struct Options(pub u32); }; } pub(crate) use make; }
        #[cfg(not(feature = "wide"))] use left::make;
        #[cfg(feature = "wide")] use right::make;
        make!();
    "#).unwrap();
    compile_review_library(root, &[]);
    compile_review_library_with_cfg(root, &[], &["feature=\"wide\""]);
    review_fixture_macro_inputs(root, &mut ledger);
    for name in ["left", "right"] {
        assert!(
            ledger
                .supplements
                .iter()
                .any(|s| s.anchor.contains(&format!("{name}::@macro:make")))
        );
    }
    save_ffi_fixture_review(root, &ledger);
    replace(root, "core", "use left::make", "use right::make");
    compile_review_library(root, &[]);
    assert!(!check_review_fixture(root).status.success());
}

#[test]
fn compiled_restricted_modules_from_internal_globs_retain_public_members() {
    for visibility in ["pub(crate)", "pub(super)", "pub(in crate)"] {
        for imports in [
            "use a::*;",
            "mod gateway { pub(crate) use crate::a::*; } use gateway::*;",
        ] {
            let (directory, mut ledger) = fixture();
            let root = directory.path();
            fs::write(root.join("core/src/lib.rs"), format!(
                "mod a {{ {visibility} mod b {{ pub struct V {{ pub x: u16 }} }} }} {imports} pub fn value() -> b::V {{ b::V {{ x: 1 }} }}"
            )).unwrap();
            compile_review_library(root, &[]);
            let consumer = "fn main() { let _: u16 = fixture::value().x; }";
            assert!(compile_review_consumer(root, consumer).status.success());
            review(root, &mut ledger);
            assert!(
                ledger
                    .declarations
                    .iter()
                    .any(|d| d.id == "core|core::a::b::V::x#field")
            );
            assert!(
                !ledger
                    .declarations
                    .iter()
                    .any(|d| d.id == "core|core::a::b#module")
            );
            let original = save_ffi_fixture_review(root, &ledger);
            replace(root, "core", "pub x: u16", "pub x: u32");
            compile_review_library(root, &[]);
            assert!(!compile_review_consumer(root, consumer).status.success());
            let checked = check_review_fixture(root);
            assert!(!checked.status.success());
            assert!(String::from_utf8_lossy(&checked.stderr).contains("core::a::b::V::x#field"));
            assert_eq!(
                fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
                original
            );
        }
    }
}

#[test]
fn compiled_private_tuple_constructors_do_not_shadow_exported_glob_values() {
    for field in ["u16", "pub(self) u16", "pub(in crate::types) u16"] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), format!(
            "mod types {{ pub struct S({field}); }} pub use types::S; mod values {{ pub const S: u16 = 1; }} pub use values::*;"
        )).unwrap();
        compile_review_library(root, &[]);
        let consumer = "fn main() { let _: u16 = fixture::S; let _: Option<fixture::S> = None; }";
        assert!(compile_review_consumer(root, consumer).status.success());
        review(root, &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == "core|core::values::S#constant")
        );
        let original = save_ffi_fixture_review(root, &ledger);
        replace(root, "core", "const S: u16", "const S: u32");
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        let checked = check_review_fixture(root);
        assert!(!checked.status.success());
        assert!(String::from_utf8_lossy(&checked.stderr).contains("core::values::S#constant"));
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn constructor_visibility_respects_local_and_restricted_access() {
    for source in [
        "mod types { pub struct S(u16); pub mod child { use super::*; pub fn make() -> S { S(1) } } } pub use types::child::make;",
        "mod types { pub struct S(pub(crate) u16); } use types::*; pub fn make() -> S { S(1) }",
        "mod types { pub struct S(pub(super) u16); } use types::S; pub fn make() -> S { S(1) }",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == "core|core::types::S#struct")
        );
    }
}

#[test]
fn conditional_tuple_fields_preserve_value_exports_only_where_constructor_is_private() {
    for (types, constructor) in [
        (
            "pub struct S(#[cfg(feature = \"hidden\")] u16);",
            "fixture::S()",
        ),
        (
            "#[cfg(feature = \"hidden\")] pub struct S(u16); #[cfg(not(feature = \"hidden\"))] pub struct S(pub u16);",
            "fixture::S(1)",
        ),
        (
            "#[cfg(not(feature = \"hidden\"))] pub struct S(pub u16); #[cfg(feature = \"hidden\")] pub struct S(u16);",
            "fixture::S(1)",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!(
                r"
        mod types {{ {types} }}
        pub use types::S;
        mod values {{ pub const S: u16 = 1; }}
        pub use values::*;
    "
            ),
        )
        .unwrap();
        compile_review_library(root, &[]);
        assert!(
            compile_review_consumer(
                root,
                &format!("fn main() {{ let _: fixture::S = {constructor}; }}")
            )
            .status
            .success()
        );
        compile_review_library_with_cfg(root, &[], &["feature=\"hidden\""]);
        let consumer = "fn main() { let _: u16 = fixture::S; }";
        assert!(compile_review_consumer(root, consumer).status.success());
        review(root, &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == "core|core::values::S#constant")
        );
        replace(root, "core", "const S: u16", "const S: u32");
        compile_review_library_with_cfg(root, &[], &["feature=\"hidden\""]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
    }
}

#[test]
fn associated_item_macro_imports_keep_definitions_and_attribute_generators() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        mod hidden {
            macro_rules! methods { () => { pub fn value(&self) -> u16 { 0 } }; }
            pub(crate) use methods;
        }
        use hidden::methods as make;
        pub struct Options;
        impl Options { make!(); }
    ",
    )
    .unwrap();
    compile_review_library(root, &[]);
    let consumer = "fn main() { let _: u16 = fixture::Options.value(); }";
    assert!(compile_review_consumer(root, consumer).status.success());
    review_fixture_macro_inputs(root, &mut ledger);
    assert!(
        ledger
            .supplements
            .iter()
            .any(|s| s.anchor.contains("hidden::@macro:methods"))
    );
    replace(root, "core", "-> u16", "-> u32");
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    assert!(inventory(root, &ledger).unwrap_err().contains("supplement"));

    replace(
        root,
        "core",
        "pub(crate) use methods",
        "#[rewrite] pub(crate) use methods",
    );
    review_fixture_macro_inputs(root, &mut ledger);
    assert!(
        ledger
            .supplements
            .iter()
            .any(|s| s.anchor.contains("attribute:reexport:rewrite"))
    );
    replace(root, "core", "#[rewrite]", "#[rewrite(extra)]");
    assert!(inventory(root, &ledger).unwrap_err().contains("supplement"));
}

#[test]
fn unused_macro_imports_do_not_expose_private_generators() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "pub struct Options;").unwrap();
    review(root, &mut ledger);
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        mod hidden {
            macro_rules! make { () => { pub struct Unused; }; }
            pub(crate) use make;
        }
        use hidden::make;
        pub struct Options;
    ",
    )
    .unwrap();
    compile_review_library(root, &[]);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn compiled_imported_macro_expansions_retain_qualified_private_helpers() {
    for (implementation, expansion, imports, expected) in [
        (
            "macro_rules! helper { () => { pub struct Options { pub value: u16 } }; } pub(crate) use helper;",
            "$crate::generators::implementation::helper!();",
            "",
            "implementation::@macro:helper",
        ),
        (
            "macro_rules! helper { () => { $crate::generators::implementation::leaf!(); }; } pub(crate) use helper; macro_rules! leaf { () => { pub struct Options { pub value: u16 } }; } pub(crate) use leaf;",
            "$crate::generators::implementation::helper!();",
            "",
            "implementation::@macro:leaf",
        ),
        (
            "macro_rules! leaf { () => { pub struct Options { pub value: u16 } }; } pub(crate) use leaf as helper;",
            "crate::generators::implementation::helper!();",
            "",
            "implementation::@macro:leaf",
        ),
        (
            "macro_rules! leaf { () => { pub struct Options { pub value: u16 } }; } pub(crate) use leaf as helper;",
            "helper!();",
            "use generators::implementation::helper;",
            "implementation::@macro:leaf",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            "pub struct Options { pub value: u16 }",
        )
        .unwrap();
        let outputs: Vec<_> = discover_sources(root, &ledger)
            .unwrap()
            .into_iter()
            .filter(|d| d.surface == "core")
            .collect();
        fs::write(
            root.join("core/src/lib.rs"),
            format!(
                r"
            mod generators {{
                pub(crate) mod implementation {{ {implementation} }}
                macro_rules! make {{ () => {{ {expansion} }}; }}
                pub(crate) use make;
            }}
            use generators::make;
            {imports}
            make!();
        "
            ),
        )
        .unwrap();
        compile_review_library(root, &[]);
        let consumer = "fn check(o: fixture::Options) { let _: u16 = o.value; } fn main() {}";
        assert!(compile_review_consumer(root, consumer).status.success());
        review_fixture_macro_inputs(root, &mut ledger);
        assert!(
            ledger
                .supplements
                .iter()
                .any(|s| s.anchor.contains(expected))
        );
        ledger
            .supplements
            .iter_mut()
            .find(|s| s.anchor == "core|core::@macro:make!()#macro")
            .unwrap()
            .declarations = outputs;
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success());
        replace(root, "core", "pub value: u16", "pub value: u32");
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        let checked = check_review_fixture(root);
        assert!(!checked.status.success());
        assert!(String::from_utf8_lossy(&checked.stderr).contains(expected));
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn macro_matchers_are_not_expansion_dependencies_and_dynamic_calls_fail_when_reached() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        mod unused {
            macro_rules! computed { ($callback:ident) => { $callback!(); }; }
        }
        macro_rules! make { ($callback:ident ! ()) => { pub struct Options; }; }
        make!(absent!());
    ",
    )
    .unwrap();
    compile_review_library(root, &[]);
    review_fixture_macro_inputs(root, &mut ledger);
    assert!(
        !ledger
            .supplements
            .iter()
            .any(|s| s.anchor.contains("computed"))
    );
    replace(root, "core", "$callback:ident ! ()", "$callback:ident");
    replace(
        root,
        "core",
        "pub struct Options; }; }",
        "$callback!(); }; }",
    );
    replace(root, "core", "make!(absent!())", "make!(absent)");
    let error = inventory(root, &ledger).unwrap_err();
    assert!(
        error.contains("macro-variable expansion paths") && error.contains("core::@macro:make"),
        "{error}"
    );
}

#[test]
fn compiled_foreign_item_macros_fail_the_unchanged_audit() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        macro_rules! declarations { ($name:ident) => { pub fn $name(); }; }
        pub struct Options;
    ",
    )
    .unwrap();
    compile_review_library(root, &[]);
    review_fixture_macro_inputs(root, &mut ledger);
    let original = save_ffi_fixture_review(root, &ledger);
    replace(
        root,
        "core",
        "pub struct Options;",
        "pub struct Options; unsafe extern \"C\" { declarations!(new_operation); }",
    );
    compile_review_library(root, &[]);
    assert!(
        compile_review_consumer(
            root,
            r#"
        #[unsafe(no_mangle)] pub extern "C" fn new_operation() {}
        fn main() { unsafe { fixture::new_operation(); } }
    "#
        )
        .status
        .success()
    );
    let checked = check_review_fixture(root);
    assert!(!checked.status.success());
    let error = String::from_utf8_lossy(&checked.stderr);
    assert!(
        error.contains("unsupported foreign-item macro") && error.contains("new_operation"),
        "{error}"
    );
    assert_eq!(
        fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
        original
    );
}

#[test]
fn foreign_macro_errors_follow_reachable_scopes_and_preserve_test_filtering() {
    for (visibility, export) in [
        ("", "pub use hidden::*;"),
        ("", "pub use hidden::operation;"),
        ("pub", "pub use hidden as api;"),
    ] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!(
                r#"
            macro_rules! declarations {{ ($name:ident) => {{ pub fn $name(); }}; }}
            {visibility} mod hidden {{ unsafe extern "C" {{ declarations!(operation); }} }}
            {export}
            pub struct Options;
        "#
            ),
        )
        .unwrap();
        compile_review_library(root, &[]);
        let error = inventory(root, &ledger).unwrap_err();
        assert!(
            error.contains("unsupported foreign-item macro") && error.contains("core::hidden"),
            "{error}"
        );
    }
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        macro_rules! declarations { ($name:ident) => { pub fn $name(); }; }
        macro_rules! nothing { () => {}; }
        pub struct Options;
        mod hidden { unsafe extern "C" { declarations!(unreachable); nothing!(); nothing!(); } }
        unsafe extern "C" { #[cfg_attr(all(), cfg(test))] declarations!(test_only); }
        #[cfg(test)] unsafe extern "C" { declarations!(test_block); }
    "#,
    )
    .unwrap();
    compile_review_library(root, &[]);
    review_fixture_macro_inputs(root, &mut ledger);
    replace(root, "core", "mod hidden", "pub mod hidden");
    assert!(
        inventory(root, &ledger)
            .unwrap_err()
            .contains("unsupported foreign-item macro")
    );
}

#[test]
fn computed_macro_path_repetitions_cannot_hide_local_helpers() {
    for (matcher, expansion, arguments) in [
        ("$callback:ident", "$callback!();", "helper"),
        (
            "$callback:ident $arguments:tt",
            "$callback! $arguments;",
            "helper ()",
        ),
        ("$($part:ident)::*", "$($part)::*!();", "helper"),
        ("$($part:ident ::)*", "$($part ::)*helper!();", "helpers::"),
    ] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), format!(r"
            mod helpers {{ macro_rules! helper {{ () => {{ pub struct Options; }}; }} pub(crate) use helper; }}
            use helpers::helper;
            macro_rules! make {{ ({matcher}) => {{ {expansion} }}; }}
            make!({arguments});
        ")).unwrap();
        compile_review_library(root, &[]);
        let error = discover_sources(root, &ledger).unwrap_err();
        assert!(error.contains("macro-variable expansion paths"), "{error}");
    }
}

#[test]
fn foreign_macros_in_selected_modules_and_symbolic_branches_are_rejected() {
    for predicate in ["", "#[cfg(feature = \"generated\")]"] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!(
                r#"
            macro_rules! declarations {{ ($name:ident) => {{ pub fn $name(); }}; }}
            mod hidden {{ unsafe extern "C" {{ {predicate} declarations!(operation); }} }}
            pub struct Options;
        "#
            ),
        )
        .unwrap();
        compile_review_library(root, &[]);
        ledger
            .surfaces
            .iter_mut()
            .find(|s| s.name == "core")
            .unwrap()
            .roots = vec!["core::hidden".into()];
        assert!(
            inventory(root, &ledger)
                .unwrap_err()
                .contains("unsupported foreign-item macro")
        );
    }
}

#[test]
fn foreign_macro_rejection_includes_scopes_reached_through_macro_dependencies() {
    let (directory, ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        macro_rules! declarations { ($name:ident) => { pub fn $name(); }; }
        mod helpers {
            unsafe extern "C" { declarations!(operation); }
            macro_rules! make { () => { pub use $crate::helpers::operation; }; }
            pub(crate) use make;
        }
        use helpers::make;
        make!();
    "#,
    )
    .unwrap();
    compile_review_library(root, &[]);
    let error = discover_sources(root, &ledger).unwrap_err();
    assert!(
        error.contains("unsupported foreign-item macro") && error.contains("core::helpers"),
        "{error}"
    );
}

#[test]
fn literal_macro_helpers_with_forwarded_token_groups_remain_dependencies() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        mod helpers {
            macro_rules! helper { () => { pub struct Options(pub u16); }; }
            pub(crate) use helper;
        }
        macro_rules! make { ($arguments:tt) => { $crate::helpers::helper! $arguments; }; }
        make!(());
    ",
    )
    .unwrap();
    compile_review_library(root, &[]);
    review_fixture_macro_inputs(root, &mut ledger);
    assert!(
        ledger
            .supplements
            .iter()
            .any(|s| s.anchor.contains("helpers::@macro:helper"))
    );
    replace(root, "core", "pub u16", "pub u32");
    compile_review_library(root, &[]);
    assert!(inventory(root, &ledger).unwrap_err().contains("supplement"));
}

fn build_review_dependency(
    root: &Path,
    name: &str,
    source: &str,
    kind: &str,
) -> std::path::PathBuf {
    let file = root.join(format!("{name}.rs"));
    fs::write(&file, source).unwrap();
    let library = if kind == "proc-macro" {
        root.join(format!(
            "{}{name}{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ))
    } else {
        root.join(format!("lib{name}.rlib"))
    };
    let built =
        std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args(["--edition=2024", "--crate-type", kind, "--crate-name", name])
            .arg(file)
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    library
}

#[test]
fn compiled_macro_use_crate_retargeting_invalidates_invocation_reviews() {
    for (invocation_first, alias) in [(false, ""), (true, ""), (false, " as support")] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let externs: Vec<_> = [("first", "u16"), ("second", "u32")].into_iter().map(|(name, ty)| {
            let source = format!("#[macro_export] macro_rules! make {{ () => {{ pub fn generated() -> {ty} {{ 0 }} }}; }}");
            (name, build_review_dependency(root, name, &source, "lib"))
        }).collect();
        fs::write(
            root.join("core/src/lib.rs"),
            "pub fn generated() -> u16 { 0 }",
        )
        .unwrap();
        let outputs: Vec<_> = discover_sources(root, &ledger)
            .unwrap()
            .into_iter()
            .filter(|d| d.surface == "core")
            .collect();
        let source = if invocation_first {
            format!("make!(); #[macro_use] extern crate first{alias};")
        } else {
            format!("#[macro_use] extern crate first{alias}; make!();")
        };
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &externs);
        let consumer = "fn main() { let _: u16 = fixture::generated(); }";
        assert!(compile_review_consumer(root, consumer).status.success());
        review_fixture_macro_inputs(root, &mut ledger);
        let name = if alias.is_empty() { "first" } else { "support" };
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == format!("core|core::{name}#extern_crate"))
        );
        ledger
            .supplements
            .iter_mut()
            .find(|s| s.anchor == "core|core::@macro:make!()#macro")
            .unwrap()
            .declarations = outputs;
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        replace(root, "core", "extern crate first", "extern crate second");
        compile_review_library(root, &externs);
        assert!(!compile_review_consumer(root, consumer).status.success());
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn macro_use_crate_order_and_conditions_are_selecting_inputs() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs: Vec<_> = [("first", "u16"), ("second", "u32")].into_iter().map(|(name, ty)| {
        (name, build_review_dependency(root, name, &format!("#[macro_export] macro_rules! make {{ () => {{ pub fn generated() -> {ty} {{ 0 }} }}; }}"), "lib"))
    }).collect();
    let first = "#[macro_use] extern crate first;";
    let second = "#[macro_use] extern crate second;";
    fs::write(
        root.join("core/src/lib.rs"),
        format!("{first} {second} make!();"),
    )
    .unwrap();
    compile_review_library(root, &externs);
    let consumer = "fn main() { let _: u32 = fixture::generated(); }";
    assert!(compile_review_consumer(root, consumer).status.success());
    review_fixture_macro_inputs(root, &mut ledger);
    save_ffi_fixture_review(root, &ledger);
    fs::write(
        root.join("core/src/lib.rs"),
        format!("{second} {first} make!();"),
    )
    .unwrap();
    compile_review_library(root, &externs);
    assert!(!compile_review_consumer(root, consumer).status.success());
    assert!(!check_review_fixture(root).status.success());
    fs::write(
        root.join("core/src/lib.rs"),
        format!(
            "#[cfg(not(feature = \"wide\"))] {first} #[cfg(feature = \"wide\")] {second} make!();"
        ),
    )
    .unwrap();
    compile_review_library(root, &externs);
    compile_review_library_with_cfg(root, &externs, &["feature=\"wide\""]);
    review_fixture_macro_inputs(root, &mut ledger);
    for name in ["first", "second"] {
        assert!(ledger.declarations.iter().any(|d| {
            d.id.starts_with(&format!("core|core::{name}#extern_crate")) && !d.conditions.is_empty()
        }));
    }
}

#[test]
fn selective_macro_use_names_preserve_builtin_derives_and_raw_invocations() {
    for names in ["make", ""] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let library = build_review_dependency(
            root,
            "first",
            "#[macro_export] macro_rules! make { () => { pub fn generated() -> u16 { 0 } }; }",
            "lib",
        );
        let externs = [("first", library)];
        fs::write(
            root.join("core/src/lib.rs"),
            format!(
                "#[macro_use({names})] extern crate first; #[derive(Clone)] pub struct Options;"
            ),
        )
        .unwrap();
        compile_review_library(root, &externs);
        review(root, &mut ledger);
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| matches!(d.kind.as_str(), "macro" | "extern_crate"))
        );
        if !names.is_empty() {
            replace(
                root,
                "core",
                "pub struct Options;",
                "pub struct Options; r#make!();",
            );
            compile_review_library(root, &externs);
            review_fixture_macro_inputs(root, &mut ledger);
            assert!(
                ledger
                    .declarations
                    .iter()
                    .any(|d| d.id == "core|core::first#extern_crate")
            );
        }
    }
}

fn build_review_derives(root: &Path) -> std::path::PathBuf {
    build_review_dependency(
        root,
        "generators",
        r#"
        extern crate proc_macro;
        #[proc_macro_derive(Api)]
        pub fn api(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            let ty = if input.to_string().contains("wide") { "u32" } else { "u16" };
            format!("impl Options {{ pub fn generated() -> {ty} {{ 0 }} }}").parse().unwrap()
        }
        #[proc_macro_derive(Other)]
        pub fn other(_: proc_macro::TokenStream) -> proc_macro::TokenStream {
            "impl Options { pub fn generated() -> u32 { 0 } }".parse().unwrap()
        }
    "#,
        "proc-macro",
    )
}

#[test]
fn compiled_derive_aliases_cannot_impersonate_trusted_builtin_names() {
    for (imports, attribute) in [
        ("use generators::Api as Clone;", "#[derive(Clone)]"),
        (
            "mod aliases { pub(crate) use generators::Api as Clone; } use aliases::Clone;",
            "#[derive(Clone)]",
        ),
        (
            "mod aliases { pub(crate) use generators::Api as Clone; } use aliases::*;",
            "#[derive(crate::Clone)]",
        ),
        (
            "mod serde { pub(crate) use generators::Api as Serialize; }",
            "#[derive(serde::Serialize)]",
        ),
        (
            "mod replacements { pub mod serde { pub(crate) use generators::Api as Serialize; } } use replacements::*;",
            "#[derive(serde::Serialize)]",
        ),
        (
            "use generators::Api as Clone;",
            "#[cfg_attr(all(), derive(Clone))]",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let externs = [("generators", build_review_derives(root))];
        fs::write(
            root.join("core/src/lib.rs"),
            format!("{imports} {attribute} pub struct Options;"),
        )
        .unwrap();
        compile_review_library(root, &externs);
        let consumer = "fn main() { let _: u16 = fixture::Options::generated(); }";
        assert!(compile_review_consumer(root, consumer).status.success());
        assert!(
            inventory(root, &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
        review_fixture_macro_inputs(root, &mut ledger);
        assert_eq!(ledger.supplements.len(), 1);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.kind == "reexport" && d.shape.contains("generators::Api"))
        );
        let original = save_ffi_fixture_review(root, &ledger);
        replace(root, "core", "generators::Api", "generators::Other");
        compile_review_library(root, &externs);
        assert!(!compile_review_consumer(root, consumer).status.success());
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn imported_builtin_named_derives_review_full_private_inputs() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_derives(root))];
    fs::write(
        root.join("core/src/lib.rs"),
        "use generators::Api as Clone; #[derive(Clone)] pub struct Options { private: () }",
    )
    .unwrap();
    compile_review_library(root, &externs);
    let consumer = "fn main() { let _: u16 = fixture::Options::generated(); }";
    assert!(compile_review_consumer(root, consumer).status.success());
    review_fixture_macro_inputs(root, &mut ledger);
    replace(root, "core", "private: ()", "wide: ()");
    compile_review_library(root, &externs);
    assert!(!compile_review_consumer(root, consumer).status.success());
    assert!(inventory(root, &ledger).unwrap_err().contains("supplement"));
}

#[test]
fn actual_builtin_derives_keep_their_exemption_after_import_resolution() {
    for imports in [
        "",
        "use std::clone::Clone;",
        "mod types { pub trait Clone {} } use types::Clone;",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!("{imports} #[derive(Clone)] pub struct Options;"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        review(root, &mut ledger);
        assert!(!ledger.declarations.iter().any(|d| d.kind == "macro"));
        assert!(
            compile_review_consumer(
                root,
                "fn main() { let _: fixture::Options = fixture::Options.clone(); }"
            )
            .status
            .success()
        );
    }
}

#[test]
fn derive_bindings_include_later_imports_and_macro_use_providers() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_derives(root))];
    fs::write(
        root.join("core/src/lib.rs"),
        "#[derive(Clone)] pub struct Options; use generators::Api as Clone;",
    )
    .unwrap();
    compile_review_library(root, &externs);
    assert!(
        inventory(root, &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
    let provider = build_review_dependency(
        root,
        "provider",
        r#"
        extern crate proc_macro;
        #[proc_macro_derive(Clone)]
        pub fn custom_clone(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
            let ty = if input.to_string().contains("wide") { "u32" } else { "u16" };
            format!("impl Options {{ pub fn generated() -> {ty} {{ 0 }} }}").parse().unwrap()
        }
    "#,
        "proc-macro",
    );
    let externs = [("provider", provider)];
    fs::write(
        root.join("core/src/lib.rs"),
        "#[macro_use] extern crate provider; #[derive(Clone)] pub struct Options { private: () }",
    )
    .unwrap();
    compile_review_library(root, &externs);
    let consumer = "fn main() { let _: u16 = fixture::Options::generated(); }";
    assert!(compile_review_consumer(root, consumer).status.success());
    review_fixture_macro_inputs(root, &mut ledger);
    replace(root, "core", "private: ()", "wide: ()");
    compile_review_library(root, &externs);
    assert!(!compile_review_consumer(root, consumer).status.success());
    assert!(inventory(root, &ledger).unwrap_err().contains("supplement"));
}

#[test]
fn compiled_implicit_variant_values_retain_preceding_discriminant_dependencies() {
    for (source, before, after, value, expected) in [
        (
            "mod hidden { #[repr(u8)] pub enum E { A = 1, B } } pub use hidden::E::B;",
            "A = 1",
            "A = 2",
            2,
            "hidden::E::A#discriminant",
        ),
        (
            "type Number = u8; const BASE: isize = Number::BITS as isize; mod hidden { pub enum E { A = super::BASE, B } } pub use hidden::E::B;",
            "Number = u8",
            "Number = u16",
            9,
            "core::Number#alias",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[]);
        let consumer =
            format!("const _: [(); {value}] = [(); fixture::B as usize]; fn main() {{}}");
        assert!(compile_review_consumer(root, &consumer).status.success());
        review(root, &mut ledger);
        assert!(ledger.declarations.iter().any(|d| d.id.contains(expected)));
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.id.contains("E::A#variant"))
        );
        let original = save_ffi_fixture_review(root, &ledger);
        replace(root, "core", before, after);
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, &consumer).status.success());
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn implicit_discriminants_preserve_conditional_predecessor_order() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let a = "#[cfg(feature = \"a\")] A = 1,";
    let c = "#[cfg(feature = \"c\")] C = 5,";
    let source = |a: &str, c: &str| {
        format!("mod hidden {{ #[repr(u8)] pub enum E {{ {a} {c} B }} }} pub use hidden::E::B;")
    };
    fs::write(root.join("core/src/lib.rs"), source(a, c)).unwrap();
    compile_review_library_with_cfg(root, &[], &["feature=\"a\"", "feature=\"c\""]);
    let consumer = "const _: [(); 6] = [(); fixture::B as usize]; fn main() {}";
    assert!(compile_review_consumer(root, consumer).status.success());
    review(root, &mut ledger);
    assert_eq!(
        ledger
            .declarations
            .iter()
            .filter(|d| d.kind == "discriminant")
            .count(),
        2
    );
    fs::write(root.join("core/src/lib.rs"), source(c, a)).unwrap();
    compile_review_library_with_cfg(root, &[], &["feature=\"a\"", "feature=\"c\""]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
}

#[test]
fn discriminant_dependencies_stop_at_explicit_resets_and_filter_test_helpers() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        "mod hidden { pub enum E { Unused = 1, A = 5, B } } pub use hidden::E::B;",
    )
    .unwrap();
    review(root, &mut ledger);
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id.contains("::Unused#"))
    );
    replace(root, "core", "Unused = 1", "Unused = 2");
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    replace(
        root,
        "core",
        "A = 5",
        "#[cfg_attr(all(), cfg(test))] TestHelper, A = 5",
    );
    compile_review_library(root, &[]);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    fs::write(root.join("core/src/lib.rs"), "pub enum E { A = 1, B, C }").unwrap();
    review(root, &mut ledger);
    assert!(!ledger.declarations.iter().any(|d| d.kind == "discriminant"));
}

fn build_review_attributes(root: &Path) -> std::path::PathBuf {
    build_review_dependency(
        root,
        "generators",
        r#"
        extern crate proc_macro;
        use proc_macro::TokenStream;
        #[proc_macro_attribute]
        pub fn Api(arguments: TokenStream, item: TokenStream) -> TokenStream {
            let arguments = arguments.to_string();
            let output = match arguments.as_str() {
                "member" => format!("{item} pub fn generated(&self) -> u16 {{ 0 }}"),
                "trait_member" => format!("{item} fn generated(&self) -> u16 {{ 0 }}"),
                "function" => format!("{item} pub fn generated() -> u16 {{ 0 }}"),
                target => {
                    let target = if target.is_empty() { "Options" } else { target };
                    format!("{item} impl {target} {{ pub fn generated(&self) -> u16 {{ 0 }} }}")
                }
            };
            output.parse().unwrap()
        }
        #[proc_macro_attribute]
        pub fn Other(_: TokenStream, item: TokenStream) -> TokenStream { item }
    "#,
        "proc-macro",
    )
}

#[test]
fn compiled_attribute_retargeting_invalidates_explicitly_recorded_generated_outputs() {
    for (imports, attribute) in [
        ("use generators::Api as expose;", "#[expose]"),
        ("use generators::Api as expose;", "#[r#expose]"),
        ("use generators::Api as r#expose;", "#[expose]"),
        ("use ::generators::Api as expose;", "#[expose]"),
        (
            "mod bindings { pub(crate) use generators::Api as Attr; } use bindings::Attr as expose;",
            "#[expose]",
        ),
        (
            "mod bindings { pub(crate) use generators::Api as expose; } use bindings::*;",
            "#[expose]",
        ),
        (
            "mod bindings { pub(crate) use generators::Api as Attr; } use bindings as chosen;",
            "#[chosen::Attr]",
        ),
        (
            "mod bindings { pub(crate) use generators::Api as Attr; } use bindings as chosen;",
            "#[r#chosen::r#Attr]",
        ),
        (
            "use generators::Api as expose;",
            "#[cfg_attr(all(), cfg_attr(all(), expose))]",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let externs = [("generators", build_review_attributes(root))];
        fs::write(
            root.join("core/src/lib.rs"),
            "pub struct Options; impl Options { pub fn generated(&self) -> u16 { 0 } }",
        )
        .unwrap();
        let outputs: Vec<_> = discover_sources(root, &ledger)
            .unwrap()
            .into_iter()
            .filter(|d| d.surface == "core" && d.kind == "method")
            .collect();
        fs::write(
            root.join("core/src/lib.rs"),
            format!("{imports} {attribute} pub struct Options;"),
        )
        .unwrap();
        compile_review_library(root, &externs);
        let consumer = "fn main() { let _: u16 = fixture::Options.generated(); }";
        assert!(compile_review_consumer(root, consumer).status.success());
        review_fixture_macro_inputs(root, &mut ledger);
        assert_eq!(ledger.supplements.len(), 1);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.kind == "reexport" && d.shape.contains("generators::Api"))
        );
        ledger.supplements[0].declarations = outputs;
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success());
        replace(root, "core", "generators::Api", "generators::Other");
        compile_review_library(root, &externs);
        assert!(!compile_review_consumer(root, consumer).status.success());
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn attribute_providers_resolve_in_the_source_scope_of_each_item_kind() {
    for source in [
        "use generators::Api as expose; pub struct Options; #[expose] pub mod api { use generators::Other as expose; }",
        "pub struct Options; mod implementation { use generators::Api as expose; #[expose(crate::Options)] impl crate::Options {} }",
        "pub struct Options; mod implementation { use generators::Api as expose; impl crate::Options { #[expose(member)] pub fn existing(&self) {} } }",
        "pub struct Options; mod implementation { use generators::Api as expose; impl crate::Options { #[expose(member)] pub const VALUE: u8 = 0; } }",
        "use generators::Api as expose; pub struct Options; #[expose] pub type Value = u8;",
        "use generators::Api as expose; pub struct Options; #[expose] pub use std::time::Duration;",
        "use generators::Api as expose; pub struct Options; macro_rules! empty { () => {}; } #[expose] empty!();",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let externs = [("generators", build_review_attributes(root))];
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &externs);
        let consumer = "fn main() { let _: u16 = fixture::Options.generated(); }";
        assert!(
            compile_review_consumer(root, consumer).status.success(),
            "{source}"
        );
        review_fixture_macro_inputs(root, &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.kind == "reexport" && d.shape.contains("generators::Api")),
            "{source}"
        );
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.kind == "reexport" && d.shape.contains("generators::Other")),
            "{source}"
        );
        save_ffi_fixture_review(root, &ledger);
        replace(root, "core", "generators::Api", "generators::Other");
        compile_review_library(root, &externs);
        assert!(
            !compile_review_consumer(root, consumer).status.success(),
            "{source}"
        );
        assert!(!check_review_fixture(root).status.success(), "{source}");
    }
}

#[test]
fn attribute_provider_imports_keep_conditions_and_resolve_later_declarations() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_attributes(root))];
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        #[expose] pub struct Options;
        #[cfg(not(feature = "other"))] use generators::Api as expose;
        #[cfg(feature = "other")] use generators::Other as expose;
    "#,
    )
    .unwrap();
    compile_review_library(root, &externs);
    assert!(
        compile_review_consumer(
            root,
            "fn main() { let _: u16 = fixture::Options.generated(); }"
        )
        .status
        .success()
    );
    compile_review_library_with_cfg(root, &externs, &["feature=\"other\""]);
    review_fixture_macro_inputs(root, &mut ledger);
    let imports: Vec<_> = ledger
        .declarations
        .iter()
        .filter(|d| d.kind == "reexport" && d.shape.contains("generators::"))
        .collect();
    assert_eq!(imports.len(), 2);
    assert!(imports.iter().all(|d| !d.conditions.is_empty()));
    save_ffi_fixture_review(root, &ledger);
    replace(
        root,
        "core",
        "feature = \"other\"",
        "feature = \"replacement\"",
    );
    assert!(!check_review_fixture(root).status.success());
}

#[test]
fn inactive_and_unreachable_attributes_do_not_retain_provider_imports() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_attributes(root))];
    fs::write(root.join("core/src/lib.rs"), "pub struct Options;").unwrap();
    review(root, &mut ledger);
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        use generators::Api as expose;
        #[cfg_attr(test, expose)] pub struct Options;
        #[cfg_attr(all(), cfg(test))] #[expose] pub struct TestOnly;
        mod hidden { use generators::Other as expose; #[expose] pub struct Options; }
    ",
    )
    .unwrap();
    compile_review_library(root, &externs);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn function_trait_and_referenced_constant_attributes_keep_provider_imports() {
    for (source, consumer) in [
        (
            "use generators::Api as expose; #[expose(function)] pub fn existing() {}",
            "fn main() { let _: u16 = fixture::generated(); }",
        ),
        (
            "use generators::Api as expose; #[expose(function)] const LEN: usize = 1; pub struct Options(pub [u8; LEN]);",
            "fn main() { let _: u16 = fixture::generated(); }",
        ),
        (
            "use generators::Api as expose; pub trait Policy { #[expose(trait_member)] fn existing(&self); }",
            "fn check<T: fixture::Policy>(x: T) { let _: u16 = x.generated(); } fn main() {}",
        ),
        (
            "use generators::Api as expose; pub trait Policy { #[expose(trait_member)] type Value; }",
            "fn check<T: fixture::Policy>(x: T) { let _: u16 = x.generated(); } fn main() {}",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let externs = [("generators", build_review_attributes(root))];
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &externs);
        assert!(
            compile_review_consumer(root, consumer).status.success(),
            "{source}"
        );
        review_fixture_macro_inputs(root, &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.kind == "reexport" && d.shape.contains("generators::Api")),
            "{source}"
        );
        save_ffi_fixture_review(root, &ledger);
        replace(root, "core", "generators::Api", "generators::Other");
        compile_review_library(root, &externs);
        assert!(
            !compile_review_consumer(root, consumer).status.success(),
            "{source}"
        );
        assert!(!check_review_fixture(root).status.success(), "{source}");
    }
}

#[test]
fn imported_attributes_with_thiserror_helper_names_require_generator_review() {
    for name in ["source", "error", "from", "backtrace"] {
        for attribute in [
            format!("#[{name}(function)]"),
            format!("#[r#{name}(function)]"),
            format!("#[cfg_attr(all(), cfg_attr(all(), {name}(function)))]"),
        ] {
            let (directory, mut ledger) = fixture();
            let root = directory.path();
            let externs = [("generators", build_review_attributes(root))];
            fs::write(
                root.join("core/src/lib.rs"),
                "#[derive(Clone)] pub struct Options;",
            )
            .unwrap();
            review(root, &mut ledger);
            let original = save_ffi_fixture_review(root, &ledger);
            fs::write(
                root.join("core/src/lib.rs"),
                format!("use generators::Api as {name}; #[derive(Clone)] {attribute} pub struct Options;"),
            )
            .unwrap();
            compile_review_library(root, &externs);
            let consumer = "fn main() { let _: u16 = fixture::generated(); }";
            assert!(compile_review_consumer(root, consumer).status.success());
            assert!(
                inventory(root, &ledger)
                    .unwrap_err()
                    .contains("unreviewed macro")
            );
            assert!(!check_review_fixture(root).status.success());
            assert_eq!(
                fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
                original
            );
            review_fixture_macro_inputs(root, &mut ledger);
            assert_eq!(ledger.supplements.len(), 1);
            assert!(
                ledger
                    .declarations
                    .iter()
                    .any(|d| d.kind == "reexport" && d.shape.contains("generators::Api"))
            );
            save_ffi_fixture_review(root, &ledger);
            assert!(check_review_fixture(root).status.success());
            replace(root, "core", "generators::Api", "generators::Other");
            compile_review_library(root, &externs);
            assert!(!compile_review_consumer(root, consumer).status.success());
            assert!(!check_review_fixture(root).status.success());
        }
    }
}

fn build_review_error_helpers(root: &Path) -> std::path::PathBuf {
    // Model only thiserror's helper registration, without linking its trait impls.
    // This keeps the fixture independent of the repository's dependency artifacts.
    build_review_dependency(
        root,
        "thiserror",
        r"
        extern crate proc_macro;
        #[proc_macro_derive(Error, attributes(error, source, from, backtrace))]
        pub fn error(_: proc_macro::TokenStream) -> proc_macro::TokenStream {
            proc_macro::TokenStream::new()
        }
    ",
        "proc-macro",
    )
}

#[test]
fn resolved_thiserror_derives_exempt_their_active_helper_inputs() {
    for source in [
        "#[derive(thiserror::Error)] #[error(\"options\")] pub struct Options;",
        "#[derive(thiserror::Error)] #[source] #[from] #[backtrace] pub struct Options;",
        "#[derive(r#thiserror::r#Error)] #[r#source] pub struct Options;",
        "mod aliases { pub(crate) use thiserror::Error as Failure; } use aliases::Failure; #[derive(Failure)] #[error(\"options\")] pub struct Options;",
        "#[cfg_attr(feature = \"a\", derive(thiserror::Error), error(\"options\"))] pub struct Options;",
        "#[cfg_attr(feature = \"a\", derive(thiserror::Error))] #[cfg_attr(not(feature = \"a\"), derive(thiserror::Error))] #[error(\"options\")] pub struct Options;",
        "#[cfg(feature = \"a\")] use thiserror::Error as Failure; #[cfg(not(feature = \"a\"))] use generators::Other as Failure; #[derive(Failure)] #[cfg_attr(feature = \"a\", source)] pub struct Options;",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let externs = [
            ("thiserror", build_review_error_helpers(root)),
            ("generators", build_review_derives(root)),
        ];
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &externs);
        compile_review_library_with_cfg(root, &externs, &["feature=\"a\""]);
        review_fixture_macro_inputs(root, &mut ledger);
        assert!(
            ledger
                .supplements
                .iter()
                .all(|s| s.anchor.contains("derive")),
            "{source}"
        );
        save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success(), "{source}");
    }
}

#[test]
fn conditional_thiserror_derives_do_not_exempt_attributes_outside_their_availability() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [
        ("thiserror", build_review_error_helpers(root)),
        ("generators", build_review_attributes(root)),
    ];
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        use generators::Api as source;
        #[cfg_attr(feature = "a", derive(thiserror::Error))]
        #[cfg_attr(not(feature = "a"), source(function))]
        pub struct Options;
    "#,
    )
    .unwrap();
    compile_review_library(root, &externs);
    let consumer = "fn main() { let _: u16 = fixture::generated(); }";
    assert!(compile_review_consumer(root, consumer).status.success());
    compile_review_library_with_cfg(root, &externs, &["feature=\"a\""]);
    assert!(
        inventory(root, &ledger)
            .unwrap_err()
            .contains("unreviewed macro")
    );
    review_fixture_macro_inputs(root, &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.kind == "reexport" && d.shape.contains("generators::Api"))
    );
    save_ffi_fixture_review(root, &ledger);
    replace(root, "core", "generators::Api", "generators::Other");
    compile_review_library(root, &externs);
    assert!(!compile_review_consumer(root, consumer).status.success());
    assert!(!check_review_fixture(root).status.success());
}

#[test]
fn shadowed_thiserror_derives_do_not_exempt_procedural_attributes() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [
        ("generators", build_review_attributes(root)),
        (
            "custom_derives",
            build_review_dependency(
                root,
                "custom_derives",
                r"
            extern crate proc_macro;
            #[proc_macro_derive(Error)]
            pub fn error(_: proc_macro::TokenStream) -> proc_macro::TokenStream {
                proc_macro::TokenStream::new()
            }
        ",
                "proc-macro",
            ),
        ),
    ];
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        mod thiserror { pub(crate) use custom_derives::Error; }
        use generators::Api as source;
        #[derive(thiserror::Error)] #[source(function)] pub struct Options;
    ",
    )
    .unwrap();
    compile_review_library(root, &externs);
    assert!(
        compile_review_consumer(root, "fn main() { let _: u16 = fixture::generated(); }")
            .status
            .success()
    );
    review_fixture_macro_inputs(root, &mut ledger);
    assert_eq!(ledger.supplements.len(), 2);
    assert!(
        ledger
            .supplements
            .iter()
            .any(|s| s.anchor.contains("source (function)"))
    );
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.kind == "reexport" && d.shape.contains("generators::Api"))
    );
    save_ffi_fixture_review(root, &ledger);
    replace(root, "core", "generators::Api", "generators::Other");
    compile_review_library(root, &externs);
    assert!(!check_review_fixture(root).status.success());
}

#[test]
fn conditional_alias_implementations_filter_inactive_arities_before_substitution() {
    for aliases in [
        r#"
        #[cfg(feature = "a")] type Alias = Options<u16>;
        #[cfg(not(feature = "a"))] type Alias<T> = Options<T>;
        "#,
        r#"
        #[cfg(feature = "a")] type Alias = selected::Inner;
        #[cfg(not(feature = "a"))] type Alias<T> = selected::Inner<T>;
        mod selected {
            #[cfg(feature = "a")] pub type Inner = super::Options<u16>;
            #[cfg(not(feature = "a"))] pub type Inner<T> = super::Options<T>;
        }
        "#,
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!(
                r#"
            pub struct Options<T>(pub T);
            {aliases}
            #[cfg(feature = "a")] impl Alias {{ pub fn policy(&self) {{}} }}
            #[cfg(not(feature = "a"))] impl Alias<u32> {{ pub fn policy(&self) {{}} }}
        "#
            ),
        )
        .unwrap();
        let consumer = "fn main() { fixture::Options(0).policy(); }";
        for (predicates, ty) in [(vec![], "u32"), (vec!["feature=\"a\""], "u16")] {
            compile_review_library_with_cfg(root, &[], &predicates);
            assert!(
                compile_review_consumer(root, &consumer.replace("(0)", &format!("(0{ty})")))
                    .status
                    .success()
            );
        }
        review(root, &mut ledger);
        let methods: Vec<_> = ledger
            .declarations
            .iter()
            .filter(|d| d.surface == "core" && d.kind == "method")
            .collect();
        assert_eq!(methods.len(), 2);
        for (ty, predicate) in [
            ("u16", "cfg (feature = \"a\")"),
            ("u32", "cfg (not (feature = \"a\"))"),
        ] {
            let method = methods
                .iter()
                .find(|d| d.id.contains(&format!("Options< {ty} >::policy")))
                .unwrap();
            assert_eq!(method.conditions, [predicate]);
        }
        save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success());
        replace(root, "core", "pub fn policy", "pub fn changed");
        assert!(!check_review_fixture(root).status.success());
    }
}

#[test]
fn qualified_inherent_constants_retain_owners_aliases_and_initializer_dependencies() {
    for (source, expected) in [
        (
            "const N: usize = 99; struct Limits; impl Limits { const N: usize = 3; } pub struct Options(pub [u8; <Limits>::N]);",
            "core::Limits::N#associated_constant",
        ),
        (
            "const N: usize = 99; struct Limits; impl Limits { const N: usize = 3; } pub struct Options(pub [u8; <(Limits)>::N]);",
            "core::Limits::N#associated_constant",
        ),
        (
            "const N: usize = 99; struct Limits; impl Limits { const N: usize = 3; } pub struct Options(pub [u8; <r#Limits>::r#N]);",
            "core::Limits::N#associated_constant",
        ),
        (
            "const N: usize = 99; struct r#Limits; impl Limits { const r#N: usize = 3; } pub struct Options(pub [u8; <Limits>::N]);",
            "core::r#Limits::r#N#associated_constant",
        ),
        (
            "extern crate self as scope; const N: usize = 99; struct Limits; impl Limits { const N: usize = 3; } pub struct Options(pub [u8; <::scope::Limits>::N]);",
            "core::Limits::N#associated_constant",
        ),
        (
            "mod hidden { pub struct Limits { pub unused: bool } impl Limits { pub fn unused() {} pub(crate) const N: usize = 3; } } const N: usize = 99; pub struct Options(pub [u8; <hidden::Limits>::N]);",
            "core::hidden::Limits::N#associated_constant",
        ),
        (
            "mod hidden { pub(crate) struct Limits { pub unused: bool } impl Limits { pub fn unused() {} const N: usize = 3; pub(crate) const LEN: usize = <Self>::N; } } use hidden::Limits as Choice; type Selected = Choice; const N: usize = 99; pub struct Options(pub [u8; <Selected>::LEN]);",
            "core::hidden::Limits::N#associated_constant",
        ),
        (
            "const N: usize = 99; pub struct Options(pub [u8; <Self>::LEN]); impl Options { const N: usize = 3; const LEN: usize = <Self>::N; }",
            "core::Options::N#associated_constant",
        ),
        (
            "struct Limits; impl Limits { const N: usize = 3; } const LEN: usize = <Limits>::N; pub struct Options(pub [u8; LEN]);",
            "core::Limits::N#associated_constant",
        ),
        (
            "struct Limits<T>(T); impl<T> Limits<T> { const N: usize = 3; } pub struct Options(pub [u8; <Limits<u16>>::N]);",
            "core::Limits< T >::N#associated_constant",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[]);
        let consumer = "fn main() { let _ = fixture::Options([0; 3]); }";
        assert!(
            compile_review_consumer(root, consumer).status.success(),
            "{source}"
        );
        review(root, &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id == format!("core|{expected}")),
            "{source}"
        );
        assert!(
            !ledger
                .declarations
                .iter()
                .any(|d| d.id == "core|core::N#constant" || d.id.contains("::unused#")),
            "{source}"
        );
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success());
        let definition = if source.contains("const r#N:") {
            "const r#N: usize = 3"
        } else {
            "const N: usize = 3"
        };
        replace(root, "core", definition, &definition.replace("= 3", "= 4"));
        compile_review_library(root, &[]);
        assert!(
            !compile_review_consumer(root, consumer).status.success(),
            "{source}"
        );
        let checked = check_review_fixture(root);
        assert!(!checked.status.success(), "{source}");
        assert!(String::from_utf8_lossy(&checked.stderr).contains(expected));
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn qualified_constant_generic_arguments_retain_their_own_type_dependencies() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(
        root.join("core/src/lib.rs"),
        r"
        type Value = [u8; 3];
        struct Limits<T>(T);
        impl<T> Limits<T> { const N: usize = std::mem::size_of::<T>(); }
        pub struct Options(pub [u8; <Limits<Value>>::N]);
    ",
    )
    .unwrap();
    compile_review_library(root, &[]);
    let consumer = "fn main() { let _ = fixture::Options([0; 3]); }";
    assert!(compile_review_consumer(root, consumer).status.success());
    review(root, &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::Value#alias")
    );
    save_ffi_fixture_review(root, &ledger);
    replace(root, "core", "Value = [u8; 3]", "Value = [u8; 4]");
    compile_review_library(root, &[]);
    assert!(!compile_review_consumer(root, consumer).status.success());
    assert!(!check_review_fixture(root).status.success());
}

#[test]
fn unsupported_qualified_expressions_fail_when_their_declarations_are_reached() {
    for source in [
        "struct Limits; trait Rule { const N: usize; } impl Rule for Limits { const N: usize = 3; } pub struct Options(pub [u8; <Limits as Rule>::N]);",
        "struct Limits; struct Value; impl Value { const N: usize = 3; } trait Rule { type Value; } impl Rule for Limits { type Value = Value; } pub struct Options(pub [u8; <<Limits as Rule>::Value>::N]);",
        "pub const LENGTH: fn(&[u8]) -> usize = <[u8]>::len;",
    ] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[]);
        let error = inventory(root, &ledger).unwrap_err();
        assert!(
            error.contains("unsupported qualified expression dependency"),
            "{source}: {error}"
        );
        assert!(error.contains("audited declaration core|"), "{error}");
    }
}

#[test]
fn unreachable_and_test_only_qualified_expressions_do_not_fail_the_audit() {
    for addition in [
        "const UNUSED: usize = <Limits as Rule>::N;",
        "mod hidden { pub const UNUSED: usize = <super::Limits as super::Rule>::N; }",
        "#[cfg(test)] pub const UNUSED: usize = <Limits as Rule>::N;",
        "pub struct TestOnly(#[cfg(test)] pub [u8; <Limits as Rule>::N]);",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let prefix = "struct Limits; trait Rule { const N: usize; } impl Rule for Limits { const N: usize = 3; } pub struct Options(pub u8); pub struct TestOnly();";
        fs::write(root.join("core/src/lib.rs"), prefix).unwrap();
        review(root, &mut ledger);
        let source = if addition.starts_with("pub struct TestOnly") {
            prefix.replace("pub struct TestOnly();", addition)
        } else {
            format!("{prefix} {addition}")
        };
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[]);
        assert_eq!(
            ledger.declarations,
            inventory(root, &ledger).unwrap(),
            "{addition}"
        );
    }
}

fn compile_review_cargo_library(root: &Path) {
    let built = std::process::Command::new(env!("CARGO"))
        .args(["build", "--offline", "--quiet", "--manifest-path"])
        .arg(root.join("core/Cargo.toml"))
        .arg("--target-dir")
        .arg(root.join("cargo-build"))
        .env("RUSTFLAGS", "-D warnings")
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    fs::copy(
        root.join("cargo-build/debug/libfixture.rlib"),
        root.join("libfixture.rlib"),
    )
    .unwrap();
}

#[test]
fn cargo_selected_library_sources_and_their_modules_require_review() {
    for (selected, library) in [
        ("src/actual.rs", "[lib]\npath = 'src/actual.rs'\n"),
        (
            "api/entry.rs",
            "[lib]\n'path' = \"api/entry.rs\" # selected root\n",
        ),
        ("api/lib.rs", "lib = { path = 'api/lib.rs' }\n"),
        ("api/root/mod.rs", "'lib'.path = 'api/root/mod.rs'\n"),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let package = "[package]\nname = 'fixture'\nversion = '0.1.0'\nedition = '2024'\n";
        fs::write(root.join("core/Cargo.toml"), package).unwrap();
        fs::write(root.join("core/src/lib.rs"), "pub struct Options;").unwrap();
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        let manifest = if library.starts_with("[lib]") {
            format!("{package}{library}")
        } else {
            format!("{library}{package}")
        };
        fs::write(root.join("core/Cargo.toml"), manifest).unwrap();
        let source = root.join("core").join(selected);
        let modules = source.parent().unwrap();
        fs::create_dir_all(modules).unwrap();
        fs::write(
            &source,
            "pub struct Options; mod methods; #[path = \"renamed.rs\"] pub mod api;",
        )
        .unwrap();
        fs::write(
            modules.join("methods.rs"),
            "impl crate::Options { pub fn added() {} }",
        )
        .unwrap();
        fs::write(modules.join("renamed.rs"), "pub struct Marker;").unwrap();
        compile_review_cargo_library(root);
        let consumer = "fn main() { fixture::Options::added(); let _ = fixture::api::Marker; }";
        assert!(
            compile_review_consumer(root, consumer).status.success(),
            "{selected}"
        );
        let actual = inventory(root, &ledger).unwrap();
        assert!(
            actual
                .iter()
                .any(|d| d.id == "core|core::Options::added#method")
        );
        assert!(
            actual
                .iter()
                .any(|d| d.id == "core|core::Options#struct"
                    && d.source == format!("core/{selected}"))
        );
        assert!(!check_review_fixture(root).status.success(), "{selected}");
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
        review(root, &mut ledger);
        save_ffi_fixture_review(root, &ledger);
        fs::write(
            root.join("core/src/lib.rs"),
            "compile_error!(\"unused library source\");",
        )
        .unwrap();
        compile_review_cargo_library(root);
        assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
        assert!(check_review_fixture(root).status.success());
        fs::remove_file(root.join("core/src/lib.rs")).unwrap();
        assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
        fs::write(
            modules.join("methods.rs"),
            "impl crate::Options { pub fn changed() {} }",
        )
        .unwrap();
        compile_review_cargo_library(root);
        assert!(!compile_review_consumer(root, consumer).status.success());
        assert!(!check_review_fixture(root).status.success());
    }
}

#[test]
fn explicit_and_automatic_default_library_targets_keep_the_inventory() {
    for target in [
        "",
        "autolib = true\n",
        "[lib]\nname = 'fixture'\n",
        "autolib = false\n[lib]\n",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), "pub struct Options;").unwrap();
        review(root, &mut ledger);
        fs::write(
            root.join("core/Cargo.toml"),
            format!("[package]\nname='fixture'\nversion='0.1.0'\nedition='2024'\n{target}"),
        )
        .unwrap();
        compile_review_cargo_library(root);
        assert_eq!(
            ledger.declarations,
            inventory(root, &ledger).unwrap(),
            "{target}"
        );
    }
}

#[test]
fn invalid_missing_and_disabled_library_targets_fail_instead_of_auditing_stale_sources() {
    for (manifest, expected) in [
        ("[lib]\npath = 'missing.rs'", "missing library source"),
        ("[lib]\npath = 7", "invalid manifest"),
        ("lib = 'src/actual.rs'", "invalid manifest"),
        ("[lib", "invalid manifest"),
        (
            "[package]\nname='fixture'\nversion='0.1.0'\nautolib=false",
            "no library target",
        ),
        ("[workspace]\nmembers=[]", "no package library target"),
    ] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        let manifest = if manifest.starts_with("[package]") || manifest.starts_with("[workspace]") {
            manifest.into()
        } else {
            format!("{manifest}\n[package]\nname='fixture'\nversion='0.1.0'\n")
        };
        fs::write(root.join("core/Cargo.toml"), manifest).unwrap();
        let error = inventory(root, &ledger).unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert!(error.contains("Cargo.toml"), "{error}");
    }
}

#[test]
fn parenthesized_inherent_owners_cannot_hide_public_methods() {
    for (declaration, owner, consumer) in [
        (
            "pub struct Options;",
            "(Options)",
            "fn main() { fixture::Options::added(); }",
        ),
        (
            "pub struct Options; type Choice = Options;",
            "((Choice))",
            "fn main() { fixture::Options::added(); }",
        ),
        (
            "pub struct Options<T>(pub T);",
            "((Options<u16>))",
            "fn main() { fixture::Options::<u16>::added(); }",
        ),
        (
            "pub mod actual { pub struct Options; } pub use actual::Options;",
            "(actual::Options)",
            "fn main() { fixture::Options::added(); }",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(root.join("core/src/lib.rs"), declaration).unwrap();
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        fs::write(
            root.join("core/src/lib.rs"),
            format!("{declaration} #[allow(unused_parens)] impl {owner} {{ pub fn added() {{}} }}"),
        )
        .unwrap();
        compile_review_cargo_library(root);
        assert!(
            compile_review_consumer(root, consumer).status.success(),
            "{owner}"
        );
        let error = validate(root, &ledger, &inventory(root, &ledger).unwrap()).unwrap_err();
        assert!(
            error.contains("unreviewed declaration") && error.contains("added#method"),
            "{error}"
        );
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn parenthesized_impl_owners_preserve_generic_shapes_and_self_dependencies() {
    for (declaration, owner) in [
        ("pub struct Options<T>(pub T);", "Options<u16>"),
        (
            "pub struct Options<T>(pub T); type Choice = Options<u16>;",
            "Choice",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let source = format!(
            "{declaration} #[allow(unused_parens)] impl {owner} {{ const LEN: usize = 3; pub fn bytes() -> [u8; <Self>::LEN] {{ [0; Self::LEN] }} }}"
        );
        fs::write(root.join("core/src/lib.rs"), &source).unwrap();
        review(root, &mut ledger);
        save_ffi_fixture_review(root, &ledger);
        replace(
            root,
            "core",
            &format!("impl {owner}"),
            &format!("impl (({owner}))"),
        );
        compile_review_library(root, &[]);
        let consumer = "fn main() { let _: [u8; 3] = fixture::Options::<u16>::bytes(); }";
        assert!(compile_review_consumer(root, consumer).status.success());
        assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
        assert!(check_review_fixture(root).status.success());
        replace(root, "core", "LEN: usize = 3", "LEN: usize = 4");
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        assert!(!check_review_fixture(root).status.success());
    }
}

#[test]
fn parenthesized_impl_selectors_do_not_expose_sibling_enum_variants() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "mod hidden { pub enum E { A, B } } pub use hidden::E::A; #[allow(unused_parens)] impl hidden::E { pub fn added(&self) {} }").unwrap();
    review(root, &mut ledger);
    replace(root, "core", "impl hidden::E", "impl ((hidden::E))");
    compile_review_library(root, &[]);
    assert!(
        compile_review_consumer(root, "fn main() { fixture::A.added(); }")
            .status
            .success()
    );
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::E::added#method")
    );
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::hidden::E::B#variant")
    );
}

#[test]
fn macro_typed_inherent_owners_fail_explicitly_instead_of_losing_methods() {
    let (directory, ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("core/src/lib.rs"), "macro_rules! owner { () => { Options }; } pub struct Options; impl owner!() { pub fn added() {} }").unwrap();
    compile_review_library(root, &[]);
    assert!(
        compile_review_consumer(root, "fn main() { fixture::Options::added(); }")
            .status
            .success()
    );
    let error = inventory(root, &ledger).unwrap_err();
    assert!(
        error.contains("unsupported inherent impl owner `owner ! ()`"),
        "{error}"
    );
    assert!(error.contains("core/src/lib.rs"), "{error}");
}

#[test]
fn trait_impl_generators_require_full_input_supplements_and_provider_imports() {
    for source in [
        "use generators::Api as expose; pub struct Options; #[expose] impl Default for Options { fn default() -> Self { Self } }",
        "use generators::Api as expose; pub struct Options; trait Policy {} #[expose] impl Policy for Options {}",
        "use generators::Api as expose; pub struct Options; unsafe trait Policy {} #[expose] unsafe impl Policy for Options {}",
        "use generators::Api as expose; pub struct Options; #[cfg_attr(all(), cfg_attr(all(), expose))] impl Default for Options { fn default() -> Self { Self } }",
        "pub struct Options; mod implementation { use generators::Api as expose; #[expose(crate::Options)] impl Default for crate::Options { fn default() -> Self { Self } } }",
        "use generators::Api as expose; pub struct Options; #[allow(unused_parens)] type Choice = ((Options)); #[expose] impl Default for Choice { fn default() -> Self { Self } }",
        "use generators::Api as expose; pub struct Options; #[expose] mod implementation { impl Default for crate::Options { fn default() -> Self { Self } } }",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let externs = [("generators", build_review_attributes(root))];
        fs::write(
            root.join("core/src/lib.rs"),
            "pub struct Options; impl Options { pub fn generated(&self) -> u16 { 0 } }",
        )
        .unwrap();
        let outputs = discover_sources(root, &ledger)
            .unwrap()
            .into_iter()
            .filter(|d| d.surface == "core" && d.kind == "method")
            .collect();
        fs::write(root.join("core/src/lib.rs"), "pub struct Options;").unwrap();
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &externs);
        let consumer = "fn main() { let _: u16 = fixture::Options.generated(); }";
        assert!(
            compile_review_consumer(root, consumer).status.success(),
            "{source}"
        );
        assert!(
            inventory(root, &ledger)
                .unwrap_err()
                .contains("unreviewed macro"),
            "{source}"
        );
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
        review_fixture_macro_inputs(root, &mut ledger);
        assert_eq!(ledger.supplements.len(), 1, "{source}");
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.kind == "reexport" && d.shape.contains("generators::Api")),
            "{source}"
        );
        ledger.supplements[0].declarations = outputs;
        review(root, &mut ledger);
        save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success(), "{source}");
        replace(root, "core", "generators::Api", "generators::Other");
        compile_review_library(root, &externs);
        assert!(
            !compile_review_consumer(root, consumer).status.success(),
            "{source}"
        );
        assert!(!check_review_fixture(root).status.success());
    }
}

#[test]
fn distinct_trait_impl_generators_keep_bodies_and_all_helper_attributes() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_attributes(root))];
    let source = "use generators::{Api as first, Other as second}; pub struct Options; #[first] impl Default for Options { fn default() -> Self { Self } } #[second] impl Clone for Options { fn clone(&self) -> Self { Self } }";
    fs::write(root.join("core/src/lib.rs"), source).unwrap();
    compile_review_library(root, &externs);
    review_fixture_macro_inputs(root, &mut ledger);
    assert_eq!(ledger.supplements.len(), 2);
    assert_ne!(ledger.supplements[0].anchor, ledger.supplements[1].anchor);
    assert!(
        ledger
            .supplements
            .iter()
            .any(|s| s.expected_shape.contains("impl Default for Options"))
    );
    assert!(
        ledger
            .supplements
            .iter()
            .any(|s| s.expected_shape.contains("impl Clone for Options"))
    );
    save_ffi_fixture_review(root, &ledger);
    for replacement in [
        "fn default() -> Self { let _ = 1; Self }",
        "#[allow(dead_code)] fn default() -> Self { Self }",
    ] {
        fs::write(
            root.join("core/src/lib.rs"),
            source.replace("fn default() -> Self { Self }", replacement),
        )
        .unwrap();
        compile_review_library(root, &externs);
        assert!(
            inventory(root, &ledger)
                .unwrap_err()
                .contains("changed or invalid supplement")
        );
        assert!(!check_review_fixture(root).status.success());
    }
}

#[test]
fn trait_impl_generator_headers_retain_private_aliases_and_imports() {
    for selecting in ["type Value = u16;", "use std::primitive::u16 as Value;"] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let externs = [("generators", build_review_attributes(root))];
        fs::write(root.join("core/src/lib.rs"), format!("use generators::Api as expose; {selecting} pub struct Options<T>(pub T); #[expose(Options<Value>)] impl Default for Options<Value> {{ fn default() -> Self {{ Self(0) }} }}")).unwrap();
        compile_review_library(root, &externs);
        let consumer = "fn main() { let _: u16 = fixture::Options(0u16).generated(); }";
        assert!(compile_review_consumer(root, consumer).status.success());
        review_fixture_macro_inputs(root, &mut ledger);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.id.contains("Value") && matches!(d.kind.as_str(), "alias" | "reexport")),
            "{selecting}"
        );
        save_ffi_fixture_review(root, &ledger);
        replace(root, "core", "u16", "u32");
        compile_review_library(root, &externs);
        assert!(!compile_review_consumer(root, consumer).status.success());
        assert!(!check_review_fixture(root).status.success());
    }
}

#[test]
fn ordinary_unreachable_and_test_only_trait_impls_stay_outside_the_inventory() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_attributes(root))];
    fs::write(root.join("core/src/lib.rs"), "pub struct Options;").unwrap();
    review(root, &mut ledger);
    let source = r"
        pub struct Options;
        impl Default for Options { fn default() -> Self { Self } }
        trait RefPolicy {} impl RefPolicy for &Options {}
        #[cfg(test)] #[unknown_generator] impl Clone for Options { fn clone(&self) -> Self { Self } }
        mod hidden {
            use generators::Api as expose;
            pub struct Hidden;
            #[expose(Hidden)] impl Default for Hidden { fn default() -> Self { Self } }
        }
    ";
    fs::write(root.join("core/src/lib.rs"), source).unwrap();
    compile_review_library(root, &externs);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    replace(
        root,
        "core",
        "fn default() -> Self { Self }",
        "fn default() -> Self { let _ = 1; Self }",
    );
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn unresolved_or_non_path_trait_generator_owners_fail_explicitly() {
    for owner in ["Vec<u8>", "&Options", "T", "Alias"] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        let externs = [("generators", build_review_attributes(root))];
        let generics = if owner == "T" { "<T>" } else { "" };
        let alias = if owner == "Alias" {
            "type Alias = Vec<u8>;"
        } else {
            ""
        };
        fs::write(root.join("core/src/lib.rs"), format!("use generators::Api as expose; pub struct Options; trait Policy {{}} {alias} #[expose(function)] impl{generics} Policy for {owner} {{}}")).unwrap();
        compile_review_library(root, &externs);
        assert!(
            compile_review_consumer(root, "fn main() { let _: u16 = fixture::generated(); }")
                .status
                .success()
        );
        let error = inventory(root, &ledger).unwrap_err();
        assert!(
            error.contains("unsupported attributed") && error.contains("impl owner"),
            "{owner}: {error}"
        );
    }
}

#[test]
fn parenthesized_alias_targets_attach_methods_to_the_underlying_public_type() {
    for (definitions, implementation, consumer) in [
        (
            "pub struct Options; type Alias = (Options);",
            "impl Alias",
            "fn main() { fixture::Options.new_policy(); }",
        ),
        (
            "pub struct Options; type Inner = ((Options)); type Alias = (Inner);",
            "impl (Alias)",
            "fn main() { fixture::Options.new_policy(); }",
        ),
        (
            "pub struct Options<T>(pub T); type Alias<T> = (Options<T>);",
            "impl Alias<u16>",
            "fn main() { fixture::Options(0u16).new_policy(); }",
        ),
        (
            "pub struct Options<T, const N: usize>(pub T); type Alias<T = u16, const N: usize = 3> = ((Options<T, N>));",
            "impl Alias",
            "fn main() { fixture::Options::<u16, 3>(0).new_policy(); }",
        ),
        (
            "pub struct Options<'a, T, const N: usize>(pub &'a T); type Alias<'a, T, const N: usize> = (Options<'a, T, N>);",
            "impl<'a> Alias<'a, u16, 3>",
            "fn check(x: &u16) { fixture::Options::<u16, 3>(x).new_policy(); } fn main() {}",
        ),
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let definitions = format!("#![allow(unused_parens, dead_code)] {definitions}");
        fs::write(root.join("core/src/lib.rs"), &definitions).unwrap();
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        fs::write(
            root.join("core/src/lib.rs"),
            format!("{definitions} {implementation} {{ pub fn new_policy(&self) {{}} }}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        assert!(
            compile_review_consumer(root, consumer).status.success(),
            "{implementation}"
        );
        let actual = inventory(root, &ledger).unwrap();
        assert!(
            actual
                .iter()
                .any(|d| d.id.contains("core::Options") && d.id.contains("new_policy#method")),
            "{implementation}"
        );
        let checked = check_review_fixture(root);
        assert!(!checked.status.success(), "{implementation}");
        assert!(String::from_utf8_lossy(&checked.stderr).contains("new_policy#method"));
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn conditional_parenthesized_aliases_keep_generator_and_method_availability() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_attributes(root))];
    fs::write(root.join("core/src/lib.rs"), r#"
        #![allow(unused_parens)]
        use generators::Api as expose;
        pub struct Options<T>(pub T);
        #[cfg(feature = "a")] type Alias = (Options<u16>);
        #[cfg(not(feature = "a"))] type Alias<T> = ((Options<T>));
        #[cfg(feature = "a")] #[expose(Alias)] impl Default for Alias { fn default() -> Self { Self(0) } }
        #[cfg(not(feature = "a"))] #[expose(Alias<u32>)] impl Default for Alias<u32> { fn default() -> Self { Self(0) } }
        #[cfg(feature = "a")] impl Alias { pub fn policy(&self) {} }
        #[cfg(not(feature = "a"))] impl Alias<u32> { pub fn policy(&self) {} }
    "#).unwrap();
    compile_review_library(root, &externs);
    assert!(
        compile_review_consumer(
            root,
            "fn main() { let _: u16 = fixture::Options(0u32).generated(); }"
        )
        .status
        .success()
    );
    compile_review_library_with_cfg(root, &externs, &["feature=\"a\""]);
    assert!(
        compile_review_consumer(
            root,
            "fn main() { let _: u16 = fixture::Options(0u16).generated(); }"
        )
        .status
        .success()
    );
    review_fixture_macro_inputs(root, &mut ledger);
    assert_eq!(ledger.supplements.len(), 2);
    for ty in ["u16", "u32"] {
        assert!(ledger.declarations.iter().any(|d| d.kind == "macro"
            && d.id.contains(&format!("Options< {ty} >"))
            && !d.conditions.is_empty()));
        assert!(ledger.declarations.iter().any(|d| d.kind == "method"
            && d.id.contains(&format!("Options< {ty} >::policy"))
            && !d.conditions.is_empty()));
    }
    save_ffi_fixture_review(root, &ledger);
    replace(root, "core", "generators::Api", "generators::Other");
    compile_review_library(root, &externs);
    assert!(!check_review_fixture(root).status.success());
}

#[test]
fn anonymous_constants_cannot_hide_callable_inherent_members() {
    for addition in [
        "const _: () = { impl Options { pub fn extra(&self) {} } };",
        "const _: () = { let _ = { impl Options { pub fn extra(&self) {} } 1 }; };",
        "const _: () = { const _: () = { impl Options { pub fn extra(&self) {} } }; };",
        "const _: () = const { impl Options { pub fn extra(&self) {} } };",
        "const _: [(); { impl Options { pub fn extra(&self) {} } 0 }] = [];",
        "const _: () = assert!({ impl Options { pub fn extra(&self) {} } true });",
        "const _: () = std::assert!(matches!({ impl Options { pub fn extra(&self) {} } 3 }, 3));",
        "const _: () = { if false { impl Options { pub fn extra(&self) {} } } };",
        "const _: () = { use crate::Options as Selected; impl Selected { pub fn extra(&self) {} } };",
        "const _: () = { mod hidden { impl crate::Options { pub fn extra(&self) {} } } };",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), "pub struct Options;").unwrap();
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        fs::write(
            root.join("core/src/lib.rs"),
            format!("pub struct Options; {addition}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        assert!(
            compile_review_consumer(root, "fn main() { fixture::Options.extra(); }")
                .status
                .success(),
            "{addition}"
        );
        let error = inventory(root, &ledger).unwrap_err();
        assert!(
            error.contains("unsupported anonymous constant"),
            "{addition}: {error}"
        );
        assert!(error.contains("core/src/lib.rs"), "{error}");
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn anonymous_assertions_keep_local_values_and_test_inactivity_outside_inventory() {
    for addition in [
        "const _: () = { const N: usize = 3; let value = N + 1; assert!(value == 4); };",
        "const _: () = { const fn positive() -> bool { true } assert!(positive()); };",
        "const _: () = { fn assert() {} assert!(true); };",
        "const _: () = ::core::assert!(true); const _: () = r#std::r#assert!(true);",
        "const _: () = assert!(matches!(Some(3), Some(value) if value > 0));",
        "const _: &str = stringify!(impl Options { pub fn extra(&self) {} });",
        "const _: bool = cfg!(unix); const _: usize = line!() as usize;",
        "const _: () = { #[cfg(test)] impl Options { pub fn extra(&self) {} } assert!(true); };",
        "const _: () = { #[cfg_attr(all(), cfg(test))] impl Options { pub fn extra(&self) {} } };",
        "const _: () = { #[cfg(test)] unknown_generator!(); assert!(true); };",
        "const _: () = { match true { #[cfg(test)] true => { impl Options { pub fn extra(&self) {} } }, _ => assert!(true) } };",
        "#[cfg(test)] #[unknown_generator] const _: () = { impl Options { pub fn extra(&self) {} } };",
        "#[cfg(feature = \"a\")] const _: () = { #[cfg(not(feature = \"a\"))] impl Options { pub fn extra(&self) {} } };",
        "#[cfg(feature = \"a\")] #[cfg_attr(not(feature = \"a\"), unknown_generator)] const _: () = assert!(true);",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), "pub struct Options;").unwrap();
        review(root, &mut ledger);
        save_ffi_fixture_review(root, &ledger);
        fs::write(
            root.join("core/src/lib.rs"),
            format!("pub struct Options; {addition}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        compile_review_library_with_cfg(root, &[], &["feature=\"a\""]);
        assert_eq!(
            ledger.declarations,
            inventory(root, &ledger).unwrap(),
            "{addition}"
        );
        assert!(check_review_fixture(root).status.success(), "{addition}");
    }
}

#[test]
fn anonymous_block_bindings_are_rejected_without_leaking_names_into_module_scope() {
    for addition in [
        "const _: () = { struct Options; impl Options { pub fn local(&self) {} } };",
        "const _: () = { type Options = u8; };",
        "const _: () = { use std::time::Duration as Options; };",
        "const _: () = { macro_rules! local { () => {}; } };",
        "const _: () = { fn helper() { impl Options { pub fn extra(&self) {} } } };",
    ] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            format!("pub struct Options; {addition}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        let error = inventory(root, &ledger).unwrap_err();
        assert!(
            error.contains("unsupported anonymous constant") && error.contains("block-scope"),
            "{error}"
        );
        assert!(!error.contains("duplicate declaration"), "{error}");
    }
}

#[test]
fn anonymous_constants_reject_macro_generation_and_shadowed_assertions() {
    for source in [
        "macro_rules! make { () => {{ impl Options { pub fn extra(&self) {} } }}; } pub struct Options; const _: () = make!();",
        "macro_rules! assert { ($value:expr) => {{ impl Options { pub fn extra(&self) {} } }}; } pub struct Options; const _: () = assert!(true);",
        "mod hidden { macro_rules! assert { ($value:expr) => {{ impl crate::Options { pub fn extra(&self) {} } }}; } pub(crate) use assert; } use hidden::assert; pub struct Options; const _: () = assert!(true);",
        "mod hidden { macro_rules! assert { ($value:expr) => {{ impl crate::Options { pub fn extra(&self) {} } }}; } pub(crate) use assert; } pub struct Options; const _: () = assert!(true); use hidden::assert;",
        "#[macro_use] mod hidden { macro_rules! assert { ($value:expr) => {{ impl crate::Options { pub fn extra(&self) {} } }}; } } pub struct Options; const _: () = assert!(true);",
        "mod hidden { macro_rules! assert { ($value:expr) => {{ impl crate::Options { pub fn extra(&self) {} } }}; } pub(crate) use assert; } mod std { pub(crate) use crate::hidden::assert; } pub struct Options; const _: () = std::assert!(true);",
        "macro_rules! install { () => { mod std { macro_rules! check { ($value:expr) => {{ impl crate::Options { pub fn extra(&self) {} } }}; } pub(crate) use check as assert; } }; } install!(); pub struct Options; const _: () = crate::std::assert!(true);",
    ] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[]);
        assert!(
            compile_review_consumer(root, "fn main() { fixture::Options.extra(); }")
                .status
                .success(),
            "{source}"
        );
        let error = inventory(root, &ledger).expect_err(source);
        assert!(
            error.contains("unsupported anonymous constant"),
            "{source}: {error}"
        );
    }
}

#[test]
fn anonymous_constants_reject_imported_macro_preludes_and_crate_aliases() {
    for (selecting, delayed) in [
        ("#[macro_use] extern crate hidden;", false),
        ("#[macro_use] extern crate hidden;", true),
        ("#[macro_use(r#assert)] extern crate hidden;", false),
        ("#![no_std] extern crate hidden as std;", false),
    ] {
        let (directory, ledger) = fixture();
        let root = directory.path();
        let external = build_review_dependency(
            root,
            "hidden",
            "#[macro_export] macro_rules! assert { ($value:expr) => {{ impl Options { pub fn extra(&self) {} } }}; }",
            "lib",
        );
        let call = if selecting.contains("as std") {
            "::std::assert!(true)"
        } else {
            "assert!(true)"
        };
        let source = if delayed {
            format!("pub struct Options; const _: () = {call}; {selecting}")
        } else {
            format!("{selecting} pub struct Options; const _: () = {call};")
        };
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &[("hidden", external)]);
        assert!(
            compile_review_consumer(root, "fn main() { fixture::Options.extra(); }")
                .status
                .success()
        );
        let error = inventory(root, &ledger).unwrap_err();
        assert!(error.contains("unsupported anonymous constant"), "{error}");
    }
}

#[test]
fn anonymous_constants_reject_generator_attributes_and_linker_exports() {
    let (directory, ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_attributes(root))];
    fs::write(
        root.join("core/src/lib.rs"),
        "use generators::Api as expose; pub struct Options; #[expose(function)] const _: () = ();",
    )
    .unwrap();
    compile_review_library(root, &externs);
    assert!(
        compile_review_consumer(root, "fn main() { let _: u16 = fixture::generated(); }")
            .status
            .success()
    );
    let error = inventory(root, &ledger).unwrap_err();
    assert!(
        error.contains("unsupported anonymous constant") && error.contains("attributes"),
        "{error}"
    );
    fs::write(root.join("core/src/lib.rs"), "pub struct Options; const _: () = { #[unsafe(export_name = \"anonymous_export\")] static SYMBOL: u8 = 0; };").unwrap();
    compile_review_library(root, &[]);
    let error = inventory(root, &ledger).unwrap_err();
    assert!(
        error.contains("unsupported anonymous constant") && error.contains("linkage"),
        "{error}"
    );
}

#[test]
fn anonymous_assertions_exclude_generators_in_mutually_exclusive_configurations() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let source = r#"
        pub struct Options;
        #[cfg(feature = "a")] macro_rules! install { () => {}; }
        #[cfg(feature = "a")] install!();
    "#;
    fs::write(root.join("core/src/lib.rs"), source).unwrap();
    review_fixture_macro_inputs(root, &mut ledger);
    fs::write(
        root.join("core/src/lib.rs"),
        format!("{source} const _: () = {{ #[cfg(not(feature = \"a\"))] assert!(true); }};"),
    )
    .unwrap();
    compile_review_library(root, &[]);
    compile_review_library_with_cfg(root, &[], &["feature=\"a\""]);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn anonymous_standard_assertions_do_not_select_later_textual_macros() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let definition = "macro_rules! assert { ($value:expr) => { () }; }";
    fs::write(
        root.join("core/src/lib.rs"),
        format!("pub struct Options; {definition}"),
    )
    .unwrap();
    review_fixture_macro_inputs(root, &mut ledger);
    fs::write(
        root.join("core/src/lib.rs"),
        format!("pub struct Options; const _: () = assert!(true); {definition}"),
    )
    .unwrap();
    compile_review_library(root, &[]);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn private_method_generators_require_review_and_keep_provider_imports() {
    for source in [
        "use generators::Api as expose; pub struct Options; impl Options { #[expose(member)] fn helper(&self) {} }",
        "use generators::Api as expose; pub struct Options; impl Options { #[expose(member)] pub(crate) fn helper(&self) {} }",
        "use generators::Api as expose; pub struct Options; impl Options { #[cfg_attr(all(), cfg_attr(all(), expose(member)))] async fn helper(&self) {} }",
        "pub struct Options; mod implementation { use generators::Api as expose; impl crate::Options { #[expose(member)] pub(super) fn helper(&self) {} } }",
        "use generators::Api as expose; pub struct Options; #[allow(unused_parens)] type Choice = (Options); impl Choice { #[expose(member)] fn helper(&self) {} }",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let externs = [("generators", build_review_attributes(root))];
        fs::write(
            root.join("core/src/lib.rs"),
            "pub struct Options; impl Options { pub fn generated(&self) -> u16 { 0 } }",
        )
        .unwrap();
        let outputs = discover_sources(root, &ledger)
            .unwrap()
            .into_iter()
            .filter(|d| d.surface == "core" && d.kind == "method")
            .collect();
        fs::write(
            root.join("core/src/lib.rs"),
            "pub struct Options; impl Options { fn helper(&self) {} }",
        )
        .unwrap();
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        fs::write(root.join("core/src/lib.rs"), source).unwrap();
        compile_review_library(root, &externs);
        let consumer = "fn main() { let _: u16 = fixture::Options.generated(); }";
        assert!(
            compile_review_consumer(root, consumer).status.success(),
            "{source}"
        );
        let raw = discover_sources(root, &ledger).unwrap();
        assert!(
            !raw.iter()
                .any(|d| d.kind == "method" && d.id.contains("::helper#method"))
        );
        assert!(
            inventory(root, &ledger)
                .unwrap_err()
                .contains("unreviewed macro")
        );
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
        review_fixture_macro_inputs(root, &mut ledger);
        assert_eq!(ledger.supplements.len(), 1);
        assert!(
            ledger
                .declarations
                .iter()
                .any(|d| d.kind == "reexport" && d.shape.contains("generators::Api")),
            "{source}"
        );
        ledger.supplements[0].declarations = outputs;
        review(root, &mut ledger);
        save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success());
        replace(root, "core", "generators::Api", "generators::Other");
        compile_review_library(root, &externs);
        assert!(!compile_review_consumer(root, consumer).status.success());
        assert!(!check_review_fixture(root).status.success());
    }
}

#[test]
fn private_method_generator_inputs_include_bodies_docs_and_signature_dependencies() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_attributes(root))];
    let source = "pub struct Options; mod implementation { use generators::Api as expose; type Value = u8; impl crate::Options { #[expose(member)] #[doc = \"before\"] fn helper(&self, value: Value) { let _ = value; } } }";
    fs::write(root.join("core/src/lib.rs"), source).unwrap();
    compile_review_library(root, &externs);
    review_fixture_macro_inputs(root, &mut ledger);
    assert!(
        ledger
            .declarations
            .iter()
            .any(|d| d.id == "core|core::implementation::Value#alias")
    );
    assert!(
        !ledger
            .declarations
            .iter()
            .any(|d| d.kind == "method" && d.id.contains("helper"))
    );
    save_ffi_fixture_review(root, &ledger);
    for (before, after, supplement_changes) in [
        ("let _ = value", "let _ = (value, 1)", true),
        ("\"before\"", "\"after\"", true),
        ("Value = u8", "Value = u16", false),
    ] {
        fs::write(root.join("core/src/lib.rs"), source.replace(before, after)).unwrap();
        compile_review_library(root, &externs);
        if supplement_changes {
            assert!(
                inventory(root, &ledger)
                    .unwrap_err()
                    .contains("changed or invalid supplement")
            );
        } else {
            assert_ne!(ledger.declarations, inventory(root, &ledger).unwrap());
        }
        assert!(!check_review_fixture(root).status.success());
    }
}

#[test]
fn ordinary_private_methods_and_inactive_generators_remain_outside_inventory() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_attributes(root))];
    fs::write(root.join("core/src/lib.rs"), "pub struct Options;").unwrap();
    review(root, &mut ledger);
    let source = r"
        pub struct Options;
        impl Options {
            fn helper(&self) -> u8 { 1 }
            #[cfg_attr(test, unknown_generator)] pub(crate) fn restricted(&self) {}
            #[cfg(test)] #[unknown_generator] fn test_helper(&self) {}
        }
        mod hidden {
            use generators::Api as expose;
            pub struct Hidden;
            impl Hidden { #[expose(member)] fn helper(&self) {} }
        }
    ";
    fs::write(root.join("core/src/lib.rs"), source).unwrap();
    compile_review_library(root, &externs);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    replace(
        root,
        "core",
        "fn helper(&self) -> u8 { 1 }",
        "fn renamed(&self) -> u16 { 2 }",
    );
    compile_review_library(root, &externs);
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
}

#[test]
fn conditional_private_method_generators_retain_their_availability_inputs() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    let externs = [("generators", build_review_attributes(root))];
    fs::write(
        root.join("core/src/lib.rs"),
        r#"
        use generators::Api as expose;
        pub struct Options;
        impl Options {
            #[cfg_attr(feature = "a", expose(member))] fn helper(&self) {}
        }
    "#,
    )
    .unwrap();
    let consumer = "fn main() { let _: u16 = fixture::Options.generated(); }";
    compile_review_library(root, &externs);
    assert!(!compile_review_consumer(root, consumer).status.success());
    compile_review_library_with_cfg(root, &externs, &["feature=\"a\""]);
    assert!(compile_review_consumer(root, consumer).status.success());
    review_fixture_macro_inputs(root, &mut ledger);
    assert_eq!(ledger.supplements.len(), 1);
    save_ffi_fixture_review(root, &ledger);
    replace(root, "core", "feature = \"a\"", "feature = \"replacement\"");
    compile_review_library(root, &externs);
    let error = inventory(root, &ledger).unwrap_err();
    assert!(
        error.contains("stale supplement") || error.contains("changed or invalid supplement"),
        "{error}"
    );
    assert!(!check_review_fixture(root).status.success());
}

#[test]
fn enum_macro_documentation_changes_invalidate_explicitly_reviewed_generated_types() {
    for narrow in [
        "#[doc = \"narrow\"] pub enum Generated { A }",
        "pub enum Generated { #[doc = \"narrow\"] A }",
        "pub enum Generated { A(#[doc = \"narrow\"] u8) }",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        fs::write(
            root.join("core/src/lib.rs"),
            "pub struct Options; impl Options { pub fn generated(&self) -> u8 { 0 } }",
        )
        .unwrap();
        let outputs = discover_sources(root, &ledger)
            .unwrap()
            .into_iter()
            .filter(|d| d.surface == "core" && d.kind == "method")
            .collect();
        let wide = narrow.replace("\"narrow\"", "\"wide\"");
        let definitions = format!(
            "pub struct Options; macro_rules! choose {{ ({narrow}) => {{ impl Options {{ pub fn generated(&self) -> u8 {{ 0 }} }} }}; ({wide}) => {{ impl Options {{ pub fn generated(&self) -> u16 {{ 0 }} }} }}; }}"
        );
        fs::write(
            root.join("core/src/lib.rs"),
            format!("{definitions} choose!{{ {narrow} }}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        let consumer = "fn main() { let _: u8 = fixture::Options.generated(); }";
        assert!(compile_review_consumer(root, consumer).status.success());
        review_fixture_macro_inputs(root, &mut ledger);
        let supplement = ledger
            .supplements
            .iter_mut()
            .find(|s| s.anchor.contains("@macro:choose:Generated#macro"))
            .unwrap();
        assert!(supplement.expected_shape.contains("\"narrow\""));
        supplement.declarations = outputs;
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        assert!(check_review_fixture(root).status.success());
        let before = discover_sources(root, &ledger).unwrap();
        fs::write(
            root.join("core/src/lib.rs"),
            format!("{definitions} choose!{{ {wide} }}"),
        )
        .unwrap();
        compile_review_library(root, &[]);
        assert!(!compile_review_consumer(root, consumer).status.success());
        let after = discover_sources(root, &ledger).unwrap();
        assert!(before.iter().map(|d| &d.id).eq(after.iter().map(|d| &d.id)));
        assert!(
            inventory(root, &ledger)
                .unwrap_err()
                .contains("changed or invalid supplement")
        );
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn private_and_unreexported_c_linkage_exports_require_boundary_decisions() {
    use std::process::Command;

    for exports in [
        "#[unsafe(no_mangle)] extern \"C\" fn rumqttc_added() -> u32 { 7 } #[unsafe(no_mangle)] static rumqttc_added_value: u32 = 9;",
        "#[unsafe(export_name = \"rumqttc_added\")] extern \"C\" fn internal() -> u32 { 7 } #[unsafe(export_name = \"rumqttc_added_value\")] static INTERNAL_VALUE: u32 = 9;",
        "mod hidden { #[unsafe(no_mangle)] pub extern \"C\" fn rumqttc_added() -> u32 { 7 } #[unsafe(no_mangle)] pub static rumqttc_added_value: u32 = 9; }",
        "mod hidden { #[unsafe(export_name = \"rumqttc_added\")] pub(crate) extern \"C\" fn internal() -> u32 { 7 } #[unsafe(export_name = \"rumqttc_added_value\")] pub(crate) static INTERNAL_VALUE: u32 = 9; }",
    ] {
        let (directory, mut ledger) = fixture();
        let root = directory.path();
        let source = root.join("c/src/lib.rs");
        fs::write(&source, "pub struct Options;").unwrap();
        ledger
            .surfaces
            .iter_mut()
            .find(|s| s.name == "c")
            .unwrap()
            .roots = vec!["c::Options".into()];
        review(root, &mut ledger);
        let original = save_ffi_fixture_review(root, &ledger);
        fs::write(&source, format!("pub struct Options; {exports}")).unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let object = root.join("exports.o");
        let built = Command::new(&rustc)
            .args([
                "--edition=2024",
                "--crate-name=fixture",
                "--crate-type=lib",
                "--emit=obj",
            ])
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        // Link by C symbol names, without access to the producer's Rust paths.
        let consumer = root.join("consumer.rs");
        fs::write(&consumer, "unsafe extern \"C\" { fn rumqttc_added() -> u32; static rumqttc_added_value: u32; } fn main() { unsafe { assert_eq!(rumqttc_added(), 7); assert_eq!(rumqttc_added_value, 9); } }").unwrap();
        let executable = root.join(format!("consumer{}", std::env::consts::EXE_SUFFIX));
        let linked = Command::new(&rustc)
            .arg("--edition=2024")
            .arg(&consumer)
            .arg("-C")
            .arg(format!("link-arg={}", object.display()))
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            linked.status.success(),
            "{}",
            String::from_utf8_lossy(&linked.stderr)
        );
        assert!(Command::new(&executable).output().unwrap().status.success());
        let actual = inventory(root, &ledger).unwrap();
        assert_eq!(
            actual
                .iter()
                .filter(|d| d.surface == "c" && d.kind == "function")
                .count(),
            1
        );
        assert_eq!(
            actual
                .iter()
                .filter(|d| d.surface == "c" && d.kind == "static")
                .count(),
            1
        );
        assert!(
            !actual
                .iter()
                .any(|d| d.surface == "c" && d.kind == "module")
        );
        let error = validate(root, &ledger, &actual).unwrap_err();
        assert!(
            error.contains("unreviewed declaration") && error.contains("missing boundary decision"),
            "{error}"
        );
        assert!(!check_review_fixture(root).status.success());
        assert_eq!(
            fs::read(root.join(rumqttc_parity_audit::LEDGER)).unwrap(),
            original
        );
    }
}

#[test]
fn c_linkage_discovery_preserves_conditions_and_excludes_non_exports() {
    let (directory, mut ledger) = fixture();
    let root = directory.path();
    fs::write(root.join("c/src/lib.rs"), "pub struct Options;").unwrap();
    review(root, &mut ledger);
    let non_exports = r#"
        #![allow(dead_code)]
        pub struct Options;
        #[cfg(test)] #[unsafe(no_mangle)] extern "C" fn test_only() {}
        #[cfg_attr(test, unsafe(no_mangle))] extern "C" fn helper() {}
        #[doc = "no_mangle export_name"] static VALUE: u8 = 1;
        #[unsafe(link_section = ".data")] static PRIVATE: u8 = 1;
        #[cfg(feature = "a")] #[cfg_attr(not(feature = "a"), unsafe(no_mangle))]
        extern "C" fn impossible_export() {}
    "#;
    fs::write(root.join("c/src/lib.rs"), non_exports).unwrap();
    assert_eq!(ledger.declarations, inventory(root, &ledger).unwrap());
    fs::write(root.join("c/src/lib.rs"), format!("{non_exports} #[cfg_attr(feature = \"a\", unsafe(export_name = \"conditional_export\"))] extern \"C\" fn conditional() {{}}")).unwrap();
    let actual = inventory(root, &ledger).unwrap();
    let declaration = actual
        .iter()
        .find(|d| d.id.starts_with("c|c::conditional#function"))
        .unwrap();
    assert!(
        declaration
            .conditions
            .iter()
            .any(|cfg| cfg.contains("conditional_export") && cfg.contains("feature"))
    );
    assert!(
        !actual.iter().any(
            |d| d.id.contains("helper#function") || d.id.contains("impossible_export#function")
        )
    );
    validate(root, &ledger, &actual).unwrap_err();
}

#[test]
fn unrelated_private_generators_do_not_block_standard_assertions() {
    for assertion in [
        "::core::assert!(true)",
        "::std::assert!(true)",
        "core::assert!(true)",
        "assert!(true)",
    ] {
        for scope in ["root", "api"] {
            let (directory, mut ledger) = fixture();
            let root = directory.path();
            let source = if scope == "root" {
                format!("pub struct Options; const _: () = {assertion};")
            } else {
                format!("pub struct Options; pub mod api {{ const _: () = {assertion}; }}")
            };
            fs::write(root.join("core/src/lib.rs"), &source).unwrap();
            review(root, &mut ledger);
            save_ffi_fixture_review(root, &ledger);
            fs::write(
                root.join("core/src/lib.rs"),
                format!("{source} mod hidden {{ macro_rules! noop {{ () => {{}}; }} noop!(); }}"),
            )
            .unwrap();
            compile_review_library(root, &[]);
            assert_eq!(
                ledger.declarations,
                inventory(root, &ledger).unwrap(),
                "{scope}: {assertion}"
            );
            assert!(check_review_fixture(root).status.success());
        }
    }
}
