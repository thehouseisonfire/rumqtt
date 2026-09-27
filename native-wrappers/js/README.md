# `@rumqtt-next/rumqttc`

`@rumqtt-next/rumqttc` is one native MQTT client for MQTT 3.1.1 and MQTT 5. Each `MqttClient` selects a protocol permanently at construction. It uses stable Node-API and the same prebuilt addon under Node.js, local Deno, and Bun.

```sh
bun add @rumqtt-next/rumqttc
# or: npm install @rumqtt-next/rumqttc
```

```ts
import { MqttClient } from '@rumqtt-next/rumqttc'

const client = new MqttClient({
  protocol: '5.0',
  brokerHost: 'localhost',
  brokerPort: 1883,
  clientId: 'example',
})

await client.connect()
const events = client.events()
const completion = await client.publish('sensors/temperature', '21.5', { qos: 1 })
console.log(completion.milestone) // qos1Acknowledged
await client.close()
```

## Runtime requirements

- Node.js 24 or newer, using either ESM `import` or CommonJS `require`.
- Deno 2.9.5 or newer with a local `node_modules`, `--node-modules-dir=auto`, and `--allow-ffi`. Network, environment, and package-read permissions must also be granted as required by the application.
- Bun 1.3.14 or newer.

Supported artifacts are Linux x64 glibc, Linux x64 musl, Linux arm64 glibc, macOS x64 and arm64, and Windows x64 MSVC. Browser JavaScript, Web Workers, Deno Deploy, and other environments that cannot load native addons are unsupported.

## Semantics

Construction only validates and copies configuration. The first `connect()` call is the sole native startup boundary; concurrent calls share that transition. Publish, subscribe, unsubscribe, diagnostics, event reads, and acknowledgements reject with `NOT_CONNECTED` until it resolves, and none of them starts a native driver. MQTT 5-only command options on an MQTT 3.1.1 client reject with `COMMAND_INVALID` before native startup. Closing an unstarted client is a successful no-op.

`enqueuePublish()` resolves when bounded request-channel admission succeeds. `publish()` resolves at the MQTT milestone: local transport flush for QoS 0, PUBACK for QoS 1, and PUBCOMP for QoS 2. No result claims broker application delivery. Dropping a promise does not recall admitted network work.

`Uint8Array` and Node/Bun `Buffer` inputs are copied before an operation is admitted, including sliced views with nonzero offsets. Incoming payloads and MQTT 5 correlation data are `Buffer` under Node.js and Bun and plain `Uint8Array` under Deno; the TypeScript API remains runtime-neutral.

`events()` is a single-consumer bounded async iterator. It must be drained while a client runs. A second active iterator is rejected. In manual acknowledgement mode, eligible QoS 1/2 incoming messages carry a single-use `ack(): Promise<void>` method; QoS 0 messages do not. A disconnect invalidates acknowledgements from the previous connection.

Recoverable connection failures are emitted as `disconnected` while the native driver reconnects. The initial `connect()` remains pending until a CONNACK or terminal shutdown. `close()` performs a bounded graceful barrier; `closeNow()` requests immediate shutdown. Both are idempotent.

Errors are `MqttError` instances with stable `code`, `kind`, delivery classification, optional broker reason and operation ID, and retryability. The human-readable message is diagnostic rather than a compatibility contract.

For Deno, install the npm package into a project and run, for example:

```sh
deno run --node-modules-dir=auto --allow-ffi --allow-net=broker.example:8883 \
  --allow-read app.ts
```

where `app.ts` imports `npm:@rumqtt-next/rumqttc`. Grant only the broker,
native-addon path, and environment variables used by the application.

## Native-addon security

The platform package contains executable native code with the permissions of the hosting process. Use package-lock integrity checks and the published checksums/provenance, and apply the same supply-chain review used for other native dependencies.

## Session Present compatibility

The MQTT 3.1.1 default rejects a successful clean-session CONNACK with invalid
Session Present=true. The opt-in `AcceptAsClean` policy resolves only this case
as fresh, resets old local protocol state and replay work, and clears the
current scope/client-ID checkpoint before reporting success. Failed or cancelled
clearing remains pending for retry before checkpoint loading. The broker remains
non-conforming and raw connected-event/connection-result flags remain true.

Existing diagnostics expose an optional `connack` observation containing raw
Session Present, effective session resume, and a compatibility reason. Use the
effective session for resubscription decisions. No MQTT 5 clean-start recovery
is introduced; the existing broker-only resume policy retains its restrictions.

On `protocol: '3.1.1'`, use `sessionPresentMismatchPolicy: 'acceptAsClean'`
(or `'error'`, the default). Supplying this option on MQTT 5 is rejected, even
when its value is `'error'`. `await client.diagnostics()` returns `connack`
with `rawSessionPresent`, `sessionResumed`, and `diagnostic`, or `null` while
no connection observation is current. A v4 recovery reason is
`'sessionPresentMismatchAcceptedAsClean'`.
