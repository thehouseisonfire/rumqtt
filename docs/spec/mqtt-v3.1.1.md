# MQTT Version 3.1.1 Compliance Digest

## Metadata

- Source: https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html
- Spec version: 3.1.1
- Source Last-Modified: Wed, 29 Oct 2014 16:00:00 GMT
- Generated (UTC): 2026-03-04T03:30:32+00:00
- Unique requirements: 139

## Section Index

- [1.5.3 UTF-8 encoded strings](#1-5-3-utf-8-encoded-strings) (3 requirements)
- [2.2.2 Flags](#2-2-2-flags) (2 requirements)
- [2.3.1 Packet Identifier](#2-3-1-packet-identifier) (7 requirements)
- [3.1 CONNECT – Client requests a connection to a Server](#3-1-connect-client-requests-a-connection-to-a-server) (2 requirements)
- [3.1.2.1 Protocol Name](#3-1-2-1-protocol-name) (1 requirements)
- [3.1.2.2 Protocol Level](#3-1-2-2-protocol-level) (1 requirements)
- [3.1.2.3 Connect Flags](#3-1-2-3-connect-flags) (1 requirements)
- [3.1.2.4 Clean Session](#3-1-2-4-clean-session) (3 requirements)
- [3.1.2.5 Will Flag](#3-1-2-5-will-flag) (5 requirements)
- [3.1.2.6 Will QoS](#3-1-2-6-will-qos) (2 requirements)
- [3.1.2.7 Will Retain](#3-1-2-7-will-retain) (3 requirements)
- [3.1.2.8 User Name Flag](#3-1-2-8-user-name-flag) (2 requirements)
- [3.1.2.9 Password Flag](#3-1-2-9-password-flag) (3 requirements)
- [3.1.2.10 Keep Alive](#3-1-2-10-keep-alive) (2 requirements)
- [3.1.3 Payload](#3-1-3-payload) (1 requirements)
- [3.1.3.1 Client Identifier](#3-1-3-1-client-identifier) (8 requirements)
- [3.1.3.2 Will Topic](#3-1-3-2-will-topic) (1 requirements)
- [3.1.3.4 User Name](#3-1-3-4-user-name) (1 requirements)
- [3.1.4 Response](#3-1-4-response) (5 requirements)
- [3.2 CONNACK – Acknowledge connection request](#3-2-connack-acknowledge-connection-request) (1 requirements)
- [3.2.2.2 Session Present](#3-2-2-2-session-present) (4 requirements)
- [3.2.2.3 Connect Return code](#3-2-2-3-connect-return-code) (2 requirements)
- [3.3.1.1 DUP](#3-3-1-1-dup) (2 requirements)
- [3.3.1.2 QoS](#3-3-1-2-qos) (1 requirements)
- [3.3.1.3 RETAIN](#3-3-1-3-retain) (8 requirements)
- [3.3.2.1 Topic Name](#3-3-2-1-topic-name) (3 requirements)
- [3.3.4 Response](#3-3-4-response) (1 requirements)
- [3.3.5 Actions](#3-3-5-actions) (2 requirements)
- [3.6.1 Fixed header](#3-6-1-fixed-header) (1 requirements)
- [3.8.1 Fixed header](#3-8-1-fixed-header) (1 requirements)
- [3.8.3 Payload](#3-8-3-payload) (3 requirements)
- [3.8.4 Response](#3-8-4-response) (6 requirements)
- [3.9.3 Payload](#3-9-3-payload) (2 requirements)
- [3.10.1 Fixed header](#3-10-1-fixed-header) (1 requirements)
- [3.10.3 Payload](#3-10-3-payload) (2 requirements)
- [3.10.4 Response](#3-10-4-response) (6 requirements)
- [3.12.4 Response](#3-12-4-response) (1 requirements)
- [3.14.1 Fixed header](#3-14-1-fixed-header) (1 requirements)
- [3.14.4 Response](#3-14-4-response) (3 requirements)
- [4.1 Storing state](#4-1-storing-state) (2 requirements)
- [4.3.1 QoS 0: At most once delivery](#4-3-1-qos-0-at-most-once-delivery) (1 requirements)
- [4.3.2 QoS 1: At least once delivery](#4-3-2-qos-1-at-least-once-delivery) (2 requirements)
- [4.3.3 QoS 2: Exactly once delivery](#4-3-3-qos-2-exactly-once-delivery) (2 requirements)
- [4.4 Message delivery retry](#4-4-message-delivery-retry) (1 requirements)
- [4.5 Message receipt](#4-5-message-receipt) (2 requirements)
- [4.6 Message ordering](#4-6-message-ordering) (6 requirements)
- [4.7.1 Topic wildcards](#4-7-1-topic-wildcards) (1 requirements)
- [4.7.1.2 Multi-level wildcard](#4-7-1-2-multi-level-wildcard) (1 requirements)
- [4.7.1.3 Single level wildcard](#4-7-1-3-single-level-wildcard) (1 requirements)
- [4.7.2 Topics beginning with $](#4-7-2-topics-beginning-with) (1 requirements)
- [4.7.3 Topic semantic and usage](#4-7-3-topic-semantic-and-usage) (4 requirements)
- [4.8 Handling errors](#4-8-handling-errors) (2 requirements)
- [6 Using WebSocket as a network transport](#6-using-websocket-as-a-network-transport) (4 requirements)
- [7 Conformance](#7-conformance) (2 requirements)
- [7.1.1 MQTT Server](#7-1-1-mqtt-server) (1 requirements)
- [7.1.2 MQTT Client](#7-1-2-mqtt-client) (2 requirements)

## Compliance Sections

### 1.5.3 UTF-8 encoded strings

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-1.5.3-1 | MUST | The character data in a UTF-8 encoded string MUST be well-formed UTF-8 as defined by the Unicode specification and restated in RFC 3629 . In particular this data MUST NOT include encodings of code points between U+D800 and U+DFFF. | [_Figure_1.1_Structure](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_1.1_Structure) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | verified |
| MQTT-1.5.3-2 | MUST | The character data in a UTF-8 encoded string MUST be well-formed UTF-8 as defined by the Unicode specification and restated in RFC 3629 . In particular this data MUST NOT include encodings of code points between U+D800 and U+DFFF. | [_Figure_1.1_Structure](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_1.1_Structure) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | verified |
| MQTT-1.5.3-3 | SHOULD NOT | The data SHOULD NOT include encodings of the Unicode code points listed below. If a receiver (Server or Client) receives a Control Packet containing any of them it MAY close the Network Connection: U+0001..U+001F control characters U+007F..U+009F control characters Code points... | [_Figure_1.1_Structure](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_1.1_Structure) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | verified |

### 2.2.2 Flags

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-2.2.2-1 | MUST | The remaining bits of byte 1 in the fixed header contain flags specific to each MQTT Control Packet type as listed in the Table 2.2 - Flag Bits below. Where a flag bit is marked as “Reserved” in Table 2.2 - Flag Bits, it is reserved for future use and MUST be set to the value... | [_Toc353481062](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc353481062) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | verified |
| MQTT-2.2.2-2 | MUST | The remaining bits of byte 1 in the fixed header contain flags specific to each MQTT Control Packet type as listed in the Table 2.2 - Flag Bits below. Where a flag bit is marked as “Reserved” in Table 2.2 - Flag Bits, it is reserved for future use and MUST be set to the value... | [_Toc353481062](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc353481062) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | verified |

### 2.3.1 Packet Identifier

Compliance digest: 7 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-2.3.1-1 | MUST | SUBSCRIBE, UNSUBSCRIBE, and PUBLISH (in cases where QoS > 0) Control Packets MUST contain a non-zero 16-bit Packet Identifier . Each time a Client sends a new packet of one of these types it MUST assign it a currently unused Packet Identifier . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_2.3_-) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | unreviewed |
| MQTT-2.3.1-2 | MUST | SUBSCRIBE, UNSUBSCRIBE, and PUBLISH (in cases where QoS > 0) Control Packets MUST contain a non-zero 16-bit Packet Identifier . Each time a Client sends a new packet of one of these types it MUST assign it a currently unused Packet Identifier . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_2.3_-) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | unreviewed |
| MQTT-2.3.1-3 | MUST | SUBSCRIBE, UNSUBSCRIBE, and PUBLISH (in cases where QoS > 0) Control Packets MUST contain a non-zero 16-bit Packet Identifier . Each time a Client sends a new packet of one of these types it MUST assign it a currently unused Packet Identifier . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_2.3_-) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | unreviewed |
| MQTT-2.3.1-4 | MUST | SUBSCRIBE, UNSUBSCRIBE, and PUBLISH (in cases where QoS > 0) Control Packets MUST contain a non-zero 16-bit Packet Identifier . Each time a Client sends a new packet of one of these types it MUST assign it a currently unused Packet Identifier . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_2.3_-) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | unreviewed |
| MQTT-2.3.1-5 | MUST NOT | A PUBLISH Packet MUST NOT contain a Packet Identifier if its QoS value is set to 0 . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_2.3_-) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | unreviewed |
| MQTT-2.3.1-6 | MUST | A PUBACK, PUBREC or PUBREL Packet MUST contain the same Packet Identifier as the PUBLISH Packet that was originally sent . Similarly SUBACK and UNSUBACK MUST contain the Packet Identifier that was used in the corresponding SUBSCRIBE and UNSUBSCRIBE Packet respectively . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_2.3_-) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | unreviewed |
| MQTT-2.3.1-7 | MUST | A PUBACK, PUBREC or PUBREL Packet MUST contain the same Packet Identifier as the PUBLISH Packet that was originally sent . Similarly SUBACK and UNSUBACK MUST contain the Packet Identifier that was used in the corresponding SUBSCRIBE and UNSUBSCRIBE Packet respectively . | [_Figure_2.3_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_2.3_-) | rumqttc-v4/src/mqttbytes/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/mod.rs<br>rumqttc-v4/src/mqttbytes/v4/codec.rs | unreviewed |

### 3.1 CONNECT – Client requests a connection to a Server

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.0-1 | MUST | After a Network Connection is established by a Client to a Server, the first Packet sent from the Client to the Server MUST be a CONNECT Packet . | [_Ref363033523](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref363033523) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.0-2 | MUST | A Client can only send the CONNECT Packet once over a Network Connection. The Server MUST process a second CONNECT Packet sent from a Client as a protocol violation and disconnect the Client . See section 4.8 for information about handling errors. | [_Ref363033523](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref363033523) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.2.1 Protocol Name

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-1 | MAY | If the protocol name is incorrect the Server MAY disconnect the Client, or it MAY continue processing the CONNECT packet in accordance with some other specification. In the latter case, the Server MUST NOT continue to process the CONNECT packet in line with this specification . | [_Figure_3.2_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_3.2_-) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.2.2 Protocol Level

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-2 | MUST | The 8 bit unsigned value that represents the revision level of the protocol used by the Client. The value of the Protocol Level field for the version 3.1.1 of the protocol is 4 (0x04). The Server MUST respond to the CONNECT Packet with a CONNACK return code 0x01 (unacceptable... | [_Figure_3.3_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_3.3_-) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.2.3 Connect Flags

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-3 | MUST | The Server MUST validate that the reserved flag in the CONNECT Control Packet is set to zero and disconnect the Client if it is not zero . | [_Figure_3.4_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_3.4_-) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.2.4 Clean Session

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-4 | MUST | If CleanSession is set to 0, the Server MUST resume communications with the Client based on state from the current Session (as identified by the Client identifier). If there is no Session associated with the Client identifier the Server MUST create a new Session. | [_Ref362965194](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref362965194) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-5 | MUST | If CleanSession is set to 0, the Server MUST resume communications with the Client based on state from the current Session (as identified by the Client identifier). If there is no Session associated with the Client identifier the Server MUST create a new Session. | [_Ref362965194](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref362965194) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-6 | MUST | If CleanSession is set to 1, the Client and Server MUST discard any previous Session and start a new one. This Session lasts as long as the Network Connection. State data associated with this Session MUST NOT be reused in any subsequent Session . | [_Ref362965194](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref362965194) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.2.5 Will Flag

Compliance digest: 5 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-8 | MUST | If the Will Flag is set to 1 this indicates that, if the Connect request is accepted, a Will Message MUST be stored on the Server and associated with the Network Connection. The Will Message MUST be published when the Network Connection is subsequently closed unless the Will M... | [_Will_Flag](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Will_Flag) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-9 | MUST | If the Will Flag is set to 1, the Will QoS and Will Retain fields in the Connect Flags will be used by the Server, and the Will Topic and Will Message fields MUST be present in the payload . | [_Will_Flag](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Will_Flag) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-10 | MUST | The Will Message MUST be removed from the stored Session state in the Server once it has been published or the Server has received a DISCONNECT packet from the Client . | [_Will_Flag](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Will_Flag) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-11 | MUST | If the Will Flag is set to 0 the Will QoS and Will Retain fields in the Connect Flags MUST be set to zero and the Will Topic and Will Message fields MUST NOT be present in the payload . | [_Will_Flag](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Will_Flag) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-12 | MUST NOT | If the Will Flag is set to 0, a Will Message MUST NOT be published when this Network Connection ends . | [_Will_Flag](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Will_Flag) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.2.6 Will QoS

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-13 | MUST | If the Will Flag is set to 0, then the Will QoS MUST be set to 0 (0x00) . | [_Toc385349233](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349233) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-14 | MUST NOT | If the Will Flag is set to 1, the value of Will QoS can be 0 (0x00), 1 (0x01), or 2 (0x02). It MUST NOT be 3 (0x03) . | [_Toc385349233](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349233) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.2.7 Will Retain

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-15 | MUST | If the Will Flag is set to 0, then the Will Retain Flag MUST be set to 0 . | [_Toc385349234](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349234) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-16 | MUST | If Will Retain is set to 0, the Server MUST publish the Will Message as a non-retained message . | [_Toc385349234](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349234) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-17 | MUST | If Will Retain is set to 1, the Server MUST publish the Will Message as a retained message . | [_Toc385349234](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349234) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.2.8 User Name Flag

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-18 | MUST NOT | If the User Name Flag is set to 0, a user name MUST NOT be present in the payload . | [_Toc385349235](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349235) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-19 | MUST | If the User Name Flag is set to 1, a user name MUST be present in the payload . | [_Toc385349235](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349235) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.2.9 Password Flag

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-20 | MUST NOT | If the Password Flag is set to 0, a password MUST NOT be present in the payload . | [_Toc385349236](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349236) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-21 | MUST | If the Password Flag is set to 1, a password MUST be present in the payload . | [_Toc385349236](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349236) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-22 | MUST | If the User Name Flag is set to 0, the Password Flag MUST be set to 0 . | [_Toc385349236](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349236) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.2.10 Keep Alive

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.2-23 | MUST | The Keep Alive is a time interval measured in seconds. Expressed as a 16-bit word, it is the maximum time interval that is permitted to elapse between the point at which the Client finishes transmitting one Control Packet and the point it starts sending the next. | [_Figure_3.5_Keep](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_3.5_Keep) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.2-24 | MUST | If the Keep Alive value is non-zero and the Server does not receive a Control Packet from the Client within one and a half times the Keep Alive time period, it MUST disconnect the Network Connection to the Client as if the network had failed . | [_Figure_3.5_Keep](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_3.5_Keep) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.3 Payload

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.3-1 | MUST | The payload of the CONNECT Packet contains one or more length-prefixed fields, whose presence is determined by the flags in the variable header. These fields, if present, MUST appear in the order Client Identifier, Will Topic, Will Message, User Name, Password . | [_Toc384800404](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800404) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.3.1 Client Identifier

Compliance digest: 8 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.3-2 | MUST | The Client Identifier (ClientId) identifies the Client to the Server. Each Client connecting to the Server has a unique ClientId. The ClientId MUST be used by Clients and by Servers to identify state that they hold relating to this MQTT Session between the Client and the Server . | [_Toc385349242](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349242) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.3-3 | MUST | The Client Identifier (ClientId) MUST be present and MUST be the first field in the CONNECT packet payload . | [_Toc385349242](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349242) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.3-4 | MUST | The ClientId MUST be a UTF-8 encoded string as defined in Section 1.5.3 . The Server MUST allow ClientIds which are between 1 and 23 UTF-8 encoded bytes in length, and that contain only the characters | [_Toc385349242](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349242) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.3-5 | UNSPECIFIED | "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ" . | [_Toc385349242](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349242) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.3-6 | MAY | The Server MAY allow ClientId’s that contain more than 23 encoded bytes. The Server MAY allow ClientId’s that contain characters not included in the list given above. A Server MAY allow a Client to supply a ClientId that has a length of zero bytes, however if it does so the Se... | [_Toc385349242](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349242) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.3-7 | MAY | The Server MAY allow ClientId’s that contain more than 23 encoded bytes. The Server MAY allow ClientId’s that contain characters not included in the list given above. A Server MAY allow a Client to supply a ClientId that has a length of zero bytes, however if it does so the Se... | [_Toc385349242](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349242) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.3-8 | MAY | The Server MAY allow ClientId’s that contain more than 23 encoded bytes. The Server MAY allow ClientId’s that contain characters not included in the list given above. A Server MAY allow a Client to supply a ClientId that has a length of zero bytes, however if it does so the Se... | [_Toc385349242](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349242) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.3-9 | MAY | The Server MAY allow ClientId’s that contain more than 23 encoded bytes. The Server MAY allow ClientId’s that contain characters not included in the list given above. A Server MAY allow a Client to supply a ClientId that has a length of zero bytes, however if it does so the Se... | [_Toc385349242](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349242) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.3.2 Will Topic

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.3-10 | MUST | If the Will Flag is set to 1, the Will Topic is the next field in the payload. The Will Topic MUST be a UTF-8 encoded string as defined in Section 1.5.3 . | [_Toc385349243](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349243) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.3.4 User Name

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.3-11 | MUST | If the User Name Flag is set to 1, this is the next field in the payload. The User Name MUST be a UTF-8 encoded string as defined in Section 1.5.3 . It can be used by the Server for authentication and authorization. | [_Toc385349245](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349245) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.1.4 Response

Compliance digest: 5 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.1.4-1 | MUST | 2. The Server MUST validate that the CONNECT Packet conforms to section 3.1 and close the Network Connection without sending a CONNACK if it does not conform . | [_Toc384800405](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800405) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.4-2 | MUST | 1. If the ClientId represents a Client already connected to the Server then the Server MUST disconnect the existing Client . | [_Toc384800405](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800405) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.4-3 | MUST | 2. The Server MUST perform the processing of CleanSession that is described in section 3.1.2.4 . | [_Toc384800405](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800405) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.4-4 | MUST | 3. The Server MUST acknowledge the CONNECT Packet with a CONNACK Packet containing a zero return code . | [_Toc384800405](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800405) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |
| MQTT-3.1.4-5 | MUST NOT | Clients are allowed to send further Control Packets immediately after sending a CONNECT Packet; Clients need not wait for a CONNACK Packet to arrive from the Server. If the Server rejects the CONNECT, it MUST NOT process any data sent by the Client after the CONNECT Packet . | [_Toc384800405](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800405) | rumqttc-v4/src/mqttbytes/v4/connect.rs | unreviewed |

### 3.2 CONNACK – Acknowledge connection request

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.0-1 | MUST | The CONNACK Packet is the packet sent by the Server in response to a CONNECT Packet received from a Client. The first packet sent from the Server to the Client MUST be a CONNACK Packet . | [_Ref362964779](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref362964779) | rumqttc-v4/src/mqttbytes/v4/connack.rs | unreviewed |

### 3.2.2.2 Session Present

Compliance digest: 4 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-1 | MUST | Position: bit 0 of the Connect Acknowledge Flags. If the Server accepts a connection with CleanSession set to 1, the Server MUST set Session Present to 0 in the CONNACK packet in addition to setting a zero return code in the CONNACK packet . | [_Toc385349255](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349255) | rumqttc-v4/src/mqttbytes/v4/connack.rs | unreviewed |
| MQTT-3.2.2-2 | MUST | Position: bit 0 of the Connect Acknowledge Flags. If the Server accepts a connection with CleanSession set to 1, the Server MUST set Session Present to 0 in the CONNACK packet in addition to setting a zero return code in the CONNACK packet . | [_Toc385349255](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349255) | rumqttc-v4/src/mqttbytes/v4/connack.rs | unreviewed |
| MQTT-3.2.2-3 | MUST | Position: bit 0 of the Connect Acknowledge Flags. If the Server accepts a connection with CleanSession set to 1, the Server MUST set Session Present to 0 in the CONNACK packet in addition to setting a zero return code in the CONNACK packet . | [_Toc385349255](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349255) | rumqttc-v4/src/mqttbytes/v4/connack.rs | unreviewed |
| MQTT-3.2.2-4 | MUST | Position: bit 0 of the Connect Acknowledge Flags. If the Server accepts a connection with CleanSession set to 1, the Server MUST set Session Present to 0 in the CONNACK packet in addition to setting a zero return code in the CONNACK packet . | [_Toc385349255](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349255) | rumqttc-v4/src/mqttbytes/v4/connack.rs | unreviewed |

### 3.2.2.3 Connect Return code

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.2.2-5 | SHOULD | The values for the one byte unsigned Connect Return code field are listed in Table 3.1 – Connect Return code values. If a well formed CONNECT Packet is received by the Server, but the Server is unable to process it for some reason, then the Server SHOULD attempt to send a CONN... | [_Toc385349256](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349256) | rumqttc-v4/src/mqttbytes/v4/connack.rs | unreviewed |
| MQTT-3.2.2-6 | MUST | If none of the return codes listed in Table 3.1 – Connect Return code values are deemed applicable, then the Server MUST close the Network Connection without sending a CONNACK . | [_Table_3.1_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Table_3.1_-) | rumqttc-v4/src/mqttbytes/v4/connack.rs | unreviewed |

### 3.3.1.1 DUP

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.1-2 | MUST | The DUP flag MUST be set to 1 by the Client or Server when it attempts to re-deliver a PUBLISH Packet . The DUP flag MUST be set to 0 for all QoS 0 messages . | [_Toc385349262](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349262) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.1-3 | MUST | The value of the DUP flag from an incoming PUBLISH packet is not propagated when the PUBLISH Packet is sent to subscribers by the Server. The DUP flag in the outgoing PUBLISH packet is set independently to the incoming PUBLISH packet, its value MUST be determined solely by whe... | [_Toc385349262](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349262) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |

### 3.3.1.2 QoS

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.1-4 | MUST NOT | A PUBLISH Packet MUST NOT have both QoS bits set to 1. If a Server or Client receives a PUBLISH Packet which has both QoS bits set to 1 it MUST close the Network Connection . | [_Table_3.11_-](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Table_3.11_-) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |

### 3.3.1.3 RETAIN

Compliance digest: 8 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.1-5 | MUST | If the RETAIN flag is set to 1, in a PUBLISH Packet sent by a Client to a Server, the Server MUST store the Application Message and its QoS, so that it can be delivered to future subscribers whose subscriptions match its topic name . | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349265) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.1-6 | MUST | If the RETAIN flag is set to 1, in a PUBLISH Packet sent by a Client to a Server, the Server MUST store the Application Message and its QoS, so that it can be delivered to future subscribers whose subscriptions match its topic name . | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349265) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.1-7 | MUST | If the RETAIN flag is set to 1, in a PUBLISH Packet sent by a Client to a Server, the Server MUST store the Application Message and its QoS, so that it can be delivered to future subscribers whose subscriptions match its topic name . | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349265) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.1-8 | MUST | When sending a PUBLISH Packet to a Client the Server MUST set the RETAIN flag to 1 if a message is sent as a result of a new subscription being made by a Client . It MUST set the RETAIN flag to 0 when a PUBLISH Packet is sent to a Client because it matches an established subsc... | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349265) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.1-9 | MUST | When sending a PUBLISH Packet to a Client the Server MUST set the RETAIN flag to 1 if a message is sent as a result of a new subscription being made by a Client . It MUST set the RETAIN flag to 0 when a PUBLISH Packet is sent to a Client because it matches an established subsc... | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349265) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.1-10 | MUST | A PUBLISH Packet with a RETAIN flag set to 1 and a payload containing zero bytes will be processed as normal by the Server and sent to Clients with a subscription matching the topic name. Additionally any existing retained message with the same topic name MUST be removed and a... | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349265) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.1-11 | MUST | A PUBLISH Packet with a RETAIN flag set to 1 and a payload containing zero bytes will be processed as normal by the Server and sent to Clients with a subscription matching the topic name. Additionally any existing retained message with the same topic name MUST be removed and a... | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349265) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.1-12 | MUST NOT | If the RETAIN flag is 0, in a PUBLISH Packet sent by a Client to a Server, the Server MUST NOT store the message and MUST NOT remove or replace any existing retained message . | [_Toc385349265](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349265) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |

### 3.3.2.1 Topic Name

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.2-1 | MUST | The Topic Name MUST be present as the first field in the PUBLISH Packet Variable header. It MUST be a UTF-8 encoded string as defined in section 1.5.3. | [_Toc385349267](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349267) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.2-2 | MUST NOT | The Topic Name in the PUBLISH Packet MUST NOT contain wildcard characters . | [_Toc385349267](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349267) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.2-3 | MUST | The Topic Name in a PUBLISH Packet sent by a Server to a subscribing Client MUST match the Subscription’s Topic Filter according to the matching process defined in Section 4.7 . However, since the Server is permitted to override the Topic Name, it might not be the same as the... | [_Toc385349267](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349267) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |

### 3.3.4 Response

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.4-1 | MUST | The receiver of a PUBLISH Packet MUST respond according to Table 3.4 - Expected Publish Packet response as determined by the QoS in the PUBLISH Packet . | [_Toc384800414](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800414) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |

### 3.3.5 Actions

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.5-1 | MUST | When Clients make subscriptions with Topic Filters that include wildcards, it is possible for a Client’s subscriptions to overlap so that a published message might match multiple filters. In this case the Server MUST deliver the message to the Client respecting the maximum QoS... | [_Toc384800415](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800415) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |
| MQTT-3.3.5-2 | MUST | If a Server implementation does not authorize a PUBLISH to be performed by a Client; it has no way of informing that Client. It MUST either make a positive acknowledgement, according to the normal QoS rules, or close the Network Connection . | [_Toc384800415](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800415) | rumqttc-v4/src/mqttbytes/v4/publish.rs | unreviewed |

### 3.6.1 Fixed header

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.6.1-1 | MUST | Bits 3,2,1 and 0 of the fixed header in the PUBREL Control Packet are reserved and MUST be set to 0,0,1 and 0 respectively. The Server MUST treat any other value as malformed and close the Network Connection . | [_Figure_3.16_–](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_3.16_–) | rumqttc-v4/src/mqttbytes/v4/pubrel.rs | unreviewed |

### 3.8.1 Fixed header

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.8.1-1 | MUST | Bits 3,2,1 and 0 of the fixed header of the SUBSCRIBE Control Packet are reserved and MUST be set to 0,0,1 and 0 respectively. The Server MUST treat any other value as malformed and close the Network Connection . | [_Figure_3.20_–](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_3.20_–) | rumqttc-v4/src/mqttbytes/v4/subscribe.rs | unreviewed |

### 3.8.3 Payload

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.8.3-1 | MUST | The payload of a SUBSCRIBE Packet contains a list of Topic Filters indicating the Topics to which the Client wants to subscribe. The Topic Filters in a SUBSCRIBE packet payload MUST be UTF-8 encoded strings as defined in Section 1.5.3 . | [_Toc384800439](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800439) | rumqttc-v4/src/mqttbytes/v4/subscribe.rs | unreviewed |
| MQTT-3.8.3-2 | MUST | The payload of a SUBSCRIBE Packet contains a list of Topic Filters indicating the Topics to which the Client wants to subscribe. The Topic Filters in a SUBSCRIBE packet payload MUST be UTF-8 encoded strings as defined in Section 1.5.3 . | [_Toc384800439](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800439) | rumqttc-v4/src/mqttbytes/v4/subscribe.rs | unreviewed |
| MQTT-3.8.3-3 | MUST | The payload of a SUBSCRIBE packet MUST contain at least one Topic Filter / QoS pair. A SUBSCRIBE packet with no payload is a protocol violation . See section 4.8 for information about handling errors. | [_Toc384800439](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800439) | rumqttc-v4/src/mqttbytes/v4/subscribe.rs | unreviewed |

### 3.8.4 Response

Compliance digest: 6 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.8.4-1 | MUST | When the Server receives a SUBSCRIBE Packet from a Client, the Server MUST respond with a SUBACK Packet . The SUBACK Packet MUST have the same Packet Identifier as the SUBSCRIBE Packet that it is acknowledging . | [_Toc384800440](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800440) | rumqttc-v4/src/mqttbytes/v4/subscribe.rs | unreviewed |
| MQTT-3.8.4-2 | MUST | When the Server receives a SUBSCRIBE Packet from a Client, the Server MUST respond with a SUBACK Packet . The SUBACK Packet MUST have the same Packet Identifier as the SUBSCRIBE Packet that it is acknowledging . | [_Toc384800440](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800440) | rumqttc-v4/src/mqttbytes/v4/subscribe.rs | unreviewed |
| MQTT-3.8.4-3 | MUST | If a Server receives a SUBSCRIBE Packet containing a Topic Filter that is identical to an existing Subscription’s Topic Filter then it MUST completely replace that existing Subscription with a new Subscription. The Topic Filter in the new Subscription will be identical to that... | [_Toc384800440](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800440) | rumqttc-v4/src/mqttbytes/v4/subscribe.rs | unreviewed |
| MQTT-3.8.4-4 | MUST | If a Server receives a SUBSCRIBE packet that contains multiple Topic Filters it MUST handle that packet as if it had received a sequence of multiple SUBSCRIBE packets, except that it combines their responses into a single SUBACK response . | [_Toc384800440](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800440) | rumqttc-v4/src/mqttbytes/v4/subscribe.rs | unreviewed |
| MQTT-3.8.4-5 | MUST | The SUBACK Packet sent by the Server to the Client MUST contain a return code for each Topic Filter/QoS pair. This return code MUST either show the maximum QoS that was granted for that Subscription or indicate that the subscription failed . | [_Toc384800440](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800440) | rumqttc-v4/src/mqttbytes/v4/subscribe.rs | unreviewed |
| MQTT-3.8.4-6 | MUST | The SUBACK Packet sent by the Server to the Client MUST contain a return code for each Topic Filter/QoS pair. This return code MUST either show the maximum QoS that was granted for that Subscription or indicate that the subscription failed . | [_Toc384800440](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800440) | rumqttc-v4/src/mqttbytes/v4/subscribe.rs | unreviewed |

### 3.9.3 Payload

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.9.3-1 | MUST | The payload contains a list of return codes. Each return code corresponds to a Topic Filter in the SUBSCRIBE Packet being acknowledged. The order of return codes in the SUBACK Packet MUST match the order of Topic Filters in the SUBSCRIBE Packet . | [_Toc384800444](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800444) | rumqttc-v4/src/mqttbytes/v4/suback.rs | unreviewed |
| MQTT-3.9.3-2 | MUST NOT | SUBACK return codes other than 0x00, 0x01, 0x02 and 0x80 are reserved and MUST NOT be used . | [_Figure_3.26_–](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_3.26_–) | rumqttc-v4/src/mqttbytes/v4/suback.rs | unreviewed |

### 3.10.1 Fixed header

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.10.1-1 | MUST | Bits 3,2,1 and 0 of the fixed header of the UNSUBSCRIBE Control Packet are reserved and MUST be set to 0,0,1 and 0 respectively. The Server MUST treat any other value as malformed and close the Network Connection . | [_Figure_3.28_–](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_3.28_–) | rumqttc-v4/src/mqttbytes/v4/unsubscribe.rs | unreviewed |

### 3.10.3 Payload

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.10.3-1 | MUST | The payload for the UNSUBSCRIBE Packet contains the list of Topic Filters that the Client wishes to unsubscribe from. The Topic Filters in an UNSUBSCRIBE packet MUST be UTF-8 encoded strings as defined in Section 1.5.3, packed contiguously . | [_Toc384800448](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800448) | rumqttc-v4/src/mqttbytes/v4/unsubscribe.rs | unreviewed |
| MQTT-3.10.3-2 | MUST | The Payload of an UNSUBSCRIBE packet MUST contain at least one Topic Filter. An UNSUBSCRIBE packet with no payload is a protocol violation . See section 4.8 for information about handling errors. | [_Toc384800448](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800448) | rumqttc-v4/src/mqttbytes/v4/unsubscribe.rs | unreviewed |

### 3.10.4 Response

Compliance digest: 6 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.10.4-1 | MUST | The Topic Filters (whether they contain wildcards or not) supplied in an UNSUBSCRIBE packet MUST be compared character-by-character with the current set of Topic Filters held by the Server for the Client. If any filter matches exactly then its owning Subscription is deleted, o... | [_Toc384800449](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800449) | rumqttc-v4/src/mqttbytes/v4/unsubscribe.rs | unreviewed |
| MQTT-3.10.4-2 | MUST | It MUST stop adding any new messages for delivery to the Client . | [_Toc384800449](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800449) | rumqttc-v4/src/mqttbytes/v4/unsubscribe.rs | unreviewed |
| MQTT-3.10.4-3 | MUST | It MUST complete the delivery of any QoS 1 or QoS 2 messages which it has started to send to the Client . | [_Toc384800449](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800449) | rumqttc-v4/src/mqttbytes/v4/unsubscribe.rs | unreviewed |
| MQTT-3.10.4-4 | MUST | The Server MUST respond to an UNSUBSUBCRIBE request by sending an UNSUBACK packet. The UNSUBACK Packet MUST have the same Packet Identifier as the UNSUBSCRIBE Packet . Even where no Topic Subscriptions are deleted, the Server MUST respond with an UNSUBACK . | [_Toc384800449](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800449) | rumqttc-v4/src/mqttbytes/v4/unsubscribe.rs | unreviewed |
| MQTT-3.10.4-5 | MUST | The Server MUST respond to an UNSUBSUBCRIBE request by sending an UNSUBACK packet. The UNSUBACK Packet MUST have the same Packet Identifier as the UNSUBSCRIBE Packet . Even where no Topic Subscriptions are deleted, the Server MUST respond with an UNSUBACK . | [_Toc384800449](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800449) | rumqttc-v4/src/mqttbytes/v4/unsubscribe.rs | unreviewed |
| MQTT-3.10.4-6 | MUST | If a Server receives an UNSUBSCRIBE packet that contains multiple Topic Filters it MUST handle that packet as if it had received a sequence of multiple UNSUBSCRIBE packets, except that it sends just one UNSUBACK response . | [_Toc384800449](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800449) | rumqttc-v4/src/mqttbytes/v4/unsubscribe.rs | unreviewed |

### 3.12.4 Response

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.12.4-1 | MUST | The Server MUST send a PINGRESP Packet in response to a PINGREQ Packet . | [_Toc384800458](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800458) | rumqttc-v4/src/mqttbytes/v4/ping.rs | unreviewed |

### 3.14.1 Fixed header

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.14.1-1 | MUST | The Server MUST validate that reserved bits are set to zero and disconnect the Client if they are not zero . | [_Figure_3.35_–](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Figure_3.35_–) | rumqttc-v4/src/mqttbytes/v4/disconnect.rs | unreviewed |

### 3.14.4 Response

Compliance digest: 3 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.14.4-1 | MUST | MUST close the Network Connection . | [_Toc384800467](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800467) | rumqttc-v4/src/mqttbytes/v4/disconnect.rs | unreviewed |
| MQTT-3.14.4-2 | MUST NOT | MUST NOT send any more Control Packets on that Network Connection . | [_Toc384800467](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800467) | rumqttc-v4/src/mqttbytes/v4/disconnect.rs | unreviewed |
| MQTT-3.14.4-3 | MUST | MUST discard any Will Message associated with the current connection without publishing it, as described in Section 3.1.2.5 . | [_Toc384800467](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800467) | rumqttc-v4/src/mqttbytes/v4/disconnect.rs | unreviewed |

### 4.1 Storing state

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.1.0-1 | MUST | It is necessary for the Client and Server to store Session state in order to provide Quality of Service guarantees. The Client and Server MUST store Session state for the entire duration of the Session . A Session MUST last at least as long it has an active Network Connection . | [_Storing_state](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Storing_state) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.1.0-2 | MUST | It is necessary for the Client and Server to store Session state in order to provide Quality of Service guarantees. The Client and Server MUST store Session state for the entire duration of the Session . A Session MUST last at least as long it has an active Network Connection . | [_Storing_state](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Storing_state) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.3.1 QoS 0: At most once delivery

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.3.1-1 | MUST | · MUST send a PUBLISH packet with QoS=0, DUP=0 . | [_Toc384800473](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800473) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.3.2 QoS 1: At least once delivery

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.3.2-1 | UNSPECIFIED | . | [_Ref384138490](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref384138490) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.2-2 | UNSPECIFIED | . | [_Ref384138490](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref384138490) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.3.3 QoS 2: Exactly once delivery

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.3.3-1 | UNSPECIFIED | . | [_Ref384138602](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref384138602) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.3.3-2 | UNSPECIFIED | . | [_Ref384138602](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref384138602) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.4 Message delivery retry

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.4.0-1 | MUST | When a Client reconnects with CleanSession set to 0, both the Client and Server MUST re-send any unacknowledged PUBLISH Packets (where QoS > 0) and PUBREL Packets using their original Packet Identifiers . This is the only circumstance where a Client or Server is REQUIRED to re... | [_Ref383618483](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref383618483) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.5 Message receipt

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.5.0-1 | MUST | When a Server takes ownership of an incoming Application Message it MUST add it to the Session state of those clients that have matching Subscriptions. Matching rules are defined in Section 4.7 . | [_Toc384800477](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800477) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.5.0-2 | MUST | Under normal circumstances Clients receive messages in response to Subscriptions they have created. A Client could also receive messages that do not match any of its explicit Subscriptions. This can happen if the Server automatically assigned a subscription to the Client. | [_Toc384800477](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800477) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.6 Message ordering

Compliance digest: 6 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.6.0-1 | MUST | When it re-sends any PUBLISH packets, it MUST re-send them in the order in which the original PUBLISH packets were sent (this applies to QoS 1 and QoS 2 messages) | [_Toc370160808](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc370160808) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.6.0-2 | MUST | It MUST send PUBACK packets in the order in which the corresponding PUBLISH packets were received (QoS 1 messages) | [_Toc370160808](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc370160808) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.6.0-3 | MUST | It MUST send PUBREC packets in the order in which the corresponding PUBLISH packets were received (QoS 2 messages) | [_Toc370160808](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc370160808) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.6.0-4 | MUST | It MUST send PUBREL packets in the order in which the corresponding PUBREC packets were received (QoS 2 messages) | [_Toc370160808](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc370160808) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.6.0-5 | MUST | A Server MUST by default treat each Topic as an "Ordered Topic". It MAY provide an administrative or other mechanism to allow one or more Topics to be treated as an "Unordered Topic" . | [_Toc370160808](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc370160808) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.6.0-6 | MUST | When a Server processes a message that has been published to an Ordered Topic, it MUST follow the rules listed above when delivering messages to each of its subscribers. In addition it MUST send PUBLISH packets to consumers (for the same Topic and QoS) in the order that they w... | [_Toc370160808](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc370160808) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.7.1 Topic wildcards

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.7.1-1 | MUST NOT | The wildcard characters can be used in Topic Filters, but MUST NOT be used within a Topic Name . | [_Topic_wildcards](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Topic_wildcards) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.7.1.2 Multi-level wildcard

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.7.1-2 | MUST | The number sign (‘#’ U+0023) is a wildcard character that matches any number of levels within a topic. The multi-level wildcard represents the parent and any number of child levels. The multi-level wildcard character MUST be specified either on its own or following a topic lev... | [_Toc385349376](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349376) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.7.1.3 Single level wildcard

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.7.1-3 | MUST | The single-level wildcard can be used at any level in the Topic Filter, including first and last levels. Where it is used it MUST occupy an entire level of the filter . It can be used at more than one level in the Topic Filter and can be used in conjunction with the multilevel... | [_Toc385349377](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc385349377) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.7.2 Topics beginning with $

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.7.2-1 | MUST NOT | The Server MUST NOT match Topic Filters starting with a wildcard character (# or +) with Topic Names beginning with a $ character . The Server SHOULD prevent Clients from using such Topic Names to exchange messages with other Clients. | [_Toc384800481](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800481) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.7.3 Topic semantic and usage

Compliance digest: 4 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.7.3-1 | MUST | All Topic Names and Topic Filters MUST be at least one character long | [_Toc384800482](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800482) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.7.3-2 | MUST NOT | Topic Names and Topic Filters MUST NOT include the null character (Unicode U+0000) | [_Toc384800482](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800482) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.7.3-3 | MUST NOT | Topic Names and Topic Filters are UTF-8 encoded strings, they MUST NOT encode to more than 65535 bytes . See Section 1.5.3 | [_Toc384800482](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800482) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.7.3-4 | MUST NOT | When it performs subscription matching the Server MUST NOT perform any normalization of Topic Names or Topic Filters, or any modification or substitution of unrecognized characters . Each non-wildcarded level in the Topic Filter has to match the corresponding level in the Topi... | [_Toc384800482](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800482) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 4.8 Handling errors

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-4.8.0-1 | MUST | Unless stated otherwise, if either the Server or Client encounters a protocol violation, it MUST close the Network Connection on which it received that Control Packet which caused the protocol violation . | [_Ref381955543](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref381955543) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |
| MQTT-4.8.0-2 | MUST | If the Client or Server encounters a Transient Error while processing an inbound Control Packet it MUST close the Network Connection on which it received that Control Packet . If a Server detects a Transient Error it SHOULD NOT disconnect or have any other effect on its intera... | [_Ref381955543](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Ref381955543) | rumqttc-v4/src/state.rs<br>rumqttc-v4/src/eventloop.rs<br>rumqttc-v4/src/client.rs<br>mqttbytes-core/src/topic.rs | unreviewed |

### 6 Using WebSocket as a network transport

Compliance digest: 4 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-6.0.0-1 | MUST | · MQTT Control Packets MUST be sent in WebSocket binary data frames. If any other type of data frame is received the recipient MUST close the Network Connection . | [_Using_WebSocket_as](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Using_WebSocket_as) | rumqttc-v4/src | unreviewed |
| MQTT-6.0.0-2 | MUST NOT | · A single WebSocket data frame can contain multiple or partial MQTT Control Packets. The receiver MUST NOT assume that MQTT Control Packets are aligned on WebSocket frame boundaries . | [_Using_WebSocket_as](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Using_WebSocket_as) | rumqttc-v4/src | unreviewed |
| MQTT-6.0.0-3 | MUST | · The client MUST include “mqtt” in the list of WebSocket Sub Protocols it offers . | [_Using_WebSocket_as](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Using_WebSocket_as) | rumqttc-v4/src | unreviewed |
| MQTT-6.0.0-4 | MUST | · The WebSocket Sub Protocol name selected and returned by the server MUST be “mqtt” . | [_Using_WebSocket_as](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Using_WebSocket_as) | rumqttc-v4/src | unreviewed |

### 7 Conformance

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-7.0.0-1 | MAY | An MQTT implementation MAY conform as both an MQTT Client and MQTT Server implementation. A Server that both accepts inbound connections and establishes outbound connections to other Servers MUST conform as both an MQTT Client and MQTT Server . | [_Conformance](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Conformance) | rumqttc-v4/src | unreviewed |
| MQTT-7.0.0-2 | MUST NOT | Conformant implementations MUST NOT require the use of any extensions defined outside of this specification in order to interoperate with any other conformant implementation . | [_Conformance](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Conformance) | rumqttc-v4/src | unreviewed |

### 7.1.1 MQTT Server

Compliance digest: 1 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-7.1.1-1 | MUST | A conformant Server MUST support the use of one or more underlying transport protocols that provide an ordered, lossless, stream of bytes from the Client to Server and Server to Client . However conformance does not depend on it supporting any specific transport protocols. | [_Toc384800504](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800504) | rumqttc-v4/tests<br>rumqttc-v4/src/mqttbytes/v4 | unreviewed |

### 7.1.2 MQTT Client

Compliance digest: 2 requirement IDs extracted and mapped to candidate implementation files.

| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |
| --- | --- | --- | --- | --- | --- |
| MQTT-3.3.1-1 | UNSPECIFIED |  | [_Toc384800507](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800507) | rumqttc-v4/tests<br>rumqttc-v4/src/mqttbytes/v4 | unreviewed |
| MQTT-7.1.2-1 | MUST | A conformant Client MUST support the use of one or more underlying transport protocols that provide an ordered, lossless, stream of bytes from the Client to Server and Server to Client . However conformance does not depend on it supporting any specific transport protocols. | [_Toc384800505](https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html#_Toc384800505) | rumqttc-v4/tests<br>rumqttc-v4/src/mqttbytes/v4 | unreviewed |
