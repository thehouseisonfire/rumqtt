# Enforce Rust-to-Core-to-C Parity Review

Status: planned.

## Goal

Make the repository's parity audit execute reliably and detect meaningful
exposure gaps, rather than equating reviewed method names with complete parity.

## Foundation and scope

`native-wrappers/wrapper-core/tests/parity_inventory.rs` returns early unless
the repository contains `TODO15.md`; that file is absent. Its syntax inventory
also selects only mutable options methods, client methods, and a few enum
variant names. It excludes builder policy, public fields, semantic restrictions,
and the actual core-to-C projection.

Keep `PARITY.md` as a human-reviewed contract with reasons and evidence. This
task improves review enforcement; it must not expose every Rust internal API
merely to make an inventory exhaustive.

## Implementation requirements

- [ ] Remove the planning-file sentinel. Identify repository source availability
  from required manifests/source roots, always run in this checkout and CI, and
  fail on partially missing source trees. Handle packaged-crate tests explicitly
  rather than silently passing a repository audit when its inputs are unavailable.
- [ ] Add a dedicated CI invocation whose output proves inventory work occurred,
  with nonzero inventories for both protocols and C. Verify any documented
  packaged-source skip separately; never report it as successful parity coverage.
- [ ] Inventory public options/client builders and relevant public observation
  structs/fields as well as existing operations/events. Identify types with fully
  qualified keys to avoid conflating unrelated same-named types. Account for
  feature-gated branches, const/async methods, macros, and relevant reexports;
  use explicit reviewed supplements where source syntax alone is insufficient.
- [ ] Track native-to-core and core-to-C decisions separately, including partial
  mappings, argument restrictions, omitted fields, coarse event classification,
  and intentional managed-driver boundaries. A method marked supported must not
  imply every argument and observation is preserved automatically.
- [ ] Use a structured inventory or stable machine-readable companion where the
  existing Markdown table cannot express field mappings cleanly. Preserve human
  explanations and test references; validate duplicate, stale, and missing entries.
- [ ] Cover the concrete new plans [TODO41.md](TODO41.md) through
  [TODO51.md](TODO51.md) as pending/partial decisions until implemented. Keep
  intentional exclusions explicit: arbitrary Rust-object injection, mutable
  native state, unbounded queues, and disabling wrapper validation are not parity
  requirements. Runtime or platform support claims need separate execution evidence.
- [ ] Reuse C header generation/export checks as declaration coverage, not proof
  of semantic parity. Add focused field/behavior mapping tests for retained
  values and restrictions that a textual inventory cannot establish.
- [ ] Avoid brittle line-number or planning-file dependencies and automatic
  acceptance of new APIs. New entries require a reviewed decision with concrete
  source mapping or omission rationale and meaningful verification evidence.

## Review scope decisions

The actionable additions are separated into TODO41 through TODO51. Preserve
the following boundaries when recording the remaining audit differences:

- Caller-driven pumping and runtime injection remain the conditional extension
  in [TODO33.md](TODO33.md); readiness notification does not establish either.
- Custom retry decisions and pause/resume/request-attempt already have a scoped
  follow-up in [TODO34.md](TODO34.md). Do not duplicate it or expose mutable
  event-loop state to obtain application scheduling control.
- Session abandonment uses [TODO36.md](TODO36.md); live options updates stay
  within [TODO35.md](TODO35.md). Public Rust fields alone are not evidence that
  arbitrary mutation preserves derived protocol state.
- Keep bounded nonzero channels, finite publish accounting, and checked MQTT 5
  UTF-8 payloads. Reauthentication properties continue to come from the existing
  callback authority; a second competing properties source is unnecessary.
- General Rust-object/provider injection and standalone codec/session editing
  remain outside the C client contract. The portable TLS-layer extension in
  TODO49 and maintained file-store bridge in TODO50 address concrete use cases.

## Verification and completion

- [ ] Demonstrate that adding an unreviewed setter, builder option, observation
  field, or variant fails the repository audit. Removing/renaming a reviewed API
  must expose stale entries. Test duplicate keys and same-named unrelated types.
- [ ] Exercise the missing-sentinel regression, partial source trees, all relevant
  feature branches, packaged-crate behavior, and CI enforcement. Assert inventory
  counts/work rather than trusting a green test that may have returned early.
- [ ] Show a core field omitted by a C conversion is flagged for review and has
  a semantic regression when marked supported. Do not require bindings for
  unrelated codec utilities or private implementation support.
- [ ] Update `PARITY.md`, contributor/testing documentation, and CI commands.
  Run the repaired inventory, mapping tests, and existing header/export checks;
  record what they establish and their remaining limits accurately.

Complete when this checkout cannot silently bypass parity review and supported,
partial, pending, and intentionally excluded behavior is explicit at both boundaries.
