use std::path::PathBuf;
use std::process::Command;

use rumqttc_parity_audit::{check_markdown, inventory, read_ledger, validate};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn repository_audit_executes_at_every_boundary() {
    let root = root();
    let ledger = read_ledger(&root).unwrap();
    let actual = inventory(&root, &ledger).unwrap();
    for surface in &ledger.surfaces {
        let count = actual.iter().filter(|d| d.surface == surface.name).count();
        assert!(count > 0, "empty surface {}", surface.name);
        println!("{}: {count} declarations", surface.name);
    }
    validate(&root, &ledger, &actual).unwrap();
    check_markdown(&root, &ledger).unwrap();
}

#[test]
fn published_core_contains_no_repository_audit_test_or_dependency() {
    let output = Command::new(env!("CARGO"))
        .args([
            "package",
            "--locked",
            "--offline",
            "--allow-dirty",
            "--list",
            "-p",
            "rumqttc-wrapper-core-next",
        ])
        .current_dir(root().join("native-wrappers"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let files = String::from_utf8(output.stdout).unwrap();
    assert!(files.lines().any(|line| line == "src/lib.rs"));
    assert!(!files.contains("parity_inventory"));
    assert!(!files.contains("parity-audit/"));
    assert!(!files.lines().any(|line| line == "parity.json"));
    let manifest =
        std::fs::read_to_string(root().join("native-wrappers/wrapper-core/Cargo.toml")).unwrap();
    assert!(!manifest.contains("rumqttc-parity-audit"));
    // The independent backend-boundary test still needs syn as a dev dependency.
    let library_dependencies = manifest.split("[dev-dependencies]").next().unwrap();
    assert!(!library_dependencies.contains("syn ="));
}

#[test]
fn command_fails_for_absent_repository_instead_of_reporting_coverage() {
    let directory = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_rumqttc-parity-audit"))
        .args(["check", "--root"])
        .arg(directory.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    assert!(String::from_utf8_lossy(&output.stderr).contains("parity audit failed"));
}
