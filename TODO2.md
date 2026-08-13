# MQTT CLI Implementation TODO

## Objective

Build a production-oriented MQTT command-line client powered by the `rumqttc-v4-next` and `rumqttc-v5-next` crates.

The CLI should preserve the familiarity of `mosquitto_pub` and `mosquitto_sub`, while exposing the capabilities that distinguish `rumqttc-next`:

* MQTT 3.1.1 and MQTT 5.0 support
* durable session persistence across process restarts
* TLS provider selection
* WebSocket transports
* HTTP and SOCKS5 proxies
* MQTT 5 enhanced authentication, including SCRAM
* structured machine-readable output
* broker diagnostics
* realistic load and resilience testing

The implementation should favor:

* predictable CLI behavior
* explicit validation
* composable Unix-style output
* bounded memory usage
* graceful shutdown
* idiomatic async Rust
* reuse of shared connection-building code
* minimal protocol-specific duplication

---

# 1. Project Structure

## 1.1 Create a dedicated CLI crate

Add a workspace crate for the executable.

Suggested layout:

```text
mqtt-cli/
├── Cargo.toml
├── README.md
├── TODO.md
├── src/
│   ├── main.rs
│   ├── cli.rs
│   ├── error.rs
│   ├── config.rs
│   ├── connection.rs
│   ├── credentials.rs
│   ├── output.rs
│   ├── shutdown.rs
│   ├── topic.rs
│   ├── protocol/
│   │   ├── mod.rs
│   │   ├── v4.rs
│   │   └── v5.rs
│   └── commands/
│       ├── mod.rs
│       ├── pub.rs
│       ├── sub.rs
│       ├── inspect.rs
│       ├── session.rs
│       ├── bench/
│       │   ├── mod.rs
│       │   ├── metrics.rs
│       │   ├── publisher.rs
│       │   ├── roundtrip.rs
│       │   └── reconnect.rs
│       └── run/
│           ├── mod.rs
│           ├── model.rs
│           └── executor.rs
└── tests/
    ├── cli.rs
    ├── config.rs
    ├── pub_sub.rs
    ├── session.rs
    └── fixtures/
```

Use the repository’s existing naming conventions if they differ.

## 1.2 Define crate responsibilities

The CLI crate should contain:

* argument parsing
* configuration resolution
* connection construction
* command orchestration
* output formatting
* benchmark coordination

Protocol encoding, transport handling, reconnection behavior, session persistence, and authentication mechanisms should remain in the underlying library crates whenever possible.

Do not duplicate library functionality inside the CLI solely to make implementation easier.

## 1.3 Select core dependencies

Use mature, conventional crates unless the workspace already standardizes alternatives.

Likely dependencies:

```toml
clap
tokio
serde
serde_json
toml
thiserror
tracing
tracing-subscriber
humantime
url
bytes
base64
hex
futures-util
```

Optional dependencies may include:

```toml
csv
hdrhistogram
indicatif
```

Avoid adding a dependency when the same functionality already exists in the workspace.

---

# 2. Initial Command Surface

Implement the following top-level shape:

```text
mqtt pub
mqtt sub
mqtt inspect
mqtt session
mqtt bench
mqtt run
mqtt shell
```

The initial stable release should include:

```text
mqtt pub
mqtt sub
mqtt inspect connect
mqtt session show
mqtt session clear
```

The following may remain experimental initially:

```text
mqtt bench
mqtt run
mqtt shell
```

Mark experimental commands clearly in help output rather than hiding them behind undocumented behavior.

---

# 3. Global CLI Design

## 3.1 Define global options

Support global options before or after the subcommand where `clap` permits it.

```text
--config <PATH>
--profile <NAME>
--url <URL>
--host <HOST>
--port <PORT>
--protocol <v4|v5>
-4
-5
--client-id <ID>
--username <USERNAME>
--password <PASSWORD>
--password-file <PATH>
--password-stdin
--password-env <NAME>
--connect-timeout <DURATION>
--keep-alive <DURATION>
--clean-start <BOOL>
--session-expiry <DURATION>
--max-packet-size <BYTES>
--receive-maximum <COUNT>
--request-problem-information <BOOL>
--request-response-information <BOOL>
--tls-provider <rustls|native>
--ca <PATH>
--cert <PATH>
--key <PATH>
--key-password-file <PATH>
--server-name <NAME>
--insecure
--proxy <URL>
--websocket-header <NAME=VALUE>
--session <PATH>
--log <LEVEL>
--quiet
```

Not every option applies to both protocol versions. Reject incompatible options explicitly.

Examples:

* reject `--session-expiry` under MQTT 3.1.1 unless it can be mapped unambiguously
* reject MQTT 5 user properties under MQTT 3.1.1
* reject enhanced authentication under MQTT 3.1.1
* reject simultaneous `-4` and `-5`
* reject multiple password sources

## 3.2 Protocol selection

Use:

```text
--protocol v4
--protocol v5
```

Provide aliases:

```text
-4
-5
```

Choose and document one default protocol.

Recommended initial default:

```text
v5
```

The resolved protocol must be visible in:

```text
mqtt inspect connect
mqtt --log debug ...
```

Do not silently downgrade between MQTT versions.

## 3.3 URL handling

Support URLs such as:

```text
mqtt://host:1883
mqtts://host:8883
ws://host:8080/mqtt
wss://host:443/mqtt
```

Allow user information where supported:

```text
mqtts://user:password@host:8883
```

Treat URL credentials as lower priority than explicit credential flags.

Recommended precedence:

```text
command-line credential option
environment-backed credential option
profile
URL
default
```

Do not print URL passwords in logs, errors, debug output, or inspect output.

## 3.4 Option conflict rules

Implement one centralized validation pass after configuration resolution.

Examples:

```text
--url mqtt://... + --tls-provider rustls
```

Should fail because the URL explicitly selects a non-TLS transport.

```text
--url wss://... + --tls-provider rustls
```

Should succeed.

```text
--url mqtts://... + --host other-host
```

Should fail unless host overrides are intentionally supported and documented.

Prefer rejecting contradictions over silently choosing one value.

---

# 4. Configuration Profiles

## 4.1 Configuration file location

Use a platform-appropriate default configuration directory.

Suggested logical path:

```text
mqtt/config.toml
```

Allow overriding it with:

```text
--config <PATH>
```

Do not fail when the default configuration file does not exist.

Do fail when an explicitly supplied configuration file cannot be read or parsed.

## 4.2 Configuration format

Support named profiles.

Example:

```toml
default_profile = "development"

[profiles.development]
url = "mqtt://localhost:1883"
protocol = "v5"
client_id = "mqtt-cli-development"

[profiles.production]
url = "mqtts://broker.example.com:8883"
protocol = "v5"
tls_provider = "rustls"
ca = "/etc/example/ca.pem"
client_id = "mqtt-cli-production"
keep_alive = "30s"
```

## 4.3 Resolution precedence

Implement and test this precedence:

```text
command-line arguments
explicit environment references
selected profile
default profile
built-in defaults
```

Environment references such as `--password-env SECRET_NAME` should retrieve only the named value. Do not automatically consume a broad set of undocumented environment variables in the first release.

## 4.4 Resolved configuration model

Create one validated internal configuration type independent of `clap`.

Suggested shape:

```rust
struct ResolvedConnectionConfig {
    protocol: ProtocolVersion,
    endpoint: Endpoint,
    client_id: String,
    credentials: Option<Credentials>,
    transport: TransportConfig,
    session: SessionConfig,
    mqtt: MqttConfig,
    timeouts: TimeoutConfig,
}
```

Do not pass raw `clap` argument structs deep into command implementations.

---

# 5. Credential Safety

## 5.1 Password sources

Support:

```text
--password <VALUE>
--password-file <PATH>
--password-stdin
--password-env <NAME>
```

Only one password source may be selected.

## 5.2 Secret handling

Ensure secrets are:

* redacted from `Debug`
* redacted from error messages
* redacted from tracing fields
* not included in structured reports
* not included in panic output
* dropped as soon as practical

Use an explicit secret wrapper if the workspace does not already have one.

## 5.3 Terminal safety

When `--password-stdin` is used:

* read one value from standard input
* trim one trailing newline
* do not log it
* fail clearly if standard input is unavailable
* reject commands that simultaneously need payload input from stdin

For example, reject:

```text
mqtt pub topic --stdin --password-stdin
```

unless separate file descriptors are deliberately supported.

---

# 6. Shared Connection Layer

## 6.1 Build a protocol-independent facade

Create a narrow internal abstraction for command orchestration.

Example responsibilities:

```rust
trait CliClient {
    async fn publish(...);
    async fn subscribe(...);
    async fn unsubscribe(...);
    async fn disconnect(...);
    async fn next_event(...);
}
```

Do not force the MQTT 3.1.1 and MQTT 5 clients into a large artificial common trait if their semantics differ substantially.

A protocol enum with delegated methods may be simpler:

```rust
enum Client {
    V4(V4Client),
    V5(V5Client),
}
```

Use typed protocol-specific structures where MQTT 5 properties must remain available.

## 6.2 Event-loop ownership

Each command must clearly own and drive its event loop.

Avoid:

* detached event-loop tasks with ignored failures
* unbounded channels
* tasks that outlive command shutdown
* dropping event-loop errors
* treating task cancellation as a successful disconnect

Use structured concurrency:

```text
command task
event-loop task
signal task
optional timeout task
```

Ensure all tasks are joined or cancelled deterministically.

## 6.3 Graceful shutdown

Implement shared Ctrl-C and termination handling.

On graceful shutdown:

1. stop accepting new work
2. finish or cancel pending command operations according to command policy
3. send MQTT DISCONNECT where possible
4. flush session state
5. wait for event-loop completion with a bounded timeout
6. return an appropriate exit code

A second interrupt may terminate immediately.

---

# 7. `mqtt pub`

## 7.1 Required interface

```text
mqtt pub <TOPIC>
```

Payload source options:

```text
-m, --message <TEXT>
-f, --file <PATH>
--stdin
--hex <HEX>
--base64 <BASE64>
--empty
```

Require exactly one payload source, except that an empty payload may be the documented default if desired.

## 7.2 Publish options

Support:

```text
-q, --qos <0|1|2>
-r, --retain
--message-expiry <DURATION>
--content-type <VALUE>
--payload-format <utf8|unspecified>
--response-topic <TOPIC>
--correlation-data <VALUE>
--correlation-data-hex <HEX>
--correlation-data-base64 <BASE64>
--user-property <KEY=VALUE>
--topic-alias <NUMBER>
--repeat <COUNT>
--interval <DURATION>
--wait-for-ack
--ack-timeout <DURATION>
```

Use repeatable arguments for user properties.

## 7.3 Publish completion semantics

Define explicit completion behavior:

* QoS 0: success after the request is accepted by the client/event loop
* QoS 1: optionally wait for PUBACK
* QoS 2: optionally wait for PUBCOMP

Recommended default:

```text
wait for the protocol-level acknowledgement for QoS 1 and QoS 2
```

Allow opting out for high-throughput scripting if needed.

Do not report success merely because a request entered a local channel.

## 7.4 Repeated publishing

For:

```text
--repeat <COUNT>
--interval <DURATION>
```

Requirements:

* use a bounded loop
* do not accumulate all payloads in memory
* preserve ordering per client
* stop cleanly on interrupt
* report the sequence number on structured output when requested
* clarify whether the interval is measured from send start or send completion

Recommended behavior:

```text
interval between publish attempts
```

## 7.5 Exit behavior

Return nonzero when:

* connection establishment fails
* payload loading fails
* publishing fails
* required acknowledgement times out
* session persistence fails
* protocol validation fails

---

# 8. `mqtt sub`

## 8.1 Topic arguments

Support one or more subscriptions:

```text
mqtt sub <TOPIC_FILTER>...
```

Allow optional per-filter QoS using one well-defined syntax.

Possible shape:

```text
mqtt sub 'orders/#' 'alerts/+:2'
```

Prefer an explicit repeatable option if inline syntax becomes ambiguous:

```text
--topic 'orders/#'
--topic 'alerts/+' --qos 2
```

Do not accept invalid topic filters.

## 8.2 Subscription options

Support:

```text
-q, --qos <0|1|2>
-v, --verbose
--count <COUNT>
--timeout <DURATION>
--idle-timeout <DURATION>
--no-local
--retain-as-published
--retain-handling <always|new|never>
--subscription-id <NUMBER>
--user-property <KEY=VALUE>
```

Clarify whether `--timeout` covers:

* the entire command lifetime, or
* waiting for initial subscription acknowledgement

Recommended definitions:

```text
--timeout       total command runtime
--idle-timeout  maximum time without receiving a message
```

## 8.3 Output modes

Support:

```text
--output <payload|verbose|jsonl|raw>
--format <TEMPLATE>
--payload-encoding <auto|utf8|hex|base64>
```

Recommended default:

```text
payload
```

Recommended structured format:

```text
jsonl
```

Do not use one large JSON array for an unbounded stream.

## 8.4 JSONL schema

Define and document a versioned schema.

Example:

```json
{
  "schema_version": 1,
  "received_at": "2026-08-01T18:30:00.123Z",
  "topic": "sensors/temperature",
  "qos": 1,
  "retain": false,
  "dup": false,
  "payload": "21.4",
  "payload_encoding": "utf8",
  "properties": {
    "content_type": "text/plain",
    "response_topic": null,
    "correlation_data_base64": null,
    "subscription_identifiers": [],
    "user_properties": []
  }
}
```

If payload bytes are not valid UTF-8:

* never emit invalid JSON
* encode as base64 or hexadecimal
* indicate the encoding explicitly

## 8.5 Format templates

Support documented placeholders:

```text
{received_at}
{topic}
{payload}
{payload_hex}
{payload_base64}
{qos}
{retain}
{dup}
{content_type}
{response_topic}
{correlation_data_hex}
{correlation_data_base64}
{subscription_ids}
{user_properties}
```

Requirements:

* validate templates before connecting
* fail on unknown placeholders
* do not silently emit empty text for misspelled placeholders
* provide escaping for literal braces
* write each rendered message atomically where practical

## 8.6 Count and timeout completion

When `--count N` is supplied:

1. subscribe
2. receive exactly `N` matching messages
3. gracefully disconnect
4. return success

A count of zero should be rejected or clearly documented.

---

# 9. Persistent Sessions

## 9.1 Session option

Use:

```text
--session <PATH>
```

Avoid promising that the path is always a single file.

The path may refer to:

* a file
* a directory
* a storage namespace

## 9.2 Session identity

Persistent session state must be bound to at least:

* protocol version
* broker identity
* client ID
* relevant session configuration
* storage format version

Fail safely when state is incompatible.

Do not silently reuse a session created for another broker or client ID.

## 9.3 Locking

Prevent concurrent writers.

When a session is already in use:

* fail with a clear error
* identify the session path
* do not attempt unsafe concurrent recovery

Read-only inspection may be allowed if the storage backend supports it safely.

## 9.4 Corruption handling

Implement explicit behavior for corrupt state.

Recommended first-release behavior:

* refuse to connect using corrupt state
* explain that the session can be inspected or cleared
* never silently discard persisted QoS state

An explicit recovery option may be added later.

## 9.5 Versioning

Persist a format version.

Document whether the storage format is:

```text
stable
provisional
internal
```

Until compatibility guarantees are intentionally established, label it provisional and reject unknown newer versions.

## 9.6 Credential handling

Do not persist:

* plaintext passwords
* private-key passphrases
* raw authentication secrets

Persist only protocol/session state required for resumption.

## 9.7 Flush behavior

Flush durable state:

* after state-changing protocol transitions where required
* before successful command exit
* during graceful shutdown
* after connection loss if state has changed

Avoid excessive synchronous disk writes on the async runtime.

Use blocking-task isolation or asynchronous storage APIs as appropriate.

---

# 10. `mqtt session`

## 10.1 Initial commands

Implement:

```text
mqtt session show <PATH>
mqtt session validate <PATH>
mqtt session clear <PATH>
```

## 10.2 `session show`

Display non-secret metadata:

```text
storage format version
protocol version
broker identity
client ID
creation time
last update time
session expiry
pending outbound QoS messages
pending inbound QoS messages
subscriptions, where available
clean shutdown marker
```

Support:

```text
--output human
--output json
```

## 10.3 `session validate`

Validate:

* readable storage
* recognized format version
* internal consistency
* checksums if present
* required metadata
* protocol compatibility

Do not mutate storage.

## 10.4 `session clear`

Require an explicit path.

Consider requiring:

```text
--yes
```

when stdin is not interactive.

Do not recursively delete arbitrary parent directories. Ensure deletion is constrained to the recognized session storage path.

---

# 11. `mqtt inspect connect`

## 11.1 Purpose

Provide a connection diagnostic command that exposes transport and MQTT negotiation details without requiring packet-level expertise.

## 11.2 Usage

```text
mqtt inspect connect [connection options]
```

## 11.3 Diagnostic stages

Measure and report:

```text
configuration resolution
DNS resolution
proxy negotiation
TCP connection
TLS handshake
WebSocket handshake
MQTT CONNECT/CONNACK exchange
enhanced authentication exchange
graceful disconnect
```

Only display stages relevant to the chosen transport.

## 11.4 MQTT 5 details

Report available CONNACK properties, including:

```text
session present
assigned client identifier
server keep alive
receive maximum
maximum QoS
retain available
maximum packet size
topic alias maximum
wildcard subscription support
subscription identifier support
shared subscription support
response information
server reference
authentication method
user properties
reason string
```

Do not interpret absent optional properties as explicit support unless the specification defines a default.

## 11.5 Output modes

Support:

```text
--output human
--output json
```

JSON should include:

```text
schema version
resolved endpoint
timings by stage
negotiated transport
TLS metadata
CONNACK reason
broker capabilities
warnings
```

## 11.6 TLS information

Display:

```text
TLS provider
protocol version
cipher suite, if available
server name
certificate subject
certificate issuer
certificate validity period
certificate fingerprint
verification status
```

Do not dump private key material or excessively verbose certificate contents by default.

## 11.7 Exit codes

Differentiate at least:

```text
0  success
1  general failure
2  CLI/configuration error
3  DNS failure
4  transport connection failure
5  TLS failure
6  proxy failure
7  WebSocket failure
8  MQTT connection refused
9  authentication failure
10 timeout
```

Centralize exit-code definitions.

---

# 12. Authentication

## 12.1 Basic authentication

Support username and password for MQTT 3.1.1 and MQTT 5.0.

## 12.2 Enhanced authentication

Use a generalized interface.

Suggested flags:

```text
--auth-method <METHOD>
--auth-data <VALUE>
--auth-data-file <PATH>
--auth-data-hex <HEX>
--auth-data-base64 <BASE64>
```

## 12.3 SCRAM

Expose supported SCRAM mechanisms explicitly.

Example:

```text
--auth-method scram-sha-256
--auth-method scram-sha-512
```

Map CLI values to the library’s `auth-scram` implementation.

Requirements:

* reject SCRAM under MQTT 3.1.1
* reject incompatible auth-data options
* redact SCRAM secrets
* correctly handle multi-step AUTH exchanges
* surface broker reason codes and reason strings
* test authentication cancellation and timeout behavior

---

# 13. Proxy and Transport Support

## 13.1 Supported transports

Support according to enabled crate features:

```text
plain TCP
TLS over TCP
WebSocket
secure WebSocket
HTTP proxy
SOCKS5 proxy
```

The executable should report at runtime when a requested capability was not compiled in.

Example:

```text
this build does not include SOCKS5 proxy support
```

## 13.2 Feature-gated builds

Define CLI crate features that map clearly to underlying features.

Possible shape:

```toml
default = ["v4", "v5", "rustls", "websocket", "proxy", "session-file"]

v4 = [...]
v5 = [...]
rustls = [...]
native-tls = [...]
websocket = [...]
proxy = [...]
scram = [...]
session-file = [...]
bench = [...]
scenario = [...]
shell = [...]
```

Ensure invalid feature combinations fail at compile time where possible.

## 13.3 Transport reporting

`mqtt inspect connect` and debug logs should report:

* requested transport
* selected implementation
* TLS provider
* proxy type
* WebSocket path
* relevant compile-time capability limitations

---

# 14. Logging and Diagnostics

## 14.1 Logging interface

Support:

```text
--log error
--log warn
--log info
--log debug
--log trace
```

Default command output must remain separate from diagnostic logs.

Recommended streams:

```text
stdout: command result or message stream
stderr: logs, warnings, progress, diagnostics
```

## 14.2 Structured command output

Never interleave logs with JSON or JSONL written to stdout.

## 14.3 Packet tracing

Do not enable full packet payload tracing by default.

A later explicit option may be:

```text
--trace-packets
--trace-payloads
```

Payload tracing should display a warning because it may expose sensitive data.

---

# 15. `mqtt bench`

## 15.1 Scope

Implement load testing as a production-oriented tool, not merely as a wrapper around microbenchmarks.

Initial subcommands:

```text
mqtt bench pub
mqtt bench roundtrip
mqtt bench reconnect
```

Potential later additions:

```text
mqtt bench sub
mqtt bench fanout
mqtt bench session-resume
```

## 15.2 Shared benchmark options

Support:

```text
--clients <COUNT>
--connections-per-second <RATE>
--duration <DURATION>
--warmup <DURATION>
--rate <MESSAGES_PER_SECOND>
--payload-size <BYTES>
--payload-file <PATH>
--topic-template <TEMPLATE>
--client-id-template <TEMPLATE>
--qos <0|1|2>
--inflight <COUNT>
--report-interval <DURATION>
--output <human|json|jsonl>
--report <PATH>
--seed <NUMBER>
```

## 15.3 Topic and client templates

Support deterministic placeholders:

```text
{client}
{sequence}
{run}
{random}
```

Validate templates before starting connections.

Provide deterministic random generation when `--seed` is specified.

## 15.4 Rate models

Implement explicit modes.

### Throughput mode

No target `--rate`:

```text
send as fast as permitted by backpressure and in-flight limits
```

### Rate-controlled mode

With `--rate`:

```text
maintain a global target publish-attempt rate
```

Do not assign the full global rate independently to every client.

Use a rate limiter that avoids burst accumulation after stalls unless burst behavior is intentionally configurable.

## 15.5 Open-loop versus closed-loop

Document benchmark semantics.

### Open-loop publishing

Publish attempts are scheduled by time.

Useful for:

* overload behavior
* queueing behavior
* sustained target-rate testing

### Closed-loop roundtrip

A client sends the next request based on acknowledgement or response completion.

Useful for:

* latency measurement
* maximum sustainable throughput
* backpressure behavior

Do not combine results from both models under one ambiguous latency metric.

## 15.6 Metrics

Track at least:

```text
connection attempts
successful connections
connection failures
active connections
publish attempts
publish accepted locally
publish acknowledgements
messages received
bytes sent
bytes received
protocol errors
transport errors
authentication failures
disconnects
reconnects
session resumptions
session resume failures
timeouts
dropped operations
backpressure events
in-flight utilization
```

Latency metrics:

```text
connection latency
publish acknowledgement latency
roundtrip latency
reconnect latency
session-resumption latency
```

Report percentiles:

```text
p50
p90
p95
p99
p99.9
maximum
```

Use an appropriate bounded-memory histogram.

## 15.7 Benchmark correctness

Before reporting successful results:

* verify expected acknowledgements
* detect duplicate or missing sequence IDs where applicable
* distinguish timeout from broker rejection
* distinguish local enqueue from broker acknowledgement
* detect measurement warmup and cooldown boundaries
* avoid counting setup traffic as steady-state traffic

## 15.8 Resource bounds

Benchmark implementation must use:

* bounded channels
* explicit in-flight limits
* bounded histogram memory
* controlled task counts
* connection ramp-up
* cancellation propagation

Do not spawn one task per message.

One task per client may be acceptable initially, but measure and document its scaling limits.

## 15.9 Reports

Human output should provide a concise summary.

JSON report should include:

```text
schema version
CLI version
crate versions
enabled features
resolved benchmark configuration
start and end timestamps
duration
aggregate counters
latency distributions
errors grouped by category
broker capability summary where available
```

Do not include credentials.

---

# 16. `mqtt bench pub`

## 16.1 Behavior

Create N clients and publish according to the selected rate model.

Support:

```text
fixed payload
generated payload
payload file
fixed topic
templated topic
```

## 16.2 Completion

At benchmark end:

1. stop scheduling new publishes
2. optionally drain pending acknowledgements for a bounded period
3. disconnect clients
4. flush session state if enabled
5. write the final report

Add:

```text
--drain-timeout <DURATION>
```

---

# 17. `mqtt bench roundtrip`

## 17.1 Behavior

Measure application-level request/response latency using MQTT 5 response topics and correlation data.

Each request should contain a unique correlation identifier.

The benchmark should:

1. subscribe to response topics
2. confirm subscriptions
3. publish requests
4. match responses to requests
5. record latency
6. detect unknown, late, and duplicate responses

## 17.2 Compatibility mode

Optionally support a topic-template-based correlation mode for MQTT 3.1.1.

Keep MQTT 5 response-topic/correlation-data behavior as the preferred path.

## 17.3 Missing responder

Fail clearly when no response is received.

Track:

```text
requests sent
responses received
responses timed out
late responses
duplicate responses
unknown correlations
```

---

# 18. `mqtt bench reconnect`

## 18.1 Purpose

Exercise reconnection and session-resumption behavior.

Support scenarios such as:

```text
periodic intentional disconnect
transport abort
broker restart window
network unavailability window
session resume after process restart
```

The first version may implement only intentional connection interruption.

## 18.2 Metrics

Track:

```text
disconnect detection time
reconnect latency
successful reconnects
failed reconnects
session-present responses
resumed in-flight messages
duplicate delivery
message loss
subscription restoration
```

## 18.3 Persistence mode

Allow benchmark clients to use distinct persistent session paths.

Ensure session paths cannot collide across clients.

---

# 19. `mqtt run`

## 19.1 Purpose

Execute declarative MQTT integration scenarios.

This should be preferred over putting scripting complexity into `mqtt shell`.

## 19.2 Initial format

Use TOML initially unless the workspace has a strong YAML convention.

Example:

```toml
version = 1

[connection]
profile = "development"
protocol = "v5"

[[steps]]
action = "subscribe"
topic = "responses/${run_id}"
qos = 1

[[steps]]
action = "publish"
topic = "requests/test"
payload_file = "fixtures/request.json"
qos = 1
response_topic = "responses/${run_id}"

[[steps]]
action = "expect"
topic = "responses/${run_id}"
timeout = "5s"

[steps.payload]
json_path = "$.status"
equals = "ok"
```

## 19.3 Initial actions

Implement:

```text
connect
subscribe
unsubscribe
publish
expect
sleep
disconnect
```

Potential later actions:

```text
repeat
parallel
set
extract
assert-connection
kill-transport
reconnect
```

## 19.4 Variables

Support built-in variables:

```text
${run_id}
${timestamp}
${sequence}
```

Allow explicit variables from:

```text
--var NAME=VALUE
```

Do not add a full programming language in the initial version.

## 19.5 Assertions

Initial assertions may include:

```text
topic equals
payload bytes equal
payload UTF-8 equals
payload contains
JSON field equals
QoS equals
retain equals
property equals
message arrives within timeout
no message arrives during duration
```

Use a maintained JSON query implementation if JSONPath is supported. Otherwise begin with simple dotted field paths.

## 19.6 Exit behavior

Return nonzero when any step or assertion fails.

Print:

* failed step number
* action
* concise reason
* relevant non-secret values

Support JSON reports for CI.

---

# 20. `mqtt shell`

## 20.1 Priority

Implement only after:

```text
pub
sub
inspect
session
initial bench
initial run
```

## 20.2 Initial commands

Potential REPL commands:

```text
connect
disconnect
reconnect
pub
sub
unsub
subscriptions
session
properties
status
help
quit
```

## 20.3 Concurrency

Incoming messages must not corrupt the input line.

Use a terminal library that supports asynchronous output and line restoration.

Do not implement an ad hoc terminal editor.

## 20.4 Scope control

Do not turn the shell into a separate scripting language.

Reusable automation belongs in `mqtt run`.

---

# 21. Error Model

## 21.1 Typed errors

Define command-level error categories using `thiserror`.

Suggested structure:

```rust
enum CliError {
    Configuration(...),
    Credentials(...),
    Io(...),
    Dns(...),
    Proxy(...),
    Tls(...),
    WebSocket(...),
    Connect(...),
    Authentication(...),
    Protocol(...),
    Session(...),
    Timeout(...),
    Output(...),
    Benchmark(...),
    Scenario(...),
}
```

## 21.2 Error chains

Human output should show:

* concise top-level failure
* useful cause chain when `--log debug` is enabled

Do not expose secrets in nested source errors.

## 21.3 Reason codes

For MQTT 5 errors, include:

* numeric reason code
* symbolic reason name
* reason string, if supplied
* relevant user properties, where safe

---

# 22. Exit Codes

Centralize exit codes in one module.

Suggested allocation:

```text
0   success
1   general runtime failure
2   invalid command-line usage or configuration
3   input/output failure
4   connection failure
5   authentication or authorization failure
6   protocol failure
7   timeout
8   session-store failure
9   assertion or scenario failure
10  benchmark completed with configured failure threshold exceeded
130 interrupted by SIGINT
```

Platform-specific signal handling may affect exact exit behavior. Document it.

---

# 23. Output and UX

## 23.1 Stable machine-readable output

Treat JSON and JSONL schemas as versioned interfaces.

Every object should contain:

```text
schema_version
```

Do not make cosmetic human-output changes affect structured output.

## 23.2 TTY awareness

When stdout is not a terminal:

* disable spinners
* disable ANSI styling unless explicitly requested
* avoid progress redraws
* preserve clean stream output

## 23.3 Color

Support:

```text
--color auto
--color always
--color never
```

Do not color JSON or raw payload output.

## 23.4 Progress indicators

Benchmarks may show progress only on stderr and only for interactive terminals.

---

# 24. Validation Utilities

Implement reusable validation for:

```text
topic names
topic filters
QoS values
durations
byte sizes
key=value arguments
URLs
proxy URLs
WebSocket headers
client identifiers
session-expiry values
receive maximum
packet-size limits
topic aliases
subscription identifiers
format templates
benchmark templates
```

Validation should happen before attempting a network connection whenever possible.

---

# 25. Testing Strategy

## 25.1 Unit tests

Cover:

```text
argument parsing
configuration precedence
URL resolution
credential-source conflicts
secret redaction
protocol-option compatibility
topic validation
template parsing
payload encoding
JSONL serialization
exit-code mapping
session metadata validation
benchmark rate calculations
```

## 25.2 Integration tests

Run against local test brokers where practical.

Cover at least:

```text
MQTT 3.1.1 publish and subscribe
MQTT 5 publish and subscribe
QoS 0
QoS 1
QoS 2
retained messages
wildcard subscriptions
MQTT 5 user properties
response topic and correlation data
TLS
WebSocket
proxy transport
authentication rejection
connection timeout
graceful disconnect
persistent session resume
```

## 25.3 Session restart tests

Create tests that:

1. start a subscriber with a persistent session
2. subscribe at QoS 1 or QoS 2
3. stop the process
4. publish while the subscriber is offline
5. restart using the same session path and client ID
6. verify resumed delivery
7. verify no session corruption

Include negative tests for:

* changed client ID
* changed broker
* changed protocol version
* concurrent session use
* truncated storage
* unsupported storage version

## 25.4 CLI snapshot tests

Use snapshot testing sparingly for:

```text
--help
configuration errors
validation errors
human inspect output
session show output
```

Prefer semantic assertions for JSON.

## 25.5 Benchmark tests

Do not require large-scale performance runs in normal CI.

CI should test:

```text
one client
small message counts
rate limiter correctness
metrics accounting
graceful cancellation
report serialization
timeout handling
duplicate response detection
```

Large-scale tests should be optional or scheduled separately.

---

# 26. Documentation

## 26.1 CLI README

Document:

```text
installation
feature support
quick-start pub/sub examples
protocol selection
configuration profiles
credential safety
TLS
WebSockets
proxies
persistent sessions
structured output
inspect usage
benchmark semantics
scenario runner
exit codes
```

## 26.2 Help examples

Each major command should include examples in long help output.

Example:

```text
mqtt pub sensors/temperature -m 21.4 -q 1

mqtt sub 'sensors/#' --output jsonl

mqtt --profile production inspect connect

mqtt sub 'orders/#' \
  --client-id order-monitor \
  --session ./sessions/order-monitor \
  --session-expiry 7d
```

## 26.3 Feature matrix

Publish a table showing support by:

```text
MQTT 3.1.1
MQTT 5.0
rustls
native TLS
WebSocket
HTTP proxy
SOCKS5 proxy
SCRAM
file session store
```

Generate or test this matrix against Cargo feature definitions where practical.

---

# 27. Packaging and Distribution

## 27.1 Binary name

Preferred binary:

```text
mqtt
```

Verify that this does not create an unacceptable packaging or ecosystem conflict.

Fallback names:

```text
rumqtt
rumqtt-cli
rumqttc
```

The crate package name may differ from the installed binary name.

## 27.2 Completion scripts

Generate shell completion for:

```text
bash
zsh
fish
PowerShell
elvish
```

## 27.3 Manual pages

Generate command documentation or man pages from `clap` metadata if practical.

## 27.4 Release artifacts

Consider release binaries for:

```text
Linux x86_64
Linux aarch64
macOS x86_64
macOS aarch64
Windows x86_64
```

Feature support may differ by platform. Report those differences clearly.

---

# 28. Implementation Phases

## Phase 0: Architecture and scaffolding

* [ ] Add the CLI crate to the workspace.
* [ ] Define Cargo features and map them to underlying crate features.
* [ ] Add `clap` command definitions.
* [ ] Add typed errors and exit-code mapping.
* [ ] Add tracing initialization.
* [ ] Add resolved configuration types.
* [ ] Add protocol and transport enums.
* [ ] Add centralized validation.
* [ ] Add secret-redaction utilities.
* [ ] Add graceful-shutdown infrastructure.
* [ ] Add CI checks for the CLI crate.

### Phase 0 acceptance criteria

* [ ] `mqtt --help` works.
* [ ] All planned subcommands appear.
* [ ] Invalid option combinations fail before network access.
* [ ] Secrets are redacted from debug output.
* [ ] The crate builds with minimal and default feature sets.

---

## Phase 1: Connection builder

* [ ] Implement URL parsing.
* [ ] Implement host/port resolution.
* [ ] Implement MQTT protocol selection.
* [ ] Implement client-ID resolution.
* [ ] Implement credential loading.
* [ ] Implement plain TCP.
* [ ] Implement rustls.
* [ ] Implement native TLS where enabled.
* [ ] Implement WebSocket and secure WebSocket.
* [ ] Implement HTTP proxy support where available.
* [ ] Implement SOCKS5 proxy support where available.
* [ ] Implement MQTT 5 enhanced authentication.
* [ ] Implement SCRAM mapping.
* [ ] Implement shared event-loop supervision.
* [ ] Implement connection and acknowledgement timeouts.

### Phase 1 acceptance criteria

* [ ] One resolved config can construct either a v4 or v5 client.
* [ ] Contradictory transport options produce actionable errors.
* [ ] Event-loop failures reach the calling command.
* [ ] Ctrl-C causes a bounded graceful shutdown.
* [ ] No command leaves detached tasks running.

---

## Phase 2: `mqtt pub`

* [ ] Implement text payloads.
* [ ] Implement file payloads.
* [ ] Implement stdin payloads.
* [ ] Implement hexadecimal payloads.
* [ ] Implement base64 payloads.
* [ ] Implement empty payloads.
* [ ] Implement QoS 0, 1, and 2.
* [ ] Implement retained publishing.
* [ ] Implement MQTT 5 publish properties.
* [ ] Implement repeat and interval.
* [ ] Implement acknowledgement waiting.
* [ ] Implement acknowledgement timeout.
* [ ] Implement human result output.
* [ ] Implement JSON result output.
* [ ] Add unit and integration tests.

### Phase 2 acceptance criteria

* [ ] QoS 1 success means PUBACK was observed.
* [ ] QoS 2 success means PUBCOMP was observed.
* [ ] Payload bytes are preserved exactly.
* [ ] Invalid MQTT 5 options fail in v311 mode.
* [ ] Interrupting repeated publishing exits cleanly.

---

## Phase 3: `mqtt sub`

* [ ] Implement one or more topic filters.
* [ ] Implement wildcard subscriptions.
* [ ] Implement per-subscription QoS or a documented shared QoS.
* [ ] Implement MQTT 5 subscription options.
* [ ] Implement message count.
* [ ] Implement total timeout.
* [ ] Implement idle timeout.
* [ ] Implement payload output.
* [ ] Implement verbose output.
* [ ] Implement raw output.
* [ ] Implement JSONL output.
* [ ] Implement payload encoding detection.
* [ ] Implement format templates.
* [ ] Implement graceful unsubscribe/disconnect behavior.
* [ ] Add unit and integration tests.

### Phase 3 acceptance criteria

* [ ] Binary payloads always produce valid structured output.
* [ ] JSONL contains one object per received message.
* [ ] `--count N` receives N messages and exits successfully.
* [ ] Template errors are detected before connection.
* [ ] stdout remains free of logs.

---

## Phase 4: Configuration profiles

* [ ] Implement default configuration path discovery.
* [ ] Implement explicit `--config`.
* [ ] Implement named profiles.
* [ ] Implement default profile selection.
* [ ] Implement documented precedence.
* [ ] Implement profile validation.
* [ ] Add resolved-config debug output with secrets redacted.
* [ ] Add tests for all precedence combinations.

### Phase 4 acceptance criteria

* [ ] CLI values override profile values.
* [ ] Missing default config is not an error.
* [ ] Missing explicitly requested config is an error.
* [ ] Sensitive values never appear in diagnostics.

---

## Phase 5: Persistent sessions

* [ ] Integrate session-store-file adapters.
* [ ] Define session path semantics.
* [ ] Bind session state to broker, protocol, and client ID.
* [ ] Implement storage versioning.
* [ ] Implement exclusive locking.
* [ ] Implement corruption detection.
* [ ] Implement graceful flush.
* [ ] Implement crash/restart tests.
* [ ] Document compatibility guarantees.
* [ ] Ensure credentials are not persisted.

### Phase 5 acceptance criteria

* [ ] QoS state survives process restart.
* [ ] Offline messages can be delivered after session resumption.
* [ ] Concurrent writers are rejected safely.
* [ ] Incompatible session state is never silently reused.
* [ ] Corrupt state is reported without silent deletion.

---

## Phase 6: `mqtt session`

* [ ] Implement `session show`.
* [ ] Implement human output.
* [ ] Implement JSON output.
* [ ] Implement `session validate`.
* [ ] Implement `session clear`.
* [ ] Add non-interactive deletion safeguards.
* [ ] Add tests for valid, incompatible, and corrupt state.

### Phase 6 acceptance criteria

* [ ] Session metadata can be inspected without connecting.
* [ ] Validation does not mutate state.
* [ ] Clear cannot delete outside the intended session location.

---

## Phase 7: `mqtt inspect connect`

* [ ] Measure DNS resolution.
* [ ] Measure proxy negotiation.
* [ ] Measure TCP connection.
* [ ] Measure TLS negotiation.
* [ ] Measure WebSocket negotiation.
* [ ] Measure MQTT connection.
* [ ] Report MQTT 5 broker capabilities.
* [ ] Report TLS certificate summary.
* [ ] Implement human output.
* [ ] Implement JSON output.
* [ ] Implement categorized exit codes.
* [ ] Add integration tests for success and failure stages.

### Phase 7 acceptance criteria

* [ ] A user can identify the stage at which connection failed.
* [ ] JSON diagnostics are stable and versioned.
* [ ] Broker-provided reason codes are preserved.
* [ ] Credentials remain redacted.

---

## Phase 8: Initial benchmarks

* [ ] Define benchmark configuration types.
* [ ] Implement deterministic templates.
* [ ] Implement connection ramp-up.
* [ ] Implement throughput mode.
* [ ] Implement global rate-controlled mode.
* [ ] Implement bounded in-flight tracking.
* [ ] Implement metrics aggregation.
* [ ] Implement latency histograms.
* [ ] Implement periodic human output.
* [ ] Implement JSON and JSONL output.
* [ ] Implement final report files.
* [ ] Implement bounded shutdown and drain behavior.
* [ ] Implement `bench pub`.
* [ ] Add small deterministic tests.

### Phase 8 acceptance criteria

* [ ] Target rate is global, not multiplied per client.
* [ ] Local enqueue and broker acknowledgement are separate metrics.
* [ ] Benchmark memory remains bounded.
* [ ] No task is spawned per message.
* [ ] Final reports contain no secrets.

---

## Phase 9: Roundtrip and reconnect benchmarks

* [ ] Implement MQTT 5 response-topic roundtrips.
* [ ] Implement correlation-data matching.
* [ ] Detect duplicate responses.
* [ ] Detect late responses.
* [ ] Detect unknown responses.
* [ ] Implement request timeout.
* [ ] Implement reconnect benchmark.
* [ ] Measure session-present behavior.
* [ ] Measure resumed message delivery.
* [ ] Add fault-oriented integration tests.

### Phase 9 acceptance criteria

* [ ] Roundtrip latency measures matched application responses.
* [ ] Timed-out requests are not counted as successful latency samples.
* [ ] Reconnect reports distinguish reconnect from session resumption.
* [ ] Duplicate and missing messages are visible in reports.

---

## Phase 10: Scenario runner

* [ ] Define versioned scenario schema.
* [ ] Implement TOML parsing.
* [ ] Implement variable substitution.
* [ ] Implement connect.
* [ ] Implement subscribe.
* [ ] Implement publish.
* [ ] Implement expect.
* [ ] Implement unsubscribe.
* [ ] Implement sleep.
* [ ] Implement disconnect.
* [ ] Implement basic payload assertions.
* [ ] Implement MQTT property assertions.
* [ ] Implement JSON reports.
* [ ] Add CI-oriented examples and tests.

### Phase 10 acceptance criteria

* [ ] Scenario failures identify the exact failed step.
* [ ] The process returns nonzero on failed assertions.
* [ ] Scenario files cannot access arbitrary secrets unless explicitly passed.
* [ ] Scenario execution shuts down cleanly after failure.

---

## Phase 11: Interactive shell

* [ ] Select an appropriate async terminal library.
* [ ] Implement persistent connection state.
* [ ] Implement asynchronous message display.
* [ ] Implement publish and subscribe commands.
* [ ] Implement connection status.
* [ ] Implement reconnect and disconnect.
* [ ] Implement history without recording secrets.
* [ ] Implement clean terminal restoration.
* [ ] Add basic interactive tests where practical.

### Phase 11 acceptance criteria

* [ ] Incoming messages do not corrupt user input.
* [ ] Shell history excludes passwords and auth data.
* [ ] Exiting the shell gracefully disconnects.
* [ ] The shell reuses the same connection builder as other commands.

---

# 29. Quality Gates

Before marking the first stable release complete:

* [ ] `cargo fmt --check` passes.
* [ ] `cargo clippy --all-targets --all-features` passes without unjustified allows.
* [ ] Default-feature tests pass.
* [ ] Minimal-feature builds pass.
* [ ] MQTT 3.1.1 tests pass.
* [ ] MQTT 5.0 tests pass.
* [ ] TLS tests pass.
* [ ] WebSocket tests pass where enabled.
* [ ] Proxy tests pass where enabled.
* [ ] Session restart tests pass.
* [ ] Secret-redaction tests pass.
* [ ] Structured-output schemas are documented.
* [ ] Exit codes are documented.
* [ ] Command help includes working examples.
* [ ] No detached event-loop tasks remain.
* [ ] No unbounded channels exist without a documented justification.
* [ ] No `unwrap` or `expect` remains on user-controlled runtime paths.
* [ ] Unsafe code is absent or explicitly justified.
* [ ] Benchmark reports distinguish attempted, locally accepted, acknowledged, and received operations.
* [ ] The README explains the difference between clean start and durable session state.

---

# 30. Non-Goals for the First Stable Release

Do not block the first release on:

* a graphical interface
* broker administration APIs
* full packet capture decoding
* plugin loading
* an embedded scripting language
* cluster orchestration
* distributed load generation
* Prometheus server mode
* arbitrary certificate-management workflows
* automatic protocol downgrade
* silent session recovery after corruption
* perfect compatibility with every `mosquitto_pub` or `mosquitto_sub` flag

Compatibility aliases may be added where they improve migration, but the CLI should retain internally consistent semantics.

---

# 31. Recommended First Stable Release Boundary

The first stable release should contain:

* [ ] MQTT 3.1.1 and MQTT 5.0 selection
* [ ] `mqtt pub`
* [ ] `mqtt sub`
* [ ] plain TCP
* [ ] TLS
* [ ] WebSocket
* [ ] supported proxy transports
* [ ] basic and SCRAM authentication
* [ ] configuration profiles
* [ ] safe credential input
* [ ] JSONL subscription output
* [ ] MQTT 5 publish and subscription properties
* [ ] durable file-backed sessions
* [ ] `mqtt session show`
* [ ] `mqtt session validate`
* [ ] `mqtt session clear`
* [ ] `mqtt inspect connect`
* [ ] graceful shutdown
* [ ] stable structured-output schemas
* [ ] integration tests covering restart and session resumption

A first benchmark release should follow once its metrics and load model are trustworthy. Do not ship misleading latency or throughput numbers merely to include `mqtt bench` in the first release.

---

# 32. Codex Implementation Guidance

When implementing tasks from this file:

1. Inspect both `rumqttc-v4-next` and `rumqttc-v5-next` before introducing abstractions.
2. Reuse existing option, transport, authentication, and session-store types where they are suitable.
3. Do not assume that v4 and v5 APIs are structurally identical.
4. Keep protocol-specific property construction in `protocol/v4.rs` and `protocol/v5.rs`.
5. Keep command orchestration protocol-neutral where doing so does not erase meaningful semantics.
6. Add tests in the same change as each behavior.
7. Prefer small, reviewable commits grouped by one vertical capability.
8. Do not leave event-loop tasks detached.
9. Do not treat local channel submission as broker acknowledgement.
10. Do not log secrets, raw credentials, or private-key passwords.
11. Validate user input before opening network connections.
12. Preserve source errors while presenting concise top-level messages.
13. Avoid unbounded queues and task creation.
14. Keep JSON schemas explicit and versioned.
15. Document any public behavior that may become a compatibility commitment.
16. Run formatting, Clippy, and relevant tests before marking a task complete.

Suggested vertical implementation order:

```text
CLI parsing
→ resolved configuration
→ v5 plain TCP connection
→ v5 pub
→ v5 sub
→ v4 connection/pub/sub
→ TLS and WebSockets
→ proxies
→ authentication
→ configuration profiles
→ persistent sessions
→ inspect
→ benchmarks
→ scenarios
→ shell
```

Do not begin with a large universal client trait. Implement one complete vertical path, observe the real differences between the two protocol crates, and extract shared abstractions only after duplication becomes concrete.
