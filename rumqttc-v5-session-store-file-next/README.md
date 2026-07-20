# MQTT 5 session file store

This crate implements `rumqttc-v5-next`'s existing `SessionStore` trait using
the protocol-neutral file-store core. It passes only bytes emitted by
`PersistedSession::encode` to the core and uses `PersistedSession::decode` on
load. Filesystem/envelope errors, canonical encoding errors, canonical decoding
errors, and key encoding errors remain distinct error-chain entries. Client ID,
session expiry, and configuration compatibility remain the event loop's
responsibility.

The namespace is `<configured-root>/v5/`. Version 1 key bytes are exactly:

```text
1:u8 || 5:u8 || scope_length:u32_be || scope UTF-8 bytes ||
client_id_length:u32_be || client_id UTF-8 bytes
```

The complete byte string is hashed with BLAKE3 and the full lowercase digest is
used as the `.session` filename. The core envelope, 64 MiB default limit, Unix
atomic-write behavior, FIFO/cancellation contract, trusted-root model,
corruption policy, and durability limitations are documented by
`rumqttc-session-store-file-core-next`.

The store provides no cross-process locking, multi-writer protection,
encryption, authentication, tamper resistance, or universal power-loss
guarantee. Dependency-owned temporary files are ignored and may remain after an
abrupt interruption; this adapter does not discover or clean them up.

This is a new hashed, versioned layout. Files written by the repository's old
file-store example are incompatible and are never detected or migrated. Use a
dedicated new root. Reusing old persistence state without deliberately
resetting broker-held state can create a local/remote session mismatch.
