# Further Benchmark Harness Work



What the old harness (from commit `8fb5914e`) had that the current one lacks:

- Cross-library comparison against mqttv5-cli. The old README explicitly documented that workflow. That is useful, but not core to validating this library’s own performance.

- Profiling hooks via pprof in old parser benchmarks. Useful for deep optimization sessions, but not necessary for routine regression coverage.

- Synthetic custom router benchmarks. Potentially useful for isolating client overhead from broker behavior, but also a maintenance burden and less realistic than Mosquitto-backed validation.

- NATS parser comparison. Interesting as an external protocol baseline, but not directly relevant to rumqtt quality.
