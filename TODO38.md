# Optional Standalone C MQTT Codec Wrapper

## Goal

Expose standalone MQTT 3.1.1 and MQTT 5 packet encoding, decoding, and validation
to C applications using the maintained mqttbytes crates.

## Current foundation and feasibility

`mqttbytes-core`, `mqttbytes-v4`, and `mqttbytes-v5` are standalone codecs.
The client C ABI translates application commands and events but exposes no
general packet codec. [TODO12.md](TODO12.md) owns allocator-free codec work;
this binding must use whichever owned/borrowed APIs have actually landed.

A standalone C component is feasible. It has a different contract from a
managed MQTT client: encoding a packet does not advance a session or make it
legal to inject that packet into a live client connection.

## Implementation requirements

- [ ] Choose an optional codec component or companion C crate in the native
  workspace, with its own documented headers/features if appropriate. Reuse
  mqttbytes directly; keep runtime, client lifecycle, and network dependencies
  out of the standalone codec path.
- [ ] Inventory every v4/v5 packet and field, including client- and broker-origin
  packets, properties, reason codes, and MQTT-specific presence distinctions.
  Consult `docs/spec/` first and delegate protocol validation to the codecs.
- [ ] Define opaque owned packet/builders plus typed accessors and setters.
  Preserve binary fields, UTF-8 validation, duplicate User Properties, property
  order where retained by native models, and per-filter subscription options.
- [ ] Provide framing/decode APIs distinguishing incomplete input, malformed
  input, successful consumption, and configured size-limit rejection. Return
  exact consumed/needed sizes without reading past the supplied buffer.
- [ ] Copy decode input into an owned packet by default, or provide an explicitly
  retained buffer contract. Never return views into caller memory after its
  lifetime ends. Any zero-copy mode needs a separate auditable owner model.
- [ ] Provide sizing and caller-buffer encoding or an owned encoded-buffer
  handle. Define insufficient-buffer behavior and avoid partially valid output
  being mistaken for successful encoding.
- [ ] Bound packet length, property count, allocations, and numeric conversions.
  Parse lengths before allocation and map codec errors into stable typed C
  diagnostics without exposing Rust enum layouts.
- [ ] Preserve protocol/version distinctions and validate builder state before
  encoding. Directional client command restrictions must not accidentally
  prevent standalone encoding of legitimate broker packets.
- [ ] Keep the codec interface independent of ACK tokens, tracked operations,
  and client event queues. Do not add raw-packet injection to the managed client
  as a shortcut to exposing codec functionality.
- [ ] Define package identity and additive ABI rules explicitly; expose build
  capabilities and document allocator requirements. No claim of allocator-free
  C operation until both the native codec and binding provide it.

## Verification and completion

- [ ] Round-trip every supported packet kind against the native codec, including
  empty/optional/duplicate fields, legal reasons, and size boundaries.
- [ ] Exercise truncated and concatenated input, malformed lengths/properties,
  overflow, insufficient output buffers, and packet/buffer lifetime rules.
- [ ] Demonstrate a native C encode/decode consumer requiring no MQTT runtime.
- [ ] Update package headers/exports, examples, README, root `CHANGELOG.md`, and
  the parity inventory with a separate standalone-codec scope.

Complete when C can use the native codec capabilities independently of the
client. This TODO does not promise a broker, session engine, or raw client I/O.
