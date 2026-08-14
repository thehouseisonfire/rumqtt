# `@rumqtt/rumqttc`

`@rumqtt/rumqttc` is one native MQTT client for MQTT 3.1.1 and MQTT 5. Each `MqttClient` selects a protocol permanently at construction. It uses stable Node-API and the same prebuilt addon under Node.js, local Deno, and Bun.

```ts
import { MqttClient } from '@rumqtt/rumqttc'

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

`enqueuePublish()` resolves when bounded request-channel admission succeeds. `publish()` resolves at the MQTT milestone: local transport flush for QoS 0, PUBACK for QoS 1, and PUBCOMP for QoS 2. No result claims broker application delivery. Dropping a promise does not recall admitted network work.

`events()` is a single-consumer bounded async iterator. It must be drained while a client runs. A second active iterator is rejected. In manual acknowledgement mode, eligible QoS 1/2 incoming messages carry a single-use `ack()` method; QoS 0 messages do not.

Recoverable connection failures are emitted as `disconnected` while the native driver reconnects. The initial `connect()` remains pending until a CONNACK or terminal shutdown. `close()` performs a bounded graceful barrier; `closeNow()` requests immediate shutdown. Both are idempotent.

Errors are `MqttError` instances with stable `code`, `kind`, delivery classification, optional broker reason and operation ID, and retryability. The human-readable message is diagnostic rather than a compatibility contract.

## Native-addon security

The platform package contains executable native code with the permissions of the hosting process. Use package-lock integrity checks and the published checksums/provenance, and apply the same supply-chain review used for other native dependencies.
