# TODO: Merge the MQTT Session File-Store Adapters

## Goal

Replace the separately published `rumqttc-v4-session-store-file-next` and
`rumqttc-v5-session-store-file-next` adapter crates with one public package:

```text
rumqttc-session-store-file-next
```

The merged crate must:

- allow MQTT 3.1.1 support, MQTT 5 support, or both in one dependency graph;
- keep the v4 and v5 Rust types unambiguous;
- deduplicate all protocol-independent adapter behavior;
- preserve the existing v4 and v5 on-disk namespaces and key encodings;
- avoid making either MQTT client dependency mandatory when its feature is disabled;
- retain a clear migration path for users of the two old packages.

This task concerns the MQTT adapters. It must not be blocked on the broader generic-core
redesign in `TODO8.md`; the merged adapter may initially depend on the existing core crate.

## Target Package and Features

Create one package with additive Cargo features:

```toml
[package]
name = "rumqttc-session-store-file-next"

[features]
default = []
v4 = ["dep:rumqttc-v4"]
v5 = ["dep:rumqttc-v5"]

[dependencies]
rumqttc-v4 = {
    package = "rumqttc-v4-next",
    path = "../../rumqttc-v4",
    version = "...",
    default-features = false,
    optional = true,
}
rumqttc-v5 = {
    package = "rumqttc-v5-next",
    path = "../../rumqttc-v5",
    version = "...",
    default-features = false,
    optional = true,
}
```

Feature requirements are strict:

- `v4` and `v5` are independent and additive.
- Enabling both is supported and tested. Do not introduce mutually exclusive feature checks.
- Enabling neither must compile and expose only feature-independent APIs, or produce a deliberate,
  documented compile-time error. Prefer a compilable featureless crate so documentation and
  dependency tooling can inspect it without selecting a protocol.
- The two client crates must use distinct dependency aliases. Both library targets are named
  `rumqttc`, so importing both through the same Rust dependency name is not viable.
- Protocol features must not silently enable unrelated transport or TLS features in either client.

Decide explicitly whether either protocol feature should be enabled by default before release.
Prefer no default protocol feature: an accidental default would add a client dependency and could
make downstream feature selection less predictable.

## Public API Shape

Expose protocol-specific APIs through modules:

```rust
#[cfg(feature = "v4")]
pub mod v4;

#[cfg(feature = "v5")]
pub mod v5;
```

The intended use must remain clear when both protocols are enabled:

```rust
use rumqttc_session_store_file::v4::SessionFileStore as V4SessionFileStore;
use rumqttc_session_store_file::v5::SessionFileStore as V5SessionFileStore;
```

Each module should expose the protocol-bound surface currently offered by its adapter:

- `SessionFileStore`;
- `SessionFileStoreError`;
- `KeyEncodeError` if it remains protocol-module-local;
- `encode_session_store_key`;
- `session_filename`;
- protocol-appropriate operator inspection, quarantine, clear, and cleanup operations.

Do not expose a root-level `SessionFileStore` alias when both features are enabled. Conditional
root aliases whose meaning changes with the selected feature are also discouraged because they make
examples, diagnostics, and generated documentation configuration-dependent.

Shared filesystem types may be re-exported once from the crate root if they have exactly the same
semantics for both protocols. Do not duplicate nominally different public types merely to preserve
the old module layout.

## Internal Deduplication Design

Move the following behavior into one private implementation layer:

- store construction and option forwarding;
- inspection, quarantine, operator clear, and stale-temporary cleanup;
- stable key framing;
- legacy example filename construction;
- core load/save/clear orchestration;
- conversion of core failures into adapter failures;
- common tests for key hashing, namespace behavior, cleanup, and filesystem failure handling.

Use a small sealed protocol descriptor or an equivalent private abstraction. It should carry only
the facts that actually vary:

- protocol tag (`4` or `5`);
- namespace (`v4` or `v5`);
- protocol display name for diagnostics;
- the protocol crate's session key, persisted-session codec, session-store trait, and error types.

Rust cannot abstract directly over two unrelated foreign traits merely because their method shapes
match. Keep the foreign-trait implementations in `v4` and `v5` modules, but make them thin shims over
shared private operations. Avoid a large macro that duplicates the entire adapter invisibly. A small
macro for the final foreign-trait glue is acceptable only if a trait-based helper would be more
complex or less readable.

The concrete protocol wrappers must remain local types so implementations of both foreign
`SessionStore` traits satisfy coherence rules. Do not attempt to implement foreign traits directly
for the core store.

## On-Disk Compatibility

Merging packages must not migrate or reinterpret existing checkpoints.

Preserve exactly:

- key format version `1`;
- protocol tag `4` for MQTT 3.1.1 and `5` for MQTT 5;
- namespace component `v4` and `v5` respectively;
- length-prefix encoding and field order for `scope` and `client_id`;
- full BLAKE3-derived canonical filenames;
- the current envelope encoding;
- exact legacy-example filename detection behavior during the compatibility window.

Add golden-vector tests containing fixed keys and their expected encoded bytes, filenames, namespace
paths, and checkpoint fixtures. Tests that merely compare two calls to the same implementation are
not sufficient evidence of compatibility.

Opening v4 and v5 stores against the same configured root must create and use separate namespaces.
A v4 store must never load, inspect as canonical, clear, or quarantine a v5 checkpoint, and vice
versa.

## Migration and Release Plan

1. Add the merged package to the `session-store-file` workspace without removing the existing
   adapters.
2. Implement the shared layer and both protocol modules.
3. Run compatibility tests against fixtures produced by the existing adapter crates.
4. Convert examples and benchmarks to the merged package, including a program that opens and uses
   both protocol stores in the same process.
5. Update repository documentation and `session-store-file/CHANGELOG.md` with dependency and import
   migrations.
6. Publish the merged package before retiring the old adapter package names.
7. For at least one announced compatibility release, either:
   - publish thin deprecated facade versions of both old packages that re-export the appropriate
     module from the merged crate; or
   - clearly announce a breaking package rename if facade publication is impossible because of
     package ownership or release constraints.
8. Remove the old workspace crates only after the migration route is tested from an external
   consumer fixture.

A facade package must enable exactly one merged-crate feature and preserve the old principal import
path where practical. It must not contain a second copy of adapter logic.

## Testing Matrix

Add CI or equivalent local verification for all of these configurations:

```bash
cargo check -p rumqttc-session-store-file-next --no-default-features
cargo test -p rumqttc-session-store-file-next --no-default-features --features v4
cargo test -p rumqttc-session-store-file-next --no-default-features --features v5
cargo test -p rumqttc-session-store-file-next --no-default-features --features v4,v5
cargo test --manifest-path session-store-file/Cargo.toml --workspace
```

Also run the repository's feature-matrix and lint equivalents where the merged package participates.
Tests must cover:

- v4-only compilation without the v5 dependency;
- v5-only compilation without the v4 dependency;
- simultaneous trait implementations and simultaneous store instances;
- distinct namespaces under one root;
- existing checkpoint loading for both protocols;
- cross-protocol isolation;
- shared helper behavior exercised through both public modules;
- public documentation under each feature combination;
- examples compiled with their declared required features.

Use `cargo tree -e features` or metadata-based assertions to confirm that a single-protocol build does
not activate the other client crate.

## Documentation and Examples

Provide:

- a v4-only example;
- a v5-only example;
- a dual-protocol example that imports both client crates and both adapter modules;
- a feature table showing which dependency declaration enables each protocol;
- migration snippets from both old package names;
- an explicit statement that the two namespaces and file formats remain protocol-isolated;
- an explanation that enabling both features does not make v4 and v5 session values interchangeable.

Update the root `CHANGELOG.md` if the client-facing dependency story is documented there, and update
`session-store-file/CHANGELOG.md` for the package consolidation itself.

## Completion Criteria

This work is complete only when:

- one public adapter package supports v4, v5, and v4+v5 builds;
- adapter logic has one shared implementation rather than two copied source files;
- both protocol-specific `SessionStore` implementations remain type-safe and unambiguous;
- existing canonical checkpoints retain byte-for-byte key and path compatibility;
- a dual-protocol external-consumer test compiles and runs;
- old-package migration is documented and, if chosen, facade packages are tested;
- benchmarks, examples, release scripts, README files, and changelogs no longer assume two primary
  adapter packages.

