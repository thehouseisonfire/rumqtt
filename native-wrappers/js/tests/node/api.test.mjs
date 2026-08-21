import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import test from 'node:test'
import { MqttClient } from '../../index.js'

const require = createRequire(import.meta.url)
const addon = require('../../loader.cjs')

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

test('pre-connect operations reject without constructing native state', async () => {
  const before = addon.testActiveNativeClients?.()
  const client = new MqttClient({
    protocol: '5.0', brokerHost: '127.0.0.1', brokerPort: 1, clientId: 'inert-client',
  })
  const events = client.events()
  for (const operation of [
    client.publish('topic', new Uint8Array()),
    client.subscribe([{ filter: 'topic' }]),
    client.unsubscribe(['topic']),
    client.diagnostics(),
    events.next(),
  ]) {
    await assert.rejects(operation, error => error.code === 'NOT_CONNECTED')
  }
  if (before !== undefined) assert.equal(addon.testActiveNativeClients(), before)
  const replacement = client.events()
  await replacement.return()
  await client.closeNow()
  if (before !== undefined) assert.equal(addon.testActiveNativeClients(), before)
})

test('MQTT 5 command scopes are rejected before MQTT 3.1.1 startup', async () => {
  const before = addon.testActiveNativeClients?.()
  const client = new MqttClient({
    protocol: '3.1.1', brokerHost: '127.0.0.1', brokerPort: 1, clientId: 'incompatible-client',
  })
  for (const operation of [
    client.publish('topic', new Uint8Array(), { properties: { topicAlias: 1 } }),
    client.subscribe([{ filter: 'topic', options: { noLocal: true } }]),
    client.subscribe([{ filter: 'topic' }], { subscriptionIdentifier: 1 }),
    client.unsubscribe(['topic'], { userProperties: [['k', 'v']] }),
  ]) {
    await assert.rejects(operation, error => error.code === 'COMMAND_INVALID')
  }
  if (before !== undefined) assert.equal(addon.testActiveNativeClients(), before)
})

test('native startup failures reject with structured MqttError', async () => {
  const client = new MqttClient({
    protocol: '5.0', brokerHost: 'localhost', brokerPort: 8883, clientId: 'bad-tls',
    transport: { kind: 'tls', ca: new TextEncoder().encode('not a certificate') },
  })
  await assert.rejects(client.connect(), error => error.name === 'MqttError' && error.code === 'CONFIGURATION_INVALID')
})
