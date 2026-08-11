# Structural C ABI Comparison Tool Evaluation

## Goal

Determine whether `rumqttc-c-next` should replace or supplement its current
hand-written ABI checks with an established ABI comparison tool. Select and
adopt a tool only if it gives a more correct, idiomatic, reproducible, and
maintainable proof of the compatibility requirements than the existing
combination of generated-header compilation, native consumer tests, struct-size
assertions, and exported-symbol comparison.

This is an evidence-gathering and tooling task, not a mandate to install the
first available ABI checker. The result must identify exactly which ABI
properties the selected tool proves, which properties remain outside its
model, and which existing checks must remain because they validate packaging,
loading, ownership, behavior, or unsupported platforms rather than binary
compatibility.

The C ABI is the boundary declared by `rumqttc-c/include/rumqttc.h` and exported
by the `cdylib`/platform shared-library artifacts. Rust API compatibility for
`rumqttc-wrapper-core-next`, `rumqttc-v4-next`, or `rumqttc-v5-next` is outside
this investigation. Rust implementation changes behind unchanged C declarations
must not be reported as C ABI changes.

## Current state

`rumqttc-c/tests/abi/check.sh` and `check.ps1` currently perform several
different jobs:

1. build the Rust static and shared libraries;
2. validate CMake and pkg-config metadata;
3. compile and load C11 and C++17 consumers;
4. compare checked-in and cbindgen-generated declarations;
5. compile the checked-in and generated function declarations together;
6. assert selected C-visible type sizes in the native smoke test; and
7. require the exported `rumqttc_*` symbol set to equal a checked-in list.

These checks provide useful coverage, but symbol names and selected `sizeof`
values are not a complete structural ABI comparison. They do not, by
themselves, prove all field offsets, alignments, parameter and return types,
calling conventions, type changes behind typedefs, or platform-specific data
model differences. Conversely, an ABI diff tool will not prove runtime
ownership rules, panic containment, timeout semantics, CMake/pkg-config
relocatability, or successful loading by real C and C++ consumers.

Do not collapse all these responsibilities into one tool or call every native
consumer and packaging check an “ABI check.” Give each CI step a name matching
the property it verifies.

## Compatibility requirements to model

The evaluation must determine whether a candidate can reliably detect, at
minimum:

- removal or renaming of an existing exported C function;
- a changed parameter count, order, type, pointer qualification, or return type;
- a changed platform calling convention or symbol decoration where applicable;
- incompatible changes to the size, alignment, field order, field offset, or
  field type of a non-opaque C struct;
- incompatible changes to unions if the public ABI later exposes any;
- changes to the representation or values of public constants and enum-like
  types where the tool supports them;
- changes to public typedefs that alter the resulting machine-level contract;
- accidental exposure of implementation-only exports;
- intentional addition of a new declared function without misclassifying the
  existing ABI as broken; and
- differences between shared-library artifacts and the checked-in public
  header.

The tool must support the containment policy specified in `TODO15.md`: a
compatible release preserves the previous ABI while permitting deliberate
additions. Exact equality of old and new symbol sets is not the desired
long-term compatibility definition.

Document source-compatibility findings separately from binary-compatibility
findings. For example, a new macro, constant, declaration, or enum-like value
may affect source consumers without changing already-compiled binaries. Do not
label a report “ABI compatible” if it silently combines or confuses those
categories.

## Candidate tools

Evaluate credible maintained tools rather than designing a parser from scratch.
At minimum, investigate:

### 1. libabigail (`abidiff` and related tools)

Determine whether libabigail can compare the actual ELF shared objects built by
this repository with sufficient DWARF/type information. Establish:

- which build profile and debug-information settings are required;
- whether release stripping or LTO prevents useful analysis;
- whether a dedicated unstripped ABI-analysis artifact is equivalent to the
  shipped artifact for exported symbols and public layouts;
- whether Rust-produced C ABI functions and cbindgen declarations are modeled
  accurately rather than as unstable Rust internals;
- how suppression files would be scoped and reviewed; and
- whether the tool's exit status and report are stable enough for CI.

Prefer comparing the public C boundary only. Rust monomorphizations, compiler
internals, anonymous implementation types, and private debug information must
not create compatibility noise.

### 2. ABI Compliance Checker with ABI Dumper

Determine whether `abi-compliance-checker` and `abi-dumper` provide a clearer
header-plus-library model for this C API. Evaluate their handling of:

- opaque handles versus by-value public structures;
- fixed-width typedefs and `size_t`;
- function additions, removals, and signature changes;
- architecture-specific layouts;
- report reproducibility and machine-readable output; and
- supported compiler, DWARF, distribution-package, and licensing constraints.

Do not select this pair merely because it produces an HTML report. Its findings
must be validated against known compatible and incompatible changes.

### 3. Other credible approaches

Briefly assess any better-maintained or more suitable alternatives discovered
during implementation. Examples may include compiler-native AST/layout dumps or
platform-specific binary inspection. `cargo-semver-checks` is not, by itself, a
candidate for this boundary: it checks Rust public APIs and cannot establish C
shared-library ABI compatibility. Likewise, `nm`, `dumpbin`, or equivalent
symbol listing alone remains useful export evidence but is not a structural ABI
comparator.

## Controlled mutation corpus

Do not choose a tool from documentation claims alone. Create a temporary or
test-fixture mutation matrix based on the real `rumqttc-c` API and record each
candidate's result. The corpus must include at least:

| Mutation | Required classification |
| --- | --- |
| Change only Rust implementation internals | compatible/no ABI change |
| Add a declared and exported function | compatible addition |
| Remove an existing function | incompatible |
| Rename an existing function | incompatible removal plus addition |
| Change a scalar parameter width | incompatible |
| Change pointer constness | report source/API difference; document ABI-tool treatment |
| Change return type | incompatible |
| Reorder fields in a public struct | incompatible |
| Change a public field type without changing total size | incompatible |
| Change struct alignment or a field offset | incompatible |
| Append a field to a by-value public struct | incompatible |
| Change an opaque handle's private Rust representation | compatible/no ABI change |
| Add an undeclared exported `rumqttc_*` function | export-policy failure |
| Change only comments or documentation | compatible/no ABI change |

Where a mutation has different results on LP64 and LLP64 data models, test and
record that difference. A candidate that misses a required incompatible case
must not become the sole compatibility gate. A candidate that reports routine
Rust implementation changes must either be narrowly and transparently
configurable or be rejected.

Check mutation fixtures into the repository only if they remain compact and
provide durable regression value. Otherwise, record a reproducible generator or
script and preserve the evaluation report under `docs/`.

## Baseline source and reproducibility

The comparison baseline must come from a released, immutable artifact or an
exact release tag, never silently from the merge base of an arbitrary pull
request. Establish and document:

- how CI identifies the relevant previous release under the `TODO15.md`
  version policy;
- whether it downloads an attested release archive or rebuilds an exact tag;
- how checksums or provenance are verified before comparison;
- how the baseline header and platform library are paired;
- what happens before the first release, when no compatibility baseline exists;
- how an intentional breaking minor/major release starts a new baseline; and
- how contributors reproduce the comparison locally without credentials.

Prefer comparing published artifacts because ABI promises apply to what users
received. If reproducible comparison requires rebuilding a tag with debug
information, first prove that its public symbols and layouts match the shipped
artifact and clearly state the limitation.

Pin the comparison-tool version in CI. Do not rely on an unversioned distro
package whose diagnostics or compatibility rules may change without review.
Record the supported host architecture, compiler/toolchain, object format, and
installation method.

## Platform strategy

Select an explicit platform strategy rather than pretending an ELF-only tool
proves Mach-O and PE/COFF compatibility.

One acceptable result is:

- run deep structural comparison on Linux x86_64 using a well-supported ELF
  tool;
- keep current-header/export consistency and native C/C++ load tests on Linux,
  macOS arm64, and Windows x86_64; and
- retain targeted size, alignment, and offset assertions on platforms the deep
  comparator does not support.

If a candidate supports Mach-O or PE/COFF reliably, demonstrate that support
with the mutation corpus before using it as an authoritative gate. Otherwise,
state precisely that those platforms receive native contract smoke coverage,
not a full historical structural comparison.

Consider whether each supported architecture needs an independent baseline.
ABI layouts are target-specific; passing on Linux x86_64 cannot prove a Windows
x86_64 or macOS arm64 layout. The published README and release notes must not
claim broader verification than CI performs.

## Tool-selection decision

Write a short decision record under `docs/` containing:

- tools and pinned versions evaluated;
- the controlled mutation results;
- false positives, false negatives, and unsupported properties;
- baseline acquisition and reproducibility findings;
- expected CI installation and runtime cost;
- supported targets and object formats;
- the chosen authoritative and supplemental checks; and
- a clear adopt, supplement, or reject decision.

Adopt a structural comparator only if it:

1. detects every machine-level incompatible mutation in its claimed scope;
2. accepts internal Rust changes and compatible function additions;
3. can be pinned and reproduced locally and in CI;
4. produces actionable diagnostics;
5. compares a trustworthy historical baseline; and
6. is simpler or materially more correct than maintaining equivalent custom
   logic.

If libabigail satisfies these requirements for ELF but no comparable
cross-platform tool does, prefer it as the Linux structural gate and explicitly
retain platform-native supplemental checks. If another candidate performs
better on the mutation corpus, select it and record why. If no candidate meets
the requirements, keep the focused custom checks, strengthen layout coverage,
and document the evidence rather than adopting unreliable tooling for its own
sake.

## Integration plan after adoption

If a tool is selected:

1. add a focused script for obtaining and validating the historical baseline;
2. add a focused script for structural ABI comparison;
3. pin or reproducibly install the tool in CI;
4. keep current-header versus current-export validation separate;
5. retain checked-in versus generated declaration compatibility checks until
   the selected tool demonstrably subsumes them;
6. retain C11/C++17 compile-and-load tests on every published platform;
7. move CMake and pkg-config checks into clearly named packaging/consumer steps;
8. preserve ABI reports as CI artifacts on failure; and
9. document the local reproduction command in `rumqttc-c/README.md`.

Do not make the release workflow download its own just-published artifact as
the first time compatibility is checked. Pull-request and pre-tag CI must catch
incompatibility before publication.

## Acceptance criteria

This TODO is complete only when:

- an evidence-backed tool-selection decision exists;
- the mutation corpus and results cover all required cases;
- the selected solution implements old-ABI-contained-in-new-ABI semantics;
- current declarations and current exports are also checked for equality;
- baseline provenance and no-baseline behavior are deterministic;
- Linux, macOS, and Windows coverage is described without overclaiming;
- CI jobs have focused names and preserve useful failure diagnostics;
- existing behavioral, native-consumer, and packaging coverage is not lost;
- the selected checks pass against a known compatible release pair and reject
  every incompatible fixture in their claimed scope; and
- `rumqttc-c/README.md` and `CHANGELOG.md` describe the resulting compatibility
  guarantee and supported targets.

