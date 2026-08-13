# C ABI compatibility and release policy

This policy applies only to the C boundary in
`native-wrappers/c/include/rumqttc.h` and
the packaged shared libraries. It does not promise Rust API compatibility.

## Invariants

Every build must satisfy:

```text
current public header declarations == current rumqttc_* exports
```

Every release that claims compatibility must additionally satisfy:

```text
latest compatible published ABI ⊆ current ABI
```

Containment preserves functions, canonical machine signatures, typedef
representations, constant values, public record sizes/alignments/field
order/types/offsets, and loader identity. It permits deliberate new functions.
Pointee qualification changes are reported as source changes because they do
not alter the C machine calling contract. Opaque Rust representations are not
part of the C ABI. Ownership, threading, lifetime, and error semantics remain
behavioral promises enforced by native and Rust tests.

## Version lifecycle

- The unpublished `0.1.0-alpha` tree has no historical baseline.
- Published `0.1.0` establishes ABI line 0.1.
- Each `0.1.z` release preserves the latest earlier 0.1 release. Comparing the
  latest release is cumulative because every publication must first pass the
  same check against its predecessor.
- A new `0.y.0` may deliberately establish an incompatible pre-stable line and
  must update its ABI number, loader identity, baseline, and migration notes.
- `1.0.0` establishes the first mature ABI. Compatible additions remain allowed
  throughout 1.x; incompatible changes require 2.0.0 and a new loader identity.

The package version and ABI line are deliberately distinct. The former is
returned by `rumqttc_library_version()`. The latter is returned by
`rumqttc_abi_version()` and encoded by `RUMQTTC_ABI_VERSION_MAJOR` and
`RUMQTTC_ABI_VERSION_MINOR`.

## Baselines and release procedure

`baseline.py` selects the latest lower GitHub release in the promised line.
Pre-stable patch releases use the same `0.y`; stable releases use the same
major. A release that promises compatibility fails closed when its baseline is
missing or cannot be authenticated.

The resolver downloads the platform archive and paired SHA-256 file, verifies
the checksum and GitHub/Sigstore build attestation, and requires the header and
target-specific `share/rumqttc/abi-contract.json` to be paired in that archive.
It never uses a merge base or developer-created snapshot.

The checked-in comparator is versioned by its manifest schema. Schema or
compiler-normalization changes fail closed against an older manifest and must
ship with an explicit backwards reader or a reviewed baseline-tool migration;
published contract data is never rewritten.

Before tagging `rumqttc-c-v<package-version>`:

1. run current FFI/header, export, native-consumer, and package-consumer gates;
2. run the mutation corpus and historical comparison;
3. for a deliberate break, advance the package/ABI line and loader identity and
   document migration in `CHANGELOG.md`;
4. stage the versioned library, header, ABI contract, CMake/pkg-config metadata,
   README, checksum, and provenance attestation together; and
5. never update or bypass a baseline merely to make a comparison pass.

Linux x86_64, macOS arm64, and Windows x86_64 each receive native contracts,
export checks, loader checks, and C11/C++17/package consumers. Reports must not
claim that an ELF-only tool proves Mach-O or PE/COFF compatibility.
