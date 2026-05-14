import sys
import unittest
from dataclasses import replace
from pathlib import Path

from lxml import html

sys.path.insert(0, str(Path(__file__).resolve().parent))

from generate_mqtt_specs import (
    SPEC_CONFIGS,
    extract_requirements,
    find_obligation_with_lookback,
    normalize_text,
    parse_sections,
)


class FindObligationWithLookbackTests(unittest.TestCase):
    def test_uses_keyword_from_same_element(self) -> None:
        doc = html.fromstring(
            "<p>If the Will Flag is set to 0, a Will Message MUST NOT be "
            "published [MQTT-3.1.2-12].</p>"
        )
        node = doc.xpath("//p")[0]

        obligation = find_obligation_with_lookback(node, normalize_text(node.text_content()))

        self.assertEqual(obligation, "MUST NOT")

    def test_uses_enclosing_table_row_for_split_requirement_cells(self) -> None:
        doc = html.fromstring("""
            <table>
              <tr>
                <td><p>[MQTT-3.1.2-11]</p></td>
                <td><p>If the Will Flag is set to 0, then the Will QoS MUST be set to 0
                (0x00).</p></td>
              </tr>
            </table>
            """)
        node = doc.xpath("//td[p[contains(., 'MQTT-3.1.2-11')]]")[0]

        obligation = find_obligation_with_lookback(node, normalize_text(node.text_content()))

        self.assertEqual(obligation, "MUST")

    def test_does_not_inherit_keyword_from_document_wrapper(self) -> None:
        doc = html.fromstring("""
            <div>
              <p>The Server MUST validate the packet.</p>
              <p>If the Will Flag is set to 1, the value of Will QoS can be 0 (0x00),
              1 (0x01), or 2 (0x02) [MQTT-3.1.2-12].</p>
            </div>
            """)
        node = doc.xpath("//p[contains(., 'MQTT-3.1.2-12')]")[0]

        obligation = find_obligation_with_lookback(node, normalize_text(node.text_content()))

        self.assertEqual(obligation, "UNSPECIFIED")

    def test_uses_immediately_preceding_unfinished_clause_fragment(self) -> None:
        doc = html.fromstring("""
            <div>
              <p>The Server MUST allow ClientIds which are between 1 and 23 UTF-8
              encoded bytes in length, and that contain only the characters</p>
              <p>"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"
              [MQTT-3.1.3-5].</p>
            </div>
            """)
        node = doc.xpath("//p[contains(., 'MQTT-3.1.3-5')]")[0]

        obligation = find_obligation_with_lookback(node, normalize_text(node.text_content()))

        self.assertEqual(obligation, "MUST")

    def test_conformance_appendix_duplicate_does_not_upgrade_original_obligation(self) -> None:
        doc = html.fromstring("""
            <div>
              <h2><a name="s3"></a>3 Core Behavior</h2>
              <p>Character list only [MQTT-3.1.3-99].</p>
              <h2><a name="s7"></a>7 Conformance</h2>
              <table>
                <tr>
                  <td><p>[MQTT-3.1.3-99]</p></td>
                  <td><p>The Server MUST enforce this requirement.</p></td>
                </tr>
              </table>
            </div>
            """)
        all_nodes = list(doc.iter())
        node_index = {id(node): i for i, node in enumerate(all_nodes)}
        sections = parse_sections(doc, node_index)

        config = replace(SPEC_CONFIGS["v3.1.1"], min_requirements=1)
        requirements = extract_requirements(doc, sections, config)
        req = next(item for item in requirements if item["id"] == "MQTT-3.1.3-99")

        self.assertEqual(req["obligation"], "UNSPECIFIED")

    def test_conformance_appendix_duplicate_upgrades_placeholder_reference(self) -> None:
        doc = html.fromstring("""
            <div>
              <h2><a name="s4"></a>4 Delivery</h2>
              <p>[MQTT-4.3.2-99].</p>
              <h2><a name="s7"></a>7 Conformance</h2>
              <table>
                <tr>
                  <td><p>[MQTT-4.3.2-99]</p></td>
                  <td><p>The sender MUST assign an unused Packet Identifier.</p></td>
                </tr>
              </table>
            </div>
            """)
        all_nodes = list(doc.iter())
        node_index = {id(node): i for i, node in enumerate(all_nodes)}
        sections = parse_sections(doc, node_index)

        config = replace(SPEC_CONFIGS["v3.1.1"], min_requirements=1)
        requirements = extract_requirements(doc, sections, config)
        req = next(item for item in requirements if item["id"] == "MQTT-4.3.2-99")

        self.assertEqual(req["summary"], ".")
        self.assertEqual(req["obligation"], "MUST")


if __name__ == "__main__":
    unittest.main()
