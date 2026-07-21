# Session Store Recovery

Checkpoint corruption and exact legacy-file detection fail closed. Stop every
event loop using the same `(scope, client_id)` before recovery; local packet
identifiers and QoS progress may differ from the broker-held session.

Use `inspect` for metadata. Use `quarantine` when the checkpoint may help
diagnosis, or `operator_clear` only after deciding it has no value. Returned
paths are diagnostic and not a stable storage interface. Neither operation
changes broker state.

If inspection reports `LegacyDetected`, those canonical recovery operations do
not apply: the canonical checkpoint is absent and the adapter deliberately does
not mutate legacy data. Stop and explicitly move or remove the reported legacy
file (or select a new dedicated root) before reconnecting and realigning broker
state.

Deliberately realign both sides afterward:

- MQTT 3.1.1: connect once with `CleanSession=true`, then reconnect with the
  intended persistent configuration.
- MQTT 5: connect once with `Clean Start=true` and choose the Session Expiry
  Interval intentionally. Zero removes broker state at disconnect; nonzero
  starts a fresh expiring session. Then restore the intended policy.

The file-store examples stop after a requested local clear or quarantine. They
do not reconnect persistently until the operator has completed this broker-side
realignment.

Resetting broker state does not repair a corrupt local file, and removing local
state does not remove broker subscriptions or queued broker messages.
