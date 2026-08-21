'use strict'

const assert = require('node:assert/strict')
const { MqttClient } = require('@rumqtt-next/rumqttc')

async function main() {
  const client = new MqttClient({
    protocol: '3.1.1',
    brokerHost: process.env.RUMQTTC_TEST_HOST,
    brokerPort: Number(process.env.RUMQTTC_TEST_PORT),
    clientId: 'installed-commonjs',
  })
  await client.connect()
  const completion = await client.publish('rumqttc/native/installed', Buffer.from([0, 1, 0]), { qos: 1 })
  assert.equal(completion.milestone, 'qos1Acknowledged')
  await client.close()
}

main().catch(error => {
  console.error(error)
  process.exitCode = 1
})
