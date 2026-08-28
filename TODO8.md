# Python wrapper: remaining verification work

Complete the Python wrapper verification gaps below. A behavior is complete
only when it has a deterministic test at the public Python boundary. Run MQTT
behavior tests against both MQTT 3.1.1 and MQTT 5 unless the behavior is
explicitly version-specific.

## 1. Complete the behavior suite

Add broker-backed coverage for:

- rejecting an attempt to acknowledge a delivery through a client other than
  the client that received it;
- bounded request admission under saturation, including stable error
  classification and bounded memory use, for both protocols;
- event-buffer overflow through the MQTT 3.1.1 public boundary;
- MQTT 3.1.1 TLS trust, hostname-verification, mutual-authentication, and WSS
  rejection cases;
- malformed client-certificate and private-key material over TLS and WSS for
  both protocols;
- a positive graceful-close timeout that expires while MQTT work remains
  pending, including the resulting operation failures and terminal event;
- cancellation while waiting for acknowledgement admission; and
- cancellation races against MQTT completion for publish, subscribe,
  unsubscribe, and manual acknowledgement.

Add deterministic cross-thread and event-loop shutdown tests that:

- schedule client work from another thread through the owning event loop;
- race successful completion and cancellation with closure of that loop;
- reject new scheduling once loop shutdown has begun; and
- fail on hangs, callbacks delivered after loop closure, unhandled task
  exceptions, or work completed on the wrong loop.

Keep each race bounded and repeat it enough times to exercise both possible
outcomes. Assert the permitted outcome set and ensure admitted MQTT work is not
mistaken for work canceled before admission.

Add direct public-boundary invalid-value tests for:

- client identifiers and usernames at their type, encoding, and length
  boundaries;
- the PUBLISH retain flag and property-container type;
- the per-filter SUBSCRIBE QoS and option-container types; and
- TLS client-certificate and private-key parsing independently and as a pair.

Assert that each locally invalid command fails before native admission. Reject
`bool` wherever an integer or enum is required.

## 2. Finish lifecycle and leak verification

Track native driver threads independently of Python-managed threads on every
supported test platform. Repeated create/connect/close and
create/connect/abandon cycles must fail if native thread counts or live Python
client objects grow cumulatively after bounded cleanup.

Run the complete panic, shutdown, lifecycle, and leak suites in CI for every
supported CPython version on Linux, macOS, and Windows. In particular, closed
event-loop cases must pass on Windows without timing out or emitting unexpected
stderr diagnostics.

Run the Python boundary under AddressSanitizer and a supported leak checker on
platforms where the Python and PyO3 builds permit it. The sanitizer job must
cover normal close, immediate close, cancellation, event-buffer overflow,
panic containment, and interpreter teardown, and must fail on sanitizer or
leak-checker diagnostics.

## 3. Complete distribution verification

Run the installed-wheel behavior and lifecycle suites for CPython 3.10 through
3.14 on every advertised wheel platform:

- manylinux 2.17 x86_64 and aarch64;
- musllinux 1.2 x86_64;
- macOS 11+ x86_64 and arm64; and
- Windows x86_64.

Test every wheel with both pip 25.3, the minimum supported installer, and the
newest supported pip. Install each wheel into a clean environment, run outside
the source tree, and ensure the tests cannot reuse a development extension.

Make Linux wheel auditing enforce, rather than merely report, compatibility
with the selected manylinux or musllinux policy and its permitted native
dependencies. Fail the job when a wheel requires a newer libc or an
out-of-policy shared library.

Build the source distribution in a clean environment and verify ordinary,
bounded packaging failures for each missing source-build prerequisite:

- Rust/Cargo; and
- the native compiler or linker toolchain.

The failure path must not download Rust, a compiler, a linker, or another
executable.

## Completion criteria

This TODO is complete only after all checks above pass in CI. Required evidence
includes broker-backed saturation and acknowledgement ownership, real MQTT
completion races, cross-thread closed-loop races, cross-platform native-thread
leak detection, sanitizer and leak-checker execution, installed wheels for the
full platform and CPython matrix, and clean source-distribution failure tests.

No panic, borrowed Python memory, native thread, task, or scheduled callback
may cross an invalid interpreter or event-loop boundary.
