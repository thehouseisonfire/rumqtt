#!/usr/bin/env python3
"""Controlled ABI mutations derived from rumqttc's public C shapes."""

from __future__ import annotations

import argparse
import json
import pathlib
import subprocess
import tempfile

HEADER = r"""
#ifndef RUMQTTC_FIXTURE_H
#define RUMQTTC_FIXTURE_H
#include <stddef.h>
#include <stdint.h>
#define RUMQTTC_OK 0u
typedef uint32_t rumqttc_status_t;
typedef struct rumqttc_client_t rumqttc_client_t;
typedef struct rumqttc_bytes_view_t { const uint8_t *data; size_t len; } rumqttc_bytes_view_t;
typedef struct rumqttc_publish_options_t {
    uint32_t struct_size;
    uint32_t qos;
    uint8_t retain;
    uint8_t reserved[3];
} rumqttc_publish_options_t;
rumqttc_status_t rumqttc_client_start(rumqttc_client_t **out);
rumqttc_status_t rumqttc_client_publish(rumqttc_client_t *client, const uint8_t *payload,
                                        uint32_t payload_len, rumqttc_publish_options_t options);
#endif
"""

SOURCE = r"""
#![allow(non_camel_case_types)]
#[repr(C)]
pub struct rumqttc_client_t { private_value: i32 }
#[repr(C)]
pub struct rumqttc_publish_options_t {
    struct_size: u32,
    qos: u32,
    retain: u8,
    reserved: [u8; 3],
}
#[no_mangle]
pub extern "C" fn rumqttc_client_start(out: *mut *mut rumqttc_client_t) -> u32 {
    let _ = out;
    0
}
#[no_mangle]
pub extern "C" fn rumqttc_client_publish(
    client: *mut rumqttc_client_t,
    payload: *const u8,
    payload_len: u32,
    options: rumqttc_publish_options_t,
) -> u32 {
    let _ = (client, payload, payload_len, options);
    0
}
"""


def variants():
    yield (
        "implementation_internal",
        HEADER,
        SOURCE.replace("let _ = out;", "let _implementation_detail = out.is_null();"),
        True,
        True,
    )
    yield (
        "declared_function_addition",
        HEADER + "\nuint32_t rumqttc_added(void);\n",
        SOURCE + '\n#[no_mangle]\npub extern "C" fn rumqttc_added() -> u32 { 0 }\n',
        True,
        True,
    )
    yield (
        "function_removal",
        HEADER.replace("rumqttc_status_t rumqttc_client_start(rumqttc_client_t **out);\n", ""),
        SOURCE.replace(
            '#[no_mangle]\npub extern "C" fn rumqttc_client_start(out: *mut *mut rumqttc_client_t) -> u32 {\n'
            "    let _ = out;\n    0\n}\n",
            "",
        ),
        False,
        True,
    )
    yield (
        "function_rename",
        HEADER.replace("rumqttc_client_start", "rumqttc_client_open"),
        SOURCE.replace("rumqttc_client_start", "rumqttc_client_open"),
        False,
        True,
    )
    yield (
        "scalar_parameter_width",
        HEADER.replace("uint32_t payload_len", "uint64_t payload_len"),
        SOURCE.replace("payload_len: u32", "payload_len: u64"),
        False,
        True,
    )
    yield (
        "pointer_constness",
        HEADER.replace("const uint8_t *payload", "uint8_t *payload"),
        SOURCE.replace("payload: *const u8", "payload: *mut u8"),
        True,
        True,
    )
    yield (
        "return_type",
        HEADER.replace("rumqttc_status_t rumqttc_client_start", "uint64_t rumqttc_client_start"),
        SOURCE.replace(
            "rumqttc_client_start(out: *mut *mut rumqttc_client_t) -> u32",
            "rumqttc_client_start(out: *mut *mut rumqttc_client_t) -> u64",
        ),
        False,
        True,
    )
    yield (
        "field_reorder",
        HEADER.replace(
            "uint32_t struct_size;\n    uint32_t qos;",
            "uint32_t qos;\n    uint32_t struct_size;",
        ),
        SOURCE.replace("struct_size: u32,\n    qos: u32,", "qos: u32,\n    struct_size: u32,"),
        False,
        True,
    )
    yield (
        "same_size_field_type",
        HEADER.replace("uint32_t qos;", "int32_t qos;"),
        SOURCE.replace("qos: u32", "qos: i32"),
        False,
        True,
    )
    yield (
        "alignment_and_offset",
        HEADER.replace("uint32_t qos;", "uint64_t qos;"),
        SOURCE.replace("qos: u32", "qos: u64"),
        False,
        True,
    )
    yield (
        "append_by_value_field",
        HEADER.replace("uint8_t reserved[3];", "uint8_t reserved[3];\n    uint32_t extension;"),
        SOURCE.replace("reserved: [u8; 3],", "reserved: [u8; 3],\n    extension: u32,"),
        False,
        True,
    )
    yield (
        "opaque_private_representation",
        HEADER,
        SOURCE.replace("private_value: i32", "private_value: u64"),
        True,
        True,
    )
    yield (
        "undeclared_export",
        HEADER,
        SOURCE + '\n#[no_mangle]\npub extern "C" fn rumqttc_undeclared() -> u32 { 0 }\n',
        True,
        False,
    )
    yield (
        "comments_only",
        HEADER.replace("#include <stddef.h>", "/* documentation change */\n#include <stddef.h>"),
        SOURCE,
        True,
        True,
    )


def command(arguments: list[str], expected: int | None = 0) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(arguments, text=True, capture_output=True)
    if expected is not None and (result.returncode == 0) != (expected == 0):
        raise RuntimeError(f"unexpected exit from {' '.join(arguments)}\n{result.stdout}\n{result.stderr}")
    return result


def build(root: pathlib.Path, name: str, header: str, source: str, contract_tool: pathlib.Path):
    directory = root / name
    directory.mkdir()
    header_path = directory / "fixture.h"
    source_path = directory / "fixture.rs"
    library = directory / "libfixture.so"
    contract = directory / "contract.json"
    header_path.write_text(header, encoding="utf-8")
    source_path.write_text(source, encoding="utf-8")
    command(
        [
            "rustc",
            "--edition=2021",
            "--crate-type=cdylib",
            "-C",
            "debuginfo=2",
            str(source_path),
            "-o",
            str(library),
        ]
    )
    command(
        [
            "python3",
            str(contract_tool),
            "generate",
            "--header",
            str(header_path),
            "--library",
            str(library),
            "--output",
            str(contract),
        ]
    )
    return header_path, library, contract


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--abicc")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()
    output = pathlib.Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    contract_tool = pathlib.Path(__file__).with_name("contract.py")
    results = []
    with tempfile.TemporaryDirectory(prefix="rumqttc-mutations-") as temporary:
        root = pathlib.Path(temporary)
        base_header, base_library, base_contract = build(root, "base", HEADER, SOURCE, contract_tool)
        for name, header, source, compatible, exports_equal in variants():
            new_header, new_library, new_contract = build(root, name, header, source, contract_tool)
            comparison = command(
                ["python3", str(contract_tool), "compare", "--old", str(base_contract), "--new", str(new_contract)],
                expected=None,
            )
            if (comparison.returncode == 0) != compatible:
                raise RuntimeError(f"contract misclassified {name}\n{comparison.stdout}\n{comparison.stderr}")
            export_check = command(
                ["python3", str(contract_tool), "verify-exports", "--contract", str(new_contract)], expected=None
            )
            if (export_check.returncode == 0) != exports_equal:
                raise RuntimeError(f"export policy misclassified {name}")

            abicc_result = None
            if args.abicc:
                old_xml = root / f"{name}-old.xml"
                new_xml = root / f"{name}-new.xml"
                old_xml.write_text(
                    f"<version>old</version>\n<headers>{base_header}</headers>\n<libs>{base_library}</libs>\n",
                    encoding="utf-8",
                )
                new_xml.write_text(
                    f"<version>new</version>\n<headers>{new_header}</headers>\n<libs>{new_library}</libs>\n",
                    encoding="utf-8",
                )
                report = output / f"{name}.html"
                checked = command(
                    [
                        args.abicc,
                        "-l",
                        "rumqttc-fixture",
                        "-old",
                        str(old_xml),
                        "-new",
                        str(new_xml),
                        "-binary",
                        "-report-path",
                        str(report),
                    ],
                    expected=None,
                )
                abicc_result = "compatible" if checked.returncode == 0 else "incompatible"
            results.append(
                {
                    "mutation": name,
                    "required_binary": "compatible" if compatible else "incompatible",
                    "contract": "compatible" if comparison.returncode == 0 else "incompatible",
                    "export_policy": "equal" if export_check.returncode == 0 else "mismatch",
                    "abi_compliance_checker": abicc_result,
                }
            )
    (output / "mutation-results.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
