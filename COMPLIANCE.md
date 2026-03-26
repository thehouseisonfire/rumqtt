# Compliance Notes

## Purpose

This document tracks known spec-compliance edge cases and records whether they are supported,
partially supported, intentionally unsupported, or still under evaluation.

## Status Values

- `Supported`: Implemented and intended to behave according to the relevant spec requirement.
- `Partial`: Implemented for common cases, but with known gaps or narrower behavior than the spec
  allows.
- `Won't Support`: Known behavior gap that is intentionally left unsupported for now.
- `Pending`: Known issue or open question that has not yet been assigned a final support decision.

## Edge Cases

### Explicitly Empty CONNECT Passwords

- `Status`: `Won't Support`
- `Area`: MQTT CONNECT authentication fields
- `Summary`: An explicitly empty CONNECT password is not preserved as distinct from an omitted
  password during serialization.
- `Current Behavior`: `Bytes::new()` passed to `set_credentials(...)` is serialized the same way
  as an absent password, rather than setting `Password Flag = 1` with a zero-length password
  field.
