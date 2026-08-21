import assert from 'node:assert/strict'
import { readdirSync } from 'node:fs'
import { MqttClient } from '../../index.js'

const linuxThreads = () => process.platform === 'linux' ? readdirSync('/proc/self/task').length : undefined
const baselineThreads = linuxThreads()
const baselineRss = process.memoryUsage.rss()

for (let index = 0; index < 50; index += 1) {
  const client = new MqttClient({
    protocol: index % 2 ? '3.1.1' : '5.0',
    brokerHost: '127.0.0.1',
    brokerPort: 1,
    clientId: `repetition-${index}`,
    connectionTimeoutSeconds: 1,
  })
  const connecting = client.connect()
  const rejection = assert.rejects(connecting)
  await client.closeNow()
  await rejection
}

await new Promise(resolve => setTimeout(resolve, 50))
if (baselineThreads !== undefined) assert.ok(linuxThreads() <= baselineThreads + 2)
assert.ok(process.memoryUsage.rss() - baselineRss < 32 * 1024 * 1024)
