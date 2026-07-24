# Changelog

## [Unreleased]

- Add genuine bounded-memory `AsyncRead`/`AsyncWrite` streaming saves and
  validation-before-output streaming loads without changing the version-1
  envelope or atomic replacement guarantees.
- Retain complete-blob `Vec` methods as allocation-heavy conveniences and
  document streaming submission, backpressure, cancellation, and two-pass
  load behavior.
- Return coordinator storage failures promptly even when a streaming save
  source read or final EOF probe remains pending.

## [0.1.0-alpha.1] - 2026-07-23

- Extract the bounded atomic blob snapshot abstraction with configurable
  identity, bounded coordination, stable format documentation, and neutral API.
