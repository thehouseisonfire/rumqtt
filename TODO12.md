# Allocator-Free MQTT Codec Requirement

## Goal

Make `mqttbytes-core-next`, `mqttbytes-v4-next`, and `mqttbytes-v5-next` fully
usable on targets that provide neither `std` nor a global allocator. A caller
must be able to frame, validate, decode, inspect, construct, size, and encode
every MQTT 3.1.1 or MQTT 5 control packet supported by the corresponding crate
using borrowed input and caller-provided output storage only.

This is stricter than the current `no_std + alloc` support. Today the crates
declare `#![no_std]`, but their codec APIs and packet models depend on
`alloc::String`, `alloc::Vec`, `bytes::Bytes`, and `bytes::BytesMut`. The new
baseline must compile and provide a useful complete codec API with:

```text
#![no_std]
no global allocator
default features disabled
```

The existing allocation-backed API is valuable and should remain available to
ordinary client code. This requirement adds an allocator-free foundation; it
does not require `rumqttc-v4-next` or `rumqttc-v5-next` themselves to support
allocator-free operation.

## Required capability tiers

The public feature contract for all three crates must have three explicit
capability tiers. The two protocol crates add a fourth, opt-in framing tier:

| Enabled features | Required environment | Available API |
| --- | --- | --- |
| none | `core` only | Complete borrowed packet codec and slice-based framing |
| `alloc` | `core` plus a global allocator | Owned packet types and allocation-backed compatibility helpers |
| `std` | standard library | `alloc` API plus standard-library integration |
| `codec` (v4/v5 only) | standard library | `std` API plus the `tokio-util` framed codec |

`std` must imply `alloc`, and `codec` must imply `std`. Default features may
remain `std` to preserve the normal desktop experience without requiring Tokio.
`bytes` and `tokio-util` must be optional and must not occur in the
feature-free target's normal dependency graph. `tokio-util` must be activated
only by `codec`. If an allocation-backed dependency is retained, its feature
must be forwarded only from this repository's `alloc` or `std` features.

`mqttbytes-core-next` must follow the `none`/`alloc`/`std` tiering even if its
allocation-backed surface is small. A protocol crate must not accidentally
reactivate a default or allocation feature on `mqttbytes-core-next`.

## Normative requirements

The words **must**, **must not**, **should**, and **may** in this document are
requirements in decreasing order of strength.

### 1. No hidden allocator requirement

With `--no-default-features`, normal library code in each crate:

- must not import or link `alloc`;
- must not use `String`, `Vec`, `Box`, `Arc`, `Rc`, `Cow` with owned data,
  `Bytes`, `BytesMut`, or any collection that allocates;
- must not require an allocator merely to format or return an error;
- must not depend on a crate whose selected runtime features require `alloc` or
  `std`; and
- must compile for a bare-metal target without defining a global allocator.

Test-only and build-time procedural-macro dependencies are not part of the
target runtime closure, but CI must separately prove the normal dependency
closure. A successful host build alone is insufficient evidence.

### 2. Complete allocator-free protocol coverage

The feature-free API must support every packet variant supported by the owned
v4 and v5 APIs. It must not provide only primitive helpers, only `PUBLISH`, or
only encoding. At minimum it must support:

- fixed-header parsing and complete-frame detection;
- decoding and inspecting all packet fields;
- constructing a packet representation from borrowed fields;
- computing the exact encoded length;
- encoding into caller-provided storage;
- MQTT UTF-8, topic-name, topic-filter, QoS, packet-identifier, flags,
  remaining-length, property, and packet-size validation; and
- all protocol errors detected by the allocation-backed codec.

The allocator-free and owned paths must implement the same wire rules. Do not
maintain two independent parsers whose validation can drift.

### 3. Borrowed decoded representations

Decoded variable-length wire values must borrow from the input frame. The
public API should use packet views with a shape similar to:

```rust,ignore
pub struct PublishRef<'a> {
    pub topic: &'a str,
    pub payload: &'a [u8],
    // Scalar fields and borrowed properties.
}

pub enum PacketRef<'a> {
    Connect(ConnectRef<'a>),
    Publish(PublishRef<'a>),
    // Every other packet variant.
}
```

Names may differ after an API review, but ownership and lifetime behavior must
remain clear in the type system. Do not use unsafe lifetime extension,
self-referential packet objects, leaked storage, or global scratch buffers.
The decoded value must not outlive the bytes from which it borrows.

MQTT binary data must be exposed as `&[u8]`. MQTT UTF-8 data must be exposed as
`&str` only after the MQTT-specific UTF-8 rules have been validated. The API
must preserve the distinction between absent, present-and-empty, and repeated
values where the protocol distinguishes them.

### 4. Allocation-free repeated fields

Subscriptions, unsubscribe filters, reason-code lists, user properties,
subscription identifiers, and other unbounded repeated fields must not be
copied into fixed-capacity internal arrays. Decode them as lightweight views or
iterators over the validated portion of the input frame.

The selected iterator contract must specify:

- whether construction validates the entire sequence;
- whether iteration can return an error;
- whether the iterator is cloneable and restartable;
- its exact ordering behavior; and
- the cost of repeated iteration.

Prefer validating the whole packet once during decode and then exposing
infallible iterators. If a field cannot be fully validated up front without
unacceptable cost, its iterator must yield `Result` and the packet-level docs
must make deferred errors explicit. A top-level successful decode must never
claim that a malformed packet is fully valid when an error is merely hidden in
an iterator the caller may not consume.

MQTT 5 singleton-property duplication and property-location rules must still
be enforced without allocation. Use bounded scalar state, bitsets, or repeated
scans as appropriate. Wire-controlled collection growth is forbidden.

### 5. Borrowed construction for encoding

Callers must be able to construct all encodable packets from references,
scalars, and repeatable iterators or views without allocating. Construction
must not require an intermediate owned packet.

For repeated values, the design may use one or more of:

- borrowed slices of application-owned elements;
- cloneable iterators;
- small public source traits with a documented iteration contract; or
- a two-pass visitor that can be replayed for sizing and writing.

Avoid mandatory const-generic capacities in the fundamental packet type: MQTT
does not define such capacities, and a separate capacity parameter for every
repeated field would make packet composition impractical. An optional
`heapless` adapter may be offered under a separate feature, but `heapless` must
not be required by the allocator-free core and must not define protocol limits.

### 6. Caller-provided output and transactional failure

Encoding must accept caller-owned bounded storage, with a primary API based on
`&mut [u8]` or a small crate-owned no-allocation writer abstraction. It must
return the number of bytes written and a distinct insufficient-capacity error
that reports the required size when known.

Before modifying output, encoding must validate the packet and establish its
exact encoded length. If validation fails or the destination is too short, the
destination must remain unchanged. Once writing starts after successful
preflight, encoding must not fail for data-dependent reasons.

The length calculation and write pass must share validation and field-walking
logic closely enough that they cannot disagree. Tests must assert:

```text
encoded_len(packet) == bytes_written(packet)
```

for every packet type and all remaining-length boundaries. Arithmetic must use
checked operations, reject MQTT lengths above `268_435_455`, and never panic or
wrap because of caller-controlled lengths.

### 7. Framing and input consumption

The allocator-free decoder must operate on borrowed byte slices. Its contract
must state whether it receives exactly one frame or a larger receive buffer.
For a larger buffer it must report the exact frame length or return the unused
remainder without copying.

Incomplete input must be distinguishable from malformed input and must report
the minimum additional byte count when determinable. Packet-size limits must
be checked before callers are encouraged to buffer the declared payload.
Neither successful nor failed inspection may mutate or consume caller storage.

The `std`/`alloc` framed codecs should delegate fixed-header and packet parsing
to this borrowed foundation, then convert to owned values only where ownership
is part of their public contract.

### 8. Errors and panic freedom

The feature-free error types must use `core` only and remain useful without
formatted heap strings. Errors must be structured, comparable where practical,
and retain relevant scalar context such as invalid codes, crossed boundaries,
required capacity, and configured packet-size limits.

All public decode, inspect, size, and encode operations must be panic-free for
arbitrary input and arbitrary legal Rust values. Indexing, slicing, length
addition, cursor advancement, and UTF-8 conversion must be checked. No unsafe
code should be needed. If unsafe code is proposed, it requires a separate
safety rationale, targeted tests, and Miri coverage; convenience or parity
with `bytes` is not sufficient justification.

### 9. Owned compatibility layer

Under `alloc`, retain ergonomic owned packet models and conversions. Existing
public API compatibility should be preserved where practical because the
rumqtt clients consume these types. Required conversions are:

- borrowed decoded packet to owned packet, with an explicit allocation step;
- borrowed construction or owned packet to the common encoder; and
- owned decoding implemented by decoding a borrowed view and then owning its
  fields, rather than reparsing independently.

Where an existing owned method consumes or splits a `Bytes` buffer, document
any unavoidable behavior change and provide a migration path. Do not force
allocator-free users to compile compatibility types, and do not expose
allocation-backed types in feature-free enum variants, errors, trait bounds,
or method signatures.

Under `std`, keep `std::io::Error` and `tokio_util::codec::{Decoder, Encoder}`
behind `std`. Tokio framing must not contaminate the `alloc`-only dependency
closure.

### 10. Performance compatibility for `alloc` and `std` users

Allocator-free support must not impose a material performance regression on
existing `alloc` or `std` users. The owned packet API and Tokio codec are
compatibility paths, not secondary implementations whose throughput,
latency, allocation behavior, or memory use may be traded away to obtain the
feature-free API.

In particular, adapting the owned decoder to the borrowed foundation must not
unnecessarily copy data that can remain range- or `Bytes`-backed, allocate per
field where the current implementation does not, or repeatedly scan complete
packets without evidence that the cost is negligible. Validation shared by
the borrowed and owned paths should reuse results or combine passes where
practical. The same expectations apply to sizing and encoding: the required
preflight pass must be designed and measured so that it does not introduce an
unexamined regression for ordinary clients.

Before implementation begins, record benchmarks from an identified baseline
commit. During development, compare the new implementation against that
baseline using the same optimized build, benchmark inputs, toolchain, and
machine class. The benchmark corpus must cover both MQTT versions, every
packet class, small and large packets, remaining-length boundaries, repeated
fields and MQTT 5 properties, and the end-to-end Tokio framed decode and encode
paths used by the rumqtt clients. Measure at least:

- decode and encode throughput;
- per-operation latency for representative packet sizes;
- allocation count and allocated bytes for owned decode and encode;
- peak or steady-state memory use where packet ownership affects retention;
  and
- any additional full-packet or repeated-field passes introduced by the new
  architecture.

Benchmark methodology, raw comparison results, and the baseline commit must be
kept with the change or linked from it. Results must use enough samples to
distinguish a repeatable regression from measurement noise. A repeatable
regression greater than 5% in throughput or latency, or any new allocation or
payload copy on a previously allocation-free or zero-copy hot path, blocks
completion unless it is specifically justified, documented, and accepted as
an explicit compatibility trade-off. Improvements in the feature-free path do
not offset regressions in the existing `alloc` or `std` paths.

## Recommended architecture

Use one wire-level implementation with progressively richer adapters:

```text
caller-owned &[u8]
        |
        v
core cursor + validation + PacketRef<'a>
        |                         |
        |                         +--> encode into caller-owned &mut [u8]
        v
alloc conversion to owned packet types
        |
        v
std Tokio framed codec and rumqtt client integration
```

`mqttbytes-core-next` should own protocol-neutral, allocation-free machinery:

- checked read cursors over `&[u8]`;
- checked write cursors over `&mut [u8]`;
- fixed-header and MQTT variable-byte integer handling;
- MQTT byte-string and UTF-8-string views and validation;
- exact-length checked arithmetic;
- topic and filter validation; and
- shared error building blocks.

`mqttbytes-v4-next` and `mqttbytes-v5-next` should own protocol-specific packet
views, property rules, reason codes, and encode/decode dispatch. Do not place
v4/v5 policy in the neutral core merely to reuse an implementation detail.

Internal cursors should advance only after bounds checks. Prefer a small
purpose-built cursor to a broad buffering trait whose contract permits hidden
allocation. Public abstractions should be added only when callers genuinely
need to implement them.

## Protocol-correctness requirements

Implementation work must consult `docs/spec/mqtt-v3.1.1.md`,
`docs/spec/mqtt-v5.0.md`, and their machine-readable requirement indexes.
Allocator-free support must not weaken existing validation. In particular,
test at least:

- every control-packet type and required fixed-header flag pattern;
- remaining lengths encoded in one, two, three, and four bytes;
- malformed, non-minimal, truncated, and over-maximum remaining lengths;
- zero and missing packet identifiers where forbidden;
- MQTT UTF-8 restrictions, including U+0000 and malformed UTF-8;
- invalid topic names and topic filters;
- empty and multiply repeated payload elements where prohibited;
- MQTT 5 property allow-lists for every packet location;
- duplicate singleton properties and repeatable properties;
- valid and invalid reason codes;
- publish QoS/packet-identifier combinations;
- maximum-packet-size rejection before payload buffering; and
- exact preservation of binary payload and correlation data.

Tests should cite the relevant specification requirement identifiers when a
case enforces a normative MQTT rule.

## Delivery plan

### Phase 1: feature and dependency boundary

1. Define `alloc` and `std` features in all three manifests.
2. Make allocation-backed dependencies optional.
3. Add CI checks proving the empty-feature dependency closure is allocator
   free.
4. Update crate READMEs to distinguish allocator-free, `alloc`, and `std`
   usage accurately.

Do not temporarily describe the crates as fully allocator-free before the
complete codec acceptance tests pass.

### Phase 2: allocation-free core primitives

1. Introduce checked slice read/write cursors.
2. Port fixed-header, integer, byte-string, UTF-8-string, topic, and filter
   validation to them.
3. Add exhaustive boundary and arbitrary-input tests.
4. Adapt existing allocation-backed primitives to call the shared core.

### Phase 3: MQTT 3.1.1 packet views

Implement every v4 packet using borrowed decode and bounded encode. Establish
the repeated-field and conversion patterns here before applying them to MQTT
5. Compare all encoded bytes and errors with the existing owned codec.

### Phase 4: MQTT 5 packet and property views

Implement every v5 packet, with particular attention to repeatable properties,
singleton duplication, property-location rules, and optional fields. Centralize
property walking sufficiently to keep validation, inspection, sizing, and
encoding consistent without erasing packet-specific rules.

### Phase 5: owned and framed adapters

Move existing owned behavior onto the borrowed foundation. Preserve rumqtt
client behavior and keep Tokio integration under `std`. Remove obsolete parser
paths after equivalence is demonstrated; do not leave a permanent legacy and
allocator-free parser pair.

Run the performance-compatibility benchmarks before and throughout this phase.
Address avoidable extra scans, allocations, copies, and retained memory before
removing the legacy path so that the recorded baseline remains directly
comparable.

### Phase 6: documentation and stabilization

Add feature documentation and runnable examples for:

- decoding a packet from a static byte array without an allocator;
- iterating v4 subscriptions and MQTT 5 user properties;
- encoding into a fixed stack buffer and handling insufficient capacity;
- converting a borrowed packet to an owned packet under `alloc`; and
- using the existing Tokio codec under `std`.

Document lifetimes, validation timing, iterator complexity, maximum lengths,
error behavior, and feature availability. Record user-facing changes in
`CHANGELOG.md`.

## Testing and CI acceptance

The requirement is complete only when all of the following are automated.

### Bare-metal compilation and dependency proof

CI must build all three packages together on an installed target such as
`thumbv7em-none-eabi`:

```sh
cargo check --locked --target thumbv7em-none-eabi --no-default-features \
    -p mqttbytes-core-next \
    -p mqttbytes-v4-next \
    -p mqttbytes-v5-next
```

Add a small bare-metal consumer fixture that imports and exercises public
decode, field inspection, repeated-field iteration, sizing, and encode APIs for
both protocol versions without declaring a global allocator. This prevents an
empty or accidentally gated library from satisfying compile-only CI.

CI must inspect the normal target dependency tree and fail if the feature-free
closure enables `bytes`, Tokio, or another allocation/runtime dependency.

### Feature matrix

Test at least these configurations:

```text
no default features
alloc only
std (and therefore alloc)
all features
```

Run the existing `cargo hack` checks for both protocol crates. Also compile the
`alloc`-only tier for a `no_std` target with allocator support so it cannot
accidentally acquire a `std` dependency.

### Behavioral equivalence

For every packet type:

- decode the same valid corpus through borrowed and owned entry points and
  compare every observable field;
- encode equivalent borrowed and owned values and compare exact wire bytes;
- compare structured errors for malformed input;
- round-trip borrowed encode/decode and owned encode/decode;
- verify iteration order and repeated iteration where promised; and
- test output buffers of exact size, one byte too small, empty, and larger than
  required.

On every preflight error, assert that the caller's output buffer is byte-for-byte
unchanged.

### Performance compatibility

Automate the representative microbenchmarks in `benchmarks/` and add an
end-to-end benchmark for the owned and Tokio codec paths if one does not
already exist. CI need not make pass/fail decisions from noisy shared runners,
but it must compile the benchmarks and make them reproducible. Release or PR
validation must run them in a stable environment against the recorded
pre-change commit and publish the comparison.

The comparison must separately report v4 and v5 decode, encode, allocation,
copying, and memory-retention results. It must call out statistically
repeatable changes rather than hiding regressions in an aggregate score. The
threshold and exception policy in requirement 10 apply to these results.

### Robustness

Use fuzz targets for v4 and v5 borrowed decoders, iterators, length calculators,
and encoders. Differential fuzzing should compare borrowed and owned results.
Seed corpora with all packet types and length boundaries. Arbitrary input must
not panic, hang, perform unbounded work relative to input length, or trigger an
allocation in the feature-free implementation.

Where practical, run Miri over cursor and iterator tests. Add a test allocator
or equivalent instrumentation on a hosted target to demonstrate zero
allocations across representative feature-free decode/inspect/encode flows;
bare-metal compilation remains the authoritative dependency check.

## Non-goals

This requirement does not require:

- allocator-free networking, DNS, TLS, WebSockets, event loops, or rumqtt
  clients;
- buffering an entire maximum-sized MQTT packet inside the codec;
- internally reassembling fragmented transport reads;
- a built-in fixed-capacity queue or session store;
- adopting `heapless` as a mandatory public dependency;
- changing MQTT wire limits to application-selected const-generic limits; or
- preserving source compatibility for newly introduced experimental borrowed
  APIs before they are stabilized.

Transport code remains responsible for accumulating enough bytes to present a
frame. The allocator-free codec is responsible for safely identifying,
validating, inspecting, and encoding that frame without owning its storage.

## Completion criteria

This TODO is complete only when:

1. the empty-feature build of all three crates links into a bare-metal consumer
   with no allocator;
2. that consumer can decode, inspect, construct, size, and encode every
   supported packet class through public APIs;
3. no allocation-backed dependency appears in the empty-feature runtime graph;
4. borrowed and owned codecs pass protocol-equivalence, malformed-input,
   boundary, fuzz, and transactional-output tests;
5. the owned rumqtt clients and `std` Tokio codecs retain their existing
   behavior and satisfy the performance-compatibility benchmark requirements;
6. benchmark results against the recorded baseline show no unapproved material
   regression in throughput, latency, allocations, copying, or memory use;
7. crate documentation states the three capability tiers and contains working
   examples; and
8. `CHANGELOG.md` records the new allocator-free public API and any required
   migration.

Passing `cargo check --no-default-features` by itself is not completion. The
feature-free configuration must expose a complete, tested, idiomatic codec
that an embedded application can use without providing a global allocator.
