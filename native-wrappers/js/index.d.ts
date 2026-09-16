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
export type ProtocolOptions<P extends ProtocolVersion = ProtocolVersion> = P extends '3.1.1'
  ? { protocol: P; cleanSession?: boolean; cleanStart?: never; sessionExpiryInterval?: never }
  : { protocol: P; cleanStart?: boolean; sessionExpiryInterval?: number; cleanSession?: never }
export type MqttClientOptions<P extends ProtocolVersion = ProtocolVersion> = CommonMqttClientOptions & ProtocolOptions<P>

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
export type PublishOptions<P extends ProtocolVersion = ProtocolVersion> = {
  qos?: QoS
  retain?: boolean
} & (P extends '3.1.1' ? { properties?: never } : { properties?: V5PublishProperties })
export interface V5SubscriptionOptions { noLocal?: boolean; retainAsPublished?: boolean; retainForwardRule?: 0 | 1 | 2 }
export type Subscription<P extends ProtocolVersion = ProtocolVersion> = {
  filter: string
  qos?: QoS
} & (P extends '3.1.1' ? { options?: never } : { options?: V5SubscriptionOptions })
export type SubscribeOptions<P extends ProtocolVersion = ProtocolVersion> = P extends '3.1.1' ? never : {
  subscriptionIdentifier?: number
  userProperties?: Array<[string, string]>
}
export type UnsubscribeOptions<P extends ProtocolVersion = ProtocolVersion> = P extends '3.1.1' ? never : {
  userProperties?: Array<[string, string]>
}

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
  ack?: () => Promise<void>
}
export type OutgoingSummary = 'publish' | 'subscribe' | 'unsubscribe' | 'acknowledgement' | 'ping' | 'disconnect' | 'awaitAcknowledgement' | 'other'
export interface V5ConnAckProperties {
  sessionExpiryInterval: number | null
  receiveMaximum: number | null
  maximumQos: number | null
  retainAvailable: number | null
  maximumPacketSize: number | null
  assignedClientIdentifier: string | null
  topicAliasMaximum: number | null
  reasonString: string | null
  wildcardSubscriptionAvailable: number | null
  subscriptionIdentifiersAvailable: number | null
  sharedSubscriptionAvailable: number | null
  serverKeepAlive: number | null
  responseInformation: string | null
  serverReference: string | null
  authenticationMethod: string | null
  authenticationData?: Uint8Array
  userProperties: Array<[string, string]>
}
export interface ConnAckDetails {
  reasonCode: number
  properties: V5ConnAckProperties | null
}
export type RedirectTarget =
  | { type: 'tcp'; host: string; port: number }
  | { type: 'websocket'; url: string }
  | { type: 'unix'; path: string }
export type MqttEvent =
  | { type: 'connected'; protocol: ProtocolVersion; sessionPresent: boolean; details: ConnAckDetails }
  | { type: 'connectionRejected'; details: ConnAckDetails }
  | { type: 'brokerDisconnect'; reasonCode: number; sessionExpiryInterval: number | null; reasonString: string | null; userProperties: Array<[string, string]>; serverReference: string | null }
  | { type: 'authentication'; method: string; exchange: 'initial' | 'reauthentication'; stage: 'started' | 'continue' | 'succeeded' | 'failed'; failure: string | null }
  | { type: 'redirect'; source: 'connack' | 'disconnect'; reason: number; serverReference: string | null; target: RedirectTarget | null; failure: string | null }
  | { type: 'disconnected'; phase: 'attempt' | 'established'; error: MqttError; reconnecting: true }
  | { type: 'publish'; message: IncomingMessage }
  | { type: 'outgoing'; packet: OutgoingSummary; packetId: number | null }
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

export class MqttClient<P extends ProtocolVersion = ProtocolVersion> {
  constructor(options: MqttClientOptions<P>)
  connect(): Promise<ConnectResult>
  enqueuePublish(topic: string, payload: Uint8Array | string, options?: PublishOptions<P>): Promise<AdmissionResult>
  publish(topic: string, payload: Uint8Array | string, options?: PublishOptions<P>): Promise<PublishCompletion>
  subscribe(filters: Subscription<P>[], options?: SubscribeOptions<P>): Promise<SubscribeCompletion>
  unsubscribe(filters: string[], options?: UnsubscribeOptions<P>): Promise<UnsubscribeCompletion>
  events(): AsyncIterableIterator<MqttEvent>
  diagnostics(): Promise<ClientDiagnostics>
  close(options?: CloseOptions): Promise<void>
  closeNow(): Promise<void>
}
