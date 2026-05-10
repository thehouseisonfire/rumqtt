# MQTT Spec Compliance Docs

This directory contains generated, compliance-oriented references for MQTT 3.1.1 and MQTT 5.

## Files

- `mqtt-v3.1.1.md`: Markdown compliance digest for MQTT 3.1.1.
- `markdown/mqtt-v5.0.md`: Markdown compliance digest for MQTT 5.0.
- `mqtt-v3.1.1.requirements.json`: Machine-readable requirement index for MQTT 3.1.1.
- `mqtt-v5.0.requirements.json`: Machine-readable requirement index for MQTT 5.0.
- `generate_mqtt_specs.py`: Generator script.

## Generate

Run from the repository root:

```bash
python3 docs/spec/generate_mqtt_specs.py --versions v3.1.1 v5.0 --out-dir docs/spec --fail-on-count-drift
```

Options:

- `--versions`: subset of specs to generate (`v3.1.1`, `v5.0`).
- `--out-dir`: output directory.
- `--timeout-seconds`: HTTP timeout when downloading sources.
- `--fail-on-count-drift`: strict guard against requirement count changes.
- `--offline`: use cached `*.source.html` files in `--out-dir` and skip downloads.

The generator also stores source cache artifacts:

- `<version>.source.html`: cached official HTML source.
- `<version>.source.meta.json`: cached source metadata (including `source_last_modified`).

## JSON Schema

Top-level keys:

- `spec_version`
- `source_url`
- `source_last_modified`
- `generated_at_utc`
- `requirements`

Each `requirements[]` item includes:

- `id`: requirement ID (`MQTT-x.x.x-y`).
- `section_number`: spec section number.
- `section_title`: spec section title.
- `obligation`: detected RFC-style keyword (`MUST`, `SHOULD`, etc.).
- `summary`: deterministic normalized requirement summary.
- `source_anchor`: source HTML anchor when available.
- `occurrences`: number of matches in source text.
- `candidate_paths`: heuristic list of likely implementation files.
- `mapping_status`: currently `unreviewed`.
- `mapping_reason`: explanation of mapping heuristic used.

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
