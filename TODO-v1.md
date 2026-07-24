# TODO: Revisit `#[non_exhaustive]` After Full Spec Verification

After the whole v4/v5 client has been verified against the individual MQTT spec requirements, revisit the temporary/public API choice of marking error enums as `#[non_exhaustive]`.

## Context

`#[non_exhaustive]` was added during the v4 error-taxonomy cleanup because spec-by-spec verification exposed missing protocol-state errors, such as duplicate CONNACK handling.

This was useful while the error taxonomy was still evolving, but once the client has been fully audited against the MQTT requirements, we may want to remove `#[non_exhaustive]` again where the public API is considered stable and intentionally closed.

## Tasks

- Review every public error enum currently marked `#[non_exhaustive]`.
- Decide whether each enum should remain open-ended or become exhaustive again.
- Prefer removing `#[non_exhaustive]` only after:
  - all relevant MQTT spec requirements have been reviewed;
  - protocol-state violations have explicit variants where appropriate;
  - packet-local codec errors and state-machine errors are clearly separated;
  - downstream exhaustive matching would be useful and unlikely to break soon.

## Candidate enums to revisit

- `mqttbytes::Error`
- `StateError`
- `ProtocolViolation`
- `ConnectionError`
- `ClientError`
- `OptionError`
- `PublishNoticeError`
- `SubscribeNoticeError`
- `UnsubscribeNoticeError`

## Desired outcome

If the spec audit shows that the error taxonomy is mature enough, remove `#[non_exhaustive]` from the enums where a closed API is preferable.

If future protocol-hardening work is still expected, keep `#[non_exhaustive]` on the enums most likely to grow.
