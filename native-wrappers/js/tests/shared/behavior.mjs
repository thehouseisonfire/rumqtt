import assert from 'node:assert/strict'
import { MqttClient, MqttError } from '../../index.js'

const [host, portText] = process.argv.slice(-2)
const port = Number(portText)
if (!host || !Number.isInteger(port)) throw new Error('broker host and port arguments are required')
const tlsPort = Number(process.env.RUMQTTC_TEST_TLS_PORT)
const ca = new TextEncoder().encode(process.env.RUMQTTC_TEST_CA_PEM)
const wrongCa = new TextEncoder().encode(process.env.RUMQTTC_TEST_WRONG_CA_PEM)

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

  const backing = typeof Buffer !== 'undefined' ? Buffer.from([99, 0, 7, 0, 8, 99]) :
    new Uint8Array([99, 0, 7, 0, 8, 99])
  const slice = backing.subarray(1, 5)
  const slicedPublish = client.publish('rumqttc/native/sliced', slice, { qos: 1 })
  slice.fill(42)
  await slicedPublish

  const subscribed = await client.subscribe([{ filter: 'rumqttc/native/incoming', qos: 1 }])
  assert.equal(subscribed.results[0].granted, true)
  await client.publish('rumqttc/native/incoming', new Uint8Array([0, 4, 0]), { qos: 1 })
  let publish
  while (!publish) {
    const event = (await iterator.next()).value
    if (event.type === 'publish') {
      const acknowledgement = event.message.ack()
      const duplicate = event.message.ack()
      const [accepted, rejected] = await Promise.allSettled([acknowledgement, duplicate])
      assert.equal(accepted.status, 'fulfilled')
      assert.equal(accepted.value, undefined)
      assert.equal(rejected.status, 'rejected')
      if (event.message.payload.length === 3) publish = event.message
    }
  }
  assert.deepEqual([...publish.payload], [0, 4, 0])
  if (typeof Deno === 'undefined') assert.equal(Buffer.isBuffer(publish.payload), true)
  else assert.equal(publish.payload.constructor, Uint8Array)
  assert.equal(typeof publish.ack, 'function')
  if (protocol === '3.1.1') {
    assert.equal('properties' in publish, false)
  } else {
    assert.equal(typeof publish.properties, 'object')
    assert.equal('contentType' in publish.properties, false)
    assert.deepEqual([...publish.properties.correlationData], [0, 5, 0])
    if (typeof Deno === 'undefined') assert.equal(Buffer.isBuffer(publish.properties.correlationData), true)
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

  void client.publish('rumqttc/native/abandoned', new Uint8Array([1]), { qos: 1 })
  await client.publish('rumqttc/native/abandoned-barrier', new Uint8Array([2]), { qos: 1 })

  await client.publish('rumqttc/native/interrupt', new Uint8Array([1]), { qos: 0 })
  let disconnected = false
  let reconnected = false
  while (!reconnected) {
    const event = (await iterator.next()).value
    if (event.type === 'disconnected') disconnected = true
    if (disconnected && event.type === 'connected') reconnected = true
  }
  await client.enqueuePublish('rumqttc/native/shutdown-delivery', new Uint8Array([3]), { qos: 1 })
  const firstClose = client.close({ timeoutMs: 5000 })
  const secondClose = client.close({ timeoutMs: 5000 })
  assert.equal(firstClose, secondClose)
  await Promise.all([firstClose, secondClose])
}

for (const protocol of ['3.1.1', '5.0']) {
  const client = new MqttClient({
    protocol, brokerHost: host, brokerPort: port, clientId: `js-stale-${protocol}`, ackMode: 'manual',
  })
  const events = client.events()
  await client.connect()
  await events.next()
  await client.subscribe([{ filter: 'rumqttc/native/incoming', qos: 1 }])
  let staleAck
  while (!staleAck) {
    const event = (await events.next()).value
    if (event.type === 'publish') staleAck = event.message.ack
  }
  await client.publish('rumqttc/native/interrupt', new Uint8Array(), { qos: 0 })
  let disconnected = false
  while (true) {
    const event = (await events.next()).value
    if (event.type === 'disconnected') disconnected = true
    if (disconnected && event.type === 'connected') break
  }
  await assert.rejects(staleAck(), MqttError)
  await client.closeNow()
}

for (const protocol of ['3.1.1', '5.0']) {
  const client = new MqttClient({
    protocol, brokerHost: host, brokerPort: port, clientId: `js-auto-${protocol}`, ackMode: 'automatic',
  })
  const events = client.events()
  await client.connect()
  await events.next()
  await client.subscribe([
    { filter: 'rumqttc/native/automatic/qos1', qos: 1 },
    { filter: 'rumqttc/native/automatic/qos2', qos: 2 },
  ])
  const received = new Set()
  while (received.size < 2) {
    const event = (await events.next()).value
    if (event.type === 'publish') {
      assert.equal('ack' in event.message, false)
      received.add(event.message.topic)
    }
  }
  await client.close()
}

const pressure = new MqttClient({
  protocol: '3.1.1', brokerHost: host, brokerPort: port, clientId: 'js-pressure', requestCapacity: 1,
})
const pressureEvents = pressure.events()
await pressure.connect()
await pressureEvents.next()
let admitted = 0
const pressureRequests = Array.from({ length: 128 }, () =>
  pressure.enqueuePublish('rumqttc/native/pressure', new Uint8Array([1]), { qos: 1 }).then(() => { admitted += 1 }))
await new Promise(resolve => setTimeout(resolve, 50))
assert.ok(admitted < pressureRequests.length)
await Promise.race([
  Promise.all(pressureRequests),
  new Promise((_, reject) => setTimeout(() => reject(new Error('backpressure did not recover')), 5000)),
])
await pressure.close()

const gracefulDrain = new MqttClient({
  protocol: '5.0', brokerHost: host, brokerPort: port, clientId: 'js-graceful-drain',
  requestCapacity: 32, eventCapacity: 1, eventDeliveryTimeoutMs: 1000, emitOutgoingEvents: true,
})
const gracefulDrainEvents = gracefulDrain.events()
await gracefulDrain.connect()
await gracefulDrainEvents.next()
await Promise.all(Array.from({ length: 16 }, (_, index) =>
  gracefulDrain.enqueuePublish('rumqttc/native/graceful-drain', new Uint8Array([index]), { qos: 0 })))
const gracefulClose = gracefulDrain.close({ timeoutMs: 5000 })
let gracefulClosed = false
while (!gracefulClosed) {
  const { value, done } = await gracefulDrainEvents.next()
  if (done) break
  if (value.type === 'closed') {
    assert.equal(value.graceful, true)
    gracefulClosed = true
  }
}
assert.equal(gracefulClosed, true)
await gracefulClose

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

const tls = new MqttClient({
  protocol: '5.0', brokerHost: 'localhost', brokerPort: tlsPort, clientId: 'js-tls-valid',
  transport: { kind: 'tls', ca }, connectionTimeoutSeconds: 1,
})
await tls.connect()
await tls.close()

for (const [clientId, brokerHost, trust] of [
  ['js-tls-wrong-ca', 'localhost', wrongCa],
  ['js-tls-wrong-host', '127.0.0.1', ca],
]) {
  const rejected = new MqttClient({
    protocol: '5.0', brokerHost, brokerPort: tlsPort, clientId,
    transport: { kind: 'tls', ca: trust }, connectionTimeoutSeconds: 1,
  })
  const connecting = rejected.connect()
  const rejectedConnection = assert.rejects(connecting, MqttError)
  const initialResult = await Promise.race([
    connecting.then(() => 'connected', () => 'rejected'),
    new Promise(resolve => setTimeout(() => resolve('pending-after-rejection'), 300)),
  ])
  assert.equal(initialResult, 'pending-after-rejection')
  await rejected.closeNow()
  await rejectedConnection
}

for (let index = 0; index < 10; index += 1) {
  const repeated = new MqttClient({
    protocol: index % 2 ? '3.1.1' : '5.0',
    brokerHost: host, brokerPort: port, clientId: `js-repeated-${index}`,
  })
  await repeated.connect()
  if (index % 2) await repeated.close()
  else await repeated.closeNow()
}
