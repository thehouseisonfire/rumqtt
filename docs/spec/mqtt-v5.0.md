# MQTT Version 5.0 Compliance Digest

## Metadata

- Source: https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html
- Spec version: 5.0
- Source Last-Modified: Thu, 07 Mar 2019 17:00:00 GMT
- Generated (UTC): 2026-03-04T03:30:32+00:00
- Unique requirements: 252

## Section Index

- [1.5.4 UTF-8 Encoded String](#1-5-4-utf-8-encoded-string) (3 requirements)
- [1.5.5 Variable Byte Integer](#1-5-5-variable-byte-integer) (1 requirements)
- [1.5.7 UTF-8 String Pair](#1-5-7-utf-8-string-pair) (1 requirements)
- [2.1.3 Flags](#2-1-3-flags) (1 requirements)
- [2.2.1 Packet Identifier](#2-2-1-packet-identifier) (5 requirements)
- [2.2.2.1 Property Length](#2-2-2-1-property-length) (1 requirements)
- [3.1 CONNECT – Connection Request](#3-1-connect-connection-request) (2 requirements)
- [3.1.2.1 Protocol Name](#3-1-2-1-protocol-name) (1 requirements)
- [3.1.2.2 Protocol Version](#3-1-2-2-protocol-version) (1 requirements)
- [3.1.2.3 Connect Flags](#3-1-2-3-connect-flags) (1 requirements)
- [3.1.2.4 Clean Start](#3-1-2-4-clean-start) (3 requirements)
- [3.1.2.5 Will Flag](#3-1-2-5-will-flag) (4 requirements)
- [3.1.2.6 Will QoS](#3-1-2-6-will-qos) (2 requirements)
- [3.1.2.7 Will Retain](#3-1-2-7-will-retain) (3 requirements)
- [3.1.2.8 User Name Flag](#3-1-2-8-user-name-flag) (2 requirements)
- [3.1.2.9 Password Flag](#3-1-2-9-password-flag) (2 requirements)
- [3.1.2.10 Keep Alive](#3-1-2-10-keep-alive) (3 requirements)
- [3.1.2.11.2 Session Expiry Interval](#3-1-2-11-2-session-expiry-interval) (1 requirements)
- [3.1.2.11.4 Maximum Packet Size](#3-1-2-11-4-maximum-packet-size) (2 requirements)
- [3.1.2.11.5 Topic Alias Maximum](#3-1-2-11-5-topic-alias-maximum) (2 requirements)
- [3.1.2.11.6 Request Response Information](#3-1-2-11-6-request-response-information) (1 requirements)
- [3.1.2.11.7 Request Problem Information](#3-1-2-11-7-request-problem-information) (1 requirements)
- [3.1.2.11.9 Authentication Method](#3-1-2-11-9-authentication-method) (1 requirements)
- [3.1.3 CONNECT Payload](#3-1-3-connect-payload) (1 requirements)
- [3.1.3.1 Client Identifier (ClientID)](#3-1-3-1-client-identifier-clientid) (7 requirements)
- [3.1.3.2.2 Will Delay Interval](#3-1-3-2-2-will-delay-interval) (1 requirements)
- [3.1.3.2.8 User Property](#3-1-3-2-8-user-property) (1 requirements)
- [3.1.3.3 Will Topic](#3-1-3-3-will-topic) (1 requirements)
- [3.1.3.5 User Name](#3-1-3-5-user-name) (1 requirements)
- [3.1.4 CONNECT Actions](#3-1-4-connect-actions) (6 requirements)
- [3.2 CONNACK – Connect acknowledgement](#3-2-connack-connect-acknowledgement) (2 requirements)
- [3.2.2.1 Connect Acknowledge Flags](#3-2-2-1-connect-acknowledge-flags) (1 requirements)
- [3.2.2.1.1 Session Present](#3-2-2-1-1-session-present) (5 requirements)
- [3.2.2.2 Connect Reason Code](#3-2-2-2-connect-reason-code) (1 requirements)
- [3.2.2.3.4 Maximum QoS](#3-2-2-3-4-maximum-qos) (4 requirements)
- [3.2.2.3.5 Retain Available](#3-2-2-3-5-retain-available) (2 requirements)
- [3.2.2.3.6 Maximum Packet Size](#3-2-2-3-6-maximum-packet-size) (1 requirements)
- [3.2.2.3.7 Assigned Client Identifier](#3-2-2-3-7-assigned-client-identifier) (1 requirements)
- [3.2.2.3.8 Topic Alias Maximum](#3-2-2-3-8-topic-alias-maximum) (2 requirements)
- [3.2.2.3.9 Reason String](#3-2-2-3-9-reason-string) (1 requirements)
- [3.2.2.3.10 User Property](#3-2-2-3-10-user-property) (1 requirements)
- [3.2.2.3.14 Server Keep Alive](#3-2-2-3-14-server-keep-alive) (2 requirements)
- [3.3.1.1 DUP](#3-3-1-1-dup) (3 requirements)
- [3.3.1.2 QoS](#3-3-1-2-qos) (1 requirements)
- [3.3.1.3 RETAIN](#3-3-1-3-retain) (9 requirements)
- [3.3.2.1 Topic Name](#3-3-2-1-topic-name) (3 requirements)
- [3.3.2.3.2 Payload Format Indicator](#3-3-2-3-2-payload-format-indicator) (1 requirements)
- [3.3.2.3.3 Message Expiry Interval`](#3-3-2-3-3-message-expiry-interval) (2 requirements)
- [3.3.2.3.4 Topic Alias](#3-3-2-3-4-topic-alias) (6 requirements)
- [3.3.2.3.5 Response Topic](#3-3-2-3-5-response-topic) (3 requirements)
- [3.3.2.3.6 Correlation Data](#3-3-2-3-6-correlation-data) (1 requirements)
- [3.3.2.3.7 User Property](#3-3-2-3-7-user-property) (2 requirements)
- [3.3.2.3.9 Content Type](#3-3-2-3-9-content-type) (2 requirements)
- [3.3.4 PUBLISH Actions](#3-3-4-publish-actions) (10 requirements)
- [3.4.2.1 PUBACK Reason Code](#3-4-2-1-puback-reason-code) (1 requirements)
- [3.4.2.2.2 Reason String](#3-4-2-2-2-reason-string) (1 requirements)
- [3.4.2.2.3 User Property](#3-4-2-2-3-user-property) (1 requirements)
- [3.5.2.1 PUBREC Reason Code](#3-5-2-1-pubrec-reason-code) (1 requirements)
- [3.5.2.2.2 Reason String](#3-5-2-2-2-reason-string) (1 requirements)
- [3.5.2.2.3 User Property](#3-5-2-2-3-user-property) (1 requirements)
- [3.6.1 PUBREL Fixed Header](#3-6-1-pubrel-fixed-header) (1 requirements)
- [3.6.2.1 PUBREL Reason Code](#3-6-2-1-pubrel-reason-code) (1 requirements)
- [3.6.2.2.2 Reason String](#3-6-2-2-2-reason-string) (1 requirements)
- [3.6.2.2.3 User Property](#3-6-2-2-3-user-property) (1 requirements)
- [3.7.2.1 PUBCOMP Reason Code](#3-7-2-1-pubcomp-reason-code) (1 requirements)
- [3.7.2.2.2 Reason String](#3-7-2-2-2-reason-string) (1 requirements)
- [3.7.2.2.3 User Property](#3-7-2-2-3-user-property) (1 requirements)
- [3.8.1 SUBSCRIBE Fixed Header](#3-8-1-subscribe-fixed-header) (1 requirements)
- [3.8.3 SUBSCRIBE Payload](#3-8-3-subscribe-payload) (2 requirements)
- [3.8.3.1 Subscription Options](#3-8-3-1-subscription-options) (3 requirements)
- [3.8.4 SUBSCRIBE Actions](#3-8-4-subscribe-actions) (8 requirements)
- [3.9.2.1.2 Reason String](#3-9-2-1-2-reason-string) (1 requirements)
- [3.9.2.1.3 User Property](#3-9-2-1-3-user-property) (1 requirements)
- [3.9.3 SUBACK Payload](#3-9-3-suback-payload) (2 requirements)
- [3.10.1 UNSUBSCRIBE Fixed Header](#3-10-1-unsubscribe-fixed-header) (1 requirements)
- [3.10.3 UNSUBSCRIBE Payload](#3-10-3-unsubscribe-payload) (2 requirements)
- [3.10.4 UNSUBSCRIBE Actions](#3-10-4-unsubscribe-actions) (6 requirements)
- [3.11.2.1.2 Reason String](#3-11-2-1-2-reason-string) (1 requirements)
- [3.11.2.1.3 User Property](#3-11-2-1-3-user-property) (1 requirements)
- [3.11.3 UNSUBACK Payload](#3-11-3-unsuback-payload) (2 requirements)
- [3.12.4 PINGREQ Actions](#3-12-4-pingreq-actions) (1 requirements)
- [3.14 DISCONNECT – Disconnect notification](#3-14-disconnect-disconnect-notification) (1 requirements)
- [3.14.1 DISCONNECT Fixed Header](#3-14-1-disconnect-fixed-header) (1 requirements)
- [3.14.2.1 Disconnect Reason Code](#3-14-2-1-disconnect-reason-code) (1 requirements)
- [3.14.2.2.2 Session Expiry Interval](#3-14-2-2-2-session-expiry-interval) (1 requirements)
- [3.14.2.2.3 Reason String](#3-14-2-2-3-reason-string) (1 requirements)
- [3.14.2.2.4 User Property](#3-14-2-2-4-user-property) (1 requirements)
- [3.14.4 DISCONNECT Actions](#3-14-4-disconnect-actions) (3 requirements)
- [3.15.1 AUTH Fixed Header](#3-15-1-auth-fixed-header) (1 requirements)
- [3.15.2.1 Authenticate Reason Code](#3-15-2-1-authenticate-reason-code) (1 requirements)
- [3.15.2.2.4 Reason String](#3-15-2-2-4-reason-string) (1 requirements)
- [3.15.2.2.5 User Property](#3-15-2-2-5-user-property) (1 requirements)
- [4.1.1 Storing Session State](#4-1-1-storing-session-state) (2 requirements)
- [4.2 Network Connections](#4-2-network-connections) (1 requirements)
- [4.3.1 QoS 0: At most once delivery](#4-3-1-qos-0-at-most-once-delivery) (1 requirements)
- [4.3.2 QoS 1: At least once delivery](#4-3-2-qos-1-at-least-once-delivery) (5 requirements)
- [4.3.3 QoS 2: Exactly once delivery](#4-3-3-qos-2-exactly-once-delivery) (13 requirements)
- [4.4 Message delivery retry](#4-4-message-delivery-retry) (2 requirements)
- [4.5 Message receipt](#4-5-message-receipt) (2 requirements)
- [4.6 Message ordering](#4-6-message-ordering) (6 requirements)
- [4.7.1 Topic wildcards](#4-7-1-topic-wildcards) (1 requirements)
- [4.7.1.2 Multi-level wildcard](#4-7-1-2-multi-level-wildcard) (1 requirements)
- [4.7.1.3 Single-level wildcard](#4-7-1-3-single-level-wildcard) (1 requirements)
- [4.7.2 Topics beginning with $](#4-7-2-topics-beginning-with) (1 requirements)
- [4.7.3 Topic semantic and usage](#4-7-3-topic-semantic-and-usage) (4 requirements)
- [4.8.2 Shared Subscriptions](#4-8-2-shared-subscriptions) (6 requirements)
- [4.9 Flow Control](#4-9-flow-control) (3 requirements)
- [4.12 Enhanced authentication](#4-12-enhanced-authentication) (7 requirements)
- [4.12.1 Re-authentication](#4-12-1-re-authentication) (2 requirements)
- [4.13.1 Malformed Packet and Protocol Errors](#4-13-1-malformed-packet-and-protocol-errors) (1 requirements)
- [4.13.2 Other errors](#4-13-2-other-errors) (1 requirements)
- [6 Using WebSocket as a network transport](#6-using-websocket-as-a-network-transport) (4 requirements)
- [7.1.2 MQTT Client conformance clause](#7-1-2-mqtt-client-conformance-clause) (2 requirements)

## Compliance Sections

### 1.5.4 UTF-8 Encoded String

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-1.5.4-1 | MUST | The character data in a UTF-8 Encoded String MUST be well-formed UTF-8 as defined by the Unicode specification and restated in RFC 3629 . In particular, the character data MUST NOT include encodings of code points between U+D800 and U+DFFF . | [_Hlk511139803](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Hlk511139803) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |
| MQTT-1.5.4-2 | MUST NOT | A UTF-8 Encoded String MUST NOT include an encoding of the null character U+0000. . If a receiver (Server or Client) receives an MQTT Control Packet containing U+0000 it is a Malformed Packet. Refer to section 4.13 for information about handling errors. | [_Hlk511139803](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Hlk511139803) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |
| MQTT-1.5.4-3 | MUST NOT | A UTF-8 encoded sequence 0xEF 0xBB 0xBF is always interpreted as U+FEFF ("ZERO WIDTH NO-BREAK SPACE") wherever it appears in a string and MUST NOT be skipped over or stripped off by a packet receiver . | [_Hlk511139803](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Hlk511139803) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |

### 1.5.5 Variable Byte Integer

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-1.5.5-1 | MUST | The Variable Byte Integer is encoded using an encoding scheme which uses a single byte for values up to 127. Larger values are handled as follows. The least significant seven bits of each byte encode the data, and the most significant bit is used to indicate whether there are... | [_Toc473619950](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473619950) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |

### 1.5.7 UTF-8 String Pair

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-1.5.7-1 | MUST | Both strings MUST comply with the requirements for UTF-8 Encoded Strings . If a receiver (Client or Server) receives a string pair which does not meet these requirements it is a Malformed Packet. Refer to section 4.13 for information about handling errors. | [_Toc464547791](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464547791) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |

### 2.1.3 Flags

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-2.1.3-1 | MUST | The remaining bits of byte 1 in the Fixed Header contain flags specific to each MQTT Control Packet type as shown below. Where a flag bit is marked as “Reserved”, it is reserved for future use and MUST be set to the value listed . | [_Toc353481062](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc353481062) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |

### 2.2.1 Packet Identifier

Compliance digest: 5 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-2.2.1-2 | MUST NOT | A PUBLISH packet MUST NOT contain a Packet Identifier if its QoS value is set to 0 . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_2.3_-) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |
| MQTT-2.2.1-3 | MUST | Each time a Client sends a new SUBSCRIBE, UNSUBSCRIBE,or PUBLISH (where QoS > 0) MQTT Control Packet it MUST assign it a non-zero Packet Identifier that is currently unused . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_2.3_-) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |
| MQTT-2.2.1-4 | MUST | Each time a Server sends a new PUBLISH (with QoS > 0) MQTT Control Packet it MUST assign it a non zero Packet Identifier that is currently unused . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_2.3_-) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |
| MQTT-2.2.1-5 | MUST | A PUBACK, PUBREC , PUBREL, or PUBCOMP packet MUST contain the same Packet Identifier as the PUBLISH packet that was originally sent . A SUBACK and UNSUBACK MUST contain the Packet Identifier that was used in the corresponding SUBSCRIBE and UNSUBSCRIBE packet respectively . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_2.3_-) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |
| MQTT-2.2.1-6 | MUST | A PUBACK, PUBREC , PUBREL, or PUBCOMP packet MUST contain the same Packet Identifier as the PUBLISH packet that was originally sent . A SUBACK and UNSUBACK MUST contain the Packet Identifier that was used in the corresponding SUBSCRIBE and UNSUBSCRIBE packet respectively . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_2.3_-) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |

### 2.2.2.1 Property Length

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-2.2.2-1 | MUST | The Property Length is encoded as a Variable Byte Integer. The Property Length does not include the bytes used to encode itself, but includes the length of the Properties. If there are no properties, this MUST be indicated by including a Property Length of zero . | [_Toc471282714](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282714) | rumqttc-v5/src/mqttbytes/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/mod.rs<br>rumqttc-v5/src/mqttbytes/v5/codec.rs | unreviewed |

### 3.1 CONNECT – Connection Request

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.0-1 | MUST | After a Network Connection is established by a Client to a Server, the first packet sent from the Client to the Server MUST be a CONNECT packet . | [_CONNECT_–_Connection](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_CONNECT_–_Connection) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.0-2 | MUST | A Client can only send the CONNECT packet once over a Network Connection. The Server MUST process a second CONNECT packet sent from a Client as a Protocol Error and close the Network Connection . Refer to section 4.13 for information about handling errors. | [_CONNECT_–_Connection](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_CONNECT_–_Connection) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.1 Protocol Name

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-1 | MUST | A Server which support multiple protocols uses the Protocol Name to determine whether the data is MQTT. The protocol name MUST be the UTF-8 String "MQTT". If the Server does not want to accept the CONNECT, and wishes to reveal that it is an MQTT Server it MAY send a CONNACK pa... | [_Figure_3.2_-](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.2_-) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.2 Protocol Version

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-2 | MAY | A Server which supports multiple versions of the MQTT protocol uses the Protocol Version to determine which version of MQTT the Client is using. If the Protocol Version is not 5 and the Server does not want to accept the CONNECT packet, the Server MAY send a CONNACK packet wit... | [_Figure_3.3_-](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.3_-) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.3 Connect Flags

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-3 | MUST | The Server MUST validate that the reserved flag in the CONNECT packet is set to 0 . If the reserved flag is not 0 it is a Malformed Packet. Refer to section 4.13 for information about handling errors. | [_Figure_3.4_-](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.4_-) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.4 Clean Start

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-4 | MUST | If a CONNECT packet is received with Clean Start is set to 1, the Client and Server MUST discard any existing Session and start a new Session . Consequently, the Session Present flag in CONNACK is always set to 0 if Clean Start is set to 1. | [_Clean_Start](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Clean_Start) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-5 | MUST | If a CONNECT packet is received with Clean Start set to 0 and there is a Session associated with the Client Identifier, the Server MUST resume communications with the Client based on state from the existing Session . If a CONNECT packet is received with Clean Start set to 0 an... | [_Clean_Start](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Clean_Start) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-6 | MUST | If a CONNECT packet is received with Clean Start set to 0 and there is a Session associated with the Client Identifier, the Server MUST resume communications with the Client based on state from the existing Session . If a CONNECT packet is received with Clean Start set to 0 an... | [_Clean_Start](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Clean_Start) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.5 Will Flag

Compliance digest: 4 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-7 | MUST | If the Will Flag is set to 1 this indicates that a Will Message MUST be stored on the Server and associated with the Session . The Will Message consists of the Will Properties, Will Topic, and Will Payload fields in the CONNECT Payload. | [_Will_Flag](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Will_Flag) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-8 | MUST | If the Will Flag is set to 1 this indicates that a Will Message MUST be stored on the Server and associated with the Session . The Will Message consists of the Will Properties, Will Topic, and Will Payload fields in the CONNECT Payload. | [_Will_Flag](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Will_Flag) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-9 | MUST | If the Will Flag is set to 1, the Will Properties, Will Topic, and Will Payload fields MUST be present in the Payload . The Will Message MUST be removed from the stored Session State in the Server once it has been published or the Server has received a DISCONNECT packet with a... | [_Will_Flag](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Will_Flag) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-10 | MUST | If the Will Flag is set to 1, the Will Properties, Will Topic, and Will Payload fields MUST be present in the Payload . The Will Message MUST be removed from the stored Session State in the Server once it has been published or the Server has received a DISCONNECT packet with a... | [_Will_Flag](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Will_Flag) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.6 Will QoS

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-11 | MUST | If the Will Flag is set to 0, then the Will QoS MUST be set to 0 (0x00) . | [_Toc385349233](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349233) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-12 | UNSPECIFIED | If the Will Flag is set to 1, the value of Will QoS can be 0 (0x00), 1 (0x01), or 2 (0x02) . A value of 3 (0x03) is a Malformed Packet. Refer to section 4.13 for information about handling errors. | [_Toc385349233](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349233) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.7 Will Retain

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-13 | MUST | If the Will Flag is set to 0, then Will Retain MUST be set to 0 . If the Will Flag is set to 1 and Will Retain is set to 0, the Server MUST publish the Will Message as a non-retained message . If the Will Flag is set to 1 and Will Retain is set to 1, the Server MUST publish th... | [_Toc385349234](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349234) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-14 | MUST | If the Will Flag is set to 0, then Will Retain MUST be set to 0 . If the Will Flag is set to 1 and Will Retain is set to 0, the Server MUST publish the Will Message as a non-retained message . If the Will Flag is set to 1 and Will Retain is set to 1, the Server MUST publish th... | [_Toc385349234](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349234) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-15 | MUST | If the Will Flag is set to 0, then Will Retain MUST be set to 0 . If the Will Flag is set to 1 and Will Retain is set to 0, the Server MUST publish the Will Message as a non-retained message . If the Will Flag is set to 1 and Will Retain is set to 1, the Server MUST publish th... | [_Toc385349234](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349234) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.8 User Name Flag

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-16 | MUST NOT | If the User Name Flag is set to 0, a User Name MUST NOT be present in the Payload . If the User Name Flag is set to 1, a User Name MUST be present in the Payload . | [_Toc385349235](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349235) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-17 | MUST NOT | If the User Name Flag is set to 0, a User Name MUST NOT be present in the Payload . If the User Name Flag is set to 1, a User Name MUST be present in the Payload . | [_Toc385349235](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349235) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.9 Password Flag

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-18 | MUST NOT | If the Password Flag is set to 0, a Password MUST NOT be present in the Payload . If the Password Flag is set to 1, a Password MUST be present in the Payload . | [_Toc462729098](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc462729098) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-19 | MUST NOT | If the Password Flag is set to 0, a Password MUST NOT be present in the Payload . If the Password Flag is set to 1, a Password MUST be present in the Payload . | [_Toc462729098](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc462729098) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.10 Keep Alive

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-20 | MUST | The Keep Alive is a Two Byte Integer which is a time interval measured in seconds. It is the maximum time interval that is permitted to elapse between the point at which the Client finishes transmitting one MQTT Control Packet and the point it starts sending the next. | [_Figure_3.5_Keep](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.5_Keep) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-21 | MUST | If the Server returns a Server Keep Alive on the CONNACK packet, the Client MUST use that value instead of the value it sent as the Keep Alive . | [_Figure_3.5_Keep](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.5_Keep) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-22 | MUST | If the Keep Alive value is non-zero and the Server does not receive an MQTT Control Packet from the Client within one and a half times the Keep Alive time period, it MUST close the Network Connection to the Client as if the network had failed . | [_Figure_3.5_Keep](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.5_Keep) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.11.2 Session Expiry Interval

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-23 | MUST | The Client and Server MUST store the Session State after the Network Connection is closed if the Session Expiry Interval is greater than 0 . | [_Toc471483565](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483565) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.11.4 Maximum Packet Size

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-24 | MUST NOT | The Server MUST NOT send packets exceeding Maximum Packet Size to the Client . If a Client receives a packet whose size exceeds this limit, this is a Protocol Error, the Client uses DISCONNECT with Reason Code 0x95 (Packet too large), as described in section 4.13. | [_Toc471483569](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483569) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-25 | MUST | Where a Packet is too large to send, the Server MUST discard it without sending it and then behave as if it had completed sending that Application Message . | [_Toc471483569](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483569) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.11.5 Topic Alias Maximum

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-26 | MUST NOT | This value indicates the highest value that the Client will accept as a Topic Alias sent by the Server. The Client uses this value to limit the number of Topic Aliases that it is willing to hold on this Connection. The Server MUST NOT send a Topic Alias in a PUBLISH packet to... | [_Toc464547825](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464547825) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.2-27 | MUST NOT | This value indicates the highest value that the Client will accept as a Topic Alias sent by the Server. The Client uses this value to limit the number of Topic Aliases that it is willing to hold on this Connection. The Server MUST NOT send a Topic Alias in a PUBLISH packet to... | [_Toc464547825](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464547825) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.11.6 Request Response Information

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-28 | MUST NOT | The Client uses this value to request the Server to return Response Information in the CONNACK. A value of 0 indicates that the Server MUST NOT return Response Information . If the value is 1 the Server MAY return Response Information in the CONNACK packet. | [_Request_Response_Information](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Request_Response_Information) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.11.7 Request Problem Information

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-29 | MAY | If the value of Request Problem Information is 0, the Server MAY return a Reason String or User Properties on a CONNACK or DISCONNECT packet, but MUST NOT send a Reason String or User Properties on any packet other than PUBLISH, CONNACK, or DISCONNECT . | [_Toc464547827](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464547827) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.2.11.9 Authentication Method

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-30 | MUST NOT | If a Client sets an Authentication Method in the CONNECT, the Client MUST NOT send any packets other than AUTH or DISCONNECT packets until it has received a CONNACK packet . | [_Toc511988532](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc511988532) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.3 CONNECT Payload

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.3-1 | MUST | The Payload of the CONNECT packet contains one or more length-prefixed fields, whose presence is determined by the flags in the Variable Header. These fields, if present, MUST appear in the order Client Identifier, Will Properties, Will Topic, Will Payload, User Name, Password . | [_Toc384800404](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800404) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.3.1 Client Identifier (ClientID)

Compliance digest: 7 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.3-2 | MUST | The Client Identifier (ClientID) identifies the Client to the Server. Each Client connecting to the Server has a unique ClientID. The ClientID MUST be used by Clients and by Servers to identify state that they hold relating to this MQTT Session between the Client and the Server . | [_Toc385349242](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349242) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.3-3 | MUST | The ClientID MUST be present and is the first field in the CONNECT packet Payload . | [_Toc473620032](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620032) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.3-4 | MUST | The ClientID MUST be a UTF-8 Encoded String as defined in section 1.5.4 . | [_Toc473620032](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620032) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.3-5 | UNSPECIFIED | "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ" . | [_Toc473620032](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620032) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.3-6 | MAY | A Server MAY allow a Client to supply a ClientID that has a length of zero bytes, however if it does so the Server MUST treat this as a special case and assign a unique ClientID to that Client . It MUST then process the CONNECT packet as if the Client had provided that unique... | [_Toc473620032](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620032) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.3-7 | MAY | A Server MAY allow a Client to supply a ClientID that has a length of zero bytes, however if it does so the Server MUST treat this as a special case and assign a unique ClientID to that Client . It MUST then process the CONNECT packet as if the Client had provided that unique... | [_Toc473620032](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620032) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.3-8 | MAY | If the Server rejects the ClientID it MAY respond to the CONNECT packet with a CONNACK using Reason Code 0x85 (Client Identifier not valid) as described in section 4.13 Handling errors, and then it MUST close the Network Connection . | [_Toc473620032](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620032) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.3.2.2 Will Delay Interval

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.3-9 | MUST NOT | The Server delays publishing the Client’s Will Message until the Will Delay Interval has passed or the Session ends, whichever happens first. If a new Network Connection to this Session is made before the Will Delay Interval has passed, the Server MUST NOT send the Will Message . | [_Will_Delay_Interval_1](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Will_Delay_Interval_1) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.3.2.8 User Property

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.3-10 | MUST | The Server MUST maintain the order of User Properties when publishing the Will Message . | [_Toc493772021](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc493772021) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.3.3 Will Topic

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.3-11 | MUST | If the Will Flag is set to 1, the Will Topic is the next field in the Payload. The Will Topic MUST be a UTF-8 Encoded String as defined in section 1.5.4 . | [_Toc511988546](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc511988546) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.3.5 User Name

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.3-12 | MUST | If the User Name Flag is set to 1, the User Name is the next field in the Payload. The User Name MUST be a UTF-8 Encoded String as defined in section 1.5.4 . It can be used by the Server for authentication and authorization. | [_Toc385349245](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349245) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.1.4 CONNECT Actions

Compliance digest: 6 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.4-1 | MUST | 2. The Server MUST validate that the CONNECT packet matches the format described in section 3.1 and close the Network Connection if it does not match . The Server MAY send a CONNACK with a Reason Code of 0x80 or greater as described in section 4.13 before closing the Network C... | [_CONNECT_Actions](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_CONNECT_Actions) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.4-2 | MAY | The Server MAY check that the contents of the CONNECT packet meet any further restrictions and SHOULD perform authentication and authorization checks. If any of these checks fail, it MUST close the Network Connection . Before closing the Network Connection, it MAY send an appr... | [_CONNECT_Actions](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_CONNECT_Actions) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.4-3 | MUST | 1. If the ClientID represents a Client already connected to the Server, the Server sends a DISCONNECT packet to the existing Client with Reason Code of 0x8E (Session taken over) as described in section 4.13 and MUST close the Network Connection of the existing Client . | [_Toc473620039](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620039) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.4-4 | MUST | 2. The Server MUST perform the processing of Clean Start that is described in section 3.1.2.4 . | [_Toc473620039](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620039) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.4-5 | MUST | 3. The Server MUST acknowledge the CONNECT packet with a CONNACK packet containing a 0x00 (Success) Reason Code . | [_Toc473620039](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620039) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |
| MQTT-3.1.4-6 | MUST NOT | Clients are allowed to send further MQTT Control Packets immediately after sending a CONNECT packet; Clients need not wait for a CONNACK packet to arrive from the Server. If the Server rejects the CONNECT, it MUST NOT process any data sent by the Client after the CONNECT packe... | [_Toc473620039](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620039) | rumqttc-v5/src/mqttbytes/v5/connect.rs | unreviewed |

### 3.2 CONNACK – Connect acknowledgement

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.0-1 | MUST | The CONNACK packet is the packet sent by the Server in response to a CONNECT packet received from a Client. The Server MUST send a CONNACK with a 0x00 (Success) Reason Code before sending any Packet other than AUTH . The Server MUST NOT send more than one CONNACK in a Network... | [_CONNACK_–_Connect](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_CONNACK_–_Connect) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.0-2 | MUST | The CONNACK packet is the packet sent by the Server in response to a CONNECT packet received from a Client. The Server MUST send a CONNACK with a 0x00 (Success) Reason Code before sending any Packet other than AUTH . The Server MUST NOT send more than one CONNACK in a Network... | [_CONNACK_–_Connect](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_CONNACK_–_Connect) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.1 Connect Acknowledge Flags

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-1 | MUST | Byte 1 is the "Connect Acknowledge Flags". Bits 7-1 are reserved and MUST be set to 0 . | [_Figure_3.9_–](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.9_–) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.1.1 Session Present

Compliance digest: 5 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-2 | MUST | If the Server accepts a connection with Clean Start set to 1, the Server MUST set Session Present to 0 in the CONNACK packet in addition to setting a 0x00 (Success) Reason Code in the CONNACK packet . | [_Toc385349255](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349255) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.2-3 | MUST | If the Server accepts a connection with Clean Start set to 0 and the Server has Session State for the ClientID, it MUST set Session Present to 1 in the CONNACK packet, otherwise it MUST set Session Present to 0 in the CONNACK packet. | [_Toc385349255](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349255) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.2-4 | MUST | · If the Client does not have Session State and receives Session Present set to 1 it MUST close the Network Connection . If it wishes to restart with a new Session the Client can reconnect using Clean Start set to 1. | [_Toc385349255](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349255) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.2-5 | MUST | · If the Client does have Session State and receives Session Present set to 0 it MUST discard its Session State if it continues with the Network Connection . | [_Toc385349255](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349255) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.2-6 | MUST | If a Server sends a CONNACK packet containing a non-zero Reason Code it MUST set Session Present to 0 . | [_Toc385349255](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349255) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.2 Connect Reason Code

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-7 | MAY | The values the Connect Reason Code are shown below. If a well formed CONNECT packet is received by the Server, but the Server is unable to complete the Connection the Server MAY send a CONNACK packet containing the appropriate Connect Reason code from this table. | [_Connect_Reason_Code](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Connect_Reason_Code) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.3.4 Maximum QoS

Compliance digest: 4 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-9 | MUST | If a Server does not support QoS 1 or QoS 2 PUBLISH packets it MUST send a Maximum QoS in the CONNACK packet specifying the highest QoS it supports . A Server that does not support QoS 1 or QoS 2 PUBLISH packets MUST still accept SUBSCRIBE packets containing a Requested QoS of... | [_Toc471282758](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282758) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.2-10 | MUST | If a Server does not support QoS 1 or QoS 2 PUBLISH packets it MUST send a Maximum QoS in the CONNACK packet specifying the highest QoS it supports . A Server that does not support QoS 1 or QoS 2 PUBLISH packets MUST still accept SUBSCRIBE packets containing a Requested QoS of... | [_Toc471282758](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282758) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.2-11 | MUST NOT | If a Client receives a Maximum QoS from a Server, it MUST NOT send PUBLISH packets at a QoS level exceeding the Maximum QoS level specified . It is a Protocol Error if the Server receives a PUBLISH packet with a QoS greater than the Maximum QoS it specified. | [_Toc471282758](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282758) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.2-12 | MUST | If a Server receives a CONNECT packet containing a Will QoS that exceeds its capabilities, it MUST reject the connection. It SHOULD use a CONNACK packet with Reason Code 0x9B (QoS not supported) as described in section 4.13 Handling errors, and MUST close the Network Connection . | [_Toc471282758](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282758) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.3.5 Retain Available

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-13 | MUST | If a Server receives a CONNECT packet containing a Will Message with the Will Retain set to 1, and it does not support retained messages, the Server MUST reject the connection request. It SHOULD send CONNACK with Reason Code 0x9A (Retain not supported) and then it MUST close t... | [_Toc471282759](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282759) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.2-14 | MUST NOT | A Client receiving Retain Available set to 0 from the Server MUST NOT send a PUBLISH packet with the RETAIN flag set to 1 . If the Server receives such a packet, this is a Protocol Error. The Server SHOULD send a DISCONNECT with Reason Code of 0x9A (Retain not supported) as de... | [_Toc471282759](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282759) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.3.6 Maximum Packet Size

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-15 | MUST NOT | The Client MUST NOT send packets exceeding Maximum Packet Size to the Server . If a Server receives a packet whose size exceeds this limit, this is a Protocol Error, the Server uses DISCONNECT with Reason Code 0x95 (Packet too large), as described in section 4.13. | [_Toc470189649](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc470189649) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.3.7 Assigned Client Identifier

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-16 | MUST | If the Client connects using a zero length Client Identifier, the Server MUST respond with a CONNACK containing an Assigned Client Identifier. The Assigned Client Identifier MUST be a new Client Identifier not used by any other Session currently in the Server . | [_Toc511988564](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc511988564) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.3.8 Topic Alias Maximum

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-17 | MUST NOT | This value indicates the highest value that the Server will accept as a Topic Alias sent by the Client. The Server uses this value to limit the number of Topic Aliases that it is willing to hold on this Connection. The Client MUST NOT send a Topic Alias in a PUBLISH packet to... | [_Toc464547849](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464547849) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.2-18 | MUST NOT | This value indicates the highest value that the Server will accept as a Topic Alias sent by the Client. The Server uses this value to limit the number of Topic Aliases that it is willing to hold on this Connection. The Client MUST NOT send a Topic Alias in a PUBLISH packet to... | [_Toc464547849](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464547849) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.3.9 Reason String

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-19 | MUST NOT | The Server uses this value to give additional information to the Client. The Server MUST NOT send this property if it would increase the size of the CONNACK packet beyond the Maximum Packet Size specified by the Client . | [_Toc464547850](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464547850) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.3.10 User Property

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-20 | MUST NOT | Followed by a UTF-8 String Pair. This property can be used to provide additional information to the Client including diagnostic information. The Server MUST NOT send this property if it would increase the size of the CONNACK packet beyond the Maximum Packet Size specified by t... | [_Toc471483598](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483598) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.2.2.3.14 Server Keep Alive

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-21 | MUST | Followed by a Two Byte Integer with the Keep Alive time assigned by the Server. If the Server sends a Server Keep Alive on the CONNACK packet, the Client MUST use this value instead of the Keep Alive value the Client sent on CONNECT . | [_Toc471483602](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483602) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |
| MQTT-3.2.2-22 | MUST | Followed by a Two Byte Integer with the Keep Alive time assigned by the Server. If the Server sends a Server Keep Alive on the CONNACK packet, the Client MUST use this value instead of the Keep Alive value the Client sent on CONNECT . | [_Toc471483602](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483602) | rumqttc-v5/src/mqttbytes/v5/connack.rs | unreviewed |

### 3.3.1.1 DUP

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.1-1 | MUST | The DUP flag MUST be set to 1 by the Client or Server when it attempts to re-deliver a PUBLISH packet . The DUP flag MUST be set to 0 for all QoS 0 messages . | [_Toc385349262](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349262) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.1-2 | MUST | The DUP flag MUST be set to 1 by the Client or Server when it attempts to re-deliver a PUBLISH packet . The DUP flag MUST be set to 0 for all QoS 0 messages . | [_Toc385349262](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349262) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.1-3 | MUST | The value of the DUP flag from an incoming PUBLISH packet is not propagated when the PUBLISH packet is sent to subscribers by the Server. The DUP flag in the outgoing PUBLISH packet is set independently to the incoming PUBLISH packet, its value MUST be determined solely by whe... | [_Toc385349262](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349262) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.1.2 QoS

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.1-4 | MUST NOT | A PUBLISH Packet MUST NOT have both QoS bits set to 1 . If a Server or Client receives a PUBLISH packet which has both QoS bits set to 1 it is a Malformed Packet. Use DISCONNECT with Reason Code 0x81 (Malformed Packet) as described in section 4.13. | [_Table_3.11_-](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Table_3.11_-) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.1.3 RETAIN

Compliance digest: 9 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.1-5 | MUST | If the RETAIN flag is set to 1 in a PUBLISH packet sent by a Client to a Server, the Server MUST replace any existing retained message for this topic and store the Application Message , so that it can be delivered to future subscribers whose subscriptions match its Topic Name. | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349265) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.1-6 | MUST | If the RETAIN flag is set to 1 in a PUBLISH packet sent by a Client to a Server, the Server MUST replace any existing retained message for this topic and store the Application Message , so that it can be delivered to future subscribers whose subscriptions match its Topic Name. | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349265) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.1-7 | MUST | If the RETAIN flag is set to 1 in a PUBLISH packet sent by a Client to a Server, the Server MUST replace any existing retained message for this topic and store the Application Message , so that it can be delivered to future subscribers whose subscriptions match its Topic Name. | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349265) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.1-8 | MUST NOT | If the RETAIN flag is 0 in a PUBLISH packet sent by a Client to a Server, the Server MUST NOT store the message as a retained message and MUST NOT remove or replace any existing retained message . | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349265) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.1-9 | MUST | · If Retain Handling is set to 0 the Server MUST send the retained messages matching the Topic Filter of the subscription to the Client . | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349265) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.1-10 | MUST | · If Retain Handling is set to 1 then if the subscription did not already exist, the Server MUST send all retained message matching the Topic Filter of the subscription to the Client, and if the subscription did exist the Server MUST NOT send the retained messages. | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349265) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.1-11 | MUST NOT | · If Retain Handling is set to 2, the Server MUST NOT send the retained messages . | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349265) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.1-12 | MUST | · If the value of Retain As Published subscription option is set to 0, the Server MUST set the RETAIN flag to 0 when forwarding an Application Message regardless of how the RETAIN flag was set in the received PUBLISH packet . | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349265) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.1-13 | MUST | · If the value of Retain As Published subscription option is set to 1, the Server MUST set the RETAIN flag equal to the RETAIN flag in the received PUBLISH packet . | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349265) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.2.1 Topic Name

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.2-1 | MUST | The Topic Name MUST be present as the first field in the PUBLISH packet Variable Header. It MUST be a UTF-8 Encoded String as defined in section 1.5.4 . | [_Toc385349267](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349267) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-2 | MUST NOT | The Topic Name in the PUBLISH packet MUST NOT contain wildcard characters . | [_Toc473620093](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620093) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-3 | MUST | The Topic Name in a PUBLISH packet sent by a Server to a subscribing Client MUST match the Subscription’s Topic Filter according to the matching process defined in section 4.7 . However, as the Server is permitted to map the Topic Name to another name, it might not be the same... | [_Toc473620093](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620093) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.2.3.2 Payload Format Indicator

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.2-4 | MUST | A Server MUST send the Payload Format Indicator unaltered to all subscribers receiving the Application Message . The receiver MAY validate that the Payload is of the format indicated, and if it is not send a PUBACK, PUBREC, or DISCONNECT with Reason Code of 0x99 (Payload forma... | [_Toc471483618](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483618) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.2.3.3 Message Expiry Interval`

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.2-5 | MUST | If present, the Four Byte value is the lifetime of the Application Message in seconds. If the Message Expiry Interval has passed and the Server has not managed to start onward delivery to a matching subscriber, then it MUST delete the copy of the message for that subscriber . | [_Toc471483619](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483619) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-6 | MUST | The PUBLISH packet sent to a Client by the Server MUST contain a Message Expiry Interval set to the received value minus the time that the Application Message has been waiting in the Server . Refer to section 4.1 for details and limitations of stored state. | [_Toc473620099](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620099) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.2.3.4 Topic Alias

Compliance digest: 6 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.2-7 | MUST NOT | Topic Alias mappings exist only within a Network Connection and last only for the lifetime of that Network Connection. A receiver MUST NOT carry forward any Topic Alias mappings from one Network Connection to another . | [_Topic_Alias](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Topic_Alias) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-8 | MUST NOT | A Topic Alias of 0 is not permitted. A sender MUST NOT send a PUBLISH packet containing a Topic Alias which has the value 0 . | [_Topic_Alias](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Topic_Alias) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-9 | MUST NOT | A Client MUST NOT send a PUBLISH packet with a Topic Alias greater than the Topic Alias Maximum value returned by the Server in the CONNACK packet . A Client MUST accept all Topic Alias values greater than 0 and less than or equal to the Topic Alias Maximum value that it sent... | [_Topic_Alias](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Topic_Alias) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-10 | MUST NOT | A Client MUST NOT send a PUBLISH packet with a Topic Alias greater than the Topic Alias Maximum value returned by the Server in the CONNACK packet . A Client MUST accept all Topic Alias values greater than 0 and less than or equal to the Topic Alias Maximum value that it sent... | [_Topic_Alias](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Topic_Alias) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-11 | MUST NOT | A Server MUST NOT send a PUBLISH packet with a Topic Alias greater than the Topic Alias Maximum value sent by the Client in the CONNECT packet . A Server MUST accept all Topic Alias values greater than 0 and less than or equal to the Topic Alias Maximum value that it returned... | [_Topic_Alias](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Topic_Alias) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-12 | MUST NOT | A Server MUST NOT send a PUBLISH packet with a Topic Alias greater than the Topic Alias Maximum value sent by the Client in the CONNECT packet . A Server MUST accept all Topic Alias values greater than 0 and less than or equal to the Topic Alias Maximum value that it returned... | [_Topic_Alias](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Topic_Alias) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.2.3.5 Response Topic

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.2-13 | MUST | Followed by a UTF-8 Encoded String which is used as the Topic Name for a response message. The Response Topic MUST be a UTF-8 Encoded String as defined in section 1.5.4 . The Response Topic MUST NOT contain wildcard characters . | [_Response_Topic](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Response_Topic) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-14 | MUST | Followed by a UTF-8 Encoded String which is used as the Topic Name for a response message. The Response Topic MUST be a UTF-8 Encoded String as defined in section 1.5.4 . The Response Topic MUST NOT contain wildcard characters . | [_Response_Topic](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Response_Topic) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-15 | MUST | The Server MUST send the Response Topic unaltered to all subscribers receiving the Application Message . | [_Toc473620102](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620102) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.2.3.6 Correlation Data

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.2-16 | MUST | The Server MUST send the Correlation Data unaltered to all subscribers receiving the Application Message . The value of the Correlation Data only has meaning to the sender of the Request Message and receiver of the Response Message. | [_Toc473620105](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620105) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.2.3.7 User Property

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.2-17 | MUST | The Server MUST send all User Properties unaltered in a PUBLISH packet when forwarding the Application Message to a Client . The Server MUST maintain the order of User Properties when forwarding the Application Message . | [_Toc464547991](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464547991) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-18 | MUST | The Server MUST send all User Properties unaltered in a PUBLISH packet when forwarding the Application Message to a Client . The Server MUST maintain the order of User Properties when forwarding the Application Message . | [_Toc464547991](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464547991) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.2.3.9 Content Type

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.2-19 | MUST | Followed by a UTF-8 Encoded String describing the content of the Application Message. The Content Type MUST be a UTF-8 Encoded String as defined in section 1.5.4 . | [_Toc471282787](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282787) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.2-20 | MUST | A Server MUST send the Content Type unaltered to all subscribers receiving the Application Message . | [_Toc473620114](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620114) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.3.4 PUBLISH Actions

Compliance digest: 10 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.4-1 | MUST | The receiver of a PUBLISH Packet MUST respond with the packet as determined by the QoS in the PUBLISH Packet . | [_PUBLISH_Actions](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_PUBLISH_Actions) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.4-2 | MUST | When Clients make subscriptions with Topic Filters that include wildcards, it is possible for a Client’s subscriptions to overlap so that a published message might match multiple filters. In this case the Server MUST deliver the message to the Client respecting the maximum QoS... | [_PUBLISH_Actions](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_PUBLISH_Actions) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.4-3 | MUST | If the Client specified a Subscription Identifier for any of the overlapping subscriptions the Server MUST send those Subscription Identifiers in the message which is published as the result of the subscriptions . If the Server sends a single copy of the message it MUST includ... | [_PUBLISH_Actions](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_PUBLISH_Actions) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.4-4 | MUST | If the Client specified a Subscription Identifier for any of the overlapping subscriptions the Server MUST send those Subscription Identifiers in the message which is published as the result of the subscriptions . If the Server sends a single copy of the message it MUST includ... | [_PUBLISH_Actions](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_PUBLISH_Actions) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.4-5 | MUST | If the Client specified a Subscription Identifier for any of the overlapping subscriptions the Server MUST send those Subscription Identifiers in the message which is published as the result of the subscriptions . If the Server sends a single copy of the message it MUST includ... | [_PUBLISH_Actions](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_PUBLISH_Actions) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.4-6 | MUST NOT | It is a Protocol Error for a PUBLISH packet to contain any Subscription Identifier other than those received in SUBSCRIBE packet which caused it to flow. A PUBLISH packet sent from a Client to a Server MUST NOT contain a Subscription Identifier . | [_PUBLISH_Actions](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_PUBLISH_Actions) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.4-7 | MUST NOT | The Client MUST NOT send more than Receive Maximum QoS 1 and QoS 2 PUBLISH packets for which it has not received PUBACK, PUBCOMP, or PUBREC with a Reason Code of 128 or greater from the Server . If it receives more than Receive Maximum QoS 1 and QoS 2 PUBLISH packets where it... | [_Toc462729133](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc462729133) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.4-8 | MUST NOT | The Client MUST NOT delay the sending of any packets other than PUBLISH packets due to having sent Receive Maximum PUBLISH packets without receiving acknowledgements for them . The value of Receive Maximum applies only to the current Network Connection. | [_Toc462729133](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc462729133) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.4-9 | MUST NOT | The Server MUST NOT send more than Receive Maximum QoS 1 and QoS 2 PUBLISH packets for which it has not received PUBACK, PUBCOMP, or PUBREC with a Reason Code of 128 or greater from the Client . If it receives more than Receive Maximum QoS 1 and QoS 2 PUBLISH packets where it... | [_Toc462729133](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc462729133) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |
| MQTT-3.3.4-10 | MUST NOT | The Server MUST NOT delay the sending of any packets other than PUBLISH packets due to having sent Receive Maximum PUBLISH packets without receiving acknowledgements for them . | [_Toc462729133](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc462729133) | rumqttc-v5/src/mqttbytes/v5/publish.rs | unreviewed |

### 3.4.2.1 PUBACK Reason Code

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.4.2-1 | MUST | The Client or Server sending the PUBACK packet MUST use one of the PUBACK Reason Codes . The Reason Code and Property Length can be omitted if the Reason Code is 0x00 (Success) and there are no Properties. In this case the PUBACK has a Remaining Length of 2. | [_Toc384800419](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800419) | rumqttc-v5/src/mqttbytes/v5/puback.rs | unreviewed |

### 3.4.2.2.2 Reason String

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.4.2-2 | MUST NOT | The sender uses this value to give additional information to the receiver. The sender MUST NOT send this property if it would increase the size of the PUBACK packet beyond the Maximum Packet Size specified by the receiver . | [_Toc464548000](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464548000) | rumqttc-v5/src/mqttbytes/v5/puback.rs | unreviewed |

### 3.4.2.2.3 User Property

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.4.2-3 | MUST NOT | Followed by UTF-8 String Pair. This property can be used to provide additional diagnostic or other information. The sender MUST NOT send this property if it would increase the size of the PUBACK packet beyond the Maximum Packet Size specified by the receiver . | [_Toc471483635](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483635) | rumqttc-v5/src/mqttbytes/v5/puback.rs | unreviewed |

### 3.5.2.1 PUBREC Reason Code

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.5.2-1 | MUST | The Client or Server sending the PUBREC packet MUST use one of the PUBREC Reason Code values. . The Reason Code and Property Length can be omitted if the Reason Code is 0x00 (Success) and there are no Properties. In this case the PUBREC has a Remaining Length of 2. | [_Ref459302849](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Ref459302849) | rumqttc-v5/src/mqttbytes/v5/pubrec.rs | unreviewed |

### 3.5.2.2.2 Reason String

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.5.2-2 | MUST NOT | The sender uses this value to give additional information to the receiver. The sender MUST NOT send this property if it would increase the size of the PUBREC packet beyond the Maximum Packet Size specified by the receiver . | [_Toc464548009](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464548009) | rumqttc-v5/src/mqttbytes/v5/pubrec.rs | unreviewed |

### 3.5.2.2.3 User Property

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.5.2-3 | MUST NOT | Followed by UTF-8 String Pair. This property can be used to provide additional diagnostic or other information. The sender MUST NOT send this property if it would increase the size of the PUBREC packet beyond the Maximum Packet Size specified by the receiver . | [_Toc471483644](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483644) | rumqttc-v5/src/mqttbytes/v5/pubrec.rs | unreviewed |

### 3.6.1 PUBREL Fixed Header

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.6.1-1 | MUST | Bits 3,2,1 and 0 of the Fixed Header in the PUBREL packet are reserved and MUST be set to 0,0,1 and 0 respectively. The Server MUST treat any other value as malformed and close the Network Connection . | [_Figure_3.16_–](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.16_–) | rumqttc-v5/src/mqttbytes/v5/pubrel.rs | unreviewed |

### 3.6.2.1 PUBREL Reason Code

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.6.2-1 | MUST | The Client or Server sending the PUBREL packet MUST use one of the PUBREL Reason Code values . The Reason Code and Property Length can be omitted if the Reason Code is 0x00 (Success) and there are no Properties. In this case the PUBREL has a Remaining Length of 2. | [_Ref459708654](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Ref459708654) | rumqttc-v5/src/mqttbytes/v5/pubrel.rs | unreviewed |

### 3.6.2.2.2 Reason String

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.6.2-2 | MUST NOT | The sender uses this value to give additional information to the receiver. The sender MUST NOT send this Property if it would increase the size of the PUBREL packet beyond the Maximum Packet Size specified by the receiver . | [_Toc464548017](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464548017) | rumqttc-v5/src/mqttbytes/v5/pubrel.rs | unreviewed |

### 3.6.2.2.3 User Property

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.6.2-3 | MUST NOT | Followed by UTF-8 String Pair. This property can be used to provide additional diagnostic or other information for the PUBREL. The sender MUST NOT send this property if it would increase the size of the PUBREL packet beyond the Maximum Packet Size specified by the receiver . | [_Toc471483653](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483653) | rumqttc-v5/src/mqttbytes/v5/pubrel.rs | unreviewed |

### 3.7.2.1 PUBCOMP Reason Code

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.7.2-1 | MUST | The Client or Server sending the PUBCOMP packet MUST use one of the PUBCOMP Reason Code values . The Reason Code and Property Length can be omitted if the Reason Code is 0x00 (Success) and there are no Properties. In this case the PUBCOMP has a Remaining Length of 2. | [_Ref459303322](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Ref459303322) | rumqttc-v5/src/mqttbytes/v5/pubcomp.rs | unreviewed |

### 3.7.2.2.2 Reason String

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.7.2-2 | MUST NOT | The sender uses this value to give additional information to the receiver. The sender MUST NOT send this Property if it would increase the size of the PUBCOMP packet beyond the Maximum Packet Size specified by the receiver . | [_Toc464548025](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464548025) | rumqttc-v5/src/mqttbytes/v5/pubcomp.rs | unreviewed |

### 3.7.2.2.3 User Property

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.7.2-3 | MUST NOT | Followed by UTF-8 String Pair. This property can be used to provide additional diagnostic or other information. The sender MUST NOT send this property if it would increase the size of the PUBCOMP packet beyond the Maximum Packet Size specified by the receiver . | [_Toc471483662](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483662) | rumqttc-v5/src/mqttbytes/v5/pubcomp.rs | unreviewed |

### 3.8.1 SUBSCRIBE Fixed Header

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.8.1-1 | MUST | Bits 3,2,1 and 0 of the Fixed Header of the SUBSCRIBE packet are reserved and MUST be set to 0,0,1 and 0 respectively. The Server MUST treat any other value as malformed and close the Network Connection . | [_Figure_3.20_–](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.20_–) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |

### 3.8.3 SUBSCRIBE Payload

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.8.3-1 | MUST | The Payload of a SUBSCRIBE packet contains a list of Topic Filters indicating the Topics to which the Client wants to subscribe. The Topic Filters MUST be a UTF-8 Encoded String . Each Topic Filter is followed by a Subscription Options byte. | [_Toc471282828](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282828) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |
| MQTT-3.8.3-2 | MUST | The Payload MUST contain at least one Topic Filter and Subscription Options pair . A SUBSCRIBE packet with no Payload is a Protocol Error. Refer to section 4.13 for information about handling errors. | [_Toc471282828](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282828) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |

### 3.8.3.1 Subscription Options

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.8.3-3 | MUST NOT | Bit 2 of the Subscription Options represents the No Local option. If the value is 1, Application Messages MUST NOT be forwarded to a connection with a ClientID equal to the ClientID of the publishing connection . It is a Protocol Error to set the No Local bit to 1 on a Shared... | [_Subscription_Options](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Subscription_Options) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |
| MQTT-3.8.3-4 | MUST NOT | Bit 2 of the Subscription Options represents the No Local option. If the value is 1, Application Messages MUST NOT be forwarded to a connection with a ClientID equal to the ClientID of the publishing connection . It is a Protocol Error to set the No Local bit to 1 on a Shared... | [_Subscription_Options](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Subscription_Options) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |
| MQTT-3.8.3-5 | MUST | Bits 6 and 7 of the Subscription Options byte are reserved for future use. The Server MUST treat a SUBSCRIBE packet as malformed if any of Reserved bits in the Payload are non-zero . | [_Subscription_Options](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Subscription_Options) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |

### 3.8.4 SUBSCRIBE Actions

Compliance digest: 8 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.8.4-1 | MUST | When the Server receives a SUBSCRIBE packet from a Client, the Server MUST respond with a SUBACK packet . The SUBACK packet MUST have the same Packet Identifier as the SUBSCRIBE packet that it is acknowledging . | [_Toc384800440](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800440) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |
| MQTT-3.8.4-2 | MUST | When the Server receives a SUBSCRIBE packet from a Client, the Server MUST respond with a SUBACK packet . The SUBACK packet MUST have the same Packet Identifier as the SUBSCRIBE packet that it is acknowledging . | [_Toc384800440](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800440) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |
| MQTT-3.8.4-3 | MUST | If a Server receives a SUBSCRIBE packet containing a Topic Filter that is identical to a Non‑shared Subscription’s Topic Filter for the current Session, then it MUST replace that existing Subscription with a new Subscription . | [_Toc384800440](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800440) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |
| MQTT-3.8.4-4 | MUST | If a Server receives a SUBSCRIBE packet containing a Topic Filter that is identical to a Non‑shared Subscription’s Topic Filter for the current Session, then it MUST replace that existing Subscription with a new Subscription . | [_Toc384800440](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800440) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |
| MQTT-3.8.4-5 | MUST | If a Server receives a SUBSCRIBE packet that contains multiple Topic Filters it MUST handle that packet as if it had received a sequence of multiple SUBSCRIBE packets, except that it combines their responses into a single SUBACK response . | [_Toc473620190](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620190) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |
| MQTT-3.8.4-6 | MUST | The SUBACK packet sent by the Server to the Client MUST contain a Reason Code for each Topic Filter/Subscription Option pair . This Reason Code MUST either show the maximum QoS that was granted for that Subscription or indicate that the subscription failed . | [_Toc473620190](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620190) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |
| MQTT-3.8.4-7 | MUST | The SUBACK packet sent by the Server to the Client MUST contain a Reason Code for each Topic Filter/Subscription Option pair . This Reason Code MUST either show the maximum QoS that was granted for that Subscription or indicate that the subscription failed . | [_Toc473620190](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620190) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |
| MQTT-3.8.4-8 | MUST | The SUBACK packet sent by the Server to the Client MUST contain a Reason Code for each Topic Filter/Subscription Option pair . This Reason Code MUST either show the maximum QoS that was granted for that Subscription or indicate that the subscription failed . | [_Toc473620190](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620190) | rumqttc-v5/src/mqttbytes/v5/subscribe.rs | unreviewed |

### 3.9.2.1.2 Reason String

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.9.2-1 | MUST NOT | The Server uses this value to give additional information to the Client. The Server MUST NOT send this Property if it would increase the size of the SUBACK packet beyond the Maximum Packet Size specified by the Client . It is a Protocol Error to include the Reason String more... | [_Toc464548040](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464548040) | rumqttc-v5/src/mqttbytes/v5/suback.rs | unreviewed |

### 3.9.2.1.3 User Property

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.9.2-2 | MUST NOT | Followed by UTF-8 String Pair. This property can be used to provide additional diagnostic or other information. The Server MUST NOT send this property if it would increase the size of the SUBACK packet beyond the Maximum Packet Size specified by Client . | [_Toc471483679](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483679) | rumqttc-v5/src/mqttbytes/v5/suback.rs | unreviewed |

### 3.9.3 SUBACK Payload

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.9.3-1 | MUST | The Payload contains a list of Reason Codes. Each Reason Code corresponds to a Topic Filter in the SUBSCRIBE packet being acknowledged. The order of Reason Codes in the SUBACK packet MUST match the order of Topic Filters in the SUBSCRIBE packet . | [_Toc471483680](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483680) | rumqttc-v5/src/mqttbytes/v5/suback.rs | unreviewed |
| MQTT-3.9.3-2 | MUST | The Server sending a SUBACK packet MUST use one of the Subscribe Reason Codes for each Topic Filter received . | [_Toc473620203](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620203) | rumqttc-v5/src/mqttbytes/v5/suback.rs | unreviewed |

### 3.10.1 UNSUBSCRIBE Fixed Header

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.10.1-1 | MUST | Bits 3,2,1 and 0 of the Fixed Header of the UNSUBSCRIBE packet are reserved and MUST be set to 0,0,1 and 0 respectively. The Server MUST treat any other value as malformed and close the Network Connection . | [_Figure_3.28_–](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.28_–) | rumqttc-v5/src/mqttbytes/v5/unsubscribe.rs | unreviewed |

### 3.10.3 UNSUBSCRIBE Payload

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.10.3-1 | MUST | The Payload for the UNSUBSCRIBE packet contains the list of Topic Filters that the Client wishes to unsubscribe from. The Topic Filters in an UNSUBSCRIBE packet MUST be UTF-8 Encoded Strings as defined in section 1.5.4, packed contiguously. | [_Toc384800448](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800448) | rumqttc-v5/src/mqttbytes/v5/unsubscribe.rs | unreviewed |
| MQTT-3.10.3-2 | MUST | The Payload of an UNSUBSCRIBE packet MUST contain at least one Topic Filter . An UNSUBSCRIBE packet with no Payload is a Protocol Error. Refer to section 4.13 for information about handling errors. | [_Toc384800448](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800448) | rumqttc-v5/src/mqttbytes/v5/unsubscribe.rs | unreviewed |

### 3.10.4 UNSUBSCRIBE Actions

Compliance digest: 6 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.10.4-1 | MUST | The Topic Filters (whether they contain wildcards or not) supplied in an UNSUBSCRIBE packet MUST be compared character-by-character with the current set of Topic Filters held by the Server for the Client. If any filter matches exactly then its owning Subscription MUST be delet... | [_Toc473620212](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620212) | rumqttc-v5/src/mqttbytes/v5/unsubscribe.rs | unreviewed |
| MQTT-3.10.4-2 | MUST | It MUST stop adding any new messages which match the Topic Filters, for delivery to the Client . | [_Toc473620212](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620212) | rumqttc-v5/src/mqttbytes/v5/unsubscribe.rs | unreviewed |
| MQTT-3.10.4-3 | MUST | It MUST complete the delivery of any QoS 1 or QoS 2 messages which match the Topic Filters and it has started to send to the Client . | [_Toc473620212](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620212) | rumqttc-v5/src/mqttbytes/v5/unsubscribe.rs | unreviewed |
| MQTT-3.10.4-4 | MUST | The Server MUST respond to an UNSUBSCRIBE request by sending an UNSUBACK packet . The UNSUBACK packet MUST have the same Packet Identifier as the UNSUBSCRIBE packet. Even where no Topic Subscriptions are deleted, the Server MUST respond with an UNSUBACK . | [_Toc473620212](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620212) | rumqttc-v5/src/mqttbytes/v5/unsubscribe.rs | unreviewed |
| MQTT-3.10.4-5 | MUST | The Server MUST respond to an UNSUBSCRIBE request by sending an UNSUBACK packet . The UNSUBACK packet MUST have the same Packet Identifier as the UNSUBSCRIBE packet. Even where no Topic Subscriptions are deleted, the Server MUST respond with an UNSUBACK . | [_Toc473620212](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620212) | rumqttc-v5/src/mqttbytes/v5/unsubscribe.rs | unreviewed |
| MQTT-3.10.4-6 | MUST | If a Server receives an UNSUBSCRIBE packet that contains multiple Topic Filters, it MUST process that packet as if it had received a sequence of multiple UNSUBSCRIBE packets, except that it sends just one UNSUBACK response . | [_Toc473620212](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620212) | rumqttc-v5/src/mqttbytes/v5/unsubscribe.rs | unreviewed |

### 3.11.2.1.2 Reason String

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.11.2-1 | MUST NOT | The Server uses this value to give additional information to the Client. The Server MUST NOT send this Property if it would increase the size of the UNSUBACK packet beyond the Maximum Packet Size specified by the Client . | [_Toc471282848](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471282848) | rumqttc-v5/src/mqttbytes/v5/unsuback.rs | unreviewed |

### 3.11.2.1.3 User Property

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.11.2-2 | MUST NOT | Followed by UTF-8 String Pair. This property can be used to provide additional diagnostic or other information. The Server MUST NOT send this property if it would increase the size of the UNSUBACK packet beyond the Maximum Packet Size specified by the Client . | [_Toc471483692](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483692) | rumqttc-v5/src/mqttbytes/v5/unsuback.rs | unreviewed |

### 3.11.3 UNSUBACK Payload

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.11.3-1 | MUST | The Payload contains a list of Reason Codes. Each Reason Code corresponds to a Topic Filter in the UNSUBSCRIBE packet being acknowledged. The order of Reason Codes in the UNSUBACK packet MUST match the order of Topic Filters in the UNSUBSCRIBE packet . | [_Toc471483693](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483693) | rumqttc-v5/src/mqttbytes/v5/unsuback.rs | unreviewed |
| MQTT-3.11.3-2 | MUST | The values for the one byte unsigned Unsubscribe Reason Codes are shown below. The Server sending an UNSUBACK packet MUST use one of the Unsubscribe Reason Code values for each Topic Filter received . | [_Toc471483693](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483693) | rumqttc-v5/src/mqttbytes/v5/unsuback.rs | unreviewed |

### 3.12.4 PINGREQ Actions

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.12.4-1 | MUST | The Server MUST send a PINGRESP packet in response to a PINGREQ packet . | [_Toc384800458](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800458) | rumqttc-v5/src/mqttbytes/v5/ping.rs | unreviewed |

### 3.14 DISCONNECT – Disconnect notification

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.14.0-1 | MUST NOT | A Server MUST NOT send a DISCONNECT until after it has sent a CONNACK with Reason Code of less than 0x80 . | [_Toc384800463](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800463) | rumqttc-v5/src/mqttbytes/v5/disconnect.rs | unreviewed |

### 3.14.1 DISCONNECT Fixed Header

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.14.1-1 | MUST | The Client or Server MUST validate that reserved bits are set to 0. If they are not zero it sends a DISCONNECT packet with a Reason code of 0x81 (Malformed Packet) as described in section 4.13 . | [_Figure_3.35_–](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Figure_3.35_–) | rumqttc-v5/src/mqttbytes/v5/disconnect.rs | unreviewed |

### 3.14.2.1 Disconnect Reason Code

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.14.2-1 | MUST | The Client or Server sending the DISCONNECT packet MUST use one of the DISCONNECT Reason Code values . The Reason Code and Property Length can be omitted if the Reason Code is 0x00 (Normal disconnecton) and there are no Properties. | [DisconnectReasonCode](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#DisconnectReasonCode) | rumqttc-v5/src/mqttbytes/v5/disconnect.rs | unreviewed |

### 3.14.2.2.2 Session Expiry Interval

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.14.2-2 | MUST NOT | The Session Expiry Interval MUST NOT be sent on a DISCONNECT by the Server . | [_Toc462729193](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc462729193) | rumqttc-v5/src/mqttbytes/v5/disconnect.rs | unreviewed |

### 3.14.2.2.3 Reason String

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.14.2-3 | MUST NOT | The sender MUST NOT send this Property if it would increase the size of the DISCONNECT packet beyond the Maximum Packet Size specified by the receiver . It is a Protocol Error to include the Reason String more than once. | [_Toc464548071](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc464548071) | rumqttc-v5/src/mqttbytes/v5/disconnect.rs | unreviewed |

### 3.14.2.2.4 User Property

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.14.2-4 | MUST NOT | Followed by UTF-8 String Pair. This property may be used to provide additional diagnostic or other information. The sender MUST NOT send this property if it would increase the size of the DISCONNECT packet beyond the Maximum Packet Size specified by the receiver . | [_Toc471483711](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483711) | rumqttc-v5/src/mqttbytes/v5/disconnect.rs | unreviewed |

### 3.14.4 DISCONNECT Actions

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.14.4-1 | MUST NOT | MUST NOT send any more MQTT Control Packets on that Network Connection . | [_Toc473906327](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473906327) | rumqttc-v5/src/mqttbytes/v5/disconnect.rs | unreviewed |
| MQTT-3.14.4-2 | MUST | MUST close the Network Connection . | [_Toc473906327](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473906327) | rumqttc-v5/src/mqttbytes/v5/disconnect.rs | unreviewed |
| MQTT-3.14.4-3 | MUST | MUST discard any Will Message associated with the current Connection without publishing it , as described in section 3.1.2.5. | [_Toc473906327](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473906327) | rumqttc-v5/src/mqttbytes/v5/disconnect.rs | unreviewed |

### 3.15.1 AUTH Fixed Header

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.15.1-1 | MUST | Bits 3,2,1 and 0 of the Fixed Header of the AUTH packet are reserved and MUST all be set to 0. The Client or Server MUST treat any other value as malformed and close the Network Connection . | [_Toc473620250](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620250) | rumqttc-v5/src/mqttbytes/v5/auth.rs | unreviewed |

### 3.15.2.1 Authenticate Reason Code

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.15.2-1 | MUST | Byte 0 in the Variable Header is the Authenticate Reason Code. The values for the one byte unsigned Authenticate Reason Code field are shown below. The sender of the AUTH Packet MUST use one of the Authenticate Reason Codes . | [_Toc511988697](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc511988697) | rumqttc-v5/src/mqttbytes/v5/auth.rs | unreviewed |

### 3.15.2.2.4 Reason String

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.15.2-2 | MUST NOT | The sender MUST NOT send this property if it would increase the size of the AUTH packet beyond the Maximum Packet Size specified by the receiver . It is a Protocol Error to include the Reason String more than once. | [_Toc511988702](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc511988702) | rumqttc-v5/src/mqttbytes/v5/auth.rs | unreviewed |

### 3.15.2.2.5 User Property

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.15.2-3 | MUST NOT | Followed by UTF-8 String Pair. This property may be used to provide additional diagnostic or other information. The sender MUST NOT send this property if it would increase the size of the AUTH packet beyond the Maximum Packet Size specified by the receiver . | [_Toc471483722](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc471483722) | rumqttc-v5/src/mqttbytes/v5/auth.rs | unreviewed |

### 4.1.1 Storing Session State

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.1.0-1 | MUST NOT | The Client and Server MUST NOT discard the Session State while the Network Connection is open . The Server MUST discard the Session State when the Network Connection is closed and the Session Expiry Interval has passed . | [_Storing_Session_State](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Storing_Session_State) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.1.0-2 | MUST NOT | The Client and Server MUST NOT discard the Session State while the Network Connection is open . The Server MUST discard the Session State when the Network Connection is closed and the Session Expiry Interval has passed . | [_Storing_Session_State](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Storing_Session_State) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.2 Network Connections

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.2-1 | MUST | A Client or Server MUST support the use of one or more underlying transport protocols that provide an ordered, lossless, stream of bytes from the Client to Server and Server to Client . | [_Network_Connections](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Network_Connections) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.3.1 QoS 0: At most once delivery

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.3.1-1 | MUST | · MUST send a PUBLISH packet with QoS 0 and DUP flag set to 0 . | [_Toc473620274](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620274) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.3.2 QoS 1: At least once delivery

Compliance digest: 5 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.3.2-1 | MUST | · MUST assign an unused Packet Identifier each time it has a new Application Message to publish . | [_Toc473620277](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620277) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.2-2 | MUST | · MUST send a PUBLISH packet containing this Packet Identifier with QoS 1 and DUP flag set to 0 . | [_Toc473620277](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620277) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.2-3 | MUST | · MUST treat the PUBLISH packet as “unacknowledged” until it has received the corresponding PUBACK packet from the receiver. Refer to section 4.4 for a discussion of unacknowledged messages . | [_Toc473620277](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620277) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.2-4 | MUST | MUST respond with a PUBACK packet containing the Packet Identifier from the incoming PUBLISH packet, having accepted ownership of the Application Message . | [_Toc473620279](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620279) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.2-5 | MUST | After it has sent a PUBACK packet the receiver MUST treat any incoming PUBLISH packet that contains the same Packet Identifier as being a new Application Message, irrespective of the setting of its DUP flag . | [_Toc473620279](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620279) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.3.3 QoS 2: Exactly once delivery

Compliance digest: 13 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.3.3-1 | MUST | MUST assign an unused Packet Identifier when it has a new Application Message to publish . | [_Toc473620281](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620281) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-2 | MUST | MUST send a PUBLISH packet containing this Packet Identifier with QoS 2 and DUP flag set to 0 . | [_Toc473620281](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620281) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-3 | MUST | MUST treat the PUBLISH packet as “unacknowledged” until it has received the corresponding PUBREC packet from the receiver . Refer to section 4.4 for a discussion of unacknowledged messages. | [_Toc473620281](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620281) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-4 | MUST | MUST send a PUBREL packet when it receives a PUBREC packet from the receiver with a Reason Code value less than 0x80. This PUBREL packet MUST contain the same Packet Identifier as the original PUBLISH packet . | [_Toc473620281](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620281) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-5 | MUST | MUST treat the PUBREL packet as “unacknowledged” until it has received the corresponding PUBCOMP packet from the receiver . | [_Toc473620281](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620281) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-6 | MUST NOT | MUST NOT re-send the PUBLISH once it has sent the corresponding PUBREL packet . | [_Toc473620281](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620281) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-7 | MUST NOT | MUST NOT apply Message expiry if a PUBLISH packet has been sent . | [_Toc473620281](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620281) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-8 | MUST | MUST respond with a PUBREC containing the Packet Identifier from the incoming PUBLISH packet, having accepted ownership of the Application Message . | [_Toc473620282](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620282) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-9 | MUST | If it has sent a PUBREC with a Reason Code of 0x80 or greater, the receiver MUST treat any subsequent PUBLISH packet that contains that Packet Identifier as being a new Application Message . | [_Toc473620282](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620282) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-10 | MUST | Until it has received the corresponding PUBREL packet, the receiver MUST acknowledge any subsequent PUBLISH packet with the same Packet Identifier by sending a PUBREC. It MUST NOT cause duplicate messages to be delivered to any onward recipients in this case . | [_Toc473620282](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620282) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-11 | MUST | MUST respond to a PUBREL packet by sending a PUBCOMP packet containing the same Packet Identifier as the PUBREL . | [_Toc473620282](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620282) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-12 | MUST | After it has sent a PUBCOMP, the receiver MUST treat any subsequent PUBLISH packet that contains that Packet Identifier as being a new Application Message . | [_Toc473620282](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620282) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-13 | MUST | MUST continue the QoS 2 acknowledgement sequence even if it has applied message expiry . | [_Toc473620282](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620282) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.4 Message delivery retry

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.4.0-1 | MUST | When a Client reconnects with Clean Start set to 0 and a session is present, both the Client and Server MUST resend any unacknowledged PUBLISH packets (where QoS > 0) and PUBREL packets using their original Packet Identifiers. | [_Message_delivery_retry](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Message_delivery_retry) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.4.0-2 | MUST NOT | If PUBACK or PUBREC is received containing a Reason Code of 0x80 or greater the corresponding PUBLISH packet is treated as acknowledged, and MUST NOT be retransmitted . | [_Message_delivery_retry](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Message_delivery_retry) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.5 Message receipt

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.5.0-1 | MUST | When a Server takes ownership of an incoming Application Message it MUST add it to the Session State for those Clients that have matching Subscriptions . Matching rules are defined in section 4.7. | [_Toc384800477](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800477) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.5.0-2 | MUST | Under normal circumstances Clients receive messages in response to Subscriptions they have created. A Client could also receive messages that do not match any of its explicit Subscriptions. This can happen if the Server automatically assigned a subscription to the Client. | [_Toc384800477](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800477) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.6 Message ordering

Compliance digest: 6 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.6.0-1 | MUST | When the Client re-sends any PUBLISH packets, it MUST re-send them in the order in which the original PUBLISH packets were sent (this applies to QoS 1 and QoS 2 messages) | [_Toc384800478](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800478) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.6.0-2 | MUST | The Client MUST send PUBACK packets in the order in which the corresponding PUBLISH packets were received (QoS 1 messages) | [_Toc384800478](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800478) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.6.0-3 | MUST | The Client MUST send PUBREC packets in the order in which the corresponding PUBLISH packets were received (QoS 2 messages) | [_Toc384800478](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800478) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.6.0-4 | MUST | The Client MUST send PUBREL packets in the order in which the corresponding PUBREC packets were received (QoS 2 messages) | [_Toc384800478](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800478) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.6.0-5 | MUST | An Ordered Topic is a Topic where the Client can be certain that the Application Messages in that Topic from the same Client and at the same QoS are received are in the order they were published. When a Server processes a message that has been published to an Ordered Topic, it... | [_Toc384800478](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800478) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.6.0-6 | MUST | By default, a Server MUST treat every Topic as an Ordered Topic when it is forwarding messages on Non‑shared Subscriptions. . A Server MAY provide an administrative or other mechanism to allow one or more Topics to not be treated as an Ordered Topic. | [_Toc384800478](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800478) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.7.1 Topic wildcards

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.7.0-1 | MUST NOT | The wildcard characters can be used in Topic Filters, but MUST NOT be used within a Topic Name . | [_Topic_wildcards](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Topic_wildcards) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.7.1.2 Multi-level wildcard

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.7.1-1 | MUST | The number sign (‘#’ U+0023) is a wildcard character that matches any number of levels within a topic. The multi-level wildcard represents the parent and any number of child levels. The multi-level wildcard character MUST be specified either on its own or following a topic lev... | [_Toc385349376](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349376) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.7.1.3 Single-level wildcard

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.7.1-2 | MUST | The single-level wildcard can be used at any level in the Topic Filter, including first and last levels. Where it is used, it MUST occupy an entire level of the filter . It can be used at more than one level in the Topic Filter and can be used in conjunction with the multi-lev... | [_Toc385349377](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc385349377) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.7.2 Topics beginning with $

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.7.2-1 | MUST NOT | The Server MUST NOT match Topic Filters starting with a wildcard character (# or +) with Topic Names beginning with a $ character . The Server SHOULD prevent Clients from using such Topic Names to exchange messages with other Clients. | [_Toc384800481](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800481) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.7.3 Topic semantic and usage

Compliance digest: 4 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.7.3-1 | MUST | · All Topic Names and Topic Filters MUST be at least one character long | [_Toc384800482](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800482) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.7.3-2 | MUST NOT | · Topic Names and Topic Filters MUST NOT include the null character (Unicode U+0000) | [_Toc384800482](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800482) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.7.3-3 | MUST NOT | · Topic Names and Topic Filters are UTF-8 Encoded Strings; they MUST NOT encode to more than 65,535 bytes . Refer to section 1.5.4. | [_Toc384800482](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800482) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.7.3-4 | MUST NOT | When it performs subscription matching the Server MUST NOT perform any normalization of Topic Names or Topic Filters, or any modification or substitution of unrecognized characters . Each non-wildcarded level in the Topic Filter has to match the corresponding level in the Topi... | [_Toc384800482](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc384800482) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.8.2 Shared Subscriptions

Compliance digest: 6 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.8.2-1 | MUST | A Shared Subscription's Topic Filter MUST start with $share/ and MUST contain a ShareName that is at least one character long . The ShareName MUST NOT contain the characters "/", "+" or "#", but MUST be followed by a "/" character. | [_Shared_Subscriptions](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Shared_Subscriptions) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.8.2-2 | MUST | A Shared Subscription's Topic Filter MUST start with $share/ and MUST contain a ShareName that is at least one character long . The ShareName MUST NOT contain the characters "/", "+" or "#", but MUST be followed by a "/" character. | [_Shared_Subscriptions](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Shared_Subscriptions) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.8.2-3 | MUST | · Different subscribing Clients are permitted to ask for different Requested QoS levels in their SUBSCRIBE packets. The Server decides which Maximum QoS to grant to each Client, and it is permitted to grant different Maximum QoS levels to different subscribers. | [_Toc473620306](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620306) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.8.2-4 | MUST | · If the Server is in the process of sending a QoS 2 message to its chosen subscribing Client and the connection to the Client breaks before delivery is complete, the Server MUST complete the delivery of the message to that Client when it reconnects as described in section 4.3.3. | [_Toc473620306](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620306) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.8.2-5 | MUST | · If the Server is in the process of sending a QoS 2 message to its chosen subscribing Client and the connection to the Client breaks before delivery is complete, the Server MUST complete the delivery of the message to that Client when it reconnects as described in section 4.3.3. | [_Toc473620306](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620306) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.8.2-6 | MUST | · If a Client responds with a PUBACK or PUBREC containing a Reason Code of 0x80 or greater to a PUBLISH packet from the Server, the Server MUST discard the Application Message and not attempt to send it to any other Subscriber . | [_Toc473620306](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620306) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.9 Flow Control

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.9.0-1 | MUST | The Client or Server MUST set its initial send quota to a non-zero value not exceeding the Receive Maximum . | [_Hlk484158938](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Hlk484158938) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.9.0-2 | MUST NOT | Each time the Client or Server sends a PUBLISH packet at QoS > 0, it decrements the send quota. If the send quota reaches zero, the Client or Server MUST NOT send any more PUBLISH packets with QoS > 0 . It MAY continue to send PUBLISH packets with QoS 0, or it MAY choose to su... | [_Hlk484158938](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Hlk484158938) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.9.0-3 | MUST NOT | Each time the Client or Server sends a PUBLISH packet at QoS > 0, it decrements the send quota. If the send quota reaches zero, the Client or Server MUST NOT send any more PUBLISH packets with QoS > 0 . It MAY continue to send PUBLISH packets with QoS 0, or it MAY choose to su... | [_Hlk484158938](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Hlk484158938) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.12 Enhanced authentication

Compliance digest: 7 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.12.0-1 | MAY | To begin an enhanced authentication, the Client includes an Authentication Method in the CONNECT packet. This specifies the authentication method to use. If the Server does not support the Authentication Method supplied by the Client, it MAY send a CONNACK with a Reason Code o... | [_Enhanced_authentication](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Enhanced_authentication) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.12.0-2 | MUST | If the Server requires additional information to complete the authentication, it can send an AUTH packet to the Client. This packet MUST contain a Reason Code of 0x18 (Continue authentication) . If the authentication method requires the Server to send authentication data to th... | [_Toc473620317](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620317) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.12.0-3 | MUST | The Client responds to an AUTH packet from the Server by sending a further AUTH packet. This packet MUST contain a Reason Code of 0x18 (Continue authentication) . If the authentication method requires the Client to send authentication data for the Server, it is sent in the Aut... | [_Toc473620317](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620317) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.12.0-4 | MAY | The Client can close the connection at any point in this process. It MAY send a DISCONNECT packet before doing so. The Server can reject the authentication at any point in this process. It MAY send a CONNACK with a Reason Code of 0x80 or above as described in section 4.13, and... | [_Toc473620317](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620317) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.12.0-5 | MUST | If the initial CONNECT packet included an Authentication Method property then all AUTH packets, and any successful CONNACK packet MUST include an Authentication Method Property with the same value as in the CONNECT packet . | [_Toc473620317](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620317) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.12.0-6 | MUST NOT | The implementation of enhanced authentication is OPTIONAL for both Clients and Servers. If the Client does not include an Authentication Method in the CONNECT, the Server MUST NOT send an AUTH packet, and it MUST NOT send an Authentication Method in the CONNACK packet . | [_Toc473620317](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620317) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.12.0-7 | MUST NOT | The implementation of enhanced authentication is OPTIONAL for both Clients and Servers. If the Client does not include an Authentication Method in the CONNECT, the Server MUST NOT send an AUTH packet, and it MUST NOT send an Authentication Method in the CONNACK packet . | [_Toc473620317](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc473620317) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.12.1 Re-authentication

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.12.1-1 | MUST | If the Client supplied an Authentication Method in the CONNECT packet it can initiate a re-authentication at any time after receiving a CONNACK. It does this by sending an AUTH packet with a Reason Code of 0x19 (Re-authentication). | [_Re-authentication](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Re-authentication) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.12.1-2 | SHOULD | If the re-authentication fails, the Client or Server SHOULD send DISCONNECT with an appropriate Reason Code as described in section 4.13, and MUST close the Network Connection . | [_Re-authentication](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Re-authentication) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.13.1 Malformed Packet and Protocol Errors

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.13.1-1 | MUST | When a Server detects a Malformed Packet or Protocol Error, and a Reason Code is given in the specification, it MUST close the Network Connection . In the case of an error in a CONNECT packet it MAY send a CONNACK packet containing the Reason Code, before closing the Network C... | [_Ref477790493](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Ref477790493) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.13.2 Other errors

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.13.2-1 | MUST | The CONNACK and DISCONNECT packets allow a Reason Code of 0x80 or greater to indicate that the Network Connection will be closed. If a Reason Code of 0x80 or greater is specified, then the Network Connection MUST be closed whether or not the CONNACK or DISCONNECT is sent . | [_Toc511988737](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Toc511988737) | rumqttc-v5/src/state.rs<br>rumqttc-v5/src/eventloop.rs<br>rumqttc-v5/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 6 Using WebSocket as a network transport

Compliance digest: 4 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-6.0.0-1 | MUST | · MQTT Control Packets MUST be sent in WebSocket binary data frames. If any other type of data frame is received the recipient MUST close the Network Connection . | [_Using_WebSocket_as](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Using_WebSocket_as) | rumqttc-v5/src | unreviewed |
| MQTT-6.0.0-2 | MUST NOT | · A single WebSocket data frame can contain multiple or partial MQTT Control Packets. The receiver MUST NOT assume that MQTT Control Packets are aligned on WebSocket frame boundaries . | [_Using_WebSocket_as](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Using_WebSocket_as) | rumqttc-v5/src | unreviewed |
| MQTT-6.0.0-3 | MUST | · The Client MUST include “mqtt” in the list of WebSocket Sub Protocols it offers . | [_Using_WebSocket_as](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Using_WebSocket_as) | rumqttc-v5/src | unreviewed |
| MQTT-6.0.0-4 | MUST | · The WebSocket Subprotocol name selected and returned by the Server MUST be “mqtt” . | [_Using_WebSocket_as](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#_Using_WebSocket_as) | rumqttc-v5/src | unreviewed |

### 7.1.2 MQTT Client conformance clause

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-8 | UNSPECIFIED |  | [AppendixB](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#AppendixB) | rumqttc-v5/tests<br>rumqttc-v5/src/mqttbytes/v5 | unreviewed |
| MQTT-4.2.0-1 | UNSPECIFIED |  | [AppendixB](https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html#AppendixB) | rumqttc-v5/tests<br>rumqttc-v5/src/mqttbytes/v5 | unreviewed |

