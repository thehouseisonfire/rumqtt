#!/usr/bin/env python3
"""Generate compliance-oriented MQTT spec docs from OASIS HTML sources.

Outputs, per version:
- Markdown digest with per-requirement trace table
- JSON requirement index for tooling
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import urllib.error
import urllib.request
from bisect import bisect_right
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from lxml import html

REQUIREMENT_RE = re.compile(r"MQTT-\d+(?:\.\d+)+-\d+")
HEADING_RE = re.compile(r"^(\d+(?:\.\d+)*)\s+(.+)$")
NORMATIVE_RE = re.compile(
    r"\b(MUST NOT|SHALL NOT|SHOULD NOT|MUST|SHALL|SHOULD|MAY|REQUIRED)\b"
)


@dataclass
class SpecConfig:
    key: str
    title: str
    version: str
    url: str
    output_markdown: str
    output_json: str
    min_requirements: int


@dataclass
class Section:
    number: str
    title: str
    level: int
    start_idx: int
    end_idx: int
    anchor: str | None


SPEC_CONFIGS: dict[str, SpecConfig] = {
    "v3.1.1": SpecConfig(
        key="v3.1.1",
        title="MQTT Version 3.1.1",
        version="3.1.1",
        url="https://docs.oasis-open.org/mqtt/mqtt/v3.1.1/os/mqtt-v3.1.1-os.html",
        output_markdown="mqtt-v3.1.1.md",
        output_json="mqtt-v3.1.1.requirements.json",
        min_requirements=120,
    ),
    "v5.0": SpecConfig(
        key="v5.0",
        title="MQTT Version 5.0",
        version="5.0",
        url="https://docs.oasis-open.org/mqtt/mqtt/v5.0/os/mqtt-v5.0-os.html",
        output_markdown="mqtt-v5.0.md",
        output_json="mqtt-v5.0.requirements.json",
        min_requirements=200,
    ),
}


V4_PACKET_MAP: dict[str, list[str]] = {
    "3.1": ["rumqttc-v4/src/mqttbytes/v4/connect.rs"],
    "3.2": ["rumqttc-v4/src/mqttbytes/v4/connack.rs"],
    "3.3": ["rumqttc-v4/src/mqttbytes/v4/publish.rs"],
    "3.4": ["rumqttc-v4/src/mqttbytes/v4/puback.rs"],
    "3.5": ["rumqttc-v4/src/mqttbytes/v4/pubrec.rs"],
    "3.6": ["rumqttc-v4/src/mqttbytes/v4/pubrel.rs"],
    "3.7": ["rumqttc-v4/src/mqttbytes/v4/pubcomp.rs"],
    "3.8": ["rumqttc-v4/src/mqttbytes/v4/subscribe.rs"],
    "3.9": ["rumqttc-v4/src/mqttbytes/v4/suback.rs"],
    "3.10": ["rumqttc-v4/src/mqttbytes/v4/unsubscribe.rs"],
    "3.11": ["rumqttc-v4/src/mqttbytes/v4/unsuback.rs"],
    "3.12": ["rumqttc-v4/src/mqttbytes/v4/ping.rs"],
    "3.13": ["rumqttc-v4/src/mqttbytes/v4/ping.rs"],
    "3.14": ["rumqttc-v4/src/mqttbytes/v4/disconnect.rs"],
}


V5_PACKET_MAP: dict[str, list[str]] = {
    "3.1": ["rumqttc-v5/src/mqttbytes/v5/connect.rs"],
    "3.2": ["rumqttc-v5/src/mqttbytes/v5/connack.rs"],
    "3.3": ["rumqttc-v5/src/mqttbytes/v5/publish.rs"],
    "3.4": ["rumqttc-v5/src/mqttbytes/v5/puback.rs"],
    "3.5": ["rumqttc-v5/src/mqttbytes/v5/pubrec.rs"],
    "3.6": ["rumqttc-v5/src/mqttbytes/v5/pubrel.rs"],
    "3.7": ["rumqttc-v5/src/mqttbytes/v5/pubcomp.rs"],
    "3.8": ["rumqttc-v5/src/mqttbytes/v5/subscribe.rs"],
    "3.9": ["rumqttc-v5/src/mqttbytes/v5/suback.rs"],
    "3.10": ["rumqttc-v5/src/mqttbytes/v5/unsubscribe.rs"],
    "3.11": ["rumqttc-v5/src/mqttbytes/v5/unsuback.rs"],
    "3.12": ["rumqttc-v5/src/mqttbytes/v5/ping.rs"],
    "3.13": ["rumqttc-v5/src/mqttbytes/v5/ping.rs"],
    "3.14": ["rumqttc-v5/src/mqttbytes/v5/disconnect.rs"],
    "3.15": ["rumqttc-v5/src/mqttbytes/v5/auth.rs"],
}


def normalize_text(value: str) -> str:
    return re.sub(r"\s+", " ", value).strip()


def slugify(section_number: str, section_title: str) -> str:
    base = f"{section_number}-{section_title}".lower()
    base = re.sub(r"[^a-z0-9]+", "-", base)
    return base.strip("-")


def requirement_sort_key(req_id: str) -> tuple[int, ...]:
    return tuple(int(part) for part in re.findall(r"\d+", req_id))


def clean_summary(clause: str) -> str:
    text = clause
    text = re.sub(r"\[MQTT-[^\]]+\]", "", text)
    text = re.sub(r"\[[A-Za-z0-9 ._\-/]+\]", "", text)
    text = normalize_text(text)

    sentences = re.split(r"(?<=[.!?])\s+", text)
    picked: list[str] = []
    for sentence in sentences:
        if sentence:
            picked.append(sentence)
        if len(" ".join(picked)) >= 220:
            break

    summary = normalize_text(" ".join(picked))
    if len(summary) > 280:
        summary = summary[:277].rstrip() + "..."
    return summary


def find_first_anchor(node: Any) -> str | None:
    if not hasattr(node, "xpath"):
        return None

    if node.get("id"):
        return str(node.get("id"))
    if node.get("name"):
        return str(node.get("name"))

    anchors = node.xpath(".//a[@id or @name]")
    for anchor in anchors:
        if anchor.get("id"):
            return str(anchor.get("id"))
        if anchor.get("name"):
            return str(anchor.get("name"))
    return None


def find_direct_anchor(node: Any) -> str | None:
    if not hasattr(node, "get"):
        return None
    if node.get("id"):
        return str(node.get("id"))
    if node.get("name"):
        return str(node.get("name"))
    return None


def parse_sections(doc: Any, node_index: dict[int, int]) -> list[Section]:
    sections: list[Section] = []
    for level in range(1, 7):
        for heading in doc.xpath(f"//h{level}"):
            text = normalize_text(heading.text_content())
            match = HEADING_RE.match(text)
            if not match:
                continue

            section_number = match.group(1)
            section_title = match.group(2)
            start_idx = node_index[id(heading)]
            anchor = find_first_anchor(heading)
            sections.append(
                Section(
                    number=section_number,
                    title=section_title,
                    level=level,
                    start_idx=start_idx,
                    end_idx=0,
                    anchor=anchor,
                )
            )

    sections.sort(key=lambda s: s.start_idx)
    if not sections:
        return sections

    for i, section in enumerate(sections):
        if i + 1 < len(sections):
            section.end_idx = sections[i + 1].start_idx - 1
        else:
            section.end_idx = 10**9

    return sections


def section_for_node_idx(sections: list[Section], idx: int) -> Section | None:
    if not sections:
        return None

    starts = [section.start_idx for section in sections]
    pos = bisect_right(starts, idx) - 1
    if pos < 0:
        return None

    section = sections[pos]
    if section.start_idx <= idx <= section.end_idx:
        return section
    return None


def mapping_for_section(version: str, section_number: str, section_title: str) -> tuple[list[str], str]:
    packet_map = V4_PACKET_MAP if version == "3.1.1" else V5_PACKET_MAP
    base_paths = (
        [
            "rumqttc-v4/src/mqttbytes/mod.rs",
            "rumqttc-v4/src/mqttbytes/v4/mod.rs",
            "rumqttc-v4/src/mqttbytes/v4/codec.rs",
        ]
        if version == "3.1.1"
        else [
            "rumqttc-v5/src/mqttbytes/mod.rs",
            "rumqttc-v5/src/mqttbytes/v5/mod.rs",
            "rumqttc-v5/src/mqttbytes/v5/codec.rs",
        ]
    )

    ops_paths = (
        [
            "rumqttc-v4/src/state.rs",
            "rumqttc-v4/src/eventloop.rs",
            "rumqttc-v4/src/client.rs",
            "mqttbytes-core/src/topic.rs",
        ]
        if version == "3.1.1"
        else [
            "rumqttc-v5/src/state.rs",
            "rumqttc-v5/src/eventloop.rs",
            "rumqttc-v5/src/client.rs",
            "mqttbytes-core/src/topic.rs",
        ]
    )

    ws_paths = (
        ["rumqttc-v4/src/websockets.rs", "rumqttc-v4/src/transport.rs"]
        if version == "3.1.1"
        else ["rumqttc-v5/src/websockets.rs", "rumqttc-v5/src/transport.rs"]
    )

    sec_prefix_parts = section_number.split(".")
    sec_prefix = section_number if len(sec_prefix_parts) < 2 else ".".join(sec_prefix_parts[:2])

    if sec_prefix in packet_map:
        return packet_map[sec_prefix], f"Heuristic mapping from section prefix {sec_prefix} to packet module."

    if section_number.startswith("2.") or section_number.startswith("1."):
        return base_paths, "Heuristic mapping to core packet framing and codec modules."

    if section_number.startswith("4."):
        return ops_paths, "Heuristic mapping to state machine, event loop, and topic behavior modules."

    if section_number.startswith("6."):
        return ws_paths, "Heuristic mapping to websocket and transport modules."

    if section_number.startswith("7."):
        return [
            "rumqttc-v4/tests" if version == "3.1.1" else "rumqttc-v5/tests",
            "rumqttc-v4/src/mqttbytes/v4" if version == "3.1.1" else "rumqttc-v5/src/mqttbytes/v5",
        ], "Heuristic mapping to tests and protocol modules for conformance coverage."

    fallback_root = "rumqttc-v4/src" if version == "3.1.1" else "rumqttc-v5/src"
    return [fallback_root], f"Fallback mapping for section {section_number} ({section_title})."


def extract_requirements(doc: Any, sections: list[Section], config: SpecConfig) -> list[dict[str, Any]]:
    all_nodes = list(doc.iter())
    node_index = {id(node): i for i, node in enumerate(all_nodes)}

    requirement_items: dict[str, dict[str, Any]] = {}

    anchors: list[tuple[int, str]] = []
    for node in all_nodes:
        anchor = find_direct_anchor(node)
        if anchor:
            anchors.append((node_index[id(node)], anchor))

    anchor_positions = [item[0] for item in anchors]

    for node in doc.xpath("//p|//li|//td"):
        text = normalize_text(node.text_content())
        if not text:
            continue

        req_ids = REQUIREMENT_RE.findall(text)
        if not req_ids:
            continue

        idx = node_index[id(node)]
        section = section_for_node_idx(sections, idx)
        if not section:
            continue

        local_anchor = find_first_anchor(node)
        if not local_anchor and anchor_positions:
            pos = bisect_right(anchor_positions, idx) - 1
            if pos >= 0:
                local_anchor = anchors[pos][1]
        if not local_anchor:
            local_anchor = section.anchor

        obligation_match = NORMATIVE_RE.search(text)
        obligation = obligation_match.group(1) if obligation_match else "UNSPECIFIED"
        summary = clean_summary(text)

        for req_id in req_ids:
            mapping_paths, mapping_reason = mapping_for_section(config.version, section.number, section.title)

            req = requirement_items.get(req_id)
            if req is None:
                req = {
                    "id": req_id,
                    "section_number": section.number,
                    "section_title": section.title,
                    "obligation": obligation,
                    "summary": summary,
                    "source_anchor": local_anchor,
                    "occurrences": 1,
                    "candidate_paths": mapping_paths,
                    "mapping_status": "unreviewed",
                    "mapping_reason": mapping_reason,
                }
                requirement_items[req_id] = req
            else:
                req["occurrences"] += 1
                if req["obligation"] == "UNSPECIFIED" and obligation != "UNSPECIFIED":
                    req["obligation"] = obligation

    requirements = sorted(requirement_items.values(), key=lambda item: requirement_sort_key(item["id"]))
    if len(requirements) < config.min_requirements:
        raise RuntimeError(
            f"Requirement extraction too low for {config.key}: got {len(requirements)} "
            f"(expected at least {config.min_requirements})"
        )

    return requirements


def validate_candidate_paths(requirements: list[dict[str, Any]], repo_root: Path) -> None:
    missing: list[str] = []
    seen = set()
    for req in requirements:
        for path in req["candidate_paths"]:
            if path in seen:
                continue
            seen.add(path)
            if not (repo_root / path).exists():
                missing.append(path)

    if missing:
        joined = ", ".join(sorted(missing))
        raise RuntimeError(f"Mapped candidate paths are missing: {joined}")


def render_markdown(
    config: SpecConfig,
    generated_at_utc: str,
    source_last_modified: str,
    requirements: list[dict[str, Any]],
) -> str:
    section_map: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for req in requirements:
        key = (req["section_number"], req["section_title"])
        section_map.setdefault(key, []).append(req)

    ordered_sections = sorted(section_map.keys(), key=lambda key: tuple(int(p) for p in key[0].split(".")))

    lines: list[str] = []
    lines.append(f"# {config.title} Compliance Digest")
    lines.append("")
    lines.append("## Metadata")
    lines.append("")
    lines.append(f"- Source: {config.url}")
    lines.append(f"- Spec version: {config.version}")
    lines.append(f"- Source Last-Modified: {source_last_modified}")
    lines.append(f"- Generated (UTC): {generated_at_utc}")
    lines.append(f"- Unique requirements: {len(requirements)}")
    lines.append("")
    lines.append("## Section Index")
    lines.append("")

    for section_number, section_title in ordered_sections:
        section_slug = slugify(section_number, section_title)
        count = len(section_map[(section_number, section_title)])
        lines.append(f"- [{section_number} {section_title}](#{section_slug}) ({count} requirements)")

    lines.append("")
    lines.append("## Compliance Sections")
    lines.append("")

    for section_number, section_title in ordered_sections:
        lines.append(f"### {section_number} {section_title}")
        lines.append("")
        rows = section_map[(section_number, section_title)]
        lines.append(
            f"Compliance digest: {len(rows)} requirement IDs extracted and mapped to candidate implementation files."
        )
        lines.append("")
        lines.append("| ID | Obligation | Summary | Anchor | Candidate Code | Mapping Status |")
        lines.append("| --- | --- | --- | --- | --- | --- |")

        for req in rows:
            anchor = req["source_anchor"] or ""
            anchor_link = f"[{anchor}]({config.url}#{anchor})" if anchor else ""
            candidate = "<br>".join(req["candidate_paths"])
            summary = req["summary"].replace("|", "\\|")
            lines.append(
                f"| {req['id']} | {req['obligation']} | {summary} | {anchor_link} | {candidate} | {req['mapping_status']} |"
            )

        lines.append("")

    return "\n".join(lines) + "\n"


def fetch_source(url: str, timeout_seconds: int) -> tuple[bytes, str]:
    request = urllib.request.Request(url, headers={"User-Agent": "rumqtt-spec-generator/1.0"})
    with urllib.request.urlopen(request, timeout=timeout_seconds) as response:
        content = response.read()
        last_modified = response.headers.get("Last-Modified", "unknown")
    return content, last_modified


def load_source(
    config: SpecConfig,
    cache_dir: Path,
    timeout_seconds: int,
    offline: bool,
) -> tuple[bytes, str]:
    source_path = cache_dir / f"{config.key}.source.html"
    meta_path = cache_dir / f"{config.key}.source.meta.json"
    if offline:
        if not source_path.exists():
            raise RuntimeError(
                f"--offline requested, but cached source is missing: {source_path}. "
                "Run once online to populate cache."
            )
        content = source_path.read_bytes()
        last_modified = "unknown"
        if meta_path.exists():
            payload = json.loads(meta_path.read_text(encoding="utf-8"))
            last_modified = payload.get("source_last_modified", "unknown")
        else:
            last_modified = dt.datetime.fromtimestamp(source_path.stat().st_mtime, tz=dt.timezone.utc).isoformat()
        return content, last_modified

    content, last_modified = fetch_source(config.url, timeout_seconds)
    source_path.write_bytes(content)
    meta_payload = {
        "source_url": config.url,
        "source_last_modified": last_modified,
        "cached_at_utc": dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat(),
    }
    meta_path.write_text(json.dumps(meta_payload, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")
    return content, last_modified


def process_spec(
    config: SpecConfig,
    out_dir: Path,
    timeout_seconds: int,
    generated_at_utc: str,
    offline: bool,
    repo_root: Path,
) -> None:
    source_bytes, source_last_modified = load_source(config, out_dir, timeout_seconds, offline)
    doc = html.fromstring(source_bytes)

    all_nodes = list(doc.iter())
    node_index = {id(node): i for i, node in enumerate(all_nodes)}
    sections = parse_sections(doc, node_index)
    requirements = extract_requirements(doc, sections, config)
    validate_candidate_paths(requirements, repo_root)

    markdown_content = render_markdown(config, generated_at_utc, source_last_modified, requirements)

    payload = {
        "spec_version": config.version,
        "source_url": config.url,
        "source_last_modified": source_last_modified,
        "generated_at_utc": generated_at_utc,
        "requirements": requirements,
    }

    markdown_path = out_dir / config.output_markdown
    json_path = out_dir / config.output_json

    markdown_path.write_text(markdown_content, encoding="utf-8")
    json_path.write_text(json.dumps(payload, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")

    print(f"[{config.key}] requirements={len(requirements)} -> {markdown_path} and {json_path}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate MQTT compliance docs from official specs")
    parser.add_argument(
        "--versions",
        nargs="+",
        default=["v3.1.1", "v5.0"],
        choices=sorted(SPEC_CONFIGS.keys()),
        help="Spec versions to generate",
    )
    parser.add_argument(
        "--out-dir",
        default="docs/spec",
        help="Output directory for generated markdown/json files",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=int,
        default=90,
        help="Network timeout for source downloads",
    )
    parser.add_argument(
        "--fail-on-count-drift",
        action="store_true",
        help="Fail if extracted requirement count differs from baseline expectation",
    )
    parser.add_argument(
        "--offline",
        action="store_true",
        help="Use cached local HTML sources in output directory; do not download",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    repo_root = Path(__file__).resolve().parents[2]

    generated_at_utc = dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()

    for version_key in args.versions:
        config = SPEC_CONFIGS[version_key]
        try:
            process_spec(config, out_dir, args.timeout_seconds, generated_at_utc, args.offline, repo_root)
        except urllib.error.URLError as exc:
            raise RuntimeError(f"Failed to fetch {config.url}: {exc}") from exc

    if args.fail_on_count_drift:
        expected_counts = {
            "v3.1.1": 139,
            "v5.0": 252,
        }
        for version_key in args.versions:
            json_path = out_dir / SPEC_CONFIGS[version_key].output_json
            payload = json.loads(json_path.read_text(encoding="utf-8"))
            observed = len(payload["requirements"])
            expected = expected_counts[version_key]
            if observed != expected:
                raise RuntimeError(
                    f"Count drift for {version_key}: observed={observed}, expected={expected}. "
                    "Review parser or update baseline intentionally."
                )


if __name__ == "__main__":
    main()
