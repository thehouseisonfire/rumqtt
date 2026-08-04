# syntax=docker/dockerfile:1

FROM rust:1.88.0-alpine3.21@sha256:54e937b1530d435dc83b94f5a61ef08365127f2fefbb3789712c5d6f55bbb58c AS builder
WORKDIR /src
RUN apk add --no-cache musl=1.2.5-r11 musl-dev=1.2.5-r11
COPY . .
RUN cargo build --locked --release -p low-memory-acceptance \
    --bin low-memory-v4 \
    --bin low-memory-v5 \
    --bin memory-stability-v4 \
    --bin memory-stability-v5

FROM scratch AS v4
COPY --from=builder /src/target/release/low-memory-v4 /client
ENTRYPOINT ["/client"]

FROM scratch AS v5
COPY --from=builder /src/target/release/low-memory-v5 /client
ENTRYPOINT ["/client"]

FROM scratch AS stability-v4
COPY --from=builder /src/target/release/memory-stability-v4 /client
ENTRYPOINT ["/client"]

FROM scratch AS stability-v5
COPY --from=builder /src/target/release/memory-stability-v5 /client
ENTRYPOINT ["/client"]
