# MQTT v5 AUTH Lifecycle Refactor

This change makes MQTT v5 enhanced authentication a first-class client
lifecycle instead of treating `AUTH` as an isolated packet send/receive path.

## Rationale

MQTT v5 allows enhanced authentication during the initial CONNECT flow and
client-initiated re-authentication after CONNACK. The previous implementation
could encode, decode, and respond to `AUTH` packets, but it did not model the
authentication exchange itself. That made several cases difficult or ambiguous:

- distinguishing initial enhanced authentication from later re-authentication;
- rejecting unsolicited or overlapping authentication exchanges;
- correlating `AUTH Success` with a specific client-initiated re-auth attempt;
- notifying applications when re-authentication completed or failed;
- giving authentication callbacks enough context to implement real mechanisms.

The refactor centralizes those rules in a small lifecycle state machine so
protocol validation, callback execution, events, and tracked completion handles
all agree on the current authentication state.

## High-Level Changes

- Added an internal MQTT v5 AUTH lifecycle state machine that tracks idle,
  initial CONNECT authentication, and post-CONNACK re-authentication states.
- Replaced the packet-shaped authentication callback with a context-aware
  `Authenticator` API that receives the exchange kind and method.
- Added structured authentication lifecycle events through `Event::Auth`.
- Added tracked re-authentication APIs that return an `AuthNotice`, allowing
  applications to wait for a typed authentication outcome.
- Enforced stricter protocol behavior for missing or mismatched Authentication
  Method values, unsolicited `AUTH Continue` / `AUTH Success`, server-initiated
  `AUTH ReAuthenticate`, and overlapping client re-authentication attempts.
- Kept raw `Incoming::Auth` visibility, but applications no longer need to
  infer the authentication lifecycle only from raw packet events and errors.

## Scope

This is intentionally a breaking API cleanup for MQTT v5 authentication. The
old `AuthManager` model has been superseded by `Authenticator`, and re-auth can
now be used either as a fire-and-forget request or as a tracked operation with a
structured result.

The codec remains packet-focused; lifecycle legality is enforced in the client
state and event loop where connection context is available.
