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
used as the `.session` filename. The core envelope, 64 MiB default limit, Unix/Windows
atomic-write behavior, FIFO/cancellation contract, trusted-root model,
corruption policy, and durability limitations are documented by
`rumqttc-session-store-file-core-next`.

The store provides no cross-process locking, multi-writer protection,
encryption, authentication, tamper resistance, or universal power-loss
guarantee. Typed inspection, quarantine, operator-clear, and Windows owned-staging cleanup APIs are provided.

When canonical state is absent, exactly the former example filename
`hex(scope).hex(client_id).session` below the root is detected and reported.
It is never scanned for, decoded, or migrated.
