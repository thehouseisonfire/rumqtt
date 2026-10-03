# C Wrapper Complete Broker Acknowledgement Results

## Goal

Preserve MQTT 5 terminal broker acknowledgement contents in tracked operation
results: packet kind and identifier, exact reason codes, Reason String, User
Properties, and per-filter results. Expose recovery distinctions without
changing success or failure classification.

This task covers wrapper-core ownership and C exposure. Keep Python and
JavaScript bindings compatible with the shared-core changes; exposing the new
details through those language APIs is a separate follow-up, not a completion
requirement here.

## Current foundation and feasibility

Rust notices in `rumqttc-v5/src/notice.rs` return complete terminal PUBACK,
rejected PUBREC, PUBCOMP, SUBACK, and UNSUBACK values. Successful QoS 2 PUBREC
is intermediate and is not retained in the terminal publish notice.
Wrapper-core's mappings in `src/backend/v5.rs` reduce these to coarse completions
or broker reason errors.
For example, successful PUBACK `NoMatchingSubscribers` loses its distinct code.

C's current `observe_completion` path turns terminal operation failures into
errors before existing completion accessors can inspect them. Merely adding
fields to successful `Completion` variants would leave rejected publishes
without their acknowledgement details. Completion observation also clones the
stored result, so new property-bearing results need shared immutable ownership
to avoid repeated deep copies and temporary borrowed storage.

This is feasible through richer owned operation outcomes. It does not require
publishing a second raw packet stream or giving applications QoS-state control.
Use `docs/spec/mqtt-v5.0.md` and its requirement index to preserve legal packet
properties and their optional presence.

## Implementation requirements

- [x] Add owned acknowledgement details to wrapper-core results and preserve
  them before the backend mapping discards the native packet.
- [x] Keep details available on successful and broker-rejected operations.
  Retain an immutable terminal outcome containing the existing success/error
  result and optional owned acknowledgement details, or an equivalent model.
  Add a detail observation path that does not propagate the operation's broker
  rejection before exposing its payload. Existing poll/wait functions retain
  their success/error behavior. Distinguish pending operations from terminal
  outcomes with or without an ACK; observer timeouts never replace the outcome.
- [x] Retain exact successful reason distinctions and recovered QoS 2 outcomes.
  Represent absent packets explicitly for QoS 0 and locally failed operations.
- [x] Preserve native per-filter reason-code order and cardinality for
  SUBACK/UNSUBACK. Reason String and User Properties belong to the ACK packet,
  not individual filters; retain them once at packet level. Preserve ordered
  duplicate User Properties and absent versus present-empty Reason Strings.
- [x] Add additive C completion/detail accessors and, where useful, copy APIs.
  Borrowed views remain valid for the documented owner lifetime, including
  after client destruction. Views must reference immutable storage retained by
  that owner, never transient native events or temporary cloned observations.
  Copy APIs must produce independent owned data that remains usable after the
  source owner is destroyed.
- [x] Preserve existing completion kinds, per-filter accessors, error categories,
  and delivery status. New details supplement existing behavior.
- [x] Define available v4 details explicitly without inventing v5 reason fields
  or per-filter UNSUBACK results that v4 does not carry.
- [x] Bound retained data by actual packet limits and operation ownership. Avoid
  duplicating large properties into both events and completions unnecessarily.
  Share immutable acknowledgement storage across repeated observations and
  cloned handles, for example with `Arc`, rather than deep-copying properties
  on each poll or accessor call. Release it when its final owner is dropped;
  do not introduce an unbounded driver-owned completed-result history.
- [x] Keep response properties out of automatic logs, formatted errors, and
  `Debug` output, including output from enclosing outcomes and handles. Expose
  them as explicit caller-owned observations, not as a new authentication
  authority.
- [x] Define completeness as the contents of the terminal ACK available through
  the native notice. QoS 1 exposes PUBACK; QoS 2 exposes rejected PUBREC or
  terminal PUBCOMP, including the native recovered-outcome distinction.
  Successful PUBREC and intermediate PUBREL remain internal. Do not retain
  handshake history or change native QoS transitions for this task; full raw
  packet tracing is a separate scope.

## Verification and completion

- [x] Round-trip exact acknowledgement contents for successful and rejected
  operations, mixed subscription results, and QoS 2 recovery. Cover PUBACK
  `NoMatchingSubscribers`, rejected PUBREC, ordinary versus recovered PUBCOMP,
  and packet-level properties on SUBACK/UNSUBACK. Assert existing success/error
  classification, completion kinds, and per-filter behavior remain unchanged.
- [x] Verify retention after client destruction, copy after owner destruction,
  wrong-kind access, absent fields, and initialized outputs on accessor failure.
  Verify rejected-operation details remain accessible after legacy poll/wait
  reports the rejection and after its returned error object is destroyed.
  Cover repeated observations, cloned handles, observer timeouts, stable
  borrowed views, shared property storage, and final-owner release.
- [x] Verify property contents are omitted from `Debug`, error formatting, and
  automatic logs. Run the JavaScript and Python completion/error checks against
  the shared-core changes without adding new public language API requirements.
- [x] Update C header/exports, examples, README, root `CHANGELOG.md`, and
  `PARITY.md` to distinguish coarse results from complete acknowledgements.

Complete when terminal broker information observable through Rust notices is
also accessible through C without reading a parallel raw-event channel.


## Implementation evidence

Wrapper-core retains an `Arc<TerminalOutcome>` per completed operation. Its
`try_outcome()` observation exposes both the legacy result and optional immutable
`BrokerAcknowledgement`, including on broker rejection. Native mappings move
packet properties into the outcome and preserve identifiers, exact codes,
filter order, and the recovered QoS 2 distinction. Existing waits continue
projecting their original result; the registry removes completed operations.

C adds size-versioned acknowledgement metadata and completion-owned reason,
filter-code, and User Property accessors. Views remain valid until completion
handle destruction, including after client/error destruction. Existing string
copy helpers produce caller-owned copies. No raw packet channel, intermediate
handshake history, new feature, or language-level Python/JavaScript API is added.

Core completion tests verify exact mappings, shared snapshots, observer deadlines,
redacted formatting, and final-owner release. Rust FFI round trips and the portable
`native_acknowledgement_results` consumer verify decoded packet contents,
identifiers, mixed filter results, rejected PUBREC, ordinary/recovered PUBCOMP,
initialized outputs, bounded record writes, stable views, and copied-string
lifetimes. The tracing test captures a real rejected ACK with private properties
and verifies that their contents are absent from automatic output.

Verified on Linux with:

- `cargo test --manifest-path native-wrappers/Cargo.toml --workspace --locked`
  and core/C tests with `--no-default-features`.
- Core tracing integration tests with `--features tracing`, final core unit tests,
  and C `ffi_behavior` tests.
- Root workspace check; wrapper workspace check with all targets; formatting,
  `git diff --check`, and wrapper workspace Clippy with
  `RUSTFLAGS='-D warnings -W clippy::pedantic -W clippy::nursery'`, including
  core/C without default features and the combined optional-feature configuration.
- `cargo hack test --each-feature --exclude-all-features` for wrapper-core:
  all 18 configurations passed. Strict `cargo hack clippy --all-targets
  --each-feature --exclude-all-features` for wrapper-core and C: all 33
  configurations passed. Combined optional-feature core tests also passed.
- `native-wrappers/c/tests/abi/check.sh all`, ABI helper tests, broker-fixture
  helper tests, and native CTest: all 56 passed with TLS, proxy, and SCRAM features.
  The optional-error scanner covers all 118 APIs; the six TLS-profile/capability
  APIs now have explicit NULL-error success/failure calls in the native consumer.
- Rebuilt JavaScript addon: Node tests, TypeScript checks, and broker behavior
  suite. Rebuilt Python extension: 118 unit/API tests passed, 69 broker/lifecycle
  tests passed; fixture-dependent tests are skipped in the standalone unit run.

Strict pedantic/nursery warnings and the TLS optional-error coverage gaps have
been resolved. CI now checks the wrapper workspace without applying lint fixes.
The mosquitto-dependent Rust Will test remains ignored in the normal workspace
run. No historical release comparison or non-Linux platform execution is claimed.
