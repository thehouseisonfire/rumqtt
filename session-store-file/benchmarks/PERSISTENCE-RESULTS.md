# MQTT persistence results

Checked-in JSON retains canonical codec, checkpoint-growth, recovery, and
broker-backed MQTT measurements from the original 2026-07-21 through
2026-07-26 characterization. The files include raw distributions and exact
environment metadata.

These are local Linux/Btrfs/NVMe warm-cache observations, not performance
guarantees. In particular, approximately 40 ms acknowledgement batching and
substantial host-frequency drift affected some broker runs. Compare repeated,
paired runs and preserve disabled-persistence context before drawing
conclusions.
