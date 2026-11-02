# Enforce Rust-to-Core-to-C Parity Review

Status: implemented. Local Linux validation passed; remote platform CI execution remains separate.

## Goal

Make the repository's parity audit execute reliably and detect meaningful
exposure gaps, rather than equating reviewed method names with complete parity.

## Foundation and scope

Before this change, `native-wrappers/wrapper-core/tests/parity_inventory.rs`
returned early unless the repository contained `TODO15.md`, which was absent.
Its syntax inventory selected only mutable options methods, client methods,
and a few enum variant names. Builder policy, public fields, semantic restrictions,
and the actual core-to-C projection were outside that inventory.

Keep `PARITY.md` as a human-reviewed contract with reasons and evidence. This
task improves review enforcement without expanding the public wrapper API or
adding runtime work. It must not expose every Rust internal API merely to make
an inventory exhaustive, or require completion of deferred features.

The inventory requires decisions when declarations change. It does not prove
semantic equivalence: conversion bodies, defaults, validation, and ownership can
change without changing declarations. Focused behavior tests provide that evidence.

## Stage 1: Restore execution and reconcile existing coverage

- [x] Remove the planning-file sentinel. Identify repository source availability
  from required manifests/source roots. Repository audit mode must fail on absent
  or partially missing inputs. Run it explicitly in this checkout and CI; document
  and separately verify packaged-crate behavior when repository sources are absent.
  Any packaged-source skip must be explicit and must not count as parity coverage.
- [x] Reconcile current missing/stale entries through review, including method
  names contaminated by explanatory argument text. Do not regenerate accepted
  entries automatically or add fake declarations to satisfy existing rows.
- [x] Strengthen the existing dedicated CI parity step to report actual inventory
  counts and decision checks. Require nonzero native inventories for both protocols;
  extend this assertion to core and C when stage 3 adds those inventories. Counts
  prove execution, not completeness or semantic correctness.

## Stage 2: Record decisions at both boundaries

- [x] Introduce one authoritative structured decision ledger. Store statuses,
  mappings, restrictions, explanations, and evidence there; generate corresponding
  `PARITY.md` tables or validate their projection. Keep additional narrative in
  Markdown without independently maintaining duplicate decision records.
- [x] Record native-to-core and core-to-C decisions separately. Use supported,
  partial, pending, intentionally excluded, and not applicable dispositions with
  concrete source/target symbols or an omission rationale. Distinguish declaration
  coverage, behavior evidence, and platform/performance validation limits.
- [x] Record partial argument domains, omitted fields, defaults, coarse event
  classification, and managed-driver boundaries explicitly. A supported method
  does not imply every argument or returned observation is preserved. Intentional
  exclusions are valid reviewed outcomes, not implementation requirements.
- [x] Use crate/module/type/member identities with protocol, boundary, and relevant
  feature context. Keep synchronous and asynchronous declarations distinct; record
  reviewed equivalence explicitly. Include normalized signatures, field types,
  and variant payloads so relevant declaration changes require renewed review.
  Keep prose, source line numbers, and planning-file names out of identity keys.
- [x] Validate missing, stale, duplicate, and dangling decision/mapping entries.
  New or changed entries require review; discovery must never accept them by
  updating a baseline automatically. Evidence references identify meaningful tests
  and symbols, rather than treating a nonempty reference as proof.

## Stage 3: Expand bounded discovery and mapping regressions

- [x] Define the audited client configuration, builder, operation, event, and
  observation surfaces explicitly. Discover additions within those surfaces,
  including relevant public fields and read-only accessors, without inventorying
  unrelated codec utilities or private implementation support.
- [x] Extend source traversal to relevant module/reexport paths and all declared
  feature branches. Retain const/async method handling. Use explicit reviewed
  supplements for macro-generated or otherwise unresolved declarations, with their
  source and scope recorded. Do not build a general Rust name resolver or macro
  expander; document remaining discovery limits and test the supported patterns.
- [x] Inventory corresponding core and C surfaces and require decisions for each
  audited source member, including explicit omission or accessor-based mappings.
  Reuse existing C header generation/export checks and their declaration data
  where practical; they establish declarations and ABI consistency, not semantics.
- [x] Reuse existing behavior fixtures and add focused mapping regressions for
  retained values, optional presence, property ordering, restrictions, and ownership
  where coverage is missing. Adding a source field without a decision must fail the
  inventory; removing its supported conversion while keeping declarations intact
  must fail a behavior test. Do not claim arbitrary conversion-body changes are
  detected by the inventory or that every semantic property can be inferred.

## Review scope decisions

Reconcile [TODO41.md](TODO41.md) through [TODO51.md](TODO51.md) against the current
implementation and evidence. TODO41 and TODO43 through TODO48 are implemented
with their documented validation limits. TODO42 is experimental opt-in readiness
with an unmet performance gate; TODO49 is parked, TODO50 is implemented with
platform execution outstanding, and TODO51 remains planned. Preserve these
distinctions rather than labeling every item pending or requiring deferred work.
Planning documents provide context, not executable audit inputs.

Preserve the following boundaries when recording the remaining differences:

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
  remain outside the C client contract. Record the parked TLS-layer extension in
  TODO49 separately from the implemented maintained file-store bridge in TODO50.
- Runtime, platform, and performance support claims require separate execution
  evidence. Limited validation does not by itself make an implemented mapping
  pending; record its evidence limits separately.

## Verification and completion

- [x] Demonstrate that adding an unreviewed setter, builder option, observation
  field, or variant fails the repository audit. Removing/renaming a reviewed API
  must expose stale entries; changing an audited signature/type must require
  renewed review. Test duplicate keys, same-named unrelated types, and separate
  synchronous/asynchronous declarations using isolated fixtures.
- [x] Exercise operation without planning files, absent/partial source trees,
  packaged-crate behavior, and CI enforcement. Cover disabled feature branches,
  nested/reexported declarations, and reviewed supplements within the declared
  discovery scope. Assert actual work and deliberate failure paths.
- [x] Demonstrate independent failures for an unreviewed core field and for a
  supported field lost by its C conversion. Reuse `recovery_pending` or an equally
  concrete mapping; preserve existing C ABI layouts while testing newer accessors.
- [x] Update `PARITY.md`, contributor/testing documentation, and CI commands.
  Run the repaired inventory, mapping tests, and existing header/export checks;
  record what they establish and their remaining limits accurately.

Deliver the stages in order so execution repair can land before the broader
inventory migration. Complete when this checkout cannot silently bypass review,
the declared surfaces have decisions at both boundaries, and focused mapping
tests enforce the supported behaviors with their remaining limits documented.


## Implementation and local validation

The unpublished `rumqttc-parity-audit` workspace member replaces the repository
integration test. `wrapper-core/parity.json` is the authoritative ledger;
`PARITY.md` contains its checked generated projection and additional narrative.
The ledger is excluded from the published crate, and package tests verify that
repository auditing and ordinary crate testing remain separate.

The audit checks 4,311 declarations across seven source surfaces, including
nonzero v4, v5, core and C inventories, 407 boundary decisions, and 28 reviewed
macro supplements. Exact supported fields and broader partial managed-interface
projections are distinguished. These counts prove work and review coverage within
the declared scope; they do not prove semantic equivalence or platform support.
See [the tool workflow and limits](native-wrappers/parity-audit/README.md).

Local Linux verification during implementation passed:

- The repository `check` command and 21 audit tests, including package policy,
  declaration mutations, missing inputs, macros, aliases, signatures/bounds,
  independent core-field decisions, CLI work reporting and stale documentation.
- The isolated conversion mutation: the baseline C projection test passed;
  forcing `recovery_pending` to false preserved declaration coverage and failed
  the real retained-value assertion. The checkout's conversion was untouched.
- Full wrapper-core/C Cargo tests and documentation tests. Default ignored
  broker-dependent tests are not counted as execution evidence.
- `cargo hack check --each-feature --exclude-all-features` for wrapper-core/C:
  all 40 feature compilation profiles passed.
- Existing C `ffi-header` and `exports` checks, native-workspace formatting,
  and the audit tool's Clippy checks with pedantic/nursery warnings denied.
- Rust 1.88 compilation of the audit tool and all its test targets.

CI invokes the audit with counts, runs its fixtures/package checks, and exercises
conversion mutation on Linux. Other platform execution and historical ABI or
performance evidence retain their existing limits; deferred features remain
outside this task's completion requirements.
