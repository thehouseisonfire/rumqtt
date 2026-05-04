#!/usr/bin/env bash
set -euo pipefail

# Run the ignored benchmark e2e tests against a local Mosquitto instance.

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

MOSQUITTO_BIN="${MOSQUITTO_BIN:-/home/eagle/MQTT/mosquitto/build/src/mosquitto}"
TCP_PORT="${TCP_PORT:-18883}"
TLS_PORT="${TLS_PORT:-18884}"
BROKER_HOST="${BROKER_HOST:-127.0.0.1}"
WORKDIR="${WORKDIR:-$(mktemp -d /tmp/rumqtt-bench-e2e-XXXXXX)}"
KEEP_WORKDIR="${KEEP_WORKDIR:-0}"

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

need_cmd cargo
need_cmd openssl
need_cmd ss
need_cmd rg

if [[ ! -x "$MOSQUITTO_BIN" ]]; then
  echo "mosquitto binary not found or not executable: $MOSQUITTO_BIN" >&2
  exit 1
fi

BROKER_PID=""

cleanup() {
  set +e
  if [[ -n "$BROKER_PID" ]]; then
    kill "$BROKER_PID" >/dev/null 2>&1 || true
    wait "$BROKER_PID" >/dev/null 2>&1 || true
  fi

  if [[ "$KEEP_WORKDIR" != "1" ]]; then
    rm -rf "$WORKDIR"
  fi
}
trap cleanup EXIT

mkdir -p "$WORKDIR"

cat >"$WORKDIR/ca.cnf" <<'EOF'
[req]
default_bits = 2048
prompt = no
default_md = sha256
distinguished_name = dn
x509_extensions = v3_ca

[dn]
CN = rumqtt-bench-ca

[v3_ca]
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid:always,issuer
basicConstraints = critical, CA:true
keyUsage = critical, digitalSignature, cRLSign, keyCertSign
EOF

openssl req -x509 -new -nodes -days 365 \
  -config "$WORKDIR/ca.cnf" \
  -keyout "$WORKDIR/ca.key" \
  -out "$WORKDIR/ca.crt" >/dev/null 2>&1

cat >"$WORKDIR/server.cnf" <<EOF
[req]
default_bits = 2048
prompt = no
default_md = sha256
distinguished_name = dn
req_extensions = v3_req

[dn]
CN = ${BROKER_HOST}

[v3_req]
subjectAltName = @alt_names
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth

[alt_names]
IP.1 = ${BROKER_HOST}
DNS.1 = localhost
EOF

openssl req -new -nodes \
  -config "$WORKDIR/server.cnf" \
  -keyout "$WORKDIR/server.key" \
  -out "$WORKDIR/server.csr" >/dev/null 2>&1

openssl x509 -req -days 365 \
  -in "$WORKDIR/server.csr" \
  -CA "$WORKDIR/ca.crt" \
  -CAkey "$WORKDIR/ca.key" \
  -CAcreateserial \
  -out "$WORKDIR/server.crt" \
  -extensions v3_req \
  -extfile "$WORKDIR/server.cnf" >/dev/null 2>&1

cat >"$WORKDIR/mosquitto.conf" <<EOF
allow_anonymous true
persistence false
connection_messages true
log_type all

listener ${TCP_PORT} ${BROKER_HOST}
protocol mqtt

listener ${TLS_PORT} ${BROKER_HOST}
protocol mqtt
cafile ${WORKDIR}/ca.crt
certfile ${WORKDIR}/server.crt
keyfile ${WORKDIR}/server.key
require_certificate false
EOF

"$MOSQUITTO_BIN" -c "$WORKDIR/mosquitto.conf" -v >"$WORKDIR/mosquitto.log" 2>&1 &
BROKER_PID=$!

for _ in $(seq 1 20); do
  if ss -ltn | rg -q ":(${TCP_PORT}|${TLS_PORT})\\b"; then
    break
  fi
  sleep 0.5
done

ss -ltn | rg -q ":${TCP_PORT}\\b" || {
  echo "broker failed to open TCP port ${TCP_PORT}" >&2
  tail -n 50 "$WORKDIR/mosquitto.log" >&2 || true
  exit 1
}

ss -ltn | rg -q ":${TLS_PORT}\\b" || {
  echo "broker failed to open TLS port ${TLS_PORT}" >&2
  tail -n 50 "$WORKDIR/mosquitto.log" >&2 || true
  exit 1
}

echo "running TCP benchmark e2e test against mqtt://${BROKER_HOST}:${TCP_PORT}"
RUMQTT_BENCH_TCP_URL="mqtt://${BROKER_HOST}:${TCP_PORT}" \
  cargo test -p benchmarks --test v5_bench_tcp_e2e -- --ignored --nocapture

echo "running TLS benchmark e2e test against mqtts://${BROKER_HOST}:${TLS_PORT}"
RUMQTT_BENCH_TLS_URL="mqtts://${BROKER_HOST}:${TLS_PORT}" \
RUMQTT_BENCH_CA_CERT="${WORKDIR}/ca.crt" \
  cargo test -p benchmarks --test v5_bench_tls_e2e -- --ignored --nocapture

echo "done"
if [[ "$KEEP_WORKDIR" == "1" ]]; then
  echo "CA cert: ${WORKDIR}/ca.crt"
  echo "Mosquitto log: ${WORKDIR}/mosquitto.log"
else
  echo "temporary files will be removed on exit"
fi
