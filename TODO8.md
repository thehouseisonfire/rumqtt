# Python wrapper: remaining work

Complete the Python wrapper by closing the verification, lifecycle, packaging,
and documentation gaps below. Treat a behavior as complete only when it has a
deterministic test at the public Python boundary. Run protocol behavior
tests against both MQTT 3.1.1 and MQTT 5 unless the behavior is explicitly
version-specific.

## 1. Complete the behavior suite

Add broker-backed coverage for:

- concurrent successful `connect()` calls, repeated `connect()` calls after
  success, and cancellation of one waiter without affecting the others;
- recovery after an initial connection-attempt failure and reconnection after
  an established broker connection is interrupted, including ordered
  `Disconnected` and `Connected` events;
- automatic acknowledgement and all manual-acknowledgement rejection paths:
  double acknowledgement, use after reconnect, use after terminal shutdown,
  and use with a different client;
- TLS and WSS with a trusted certificate, an untrusted certificate, a hostname
  mismatch, malformed trust or identity material, and paired client
  certificate authentication;
- bounded request admission under saturation and event-buffer overflow,
  including stable error classification, failure of pending operations, and
  iterator termination without an additional unbounded Python queue;
- MQTT 5 capability-aware publish admission while reconnecting, covering QoS
  1/2, retained publishes, Topic Alias use, and an alias-free, non-retained QoS
  0 publish;
- graceful-close timeout, zero-timeout behavior, cancellation of graceful
  close, concurrent and repeated graceful or immediate close calls, and the
  resulting terminal event classification;
- operation cancellation while waiting for admission, after admission, and
  while racing MQTT completion for publish, subscribe, unsubscribe, and manual
  acknowledgement;
- scheduling client work from another thread through the owning event loop,
  plus completion and cancellation races while that loop is closing; and
- broker rejection details and every public exception attribute, including
  `code`, `kind`, `operation_id`, `broker_reason`, `retryable`, `delivery`, and
  `ambiguous`.

Expand boundary-validation tests to cover every public option and command
field. Include integer limits without accepting `bool`, duration overflow,
non-finite and zero timeout rules, malformed topics and filters, invalid
transport combinations, MQTT-version-specific fields, MQTT 5 publish
properties, both SUBSCRIBE property scopes, and UNSUBSCRIBE properties. Verify
that locally invalid values fail before admission and that MQTT 5 fields are
rejected for MQTT 3.1.1 rather than ignored.

Add runtime API-contract tests that compare exported names and callable
signatures with their annotations. Exercise both `asyncio.run()` and manually
created event loops.

## 2. Finish panic, shutdown, and leak hardening

Add deterministic test-only panic injection for the Python asynchronous
boundary and the driver thread. Verify that each injected panic becomes an
`INTERNAL_PANIC` error, terminates the affected client, fails outstanding
operations, and never unwinds through Python or aborts the interpreter.

Use child-process tests with strict timeouts to cover:

- explicit `sys.exit()` with a live client;
- event-loop closure with command-admission, tracked-completion, and
  acknowledgement waits in flight;
- clients retained in garbage-collection cycles;
- module and interpreter teardown with live clients;
- repeated create/connect/close and create/abandon cycles; and
- abrupt child-process termination.

The child-process suite must fail on hangs, unexpected stderr diagnostics,
unretrieved task exceptions, or native threads that survive bounded cleanup.
Track native threads and Python objects across repeated cycles so the tests can
detect cumulative leaks rather than only successful process exit.

Run the Python boundary under AddressSanitizer and a supported leak checker on
the platforms where PyO3 and the Python build permit it. Cover normal close,
immediate close, cancellation, event-buffer overflow, and interpreter teardown
in those jobs.

## 3. Complete wheel and source-distribution verification

Run the installed-package behavior and lifecycle suites for every advertised
CPython version on every advertised platform:

- manylinux x86_64 and aarch64;
- musllinux x86_64;
- macOS x86_64 and arm64; and
- Windows x86_64.

Do not limit installed-wheel execution to the newest Python version. Each
wheel must be installed into a clean environment before testing; tests must
not import from the source tree or reuse a development extension.

For every wheel:

- verify its Python, ABI, platform, libc, and macOS deployment-target tags;
- audit Linux native dependencies against the selected manylinux or
  musllinux policy;
- verify that the private extension, Python facade, annotations, `py.typed`,
  license files, and no development artifacts are present;
- run import, connect, binary publish at QoS 0/1/2, receive, manual
  acknowledgement, unsubscribe, and graceful-close smoke tests for both
  protocols; and
- install with both the minimum supported `pip` and the newest supported
  `pip`.

Build the source distribution in a clean environment, install it using only
the documented source-build prerequisites, run the installed-package smoke
suite, and verify that a missing Rust or native build prerequisite produces an
ordinary packaging failure without downloading executables.

## 4. Complete user documentation

Document:

- installation from wheels and from source, including supported wheel
  platforms and source-build prerequisites;
- TCP, TLS, WebSocket, and WSS configuration, including custom roots and
  client-certificate authentication;
- reconnect behavior after both connection-attempt and established-session
  failures;
- the exact meaning of zero and omitted timeouts for every operation that
  accepts a timeout;
- how another thread safely schedules work through the client-owned event
  loop; and
- the absence of synchronous and callback facades and the prohibition on
  direct cross-loop use.

Add complete examples for a dedicated event-consumer task, cancellation-safe
application cleanup, manual acknowledgements, and MQTT 5 PUBLISH, SUBSCRIBE,
per-filter subscription, and UNSUBSCRIBE properties.

## Completion criteria

This TODO is complete only when all items above pass in CI for every advertised
CPython version and platform. The required evidence includes broker behavior,
cancellation and closed-loop races, bounded-memory failure, panic containment,
interpreter shutdown, installed wheels, source installation, and leak checks.
No panic, borrowed Python memory, native thread, or scheduled callback may
cross an invalid interpreter boundary.
