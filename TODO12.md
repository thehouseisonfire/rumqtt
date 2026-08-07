# Split per-version `mqttbytes` into their own crates and make all three `no_std`

## Objective

Extract the version-specific MQTT codecs that currently live inside the client
crates into standalone crates, mirroring `mqttbytes-core-next`, and make all
three `mqttbytes` crates (`core`, `v4`, `v5`) build as `no_std + alloc`.

## 1. Split the version codecs into their own crates

- [ ] Create `mqttbytes-v4-next` (lib `mqttbytes_v4`) containing
      `rumqttc-v4/src/mqttbytes/` as its crate root (`Error`, re-exports from
      core, `v4/` packet modules, `codec`).
- [ ] Create `mqttbytes-v5-next` (lib `mqttbytes_v5`) containing
      `rumqttc-v5/src/mqttbytes/` the same way.
- [ ] Each new crate depends on `mqttbytes-core-next` (package
      `mqttbytes-core`, lib `mqttbytes_core`), like the client crates do today.
- [ ] Add both to the workspace `members` list.
- [ ] Point every `crate::mqttbytes::*` ref inside the moved modules at the new
      external crate or `mqttbytes_core` (they're tests-only today).

## 2. Keep the client public API unchanged

- [ ] `rumqttc-v4-next` replaces `mod mqttbytes;` with a dependency on
      `mqttbytes-v4-next` plus `pub use mqttbytes_v4 as mqttbytes;`.
- [ ] `rumqttc-v5-next` does the same with `mqttbytes_v5`.
- [ ] Preserve the existing flat re-exports (`pub use mqttbytes::v4::*`,
      `pub use mqttbytes::*`) so `rumqttc::mqttbytes::…` and friends keep
      working.

## 3. Make all three `mqttbytes` crates `no_std`

- [ ] Add `#![no_std]` + `extern crate alloc;` to each lib.
- [ ] `mqttbytes-core`: swap `std::slice::Iter`, `std::str::Utf8Error`,
      `std::str::from_utf8` in `src/primitives.rs` for `core::` equivalents;
      flip `bytes`/`thiserror` to `std`-disabled with `alloc`.
- [ ] `mqttbytes-v4`/`v5`: same `core::` swaps (incl. `core::convert`
      for `TryFrom`); gate the `Io(#[from] std::io::Error)` error variant behind
      a `std` feature (its only consumer, the tokio `framed` codec layer, is
      std-only anyway); ensure `Vec`/`String` packet fields use `alloc`.

## 3. Tidy up and verify

- [ ] Discard any helpers made redundant by the extraction; confirm the shared
      `primitives` and `ping` stay only in `mqttbytes-core`.
- [ ] Run `cargo fmt --all`, `cargo check --workspace`, and the full
      `rumqttc-v4-next`/`rumqttc-v5-next` test suites.
- [ ] Check both crates build for a `no_std` target (e.g. `thumbv7em-none-eabihf`)
      with the `std` feature off.
- [ ] Publish all three crates in lockstep (`0.34.0-alpha` family) and note the
      re-exports in `CHANGELOG.md`.

## Non-goals

- Do not merge version codecs back into `mqttbytes-core`; v4/v5 packet structs
  and reason-code enums stay version-scoped.
- Do not make the client crates (`rumqttc-*`, `rumqttc-core`) `no_std`; they are
  Tokio/OS-bound by design.