'use strict'

const assert = require('node:assert/strict')
const { NativeMqttClient } = require('../../loader.cjs')

const config = JSON.stringify({
  protocol: '5.0',
  brokerHost: '127.0.0.1',
  brokerPort: 1,
  clientId: `panic-${process.argv[2]}`,
  transport: { kind: 'tcp' },
  keepAliveSeconds: 60,
  connectionTimeoutSeconds: 1,
  requestCapacity: 10,
  eventCapacity: 16,
  eventDeliveryTimeoutMs: 1000,
  ackMode: 'automatic',
  incomingPacketSizeLimit: 10240,
  emitOutgoingEvents: false,
  cleanStart: true,
})

async function main() {
  const client = new NativeMqttClient(config)
  const response = process.argv[2] === 'sync' ? client.injectSyncPanic() : await client.injectAsyncPanic()
  const failure = JSON.parse(response)
  assert.equal(failure.error.code, 'INTERNAL_PANIC')

  const deadline = Date.now() + 5000
  while (Date.now() < deadline) {
    const event = JSON.parse(await client.nextEvent())
    if (event.event?.type === 'driverError' && event.event.error.code === 'INTERNAL_PANIC') return
    if (event.done) break
  }
  throw new Error('panic did not terminate the native event stream')
}

main().catch(error => {
  console.error(error)
  process.exitCode = 1
})
