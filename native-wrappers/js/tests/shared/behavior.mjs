import assert from 'node:assert/strict'
import { MqttClient, MqttError } from '../../index.js'

const [host, portText] = process.argv.slice(-2)
const port = Number(portText)
if (!host || !Number.isInteger(port)) throw new Error('broker host and port arguments are required')

for (const protocol of ['3.1.1', '5.0']) {
  const client = new MqttClient({
    protocol,
    brokerHost: host,
    brokerPort: port,
    clientId: `js-${protocol}`,
    ackMode: 'manual',
    emitOutgoingEvents: true,
  })
  const iterator = client.events()
  const connection = await client.connect()
  assert.equal(connection.protocol, protocol)
  const connected = await iterator.next()
  assert.equal(connected.value.type, 'connected')
  if (protocol === '3.1.1') {
    await assert.rejects(
      client.enqueuePublish('rumqttc/native/incompatible', new Uint8Array([1]), { properties: { topicAlias: 1 } }),
      MqttError,
    )
  }

  for (const qos of [0, 1, 2]) {
    const completion = await client.publish('rumqttc/native/binary', new Uint8Array([0, 1, 0, 2, 255, 0]), { qos })
    assert.equal(completion.milestone, ['qos0Flushed', 'qos1Acknowledged', 'qos2Completed'][qos])
    assert.equal(typeof completion.operationId, 'bigint')
  }

  const subscribed = await client.subscribe([{ filter: 'rumqttc/native/incoming', qos: 1 }])
  assert.equal(subscribed.results[0].granted, true)
  await client.publish('rumqttc/native/incoming', new Uint8Array([0, 4, 0]), { qos: 1 })
  let publish
  while (!publish) {
    const event = (await iterator.next()).value
    if (event.type === 'publish') {
      await event.message.ack()
      if (event.message.payload.length === 3) publish = event.message
    }
  }
  assert.deepEqual([...publish.payload], [0, 4, 0])
  assert.equal(typeof publish.ack, 'function')
  if (protocol === '3.1.1') {
    assert.equal('properties' in publish, false)
  } else {
    assert.equal(typeof publish.properties, 'object')
    assert.equal('contentType' in publish.properties, false)
  }
  await assert.rejects(publish.ack(), MqttError)

  const diagnostics = await client.diagnostics()
  assert.equal(typeof diagnostics.connected, 'boolean')
  const unsubscribed = await client.unsubscribe(['rumqttc/native/incoming'])
  assert.equal(unsubscribed.type, 'unsubscribe')
  if (protocol === '3.1.1') {
    assert.equal('results' in unsubscribed, false)
  } else {
    assert.equal(Array.isArray(unsubscribed.results), true)
  }

  await client.publish('rumqttc/native/interrupt', new Uint8Array([1]), { qos: 0 })
  let disconnected = false
  let reconnected = false
  while (!reconnected) {
    const event = (await iterator.next()).value
    if (event.type === 'disconnected') disconnected = true
    if (disconnected && event.type === 'connected') reconnected = true
  }
  const firstClose = client.close({ timeoutMs: 5000 })
  const secondClose = client.close({ timeoutMs: 5000 })
  assert.notEqual(firstClose, secondClose)
  await Promise.all([firstClose, secondClose])
}

const overflow = new MqttClient({
  protocol: '3.1.1', brokerHost: host, brokerPort: port, clientId: 'js-overflow',
  eventCapacity: 1, eventDeliveryTimeoutMs: 50,
})
const overflowEvents = overflow.events()
await overflow.connect()
await overflowEvents.next()
await overflow.subscribe([{ filter: 'rumqttc/native/overflow' }])
await new Promise(resolve => setTimeout(resolve, 150))
let terminal
while (!terminal) {
  const { value, done } = await overflowEvents.next()
  if (done) break
  if (value.type === 'driverError') terminal = value
}
assert.equal(terminal.error.code, 'EVENT_BUFFER_OVERFLOW')
await overflow.closeNow()
