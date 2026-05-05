Summary for Reviewer

Problem: The MQTT spec compliance generator ([docs/spec/generate_mqtt_specs.py](/home/eagle/MQTT/rumqtt/docs/spec/generate_mqtt_specs.py:288))
could mislabel standalone requirements by inheriting normative keywords from unrelated wrapper content. In practice this changed
`MQTT-3.1.2-12` in the regenerated v5 digest from `UNSPECIFIED` to `MUST`, even though its own clause only says the value "can be"
0, 1, or 2.

Root cause:

- The obligation fallback scanned the whole parent container and nearby siblings when the current node did not contain a normative
  keyword.
- For many requirement nodes the parent is a broad wrapper element, so the fallback could match the first `MUST` or `SHOULD`
  anywhere in that larger section rather than in the requirement's local clause.

Fix:

- Keep direct same-element obligation detection unchanged.
- Recover obligations from tightly local original-spec structure only:
  adjacent paragraph fragments when the previous paragraph ends with an unfinished clause, and same-row table cells when the
  requirement ID is separated from its clause text.
- Only let section `7` conformance appendix duplicates upgrade earlier `UNSPECIFIED` obligations when the original occurrence is
  just a placeholder bare-ID reference. Substantive original clauses still remain authoritative.
- Add regression tests covering direct detection, paragraph-fragment recovery, table-row fallback, wrapper-bleed prevention, and
  selective appendix duplicate backfill.

Result:

- Standalone requirements no longer inherit unrelated normative keywords from document wrappers.
- Split-clause original requirements such as `MQTT-3.1.3-5` now resolve their original `MUST` obligation correctly.
- Split-cell table requirements still resolve their obligation correctly.
- Placeholder reference-only entries such as the v3 QoS 1/2 appendix-backed IDs still recover their `MUST` obligations.
- `MQTT-3.1.2-12` in the regenerated v5 digest is back to `UNSPECIFIED`.
- Added a matching changelog entry under [Unreleased] > Fixed.
