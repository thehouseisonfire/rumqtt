import assert from 'node:assert/strict'
import { MqttClient } from '@rumqtt-next/rumqttc'

const client = new MqttClient({
  protocol: '5.0',
  brokerHost: process.env.RUMQTTC_TEST_HOST,
  brokerPort: Number(process.env.RUMQTTC_TEST_PORT),
  clientId: 'installed-esm',
})
await client.connect()
const completion = await client.publish('rumqttc/native/installed', new Uint8Array([0, 1, 0]), { qos: 1 })
assert.equal(completion.milestone, 'qos1Acknowledged')
await client.close()
