# C Wrapper Remaining Work

The C wrapper's remaining work is native behavioral verification and complete
consumer documentation.

## Native integration tests

Add a C11 integration-test executable that links to the built library and uses
a deterministic local broker fixture. The test must exercise the API from C,
not by calling the exported functions from Rust tests.

Cover the following behavior:

- connect with MQTT 3.1.1 and MQTT 5;
- publish binary payloads containing zero bytes;
- observe tracked QoS 0, QoS 1, and QoS 2 completions;
- subscribe, receive incoming publish events, and unsubscribe;
- acknowledge incoming publishes in automatic and manual modes;
- reject invalid UTF-8, unknown discriminants, null required pointers, and
  pointer-and-length pairs with a null pointer and nonzero length;
- report request backpressure and event-queue overflow without silently losing
  an incoming publish;
- reconnect after a broker interruption;
- exercise graceful-close timeout and immediate close;
- destroy pending completion and event handles safely;
- publish concurrently from multiple native threads;
- repeatedly create and destroy clients without leaking allocations or driver
  threads; and
- call every function that accepts an optional `error_out` with `NULL` on both
  successful and failing paths.

Put broker-dependent behavior in a dedicated native integration target so
compile-only checks remain fast and deterministic.

## Memory and concurrency verification

Run the native integration executable under AddressSanitizer,
UndefinedBehaviorSanitizer, and a leak checker on each CI platform where the
tool is supported. The instrumented run must cover client startup and teardown,
events, completions, reconnects, backpressure, and multithreaded publishing.

Add a stress test that repeatedly creates, connects, interrupts, reconnects,
and destroys clients. Give every wait a finite deadline, fail on a surviving
driver thread, and make the iteration count configurable so CI can use a short
run while dedicated memory jobs use a longer run.

The concurrency test must use native threads and coordinate client destruction
only after producers and the event consumer have stopped. It must also verify
that a second simultaneous event receiver is rejected.

## Consumer documentation

Add complete, compilable C examples for:

- single-threaded event polling;
- multithreaded publishing;
- tracked completion polling and timed waiting;
- manual acknowledgement; and
- graceful and immediate shutdown.

Each example must clean up every owned handle on all exit paths. Document next
to the examples that admission is not MQTT completion, completion timeout does
not prove non-delivery, destroying an incomplete completion does not cancel the
operation, and borrowed event/error views become invalid when their owner is
destroyed.

Compile the examples in CI with warnings treated as errors and run the examples
that can use the deterministic broker fixture.

## Completion criteria

This work is complete when:

1. the native C integration suite covers every behavior listed above;
2. the native suite runs on every supported CI platform, with sanitizer and
   leak tooling enabled wherever that tooling is available;
3. all waits and shutdown paths are bounded and the stress run leaves no driver
   threads or allocations behind; and
4. every documented C example is compiled in CI and broker-backed examples pass
   against the local fixture.
