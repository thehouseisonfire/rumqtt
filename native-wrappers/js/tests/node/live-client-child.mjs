import { MqttClient } from '../../index.js'

const client = new MqttClient({
  protocol: '5.0', brokerHost: '192.0.2.1', brokerPort: 1883, clientId: 'live-exit',
  connectionTimeoutSeconds: 30,
})
void client.connect().catch(() => {})
setTimeout(async () => {
  // Bun 1.3 implements Node-API cleanup hooks, but aborts when a synchronous hook joins a
  // native thread during process.exit(). Exercise Bun's supported explicit-close path while
  // retaining the live-client environment-cleanup check under Node.js and Deno.
  if (globalThis.Bun) await client.closeNow()
  process.exit(0)
}, 25)
