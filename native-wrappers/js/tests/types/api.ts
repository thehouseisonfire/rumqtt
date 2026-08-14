import { MqttClient, type MqttClientOptions } from '../../index.js'

const options: MqttClientOptions = {
  protocol: '5.0', brokerHost: 'localhost', brokerPort: 1883, clientId: 'typed', cleanStart: true,
}
const client = new MqttClient(options)
await client.publish('topic', new Uint8Array([0]), { qos: 1, properties: { topicAlias: 1 } })
await client.subscribe([{ filter: 'topic', options: { noLocal: true } }], { subscriptionIdentifier: 1 })

// @ts-expect-error MQTT 5 session settings are rejected for MQTT 3.1.1.
const invalid: MqttClientOptions = { protocol: '3.1.1', brokerHost: 'x', brokerPort: 1, clientId: 'x', cleanStart: true }
void invalid
