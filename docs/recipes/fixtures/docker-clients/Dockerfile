FROM rust:1.88-bookworm AS builder

WORKDIR /usr/src/rumqtt
COPY . .

RUN cargo build --locked --release -p rumqttc-v4-next --example broker_recipe_smoke \
    && cargo build --locked --release -p rumqttc-v5-next --example broker_recipe_smoke_v5

FROM debian:bookworm-slim AS client-v4

COPY --from=builder /usr/src/rumqtt/target/release/examples/broker_recipe_smoke /usr/local/bin/rumqttc-client
ENTRYPOINT ["/usr/local/bin/rumqttc-client"]

FROM debian:bookworm-slim AS client-v5

COPY --from=builder /usr/src/rumqtt/target/release/examples/broker_recipe_smoke_v5 /usr/local/bin/rumqttc-client
ENTRYPOINT ["/usr/local/bin/rumqttc-client"]
