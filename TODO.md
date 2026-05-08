# TODO

## Replace panicking MQTT v4 option setters with fallible APIs

MQTT 3.1.1 requires clients that send a zero-byte `ClientId` to also set `CleanSession=1`.
The v4 codec correctly enforces this at the protocol boundary by returning
`Error::IncorrectPacketFormat` from `Connect::write` for an empty `ClientId` with
`CleanSession=0`.

`MqttOptions::set_clean_session(false)` and `MqttOptions::set_client_id("")` currently
enforce the same invariant by panicking. This is consistent with the existing setter style,
but it is not ideal API behavior for user-provided configuration because the invalid state is
recoverable.

Planned direction:

- Add fallible alternatives such as `try_set_clean_session` and `try_set_client_id`.
- Prefer returning a typed configuration error for invalid `ClientId` / `CleanSession`
  combinations.
- Keep the `Connect::write` validation as defense in depth.
- Consider deprecating the panicking setters after the fallible API is available.
- Update URL parsing and builder paths to use fallible validation where practical.
