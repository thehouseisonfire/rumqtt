#!/usr/bin/env python3
"""Generate and compare deterministic C ABI contracts for rumqttc.

Clang parses the header; this script never attempts to parse C declarations.
Native compilation supplies target-specific sizes, alignments, and offsets.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
from typing import Any

PUBLIC_PREFIX = "rumqttc_"
IGNORED_MACROS = {
    "RUMQTTC_H",
    "RUMQTTC_API",
    "RUMQTTC_STATIC",
    "RUMQTTC_BUILDING",
}


def run(command: list[str], *, input_text: str | None = None) -> str:
    result = subprocess.run(command, input=input_text, text=True, capture_output=True)
    if result.returncode:
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
        raise SystemExit(result.returncode)
    return result.stdout


def walk(node: Any):
    if isinstance(node, dict):
        yield node
        for child in node.get("inner", []):
            yield from walk(child)
    elif isinstance(node, list):
        for child in node:
            yield from walk(child)


def constant_value(node: dict[str, Any]) -> str | None:
    if "value" in node:
        return str(node["value"])
    for child in node.get("inner", []):
        if value := constant_value(child):
            return value
    return None


def binary_type(type_name: str) -> str:
    # CV qualification does not change the machine calling contract for C
    # pointers. It remains present in source_type and is reported separately.
    result = re.sub(r"\b(?:const|volatile|restrict)\s+", "", type_name)
    return re.sub(r"\b(?:struct|union|enum)\s+", "", result).strip()


def canonical_type(type_name: str, aliases: dict[str, str]) -> str:
    result = binary_type(type_name)
    for _ in range(16):
        previous = result
        for name in sorted(aliases, key=len, reverse=True):
            replacement = aliases[name]
            if replacement and replacement != name and not re.search(rf"\b{re.escape(name)}\b", replacement):
                result = re.sub(rf"\b{re.escape(name)}\b", replacement, result)
        result = binary_type(result)
        if result == previous:
            break
    return result


def parse_header(header: pathlib.Path, clang: str) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="rumqttc-contract-") as directory:
        source = pathlib.Path(directory) / "contract.c"
        source.write_text(f'#include "{header.as_posix()}"\n', encoding="utf-8")
        ast = json.loads(run([clang, "-std=c11", "-Xclang", "-ast-dump=json", "-fsyntax-only", str(source)]))

    functions: dict[str, Any] = {}
    typedefs: dict[str, Any] = {}
    records: dict[str, Any] = {}
    enums: dict[str, Any] = {}
    aliases = {
        node.get("name", ""): node.get("type", {}).get("desugaredQualType", node.get("type", {}).get("qualType", ""))
        for node in walk(ast)
        if node.get("kind") == "TypedefDecl" and node.get("name")
    }
    for node in walk(ast):
        name = node.get("name", "")
        kind = node.get("kind")
        if not name.startswith(PUBLIC_PREFIX):
            continue
        if kind == "FunctionDecl":
            source_type = node.get("type", {}).get("qualType", "")
            functions[name] = {
                "source_type": source_type,
                "binary_type": canonical_type(source_type, aliases),
                "variadic": bool(node.get("variadic")),
            }
        elif kind == "TypedefDecl":
            info = node.get("type", {})
            source_type = info.get("desugaredQualType", info.get("qualType", ""))
            typedefs[name] = {
                "source_type": source_type,
                "binary_type": canonical_type(source_type, aliases),
            }
        elif kind == "RecordDecl" and node.get("completeDefinition"):
            fields = []
            for field in node.get("inner", []):
                if field.get("kind") != "FieldDecl":
                    continue
                field_type = field.get("type", {}).get("desugaredQualType", field.get("type", {}).get("qualType", ""))
                fields.append(
                    {
                        "name": field.get("name", ""),
                        "source_type": field_type,
                        "binary_type": canonical_type(field_type, aliases),
                    }
                )
            records[name] = {"tag": node.get("tagUsed", "struct"), "fields": fields}
        elif kind == "EnumDecl":
            values = {
                item.get("name", ""): constant_value(item)
                for item in node.get("inner", [])
                if item.get("kind") == "EnumConstantDecl"
            }
            enums[name] = {
                "underlying_type": node.get("fixedUnderlyingType", {}).get("qualType"),
                "values": values,
            }

    macro_output = run([clang, "-std=c11", "-dM", "-E", "-include", str(header), "-x", "c", os.devnull])
    macros: dict[str, str] = {}
    for line in macro_output.splitlines():
        match = re.fullmatch(r"#define\s+(RUMQTTC_[A-Za-z0-9_]+)\s+(.+)", line)
        if match and match.group(1) not in IGNORED_MACROS and "(" not in match.group(1):
            macros[match.group(1)] = " ".join(match.group(2).split())

    return {
        "functions": functions,
        "typedefs": typedefs,
        "records": records,
        "enums": enums,
        "macros": macros,
    }


def add_native_layout(contract: dict[str, Any], header: pathlib.Path, cxx: str) -> None:
    lines = [
        f'#include "{header.as_posix()}"',
        "#include <cstddef>",
        "#include <cstdio>",
        "int main() {",
    ]
    for name, record in sorted(contract["records"].items()):
        lines.append(f'  std::printf("R\\t{name}\\t%zu\\t%zu\\n", sizeof({name}), alignof({name}));')
        for field in record["fields"]:
            if field["name"]:
                lines.append(
                    f'  std::printf("F\\t{name}\\t{field["name"]}\\t%zu\\n", offsetof({name}, {field["name"]}));'
                )
    for name in sorted(contract["typedefs"]):
        typedef = contract["typedefs"][name]
        if typedef["source_type"].startswith("struct rumqttc_") and name not in contract["records"]:
            typedef["opaque"] = True
            continue
        lines.append(f'  std::printf("T\\t{name}\\t%zu\\t%zu\\n", sizeof({name}), alignof({name}));')
    for name in sorted(contract["macros"]):
        lines.append(f'  std::printf("M\\t{name}\\t%llu\\n", static_cast<unsigned long long>({name}));')
    lines.extend(["  return 0;", "}"])

    with tempfile.TemporaryDirectory(prefix="rumqttc-layout-") as directory:
        root = pathlib.Path(directory)
        source = root / "layout.cpp"
        executable = root / ("layout.exe" if os.name == "nt" else "layout")
        source.write_text("\n".join(lines) + "\n", encoding="utf-8")
        run([cxx, "-std=c++17", str(source), "-o", str(executable)])
        output = run([str(executable)])

    for line in output.splitlines():
        parts = line.split("\t")
        if parts[0] == "R":
            contract["records"][parts[1]].update(size=int(parts[2]), align=int(parts[3]))
        elif parts[0] == "F":
            for field in contract["records"][parts[1]]["fields"]:
                if field["name"] == parts[2]:
                    field["offset"] = int(parts[3])
                    break
        elif parts[0] == "T":
            contract["typedefs"][parts[1]].update(size=int(parts[2]), align=int(parts[3]))
        elif parts[0] == "M":
            contract["macros"][parts[1]] = {
                "source_expression": contract["macros"][parts[1]],
                "value": int(parts[2]),
            }


def artifact_metadata(library: pathlib.Path) -> tuple[list[str], dict[str, str]]:
    if sys.platform == "darwin":
        output = run(["nm", "-gU", str(library)])
        exports = sorted({line.split()[-1].removeprefix("_") for line in output.splitlines() if line.split()})
        load = run(["otool", "-D", str(library)]).splitlines()
        identity = load[1].strip() if len(load) > 1 else ""
        return [name for name in exports if name.startswith(PUBLIC_PREFIX)], {"format": "Mach-O", "identity": identity}
    if os.name == "nt":
        output = run(["dumpbin", "/nologo", "/exports", str(library)])
        exports = sorted(set(re.findall(r"\brumqttc_[A-Za-z0-9_]+$", output, re.MULTILINE)))
        return exports, {"format": "PE/COFF", "identity": library.name}
    output = run(["nm", "-D", "--defined-only", str(library)])
    exports = sorted({line.split()[-1] for line in output.splitlines() if line.split()})
    dynamic = run(["readelf", "-d", str(library)])
    match = re.search(r"\(SONAME\).*\[([^]]+)]", dynamic)
    return [name for name in exports if name.startswith(PUBLIC_PREFIX)], {
        "format": "ELF",
        "identity": match.group(1) if match else "",
    }


def generate(args: argparse.Namespace) -> int:
    header = pathlib.Path(args.header).resolve()
    contract = parse_header(header, args.clang)
    add_native_layout(contract, header, args.cxx)
    contract["schema"] = 1
    contract["target"] = args.target or run([args.clang, "-dumpmachine"]).strip()
    contract["compiler"] = run([args.clang, "--version"]).splitlines()[0]
    contract["layout_compiler"] = run([args.cxx, "--version"]).splitlines()[0]
    if args.package_version:
        contract["package_version"] = args.package_version
    if args.library:
        exports, loader = artifact_metadata(pathlib.Path(args.library).resolve())
        contract["exports"] = exports
        contract["loader"] = loader
    pathlib.Path(args.output).write_text(json.dumps(contract, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0


def machine_view(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: machine_view(item) for key, item in value.items() if key not in {"source_type", "source_expression"}
        }
    if isinstance(value, list):
        return [machine_view(item) for item in value]
    return value


def compare_maps(
    old: dict[str, Any],
    new: dict[str, Any],
    category: str,
    exact: bool,
    errors: list[str],
    source_changes: list[str],
) -> None:
    for name, value in old.items():
        if name not in new:
            errors.append(f"removed {category}: {name}")
        elif (value if exact else machine_view(value)) != (new[name] if exact else machine_view(new[name])):
            errors.append(f"changed {category}: {name}")
        elif value != new[name]:
            source_changes.append(f"source-only {category} change: {name}")
    if exact:
        for name in new.keys() - old.keys():
            errors.append(f"added {category}: {name}")


def compare(args: argparse.Namespace) -> int:
    old = json.loads(pathlib.Path(args.old).read_text(encoding="utf-8"))
    new = json.loads(pathlib.Path(args.new).read_text(encoding="utf-8"))
    errors: list[str] = []
    source_changes: list[str] = []
    exact = args.mode == "exact"
    if old.get("schema") != new.get("schema"):
        errors.append(f"contract schema differs: {old.get('schema')} -> {new.get('schema')}")
    if old.get("target") != new.get("target"):
        errors.append(f"contract target differs: {old.get('target')} -> {new.get('target')}")
    categories = (
        args.categories.split(",") if args.categories else ("functions", "typedefs", "records", "enums", "macros")
    )
    for category in categories:
        compare_maps(old.get(category, {}), new.get(category, {}), category, exact, errors, source_changes)
    if old.get("loader") and old.get("loader") != new.get("loader"):
        errors.append(f"changed loader identity: {old.get('loader')} -> {new.get('loader')}")
    if exact and old.get("exports", []) != new.get("exports", []):
        errors.append("export sets differ")
    if not exact:
        for symbol in old.get("exports", []):
            if symbol not in new.get("exports", []):
                errors.append(f"removed export: {symbol}")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    if source_changes:
        print("\n".join(source_changes))
    return 0


def verify_exports(args: argparse.Namespace) -> int:
    contract = json.loads(pathlib.Path(args.contract).read_text(encoding="utf-8"))
    declared = sorted(contract.get("functions", {}))
    exported = sorted(contract.get("exports", []))
    if declared == exported:
        return 0
    print("declared and exported rumqttc_* symbols differ", file=sys.stderr)
    print(f"declared only: {sorted(set(declared) - set(exported))}", file=sys.stderr)
    print(f"exported only: {sorted(set(exported) - set(declared))}", file=sys.stderr)
    return 1


def verify_loader(args: argparse.Namespace) -> int:
    contract = json.loads(pathlib.Path(args.contract).read_text(encoding="utf-8"))
    macros = contract.get("macros", {})
    major = macros.get("RUMQTTC_ABI_VERSION_MAJOR", {}).get("value")
    minor = macros.get("RUMQTTC_ABI_VERSION_MINOR", {}).get("value")
    if major is None or minor is None:
        print("ABI version macros are absent from the contract", file=sys.stderr)
        return 1
    line = f"0.{minor}" if major == 0 else str(major)
    loader_format = contract.get("loader", {}).get("format")
    expected = {
        "ELF": f"librumqttc.so.{line}",
        "Mach-O": f"@rpath/librumqttc.{line}.dylib",
        "PE/COFF": f"rumqttc-{line.replace('.', '_')}.dll",
    }.get(loader_format)
    actual = contract.get("loader", {}).get("identity")
    if expected == actual:
        return 0
    print(f"loader identity differs: expected {expected}, found {actual}", file=sys.stderr)
    return 1


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser()
    subcommands = root.add_subparsers(dest="command", required=True)
    make = subcommands.add_parser("generate")
    make.add_argument("--header", required=True)
    make.add_argument("--library")
    make.add_argument("--output", required=True)
    make.add_argument("--target")
    make.add_argument("--package-version")
    make.add_argument("--clang", default=os.environ.get("CLANG", "clang"))
    make.add_argument("--cxx", default=os.environ.get("CXX", "clang++"))
    make.set_defaults(action=generate)
    diff = subcommands.add_parser("compare")
    diff.add_argument("--old", required=True)
    diff.add_argument("--new", required=True)
    diff.add_argument("--mode", choices=("containment", "exact"), default="containment")
    diff.add_argument("--categories", help="comma-separated contract categories")
    diff.set_defaults(action=compare)
    exports = subcommands.add_parser("verify-exports")
    exports.add_argument("--contract", required=True)
    exports.set_defaults(action=verify_exports)
    loader = subcommands.add_parser("verify-loader")
    loader.add_argument("--contract", required=True)
    loader.set_defaults(action=verify_loader)
    return root


if __name__ == "__main__":
    arguments = parser().parse_args()
    raise SystemExit(arguments.action(arguments))
