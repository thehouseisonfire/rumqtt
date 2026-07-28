# MQTT persistence benchmark methodology

The codec command measures canonical MQTT v4/v5 persisted-session encoding or
decoding. Growth records canonical checkpoint size from zero through 1,000
inflight publishes. Recovery writes through the production file adapter, then
separately measures adapter load and application to a newly constructed MQTT
state. Broker-backed `mqtt` runs compare persistence-enabled and disabled
traffic with the same protocol, QoS, payload, inflight bound, and local broker.

Use repeated release runs, retain raw JSON and environment metadata, and avoid
attributing short loopback throughput changes solely to persistence. Broker and
TCP acknowledgement behavior, warm caches, scheduling, and host frequency can
dominate short samples.
