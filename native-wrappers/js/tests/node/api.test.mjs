import assert from 'node:assert/strict'
import test from 'node:test'
import { MqttClient } from '../../index.js'

test('rejects protocol-incompatible and invalid bounded options before startup', () => {
  assert.throws(() => new MqttClient({
    protocol: '3.1.1', brokerHost: 'localhost', brokerPort: 1883, clientId: 'x', cleanStart: true,
  }), /MQTT 5 session options/)
  assert.throws(() => new MqttClient({
    protocol: '5.0', brokerHost: 'localhost', brokerPort: 1883, clientId: 'x', requestCapacity: 0,
  }), /requestCapacity/)
})

test('events has one active consumer', async () => {
  const client = new MqttClient({ protocol: '5.0', brokerHost: 'localhost', brokerPort: 1883, clientId: 'x' })
  const events = client.events()
  assert.throws(() => client.events(), /another event iterator/)
  await events.return()
})

test('return keeps the event consumer reserved until an outstanding read settles', async () => {
  const client = new MqttClient({
    protocol: '5.0', brokerHost: '127.0.0.1', brokerPort: 1, clientId: 'pending-events',
    connectionTimeoutMs: 100,
  })
  const events = client.events()
  const pending = events.next()
  await events.return()
  assert.throws(() => client.events(), /another event iterator/)

  await client.closeNow()
  await pending
  const replacement = client.events()
  await replacement.return()
})

test('native startup failures reject with structured MqttError', async () => {
  const client = new MqttClient({
    protocol: '5.0', brokerHost: 'localhost', brokerPort: 8883, clientId: 'bad-tls',
    transport: { kind: 'tls', ca: new TextEncoder().encode('not a certificate') },
  })
  await assert.rejects(client.connect(), error => error.name === 'MqttError' && error.code === 'CONFIGURATION_INVALID')
})
