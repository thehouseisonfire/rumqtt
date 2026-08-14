import { MqttClient } from '../../index.js'

const client = new MqttClient({
  protocol: '5.0', brokerHost: '192.0.2.1', brokerPort: 1883, clientId: 'live-exit',
  connectionTimeoutSeconds: 30,
})
void client.connect()
setTimeout(() => process.exit(0), 25)
