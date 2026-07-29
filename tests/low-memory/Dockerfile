# syntax=docker/dockerfile:1

FROM rust:1.85.0-alpine3.21@sha256:bea885d2711087e67a9f7a7cd1a164976f4c35389478512af170730014d2452a AS builder
WORKDIR /src
RUN apk add --no-cache musl=1.2.5-r11 musl-dev=1.2.5-r11
COPY . .
RUN cargo build --locked --release -p low-memory-acceptance \
    --bin low-memory-v4 \
    --bin low-memory-v5

FROM scratch AS v4
COPY --from=builder /src/target/release/low-memory-v4 /client
ENTRYPOINT ["/client"]

FROM scratch AS v5
COPY --from=builder /src/target/release/low-memory-v5 /client
ENTRYPOINT ["/client"]
