# MQTT Spec Compliance Docs

This directory contains generated, compliance-oriented references for MQTT 3.1.1 and MQTT 5.

## Files

- `mqtt-v3.1.1.md`: Markdown compliance digest for MQTT 3.1.1.
- `mqtt-v5.0.md`: Markdown compliance digest for MQTT 5.0.
- `mqtt-v3.1.1.requirements.json`: Machine-readable requirement index for MQTT 3.1.1.
- `mqtt-v5.0.requirements.json`: Machine-readable requirement index for MQTT 5.0.

## Source of Truth

`mqtt-v3.1.1.md`, `mqtt-v3.1.1.requirements.correct.json` `mqtt-v5.0.md` and `mqtt-v5.0.requirements.correct.json` are authoritative.

The generated requirements JSON files at `mqtt-v3.1.1.requirements.correct.json`, `mqtt-v3.1.1.requirements.fix-attempt.json` and `mqtt-v5.0.requirements.correct.json` are useful as indexes and mapping hints, but they may contain incorrect summaries, duplicated requirements, wrong actors, or adjacent requirement mappings. Audit passes must verify reviewed requirements against the authoritative source text.

Do not modify the generated requirements files during hardening unless explicitly instructed. Add corrected reviewed entries to `mqtt-v3.1.1.requirements.correct.json` and `mqtt-v5.0.requirements.correct.json`.

## JSON Schema

Top-level keys:

- `spec_version`
- `source_url`
- `source_last_modified`
- `generated_at_utc`
- `requirements`

Each generated `requirements[]` item includes:

- `id`: requirement ID (`MQTT-x.x.x-y`).
- `section_number`: spec section number.
- `section_title`: spec section title.
- `obligation`: detected RFC-style keyword (`MUST`, `SHOULD`, etc.).
- `summary`: deterministic normalized requirement summary.
- `source_anchor`: source HTML anchor when available.
- `occurrences`: number of matches in source text.
- `candidate_paths`: heuristic list of likely implementation files.
- `mapping_status`: extraction/review status.
- `mapping_reason`: explanation of mapping heuristic used.

Reviewed entries in `mqtt-v3.1.1.requirements.correct.json` may also include:

- `authoritative_source`: source file, section, title, and anchor checked.
- `authoritative_summary`: corrected summary from the authoritative source.
- `authoritative_actor`: actor responsible for the requirement.
- `original_mapping_assessment`: whether the generated mapping was correct, wrong, or unclear.
- `fix_attempt_mapping_assessment`: whether the abandoned fix-attempt mapping was correct, wrong, absent, or unclear.
- `wrong_mapping_client_behavior_risk`: whether following an incorrect mapping could introduce incorrect client behavior.
- `wrong_mapping_behavior_present`: whether that incorrect behavior was found in the codebase.
- `compliance_status`: reviewed implementation status for client-applicable obligations.
- `evidence`: concrete implementation paths and symbols.
- `test_coverage`: tests that assert the reviewed behavior, or notes when not applicable.
- `follow_up`: remaining work or `null`.

Audit passes may add these review fields to individual requirement entries:

- `compliance_status`: reviewed implementation status for obligations applicable to this repository's client
  crates.
- `evidence`: concrete implementation paths and symbols that enforce the client-applicable behavior.
- `test_coverage`: tests that assert the reviewed behavior, or an explanation when the requirement is not
  applicable to a client crate.
- `follow_up`: remaining work or `null` when no known client-side follow-up remains.

Use these `compliance_status` values consistently:

- `compliant`: the client crate fully satisfies every normative obligation in the requirement that is applicable
  to this repository. Reviewed requirements whose remaining normative obligations are server-only may also be
  marked `compliant` when no known client-side follow-up remains.
- `partial`: the client crate enforces only part of a requirement that remains applicable to this repository.
- `non_compliant`: the requirement has a client-applicable obligation that is not currently enforced, or the
  implementation contradicts the requirement.
- `not_applicable`: the normative obligation is purely outside this client crate's role. Defensive client behavior,
  such as decoding a server error or closing on a malformed server response, can still be recorded in `evidence`
  and `test_coverage`.
