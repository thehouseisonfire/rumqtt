<head>
  <link rel="import" href="results/benchmarks.html">
</head>

# Benchmarks

## Existing benchmarks

```bash
# Parsers
cargo run --bin v4parser --release
cargo run --bin v5parser --release
cargo run --bin natsparser --release

# Clients
cargo run --bin rumqttasync --release
cargo run --bin rumqttasyncqos0 --release
cargo run --bin rumqttsync --release
```

## MQTT v5 client benchmark (new)

```bash
# TCP
cargo run --bin rumqttv5bench --release -- \
  --mode throughput --url mqtt://127.0.0.1:1883 --qos 1

# TLS (server-auth)
cargo run --bin rumqttv5bench --release -- \
  --mode latency --url mqtts://127.0.0.1:8883 --ca-cert /path/to/ca.pem --qos 1
```

## Cross-library comparison

`regression.py` now supports a cross-library mode for comparing rumqtt v5 and mqttv5-cli:

```bash
python3 benchmarks/regression.py \
  --cross-library \
  --rumqtt-root /path/to/rumqtt \
  --mqttlib-root /path/to/mqtt-lib \
  --broker-url-tcp mqtt://127.0.0.1:1883 \
  --broker-url-tls mqtts://127.0.0.1:8883 \
  --ca-cert /path/to/ca.pem \
  --runs 5 --warmup 1
```
