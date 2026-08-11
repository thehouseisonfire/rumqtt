# C ABI comparison tool decision

## Decision

Use the checked-in compiler-derived contract comparator as the authoritative
historical gate. Reject libabigail and ABI Dumper for this Rust-produced C
boundary, and retain ABI Compliance Checker only in the controlled evaluation
corpus. Existing native consumers, behavioral tests, cbindgen checks, export
inspection, and package checks remain separate evidence.

The contract tool delegates C parsing and target layout to Clang. Its normalized
JSON contains canonical source and machine function types, typedefs, evaluated
constants, public record layout, final project exports, and native loader
identity. Historical comparison is directional, while current declarations and
exports use exact equality.

## Evaluated tools

- libabigail (`abidiff`/`abidw`) 2.10, tag `libabigail-2.10`: rejected. On the
  real Rust 1.96 profiling `cdylib`, all 71 symbols were visible, but exported
  function DIEs had no parameters and shared an unusable placeholder return
  type. It could not detect required signature or reachable-layout changes.
- ABI Dumper 1.4, commit `12779ce9345fa569cba064b481663fa6992bad90`:
  rejected. It depends on the same incomplete Rust DWARF; a C-only fixture
  would not prove the shipped Rust boundary.
- ABI Compliance Checker, commit
  `7c175c45a8ba9ac41b8e47d8ebbab557b623b18e`: rejected as a gate.
  Header-plus-library mode handled additions, removals, signatures, and most
  layouts, but reported pointer constness as a binary break, missed a same-size
  public field-type change, and ignored undeclared exports. Its HTML reports
  remain useful diagnostic evidence.
- Clang AST plus native layout/export inspection, contract schema 1: adopted.
  It detected every required machine-level mutation, accepted compatible
  additions/internal/opaque/comment changes, reported qualification separately,
  and enforces accidental-export policy. Every manifest records the CI compiler
  and host target.

Release stripping and LTO do not affect the selected model because the public
types come from the released header and the symbol/loader evidence comes from
the final released binary. No debug-only rebuild is treated as equivalent to a
published artifact.

libabigail and Clang use Apache-2.0-with-LLVM-exception licensing. ABI Dumper
and ABI Compliance Checker use LGPL-2.1 licensing; CI fetches the pinned checker
for evaluation and does not vendor or redistribute it. The checker also depends
on Perl, GCC, Binutils, and Ctags, and its own regression suite no longer builds
unchanged with the evaluated GCC 16 host. The selected checked-in Python tool
requires Clang plus each platform's native symbol/loader utilities.

## Controlled mutations

The durable generator is `rumqttc-c/tests/abi/mutation_matrix.py`. The following
results were observed on Linux x86_64 (LP64):

| Mutation | Contract | ABI Compliance Checker |
| --- | --- | --- |
| Rust/C implementation internals only | compatible | compatible |
| Declared exported function addition | compatible | compatible |
| Function removal or rename | incompatible | incompatible |
| Scalar parameter width | incompatible | incompatible |
| Pointer constness | binary-compatible; source finding | **binary-incompatible false positive** |
| Return type | incompatible | incompatible |
| Public field reorder | incompatible | incompatible |
| Same-size public field type | incompatible | **compatible false negative** |
| Alignment/offset change | incompatible | incompatible |
| Append by-value field | incompatible | incompatible |
| Opaque private representation | compatible | compatible |
| Undeclared `rumqttc_*` export | export-policy failure | missed |
| Comments only | compatible | compatible |

The real public API uses fixed-width integers and `size_t`, so the corpus has no
`long`-dependent LP64/LLP64 classification. Native manifests are nevertheless
generated independently on Linux x86_64, macOS arm64, and Windows x86_64; their
sizes, alignments, offsets, calling conventions, object formats, and loader
identities are never inferred from another target.

## Scope and cost

Contract generation takes well under a second after the native library exists.
The compact mutation corpus takes a few seconds; ABI Compliance Checker adds
roughly ten seconds on Linux and is pinned by Git commit. Suppression files are
not used. Failures preserve JSON, textual output, and third-party HTML reports.

The structural gate does not prove loading, allocation/free pairing, panic
containment, threading, timeout behavior, MQTT semantics, CMake/pkg-config
relocatability, or real C/C++ consumption. Dedicated gates continue to prove
those properties.

Upstream references: [libabigail `abidiff` manual](https://sourceware.org/libabigail/manual/abidiff.html),
[ABI Compliance Checker](https://github.com/lvc/abi-compliance-checker), and
[ABI Dumper](https://github.com/lvc/abi-dumper).
