import {
  MqttClient,
  MqttError,
  type AckMode,
  type AdmissionResult,
  type ClientDiagnostics,
  type CloseOptions,
  type CommonMqttClientOptions,
  type ConnectResult,
  type IncomingMessage,
  type MqttDeliveryStatus,
  type MqttErrorKind,
  type MqttClientOptions,
  type MqttEvent,
  type OutgoingSummary,
  type ProtocolOptions,
  type ProtocolVersion,
  type PublishCompletion,
  type PublishMilestone,
  type PublishOptions,
  type QoS,
  type SubscribeOptions,
  type SubscribeResult,
  type SubscribeCompletion,
  type Subscription,
  type TlsOptions,
  type TransportOptions,
  type UnsubscribeOptions,
  type UnsubscribeResult,
  type UnsubscribeCompletion,
  type UserProperty,
  type V5IncomingPublishProperties,
  type V5PublishProperties,
  type V5SubscriptionOptions,
} from '../../index.js'

const options: MqttClientOptions = {
  protocol: '5.0', brokerHost: 'localhost', brokerPort: 1883, clientId: 'typed', cleanStart: true,
}
const client = new MqttClient(options)
await client.publish('topic', new Uint8Array([0]), { qos: 1, properties: { topicAlias: 1 } })
await client.subscribe([{ filter: 'topic', options: { noLocal: true } }], { subscriptionIdentifier: 1 })
await client.unsubscribe(['topic'], { userProperties: [['key', 'value']] })

const transports: TransportOptions[] = [
  { kind: 'tcp' },
  { kind: 'tls', ca: new Uint8Array(), clientCertificate: new Uint8Array(), privateKey: new Uint8Array() },
  { kind: 'websocket', url: 'ws://localhost/mqtt' },
  { kind: 'wss', url: 'wss://localhost/mqtt', ca: new Uint8Array() },
]
void transports
const binaryProperties: V5PublishProperties = {
  responseTopic: 'response', correlationData: new Uint8Array([0]), contentType: 'binary',
  payloadFormatIndicator: 0, topicAlias: 1, messageExpiryInterval: 1, userProperties: [['k', 'v']],
}
await client.publish('topic', new Uint8Array(), { properties: binaryProperties })

const v4 = new MqttClient({ protocol: '3.1.1', brokerHost: 'localhost', brokerPort: 1883, clientId: 'v4' })
await v4.publish('topic', 'payload', { qos: 2 })
await v4.subscribe([{ filter: 'topic', qos: 1 }])

// @ts-expect-error MQTT 5 publish properties are rejected for MQTT 3.1.1.
void v4.publish('topic', new Uint8Array(), { properties: { topicAlias: 1 } })
// @ts-expect-error MQTT 5 per-filter options are rejected for MQTT 3.1.1.
void v4.subscribe([{ filter: 'topic', options: { noLocal: true } }])
// @ts-expect-error MQTT 5 SUBSCRIBE properties are rejected for MQTT 3.1.1.
void v4.subscribe([{ filter: 'topic' }], { subscriptionIdentifier: 1 })
// @ts-expect-error MQTT 5 UNSUBSCRIBE properties are rejected for MQTT 3.1.1.
void v4.unsubscribe(['topic'], { userProperties: [['k', 'v']] })

declare const message: IncomingMessage
if (message.ack) {
  const acknowledged: void = await message.ack()
  void acknowledged
}
declare const event: MqttEvent
if (event.type === 'publish') event.message.payload satisfies Uint8Array
declare const publish: PublishCompletion
declare const subscribe: SubscribeCompletion
declare const unsubscribe: UnsubscribeCompletion
publish.type satisfies 'publish'
subscribe.type satisfies 'subscribe'
unsubscribe.type satisfies 'unsubscribe'

type ExportedTypes = [
  ProtocolVersion, QoS, AckMode, MqttErrorKind, MqttDeliveryStatus, TlsOptions,
  TransportOptions, CommonMqttClientOptions, ProtocolOptions, MqttClientOptions,
  UserProperty, V5PublishProperties, PublishOptions, V5SubscriptionOptions,
  Subscription, SubscribeOptions, UnsubscribeOptions, ConnectResult, AdmissionResult,
  PublishMilestone, PublishCompletion, SubscribeResult, SubscribeCompletion,
  UnsubscribeResult, UnsubscribeCompletion, CloseOptions, V5IncomingPublishProperties,
  IncomingMessage, OutgoingSummary, MqttEvent, ClientDiagnostics,
]
declare const exportedTypes: ExportedTypes
void exportedTypes
declare const error: MqttError
error.code satisfies string

// @ts-expect-error MQTT 5 session settings are rejected for MQTT 3.1.1.
const invalid: MqttClientOptions = { protocol: '3.1.1', brokerHost: 'x', brokerPort: 1, clientId: 'x', cleanStart: true }
void invalid
