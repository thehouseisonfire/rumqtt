#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
workspace_dir="$(cd "${script_dir}/.." && pwd)"
manifest_path="${workspace_dir}/Cargo.toml"
evidence_dir="${ATOMIC_BLOB_PACKAGE_EVIDENCE_DIR:-${workspace_dir}/target/package-validation}"
mkdir -p "${evidence_dir}"

run_logged() {
    local log_name="$1"
    shift
    local command_line
    printf -v command_line '%q ' "$@"
    printf 'command: %s\n' "${command_line}" | tee "${evidence_dir}/${log_name}.log"
    "$@" 2>&1 | tee -a "${evidence_dir}/${log_name}.log"
}

package_dirty=()
if [[ "${ATOMIC_BLOB_PACKAGE_ALLOW_DIRTY:-0}" == "1" ]]; then
    package_dirty=(--allow-dirty)
fi

run_logged package cargo package --locked --no-verify "${package_dirty[@]}" \
    --manifest-path "${manifest_path}" -p atomic-blob-store
run_logged package-list cargo package --locked --list "${package_dirty[@]}" \
    --manifest-path "${manifest_path}" -p atomic-blob-store

metadata_parser='import json,sys; data=json.load(sys.stdin);'
metadata_parser+=' print(next(p["version"] for p in data["packages"]'
metadata_parser+=' if p["name"] == "atomic-blob-store"))'
package_version="$(
    cargo metadata --locked --no-deps --format-version 1 --manifest-path "${manifest_path}" |
        python3 -c "${metadata_parser}"
)"
crate_archive="${workspace_dir}/target/package/atomic-blob-store-${package_version}.crate"
test -f "${crate_archive}"

temporary_root="$(mktemp -d)"
trap 'rm -rf "${temporary_root}"' EXIT
mkdir -p "${temporary_root}/package" "${temporary_root}/blocking/src" "${temporary_root}/tokio/src"
tar -xzf "${crate_archive}" -C "${temporary_root}/package"
package_source="../package/atomic-blob-store-${package_version}"

cat >"${temporary_root}/blocking/Cargo.toml" <<EOF
[package]
name = "atomic-blob-store-package-blocking-smoke"
version = "0.0.0"
edition = "2024"

[dependencies]
atomic-blob-store = { path = "${package_source}", default-features = false }
EOF
cat >"${temporary_root}/blocking/src/main.rs" <<'EOF'
use std::io::Cursor;

use atomic_blob_store::{
    AtomicBlobStoreOptions, BlobFormatIdentity, BlockingAtomicBlobStore, ENVELOPE_VERSION_V1,
};

fn main() {
    let root = std::env::args_os().nth(1).expect("root argument");
    std::fs::create_dir_all(&root).unwrap();
    let format = BlobFormatIdentity::new(b"PKGBLOCK", ".blob", ENVELOPE_VERSION_V1).unwrap();
    let store =
        BlockingAtomicBlobStore::open(root, "consumer", AtomicBlobStoreOptions::new(format))
            .unwrap();
    store.save(b"complete", b"value".to_vec()).unwrap();
    assert_eq!(store.load(b"complete").unwrap(), Some(b"value".to_vec()));
    let payload = b"streamed-value";
    store
        .save_from(
            b"stream",
            &mut Cursor::new(payload),
            u64::try_from(payload.len()).unwrap(),
        )
        .unwrap();
    let mut output = Vec::new();
    store.load_into(b"stream", &mut output).unwrap().unwrap();
    assert_eq!(output, payload);
    store.flush().unwrap();
    store.close().unwrap();
    store.close().unwrap();
}
EOF

cat >"${temporary_root}/tokio/Cargo.toml" <<EOF
[package]
name = "atomic-blob-store-package-tokio-smoke"
version = "0.0.0"
edition = "2024"

[dependencies]
atomic-blob-store = { path = "${package_source}", default-features = false, features = ["tokio"] }
tokio = { version = "1.40", features = ["io-util", "macros", "rt"] }
EOF
cat >"${temporary_root}/tokio/src/main.rs" <<'EOF'
use std::io::Cursor;

use atomic_blob_store::{
    AtomicBlobStoreOptions, BlobFormatIdentity, ENVELOPE_VERSION_V1,
    tokio::AtomicBlobStore,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let root = std::env::args_os().nth(1).expect("root argument");
    std::fs::create_dir_all(&root).unwrap();
    let format = BlobFormatIdentity::new(b"PKGTOKIO", ".blob", ENVELOPE_VERSION_V1).unwrap();
    let store = AtomicBlobStore::open(root, "consumer", AtomicBlobStoreOptions::new(format))
        .await
        .unwrap();
    store.save(b"complete", b"value".to_vec()).await.unwrap();
    assert_eq!(
        store.load(b"complete").await.unwrap(),
        Some(b"value".to_vec())
    );
    let payload = b"streamed-value";
    store
        .save_from(
            b"stream",
            &mut Cursor::new(payload),
            u64::try_from(payload.len()).unwrap(),
        )
        .await
        .unwrap();
    let mut output = Vec::new();
    store
        .load_into(b"stream", &mut output)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output, payload);
    store.flush().await.unwrap();
    store.close().await.unwrap();
    store.close().await.unwrap();
}
EOF

export CARGO_TARGET_DIR="${temporary_root}/target"
run_logged blocking-tree cargo tree --manifest-path "${temporary_root}/blocking/Cargo.toml"
if grep -Eq '(^| )tokio v' "${evidence_dir}/blocking-tree.log"; then
    echo "blocking-only packaged consumer unexpectedly resolved Tokio" >&2
    exit 1
fi
run_logged blocking-consumer cargo run --manifest-path "${temporary_root}/blocking/Cargo.toml" -- \
    "${temporary_root}/blocking-root"
run_logged tokio-consumer cargo run --manifest-path "${temporary_root}/tokio/Cargo.toml" -- \
    "${temporary_root}/tokio-root"
