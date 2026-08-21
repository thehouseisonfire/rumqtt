'use strict'

const { NativeMqttClient } = require('./loader.cjs')

const textEncoder = new TextEncoder()
const textDecoder = new TextDecoder()

class MqttError extends Error {
  constructor(value) {
    super(value.message)
    this.name = 'MqttError'
    this.code = value.code
    this.kind = value.kind
    this.operationId = value.operationId === undefined ? undefined : BigInt(value.operationId)
    this.brokerReason = value.brokerReason
    this.retryable = value.retryable
    this.delivery = value.delivery
    this.ambiguous = value.ambiguous
  }
}

function finiteInteger(value, name, minimum, maximum) {
  if (!Number.isFinite(value) || !Number.isInteger(value) || value < minimum || value > maximum) {
    throw new TypeError(`${name} must be an integer from ${minimum} through ${maximum}`)
  }
  return value
}

function bytes(value, name) {
  if (!(value instanceof Uint8Array)) throw new TypeError(`${name} must be a Uint8Array`)
  return new Uint8Array(value)
}

function mqttError(code, kind, message, delivery = 'notAdmitted') {
  return new MqttError({ code, kind, message, retryable: false, delivery, ambiguous: false })
}

function base64(value, name) {
  if (value === undefined) return undefined
  const copy = bytes(value, name)
  if (typeof Buffer !== 'undefined') return Buffer.from(copy).toString('base64')
  let binary = ''
  for (const byte of copy) binary += String.fromCharCode(byte)
  return btoa(binary)
}

function fromBase64(value) {
  if (typeof Deno === 'undefined' && typeof Buffer !== 'undefined') return Buffer.from(value, 'base64')
  const binary = atob(value)
  return Uint8Array.from(binary, character => character.charCodeAt(0))
}

function normalizeTransport(transport = { kind: 'tcp' }) {
  if (!transport || !['tcp', 'tls', 'websocket', 'wss'].includes(transport.kind)) {
    throw new TypeError('transport.kind must be tcp, tls, websocket, or wss')
  }
  if ((transport.kind === 'websocket' || transport.kind === 'wss') && typeof transport.url !== 'string') {
    throw new TypeError('WebSocket transports require a URL')
  }
  const result = { kind: transport.kind, url: transport.url }
  if (transport.kind === 'tls' || transport.kind === 'wss') {
    result.caBase64 = base64(transport.ca, 'transport.ca')
    result.clientCertificateBase64 = base64(transport.clientCertificate, 'transport.clientCertificate')
    result.privateKeyBase64 = base64(transport.privateKey, 'transport.privateKey')
    if ((result.clientCertificateBase64 === undefined) !== (result.privateKeyBase64 === undefined)) {
      throw new TypeError('TLS clientCertificate and privateKey must be supplied together')
    }
  }
  return result
}

function normalizeConfig(options) {
  if (!options || typeof options !== 'object') throw new TypeError('client options are required')
  if (options.protocol !== '3.1.1' && options.protocol !== '5.0') throw new TypeError("protocol must be '3.1.1' or '5.0'")
  if (options.protocol === '3.1.1' && ('cleanStart' in options || 'sessionExpiryInterval' in options)) {
    throw new TypeError('MQTT 5 session options require protocol 5.0')
  }
  if (options.protocol === '5.0' && 'cleanSession' in options) {
    throw new TypeError('cleanSession is only valid for protocol 3.1.1')
  }
  if (typeof options.brokerHost !== 'string' || typeof options.clientId !== 'string') {
    throw new TypeError('brokerHost and clientId must be strings')
  }
  const config = {
    protocol: options.protocol,
    brokerHost: options.brokerHost,
    brokerPort: finiteInteger(options.brokerPort, 'brokerPort', 1, 65535),
    clientId: options.clientId,
    transport: normalizeTransport(options.transport),
    keepAliveSeconds: finiteInteger(options.keepAliveSeconds ?? 60, 'keepAliveSeconds', 1, 65535),
    connectionTimeoutSeconds: finiteInteger(options.connectionTimeoutSeconds ?? 5, 'connectionTimeoutSeconds', 1, 65535),
    username: options.username,
    passwordBase64: base64(options.password, 'password'),
    requestCapacity: finiteInteger(options.requestCapacity ?? 10, 'requestCapacity', 1, 0xffffffff),
    eventCapacity: finiteInteger(options.eventCapacity ?? 256, 'eventCapacity', 1, 0xffffffff),
    eventDeliveryTimeoutMs: finiteInteger(options.eventDeliveryTimeoutMs ?? 5000, 'eventDeliveryTimeoutMs', 1, Number.MAX_SAFE_INTEGER),
    ackMode: options.ackMode ?? 'automatic',
    incomingPacketSizeLimit: finiteInteger(options.incomingPacketSizeLimit ?? 10 * 1024, 'incomingPacketSizeLimit', 1, 0xffffffff),
    emitOutgoingEvents: options.emitOutgoingEvents ?? false,
    cleanSession: options.protocol === '3.1.1' ? options.cleanSession : undefined,
    cleanStart: options.protocol === '5.0' ? options.cleanStart : undefined,
    sessionExpiryInterval: options.protocol === '5.0' && options.sessionExpiryInterval !== undefined ?
      finiteInteger(options.sessionExpiryInterval, 'sessionExpiryInterval', 0, 0xffffffff) : undefined,
  }
  if (!['automatic', 'manual'].includes(config.ackMode)) throw new TypeError('ackMode must be automatic or manual')
  if (options.protocol === '3.1.1' && config.passwordBase64 !== undefined && config.username === undefined) {
    throw new TypeError('an MQTT 3.1.1 password requires a username')
  }
  return config
}

function jsonValue(value) {
  if (value instanceof Uint8Array) return Array.from(new Uint8Array(value))
  if (Array.isArray(value)) return value.map(jsonValue)
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, jsonValue(item)]))
  return value
}

function serializedOptions(value) {
  return value === undefined ? undefined : JSON.stringify(jsonValue(value))
}

function protocolOptions(protocol, operation, filters, options) {
  if (protocol !== '3.1.1') return
  const incompatible = operation === 'publish' ? options != null && 'properties' in options :
    operation === 'subscribe' ? options !== undefined || filters.some(filter => filter != null && 'options' in filter) :
      options !== undefined
  if (incompatible) {
    throw mqttError('COMMAND_INVALID', 'admission', `MQTT 5 ${operation} options require protocol 5.0`)
  }
}

function unwrap(serialized) {
  const response = JSON.parse(serialized)
  if (!response.ok) throw new MqttError(response.error)
  if (response.operationId !== undefined) response.operationId = BigInt(response.operationId)
  return response
}

class MqttClient {
  #config
  #native
  #state = 'idle'
  #startPromise
  #connectPromise
  #eventsActive = false
  #closePromise
  #closeNowPromise

  constructor(options) {
    this.#config = normalizeConfig(options)
  }

  #getNative() {
    if (!this.#native) {
      try {
        this.#native = new NativeMqttClient(JSON.stringify(this.#config))
      } catch (cause) {
        let value
        try { value = JSON.parse(cause.message) } catch {}
        throw value?.error ? new MqttError(value.error) : mqttError(
          'CONFIGURATION_INVALID', 'configuration', cause.message, 'notApplicable',
        )
      }
    }
    return this.#native
  }

  connect() {
    if (this.#state === 'closing' || this.#state === 'closed') {
      return Promise.reject(mqttError('SHUTDOWN', 'shutdown', 'client is closing'))
    }
    if (this.#state === 'failed') return this.#connectPromise
    if (!this.#connectPromise) {
      this.#state = 'connecting'
      this.#startPromise = Promise.resolve().then(() => this.#getNative())
      this.#connectPromise = this.#startPromise
        .then(native => native.connect())
        .then(unwrap)
        .then(({ protocol, sessionPresent }) => {
          if (this.#state === 'connecting') this.#state = 'connected'
          return { protocol, sessionPresent }
        }, error => {
          if (this.#state === 'connecting') this.#state = 'failed'
          throw error
        })
    }
    return this.#connectPromise
  }

  enqueuePublish(topic, payload, options) {
    try {
      protocolOptions(this.#config.protocol, 'publish', [], options)
      const input = this.#payload(payload)
      const serialized = serializedOptions(options)
      return this.#whenConnected(native => native.enqueuePublish(topic, input, serialized))
        .then(unwrap).then(({ operationId }) => ({ operationId }))
    } catch (error) { return Promise.reject(error) }
  }

  publish(topic, payload, options) {
    try {
      protocolOptions(this.#config.protocol, 'publish', [], options)
      const input = this.#payload(payload)
      const serialized = serializedOptions(options)
      return this.#whenConnected(native => native.publish(topic, input, serialized))
        .then(unwrap).then(({ operationId, result }) => ({ operationId, ...result }))
    } catch (error) { return Promise.reject(error) }
  }

  subscribe(filters, options) {
    try {
      protocolOptions(this.#config.protocol, 'subscribe', filters, options)
      const serializedFilters = JSON.stringify(jsonValue(filters))
      const serialized = serializedOptions(options)
      return this.#whenConnected(native => native.subscribe(serializedFilters, serialized))
        .then(unwrap).then(({ operationId, result }) => ({ operationId, ...result }))
    } catch (error) { return Promise.reject(error) }
  }

  unsubscribe(filters, options) {
    try {
      protocolOptions(this.#config.protocol, 'unsubscribe', [], options)
      const serializedFilters = JSON.stringify(filters)
      const serialized = serializedOptions(options)
      return this.#whenConnected(native => native.unsubscribe(serializedFilters, serialized))
        .then(unwrap).then(({ operationId, result }) => ({ operationId, ...result }))
    } catch (error) { return Promise.reject(error) }
  }

  events() {
    if (this.#eventsActive) throw new MqttError({ code: 'EVENT_CONSUMER_ACTIVE', kind: 'admission', message: 'another event iterator is active', retryable: false, delivery: 'notAdmitted', ambiguous: false })
    this.#eventsActive = true
    const client = this
    let finished = false
    let pendingReads = 0
    const release = () => {
      if (finished && pendingReads === 0) client.#eventsActive = false
    }
    return {
      [Symbol.asyncIterator]() { return this },
      async next() {
        if (finished) return { done: true, value: undefined }
        pendingReads += 1
        try {
          const response = unwrap(await client.#whenEventReadable(native => native.nextEvent()))
          if (response.done) {
            finished = true
            return { done: true, value: undefined }
          }
          return { done: false, value: client.#hydrateEvent(response.event) }
        } catch (error) {
          finished = true
          throw error
        } finally {
          pendingReads -= 1
          release()
        }
      },
      async return() {
        finished = true
        release()
        return { done: true, value: undefined }
      },
    }
  }

  diagnostics() {
    return this.#whenConnected(native => native.diagnostics()).then(unwrap).then(({ result }) => {
      const { type: _, ...diagnostics } = result
      return diagnostics
    })
  }

  close(options = {}) {
    if (this.#closeNowPromise) return this.#closeNowPromise
    if (this.#closePromise) return this.#closePromise
    let timeoutMs
    try { timeoutMs = finiteInteger(options.timeoutMs ?? 5000, 'timeoutMs', 1, 0xffffffff) } catch (error) {
      return Promise.reject(error)
    }
    if (this.#state === 'idle' || (this.#state === 'failed' && !this.#native)) {
      this.#state = 'closed'
      return (this.#closePromise = Promise.resolve())
    }
    this.#state = 'closing'
    this.#closePromise = this.#startPromise.then(native => native.close(timeoutMs)).then(unwrap).then(() => {
      this.#state = 'closed'
    })
    return this.#closePromise
  }

  closeNow() {
    if (this.#closeNowPromise) return this.#closeNowPromise
    if (this.#state === 'closed') return this.#closePromise
    if (this.#state === 'idle' || (this.#state === 'failed' && !this.#native)) {
      this.#state = 'closed'
      this.#closeNowPromise = Promise.resolve()
      return (this.#closePromise = this.#closeNowPromise)
    }
    this.#state = 'closing'
    this.#closeNowPromise = this.#startPromise.then(native => native.closeNow(5000)).then(unwrap).then(() => {
      this.#state = 'closed'
    })
    return this.#closeNowPromise
  }

  #whenConnected(operation) {
    if (this.#state !== 'connected') {
      return Promise.reject(mqttError('NOT_CONNECTED', 'admission', 'connect() must resolve before this operation'))
    }
    return Promise.resolve().then(() => operation(this.#native))
  }

  #whenEventReadable(operation) {
    if (this.#state !== 'connected' && this.#state !== 'closing') {
      return Promise.reject(mqttError('NOT_CONNECTED', 'admission', 'connect() must resolve before this operation'))
    }
    return this.#startPromise.then(operation)
  }

  #payload(payload) {
    if (typeof payload === 'string') return textEncoder.encode(payload)
    return bytes(payload, 'payload')
  }

  #hydrateEvent(event) {
    if (event.type === 'publish') {
      event.message.topic = textDecoder.decode(fromBase64(event.message.topicBase64))
      event.message.payload = fromBase64(event.message.payloadBase64)
      delete event.message.topicBase64
      delete event.message.payloadBase64
      if (event.message.properties?.correlationDataBase64 != null) {
        event.message.properties.correlationData = fromBase64(event.message.properties.correlationDataBase64)
        delete event.message.properties.correlationDataBase64
      }
      if (event.message.ackId !== undefined && event.message.ackId !== null) {
        const ackId = BigInt(event.message.ackId)
        let started = false
        event.message.ack = () => {
          if (started) return Promise.reject(mqttError('COMMAND_INVALID', 'admission', 'acknowledgement was already consumed or is invalid'))
          started = true
          return this.#whenConnected(native => native.acknowledge(ackId)).then(unwrap).then(() => undefined)
        }
      }
      delete event.message.ackId
    }
    if (event.error) event.error = new MqttError(event.error)
    return event
  }
}

module.exports = { MqttClient, MqttError }
