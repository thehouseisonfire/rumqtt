# End-to-End `ValidatedTopic` TODO

## Objective

Make `ValidatedTopic` skip redundant PUBLISH topic-name validation in the MQTT
3.1.1 and MQTT 5 encoders without adding an `unchecked` public API, weakening
validation for ordinary `String` topics, or trusting mutable public `Publish`
fields.

The optimization must be conservative: provenance is created only by
`ValidatedTopic::new`, is carried separately from the public `Publish` packet,
and is downgraded to `Checked` whenever code cannot prove that the topic bytes
are unchanged. Deserialized, persisted, restored, replayed, and directly
constructed public packets start as `Checked`.

Do not merge the production optimization based only on the existing
`codec validation-cost` microbenchmark. After implementation, benchmark the
actual client-to-encoder path as described in the Validation section and retain
the change only if it clears the stated correctness and performance gates.

## Non-goals

- Do not add `unsafe`, `publish_unchecked`, or a public unchecked codec method.
- Do not add a trusted flag to the publicly mutable `Publish` structs.
- Do not make `Publish` fields private or otherwise break public packet
  construction.
- Do not persist validation provenance. Persistence is a trust boundary.
- Do not skip packet-size, QoS/DUP, packet-identifier, topic-alias, MQTT 5
  property, or MQTT UTF-8 validation unrelated to the proven topic bytes.
- Do not optimize incoming packet decoding; all network input remains checked.

## Required design

### 1. Define internal provenance

- [ ] Add a crate-private type in both clients, using the same name and
  semantics, for example:

  ```rust
  #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
  pub(crate) enum TopicValidation {
      #[default]
      Checked,
      Prevalidated,
  }
  ```

- [ ] Treat `Checked` as the default and fallback state. Avoid booleans: named
  states make downgrade sites visible in reviews and searches.
- [ ] Keep provenance outside `mqttbytes::{v4,v5}::Publish`; the public topic
  remains mutable and therefore cannot safely own a durable proof.
- [ ] Provide a small crate-private wrapper or sidecar, such as
  `OutgoingPublish { publish, topic_validation }`, only where it reduces
  accidental separation of a packet from its provenance.
- [ ] Do not expose this type from `lib.rs` except through the existing hidden,
  feature-gated benchmark instrumentation when required by benchmarks.

### 2. Make `ValidatedTopic` a complete proof

- [ ] Centralize outgoing topic-name validation so `ValidatedTopic::new` and
  the codec use the same rules or the same protocol-specific primitive. Do not
  maintain two hand-copied rule sets.
- [ ] Cover the complete MQTT UTF-8 rules already enforced by the codec,
  wildcard rejection, the two-byte MQTT string length limit, and v4's
  non-empty topic requirement.
- [ ] Preserve MQTT 5 semantics: an empty Topic Name is only usable in a
  PUBLISH with a valid nonzero Topic Alias, and negotiated alias limits remain
  state-level checks. Topic provenance must not bypass those contextual checks.
- [ ] Add boundary tests for empty topics, `+`, `#`, U+0000, forbidden UTF-8
  scalar values covered by the codec, exactly 65,535 encoded bytes, and 65,536
  encoded bytes.
- [ ] Confirm and document any observable change in when invalid topics are
  rejected. The public client API must continue rejecting them before enqueue.

### 3. Carry provenance only through the fresh outgoing request

- [ ] Extend the crate-private `RequestEnvelope`/scheduling metadata to carry
  PUBLISH topic provenance. Do not change the public `Request::Publish(Publish)`
  shape.
- [ ] In every sync and async `publish`, `try_publish`, and tracked-publish path:

  - ordinary string/Cow inputs produce `Checked`;
  - `ValidatedTopic` produces `Prevalidated`;
  - the bytes placed in `Publish.topic` must be exactly those validated by the
    constructor.

- [ ] Requests supplied through public raw-request/sender APIs produce
  `Checked`, even if their topic happens to be valid.
- [ ] Ensure overload errors still return the original public `Request` and do
  not expose internal provenance.
- [ ] Audit priority queues, bounded/unbounded channels, tracked notices, and
  scheduling classification so metadata cannot be dropped accidentally before
  state handling.

### 4. Downgrade at every trust boundary or topic mutation

- [ ] Add a single, obvious downgrade operation, for example
  `topic_validation.mark_checked()` or assignment to
  `TopicValidation::Checked`.
- [ ] Downgrade whenever topic bytes are assigned, replaced, normalized,
  deserialized, reconstructed, or obtained from storage.
- [ ] MQTT 5 topic-alias processing must downgrade if it clears an on-wire
  topic, restores a topic from an alias mapping, substitutes an automatic
  alias, or otherwise rewrites `Publish.topic`. Initially, downgrade whenever
  an alias rewrite path is entered unless unchanged bytes are trivially proven.
- [ ] Persisted sessions must not encode provenance. Restored and replayed
  publishes are always `Checked`.
- [ ] Retransmission and collision queues may conservatively become `Checked`
  after the first send. Do not widen the invariant merely to optimize replay.
- [ ] Any clone retains `Prevalidated` only when it clones the exact immutable
  topic bytes and no public mutation could have occurred between validation and
  cloning. Otherwise downgrade.
- [ ] Add comments at non-obvious downgrade sites explaining which mutation or
  trust boundary invalidates the proof.

### 5. Select the encoder path at the final write boundary

- [ ] Extend the crate-private state/event-loop output passed to the framed
  network writer with `TopicValidation`; do not put it in public `Packet`.
- [ ] For `Packet::Publish` plus `Prevalidated`, call a crate-private encoder
  specialization that skips only `validate_publish_topic_name`.
- [ ] For all other packets and all `Checked` publishes, use the existing
  checked encoder unchanged.
- [ ] Use a const-generic or separate private function so the ordinary checked
  encoder has no runtime validation-mode branch after optimization.
- [ ] Keep transactional writes on both paths: any remaining encoder error must
  leave the destination buffer unchanged.
- [ ] Keep the production specialization crate-private. The public
  `Publish::write` method always performs complete validation.

## Correctness tests

- [ ] Add v4 and v5 tests proving checked and prevalidated paths emit identical
  bytes for QoS 0/1/2, RETAIN, DUP where permitted, empty and non-empty payloads,
  maximum-length topics, and representative MQTT 5 properties.
- [ ] Test all sync, async, `try_*`, and tracked publish entry points with both
  raw and `ValidatedTopic` inputs.
- [ ] Test that invalid raw topics still fail before enqueue and never reach the
  network writer.
- [ ] Test that public `Publish` construction and mutation always take the
  checked encoder path.
- [ ] Test provenance downgrade during MQTT 5 manual and automatic alias
  rewriting, reconnect cleanup, and alias-only replay reconstruction.
- [ ] Test that persisted-session restore and every replay source use `Checked`.
- [ ] Test QoS retransmission and packet-identifier collision paths; byte output
  and notice behavior must remain unchanged.
- [ ] Add a test-only encoder counter under `bench-instrumentation`, if needed,
  to assert path selection. Do not infer selection only from successful output.
- [ ] Run:

  ```text
  cargo fmt --all
  cargo check --workspace
  cargo test -p rumqttc-v4-next
  cargo test -p rumqttc-v5-next
  cargo test -p benchmarks
  cargo hack --each-feature --exclude-all-features test -p rumqttc-v4-next -p rumqttc-v5-next
  ```

- [ ] Run the repository's CI-equivalent Clippy command. If unrelated existing
  warnings block it, record the exact command and failures rather than changing
  unrelated code in this work.

## Performance validation

The implementer shall benchmark the delta after the production path is wired.
The checked and prevalidated cases must exercise the same public client API,
request channels, event loop, state machine, encoder, and output sink. The
existing benchmark-only direct encoder bypass is useful as a ceiling, but is
not acceptance evidence by itself.

### 1. Extend the maintained harness

- [ ] Add a paired client publish-path benchmark to `benchmarks/`. It must offer
  `checked` and `validated` variants that differ only in whether the topic
  argument is `&str`/`String` or a previously constructed `ValidatedTopic`.
- [ ] Construct `ValidatedTopic` before timing. Do not include one-time
  validation in the repeated-publish measurement.
- [ ] Drive and drain the event loop so the benchmark measures successful
  encoding, not merely channel admission. Use the synthetic router or a
  deterministic in-process sink where possible.
- [ ] Prevent network variability from hiding the client-side delta in the
  primary benchmark. Separately run broker-backed TCP and TLS scenarios as
  realism checks.
- [ ] Alternate checked/validated execution order, use the same payload/topic
  allocations and QoS, include warmup, retain raw samples, and report medians
  plus a paired confidence interval or the repository runner's paired bootstrap
  comparison.
- [ ] Add smoke tests for stable JSON fields and rejection of zero rounds or
  messages. CI smoke runs verify functionality only and must not assert a
  performance ratio.

### 2. Required matrix

- [ ] Run both MQTT v4 and v5.
- [ ] Run QoS 0 and QoS 1.
- [ ] Run payload sizes 0 B, 64 B, 1 KiB, and 4 KiB.
- [ ] Run a short topic (approximately 12 bytes) and a long topic
  (approximately 160 bytes).
- [ ] Run at least 12 measured paired repetitions after at least one warmup
  repetition in an optimized build on an otherwise idle machine.
- [ ] Run existing `codec validation-cost` scenarios with the same topic and
  payload matrix to retain the theoretical upper-bound comparison.
- [ ] Run at least one broker-backed TCP and one TLS scenario for the most
  favorable tiny-message case. Report them separately from the deterministic
  client-path result.

Suggested commands should be documented in `benchmarks/README.md`; use a
symbol-preserving optimized profile only when profiling, and `--release` for
the acceptance measurements unless `benchmarks/BENCHMARKING.md` requires a
different project-standard profile.

### 3. Regression and size checks

- [ ] Compare the ordinary checked path before and after the implementation.
  Its median throughput must not regress by more than 1%; if the paired
  confidence interval crosses a 1% regression, investigate before merging.
- [ ] Compare release artifact sizes with `bench-instrumentation` disabled.
  Record exact files, compiler version, target, features, and byte counts. Do
  not use `cargo build --workspace` artifacts for this comparison because
  workspace feature unification enables benchmark instrumentation.
- [ ] Confirm through profiling or generated-code inspection that the checked
  path does not contain a runtime mode branch and that normal downstream builds
  do not include the benchmark-only specialization.

### 4. Acceptance criteria

Merge the production optimization only when all of the following hold:

- [ ] All correctness, feature-matrix, persistence, replay, and alias tests pass.
- [ ] No public API or supported packet-construction behavior is broken.
- [ ] The ordinary checked path shows no practically meaningful regression
  (maximum accepted median regression: 1%).
- [ ] At least one deterministic, realistic tiny-publish client-path scenario
  improves by 5% or more, with the paired confidence interval excluding zero.
- [ ] Results reproduce for both protocol crates, or any protocol-specific
  implementation is explicitly justified and documented.
- [ ] Larger payload results are reported even when neutral; do not generalize
  the tiny-message gain to all workloads.
- [ ] `CHANGELOG.md` documents the user-visible `ValidatedTopic` performance
  improvement and `benchmarks/README.md` documents reproduction commands and
  interpretation limits.

If these gates are not met, retain the `codec validation-cost` exploratory
benchmark, remove the unhelpful production provenance plumbing, and document
the negative result. Do not keep complexity based solely on a codec
microbenchmark win.
