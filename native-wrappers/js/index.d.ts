export type ProtocolVersion = '3.1.1' | '5.0'
export type QoS = 0 | 1 | 2
export type AckMode = 'automatic' | 'manual'
export type MqttErrorKind = 'configuration' | 'admission' | 'backpressure' | 'network' | 'tls' | 'protocol' | 'authentication' | 'persistence' | 'timeout' | 'shutdown' | 'internal'
export type MqttDeliveryStatus = 'notApplicable' | 'notAdmitted' | 'rejected' | 'ambiguous'

export interface TlsOptions {
  ca?: Uint8Array
  clientCertificate?: Uint8Array
  privateKey?: Uint8Array
}
export type TransportOptions =
  | { kind: 'tcp' }
  | ({ kind: 'tls' } & TlsOptions)
  | { kind: 'websocket'; url: string }
  | ({ kind: 'wss'; url: string } & TlsOptions)

export interface CommonMqttClientOptions {
  brokerHost: string
  brokerPort: number
  clientId: string
  transport?: TransportOptions
  keepAliveSeconds?: number
  connectionTimeoutSeconds?: number
  username?: string
  password?: Uint8Array
  requestCapacity?: number
  eventCapacity?: number
  eventDeliveryTimeoutMs?: number
  ackMode?: AckMode
  incomingPacketSizeLimit?: number
  emitOutgoingEvents?: boolean
}
export type ProtocolOptions =
  | { protocol: '3.1.1'; cleanSession?: boolean; cleanStart?: never; sessionExpiryInterval?: never }
  | { protocol: '5.0'; cleanStart?: boolean; sessionExpiryInterval?: number; cleanSession?: never }
export type MqttClientOptions = CommonMqttClientOptions & ProtocolOptions

export interface UserProperty { 0: string; 1: string }
export interface V5PublishProperties {
  responseTopic?: string
  correlationData?: Uint8Array
  contentType?: string
  payloadFormatIndicator?: number
  topicAlias?: number
  messageExpiryInterval?: number
  userProperties?: Array<[string, string]>
}
export interface PublishOptions { qos?: QoS; retain?: boolean; properties?: V5PublishProperties }
export interface V5SubscriptionOptions { noLocal?: boolean; retainAsPublished?: boolean; retainForwardRule?: 0 | 1 | 2 }
export interface Subscription { filter: string; qos?: QoS; options?: V5SubscriptionOptions }
export interface SubscribeOptions { subscriptionIdentifier?: number; userProperties?: Array<[string, string]> }
export interface UnsubscribeOptions { userProperties?: Array<[string, string]> }

export interface ConnectResult { protocol: ProtocolVersion; sessionPresent: boolean }
export interface AdmissionResult { operationId: bigint }
export type PublishMilestone = 'qos0Flushed' | 'qos1Acknowledged' | 'qos2Completed'
export interface PublishCompletion extends AdmissionResult { type: 'publish'; milestone: PublishMilestone }
export type SubscribeResult = { granted: true; qos: QoS } | { granted: false; brokerReason: number }
export interface SubscribeCompletion extends AdmissionResult { type: 'subscribe'; results: SubscribeResult[] }
export type UnsubscribeResult = { status: 'success' | 'noSubscriptionExisted' } | { status: 'rejected'; brokerReason: number }
export interface UnsubscribeCompletion extends AdmissionResult { type: 'unsubscribe'; results?: UnsubscribeResult[] }
export interface CloseOptions { timeoutMs?: number }

export interface V5IncomingPublishProperties {
  responseTopic?: string
  correlationData?: Uint8Array
  contentType?: string
  payloadFormatIndicator?: number
  topicAlias?: number
  subscriptionIdentifiers: number[]
  messageExpiryInterval?: number
  userProperties: Array<[string, string]>
}
export interface IncomingMessage {
  topic: string
  payload: Uint8Array
  qos: QoS
  retain: boolean
  duplicate: boolean
  properties?: V5IncomingPublishProperties
  ack?: () => Promise<AdmissionResult>
}
export type OutgoingSummary = 'publish' | 'subscribe' | 'unsubscribe' | 'acknowledgement' | 'ping' | 'disconnect' | 'awaitAcknowledgement' | 'other'
export type MqttEvent =
  | { type: 'connected'; protocol: ProtocolVersion; sessionPresent: boolean }
  | { type: 'disconnected'; phase: 'attempt' | 'established'; error: MqttError; reconnecting: true }
  | { type: 'publish'; message: IncomingMessage }
  | { type: 'outgoing'; packet: OutgoingSummary }
  | { type: 'closed'; graceful: boolean }
  | { type: 'driverError'; error: MqttError }

export interface ClientDiagnostics {
  connected: boolean
  disconnecting: boolean
  pendingRequests: number
  queuedRequests: number
  inflightPublishes: number
  maxInflightPublishes: number
  pendingSubscribes: number
  pendingUnsubscribes: number
  outboundDrained: boolean
}

export class MqttError extends Error {
  readonly code: string
  readonly kind: MqttErrorKind
  readonly operationId?: bigint
  readonly brokerReason?: number
  readonly retryable: boolean
  readonly delivery: MqttDeliveryStatus
  readonly ambiguous: boolean
}

export class MqttClient {
  constructor(options: MqttClientOptions)
  connect(): Promise<ConnectResult>
  enqueuePublish(topic: string, payload: Uint8Array | string, options?: PublishOptions): Promise<AdmissionResult>
  publish(topic: string, payload: Uint8Array | string, options?: PublishOptions): Promise<PublishCompletion>
  subscribe(filters: Subscription[], options?: SubscribeOptions): Promise<SubscribeCompletion>
  unsubscribe(filters: string[], options?: UnsubscribeOptions): Promise<UnsubscribeCompletion>
  events(): AsyncIterableIterator<MqttEvent>
  diagnostics(): Promise<ClientDiagnostics>
  close(options?: CloseOptions): Promise<void>
  closeNow(): Promise<void>
}
