# Node-API JavaScript and TypeScript Wrapper: Remaining Work

## Completion target

Publish `@rumqtt/rumqttc` with installable native artifacts and demonstrate the
same public contract under Node.js 24, Deno 2.9.5, and Bun 1.3.14. Completion
requires tests against installed packages, not only source-tree execution.

Supported release targets are:

- Linux x86_64 glibc;
- Linux x86_64 musl;
- Linux aarch64 glibc;
- macOS x86_64 and aarch64; and
- Windows x86_64 MSVC.

Use a local deterministic broker for runtime tests. Do not depend on a public
MQTT service or runtime-internal extension APIs.

## 1. Close the remaining API-contract gaps

### 1.1 Make `connect()` the native startup boundary

Create the native client and start its driver only through the first
`connect()` transition. Calls made before that transition must not instantiate
native state accidentally.

Define and document pre-connect behavior for publish, subscribe, unsubscribe,
diagnostics, acknowledgement, and event reads. Either reject those calls with a
stable structured error without starting native work, or route them through one
explicit, coalesced connection-start transition. Do not let individual methods
silently establish independent startup paths.

Validate protocol-incompatible operation options before crossing the startup
boundary. In particular, MQTT 5 PUBLISH properties, SUBSCRIBE packet
properties, per-filter subscription options, and UNSUBSCRIBE properties must be
rejected for MQTT 3.1.1 without constructing a native client. Keep native
validation authoritative for values that do cross the boundary.

Add tests proving that rejected pre-connect and protocol-incompatible calls do
not create a driver thread or open a broker connection.

### 1.2 Complete `Buffer` interoperability

Accept and copy `Buffer` inputs under Node.js and Bun. Return `Buffer` for
binary payloads and correlation data in those runtimes while retaining
`Uint8Array` under Deno. Keep the runtime-neutral TypeScript surface usable
without requiring Node types; `Buffer` remains assignable to `Uint8Array`.

Test embedded zero bytes, sliced buffers with nonzero byte offsets, and mutation
of the caller's source buffer immediately after admission.

### 1.3 Normalize manual acknowledgement results

Make the public acknowledgement operation match one documented shape. Prefer
`ack(): Promise<void>` for the method form. If acknowledgement metadata must be
exposed, replace the method with a typed opaque acknowledgement object rather
than returning an undocumented admission result.

Verify single use, QoS 0 absence, reconnect invalidation, concurrent calls, and
the declared resolution value.

### 1.4 Prove panic containment at every Node-API boundary

Audit synchronous entry points, asynchronous tasks, JSON conversion, and
environment teardown for possible Rust panics. Ensure a panic is converted to
a structured `INTERNAL_PANIC` failure, outstanding promises settle, the event
stream reaches a terminal state, and no unwind crosses Node-API.

Add test-only panic injection for both a synchronous entry and an asynchronous
task. Run the tests in a child process so an abort, hang, or invalid environment
callback is observable.

## 2. Complete the runtime behavior matrix

Run one shared behavioral suite directly under Node.js, Deno, and Bun. Extend
that suite with the following missing cases:

- automatic acknowledgement for incoming QoS 1 and QoS 2 publishes;
- successful TLS verification against a deterministic local CA;
- rejection of an otherwise valid certificate for the wrong CA or hostname;
- bounded request-channel backpressure and subsequent recovery;
- dropping a JavaScript completion waiter without cancelling admitted MQTT
  work;
- repeated create, connect, graceful close, immediate close, and destruction
  cycles without native thread, handle, or memory growth; and
- process exit with a live client and worker termination in every runtime where
  the required worker and Node-API lifecycle facilities are supported.

Use explicit time bounds for every lifecycle test. Preserve broker-side
observations so waiter cancellation, automatic acknowledgements, and shutdown
delivery claims are verified on the wire rather than inferred from promise
resolution.

### 2.1 Node.js

Test both the main package's ESM `import` and CommonJS `require` entry points.
Run worker-thread cleanup, live-process exit, TLS, and installed-artifact loader
selection on every executable Node.js host target.

### 2.2 Deno

Publish the packed main and platform packages to an ephemeral local npm
registry, install them into a fresh test project, and import the client as:

```ts,ignore
import { MqttClient } from "npm:@rumqtt/rumqttc";
```

Run with `--node-modules-dir=auto`, `--allow-ffi`, and only the additional
network, environment, and read permissions required by the fixture. Confirm
that optional platform packages resolve from the package dependency graph
without an npm lifecycle build script. Do not substitute a relative
source-tree import for this distribution test.

### 2.3 Bun

Install the packed package into a fresh test project and exercise both package
loading and the full shared MQTT suite under Bun. Include every Node-API feature
used by the addon, especially async promise completion and environment cleanup.

## 3. Strengthen TypeScript and export-contract testing

Expand compile-time tests to cover every exported type and method, including:

- the MQTT 3.1.1 and MQTT 5 configuration union;
- all transport and credential combinations;
- PUBLISH, SUBSCRIBE, per-filter, and UNSUBSCRIBE property scopes;
- every completion and event discriminant;
- optional fields and their presence-based narrowing;
- manual acknowledgement resolution; and
- binary values without a dependency on Node-specific declarations.

Add negative tests for every protocol-incompatible option scope and invalid
property placement. Add a runtime export-parity test that compares the public
ESM and CommonJS exports with the declaration surface so missing or extra
runtime exports fail CI.

## 4. Verify packaged native artifacts

For every supported target, build the `.node` artifact, stage its optional
platform package, run `npm pack`, and inspect the archive to ensure it contains
the exact expected binary and package metadata.

For every host architecture available in CI:

1. install the main package tarball and its matching optional platform package
   into a fresh directory;
2. load the addon through the main package loader rather than requiring the
   platform directory directly;
3. execute a broker-backed connect/publish/close smoke test; and
4. verify unsupported OS, architecture, and libc selections fail with a clear
   loader error and never fall back to another implementation.

Execute the Linux musl package in a musl environment. Do not treat a glibc-host
cross-compilation as its runtime smoke test. Validate Linux libc detection for
both glibc and musl.

Generate SHA-256 checksums and build provenance for every platform tarball.
Fail the release if an advertised package, binary, checksum, or attestation is
missing.

## 5. Publish and verify the release

Before publishing:

- align the release tag, main package version, platform package versions, and
  optional dependency versions;
- publish platform packages before the main package;
- retain npm provenance and GitHub-hosted checksums; and
- verify the main package tarball contains only the intended JavaScript,
  declarations, loader, documentation, and package metadata.

After publishing, install the registry package into clean Node.js, Deno, and
Bun projects and rerun their package-loading and broker-backed smoke tests.
Record the exact package version, runtime versions, target triples, and test
commands in the release evidence.

## 6. Documentation updates required by the remaining changes

Document the final pre-connect behavior, runtime-specific binary return types,
manual acknowledgement result, exact installation commands, runtime
permissions, and supported artifact matrix. Keep examples valid for both ESM
and CommonJS where applicable.

Update `CHANGELOG.md` for any public API adjustment made while completing this
work.

## Completion criteria

This TODO is complete only when all of the following are true:

- the API-contract gaps in section 1 are resolved and covered by regression
  tests;
- the complete behavior matrix passes directly under Node.js 24, Deno 2.9.5,
  and Bun 1.3.14;
- TypeScript declarations and both runtime entry points pass export-parity
  checks;
- every supported platform package passes archive validation and every
  executable CI host passes an installed-package smoke test;
- Linux musl is executed in a musl environment;
- cleanup and repetition tests show no persistent native thread, handle, or
  memory growth; and
- the published registry package passes clean-install smoke tests under all
  three runtimes.
