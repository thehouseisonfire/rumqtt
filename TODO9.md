# Rumqtt Roadmap

Here’s the roadmap we’ve set up for `rumqttc`.

## Memory Stability and Performance

- Stable and fast memory usage
- Support devices with less than 10 MB of memory

## Features

- Return packet IDs, tokens, or another identifier to clients
- Graceful shutdown support
- Full MQTT 5 support
- Acknowledge messages after processing
- Synchronous publishing with the broker, where the next message is published only after the previous acknowledgment
- High-level client
- Python, C, and JavaScript wrappers

## Testing

- Stability and performance test suite using a mock broker
- 100% code coverage
- Throughput and memory-stability tests
- Comparisons with other clients
- MQTT conformance test suite
