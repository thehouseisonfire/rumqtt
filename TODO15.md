# C ABI Containment and Pre-Stable Version Policy

## Decision

Replace the permanent exact-equality interpretation of the `rumqttc-c-next` ABI
baseline with two distinct invariants:

```text
current public header declarations == current rumqttc_* exports
previous compatible ABI ⊆ current ABI
```

The first invariant prevents accidental exports and missing implementations.
The second preserves every binary contract promised to existing consumers while
allowing deliberate compatible additions. Equality of old and new exported
symbol sets is not required during a compatibility-preserving release line.

Adopt an explicit pre-stable SemVer policy because the C wrapper is new and has
not yet earned a mature ABI-v1 promise. The Rust package version, C API/ABI
compatibility line, release tags, package metadata, headers, and native library
identity must tell one coherent story. Do not declare ABI 1.0 merely because the
first implementation exists.

This policy is independent of the comparator selected by `TODO14.md`. A
structural ABI tool is preferred if the evaluation proves it reliable, but the
required containment semantics apply equally to a focused custom comparator.

## Compatibility model

For a compatible release, “old ABI is contained in new ABI” means every public
binary contract from the selected baseline remains usable with the new shared
library. It is stronger than retaining symbol names. At minimum:

- every previously exported public function remains exported under the same
  name;
- its calling convention, parameters, return type, and machine-level type
  contract remain compatible;
- every previously exposed by-value struct retains compatible size, alignment,
  field order, field offsets, and field types;
- public typedef representations and stable numeric constants retain their
  promised values;
- opaque handle internals may change because callers cannot inspect or embed
  them;
- ownership, allocation/free pairing, lifetime, threading, and error-result
  contracts promised to compiled callers remain valid; and
- a library built for a compatible line remains loadable through the same
  platform library identity expected by existing binaries.

Containment is directional:

```text
old ABI ⊆ new ABI
```

It does not mean that an older library can satisfy a program compiled against a
newer header. Applications using a newly added function require the release
that introduced it. Document this normal minimum-version requirement.

## Additions policy

A new uniquely named C function may be added within a compatible release line
when all of the following hold:

- it is deliberately declared in the authoritative public header;
- it is exported by every applicable shared-library artifact;
- its types are themselves ABI-safe for the supported targets;
- it does not modify an existing function or public type contract;
- its ownership, threading, error, and availability semantics are documented;
- header/export equality and historical containment checks pass; and
- the package version is advanced according to this document.

Do not generalize “additions are compatible” to every source change. In
particular, the following require explicit classification and normally count as
incompatible within an established line:

- appending, removing, reordering, or changing fields in a public struct passed
  by value or allocated by the caller;
- changing the size, alignment, or meaning of an existing public type;
- changing parameter or return types, including widths and signedness;
- renumbering existing status, selector, event, or reason constants;
- changing ownership so a caller must free, retain, or synchronize differently;
- weakening thread-safety or lifetime guarantees;
- changing a symbol name or platform calling convention; and
- removing a symbol, even when repository source no longer calls it.

Adding a numeric constant or enum-like value can be binary-compatible while
still creating source-compatibility or exhaustiveness concerns. Report and
document source/API compatibility separately. Prefer the current fixed-width
typedef-plus-constant representation when extensibility is intended.

Accidental extra exports are not accepted as harmless additions. The current
header is the public allowlist: exported symbols in the `rumqttc_*` namespace
must equal declarations marked public in that header. Platform runtime symbols
outside the project namespace must be filtered or controlled deliberately and
must not make the project allowlist meaningless.

## Version lifecycle

Use the following lifecycle as the normative release policy.

### UNPUBLISHED

There is no compatibility baseline. Before the first published artifact:

```text
current header == current exports
```

The API and ABI may be redesigned freely while under review. CI must still
catch disagreement between Rust FFI definitions, the checked-in header, and the
built artifacts. Do not describe this state as stable ABI 1.0 and do not treat a
worktree snapshot as a historical compatibility promise.

### 0.1.0 release

The first published `0.1.0` artifacts establish the first ABI baseline for the
`0.1.x` line. Preserve the exact released headers and libraries, their
checksums/provenance, supported target triples, and the comparison metadata
required by `TODO14.md`.

The baseline is the published artifact, not merely the source immediately after
release. If CI must reconstruct debug/type information from the release tag,
prove and document its equivalence to the published binary boundary.

### 0.1.x releases

Patch releases in the `0.1.x` line must satisfy:

```text
0.1.0 ABI ⊆ new ABI
current header == current exports
```

Compatible additions are allowed. Existing contracts may be fixed internally
or clarified, but they may not be removed or changed incompatibly. Compare
against the line's floor (`0.1.0`) and consider also comparing against the most
recent published patch to catch a mistakenly removed addition introduced after
`0.1.0`. The implemented baseline strategy must guarantee that every contract
published anywhere in the line remains available; comparing only with the
first release is insufficient once `0.1.1` adds a symbol.

One acceptable strategy is a cumulative manifest representing the union of all
public ABI introduced in the line, backed by immutable released artifacts.
Another is comparison against every prior release in the line. A third is
comparison against the latest release plus a rule that every release was itself
contained in its successor. Choose the least costly strategy that remains
auditable and cannot silently lose an ABI addition from an intermediate patch.

### 0.2.0 release

`0.2.0` may deliberately break the `0.1.x` ABI. It establishes a new baseline
and ABI identity where platform loader behavior requires one. Breaking changes
must be enumerated in `CHANGELOG.md` with migration guidance.

Do not accidentally preserve a loader identity that causes an existing program
compiled for `0.1.x` to load an incompatible `0.2.0` library. Decide and test
the appropriate mechanism for each artifact format:

- ELF SONAME and the installed shared-library filename/symlink set;
- Mach-O install name and compatibility/current versions; and
- Windows DLL filename and import-library identity.

SemVer alone does not control a native dynamic loader. If the packaging model
ships versioned archives without system-wide loader selection, document that
constraint and still prevent silent replacement of an incompatible DLL or
shared object.

### Subsequent `0.y.z` releases

Repeat the same rule:

- `0.y.0` may deliberately establish a new incompatible ABI line;
- `0.y.z`, where `z > 0`, preserves the cumulative ABI of the `0.y` line and
  permits compatible additions; and
- every release maintains current-header/current-export equality.

A minor release is permission to break during pre-stable development, not an
obligation to do so. If no break occurs, record that it remains compatible; do
not invent churn solely to justify a minor version.

### 1.0.0 release

Declare the API and ABI mature only after the supported surface, ownership
model, platform packaging, and historical compatibility checks have proven
usable. `1.0.0` establishes the mature ABI line.

For all `1.x` releases:

```text
cumulative 1.x ABI ⊆ new ABI
current header == current exports
```

Compatible additions remain allowed in minor releases and compatible fixes in
patch releases. Continue containment semantics indefinitely; reaching 1.0 does
not freeze the library against growth.

### 2.0.0 release

Require `2.0.0` for an incompatible redesign after stabilization. Establish a
new ABI baseline and platform loader identity as appropriate, preserve explicit
migration documentation, and decide whether ABI 1 and ABI 2 artifacts need a
parallel installation period.

The same rule applies to subsequent stable major versions.

## Version identities to reconcile

The current tree contains potentially confusing independent values:

- Cargo package version `0.34.0-alpha`;
- C header macros declaring `RUMQTTC_ABI_VERSION_MAJOR 1` and minor `0`;
- Rust's internal `ABI_VERSION` value;
- README text promising “stable ABI version 1.0”;
- the `rumqttc-v1.symbols` baseline name;
- CMake/package metadata and unversioned library names; and
- release tags matching `rumqttc-c-v*`.

Before the first wrapper release, decide whether `rumqttc-c-next` follows an
independent `0.y.z` package version or remains lockstep with the wider
workspace. The policy requested here is clearest with an independent wrapper
version. If workspace lockstep is retained, define a separate pre-stable C ABI
line identifier and explain how it maps to the package version without calling
it ABI 1.0.

Update every identity atomically. At minimum, reconcile:

- `rumqttc-c/Cargo.toml` and the workspace lockfile;
- `RUMQTTC_ABI_VERSION_*` in the public header;
- the Rust value returned by `rumqttc_abi_version()`;
- `rumqttc_library_version()` and its distinction from ABI-line identity;
- README stability claims and compatibility tables;
- symbol/structural baseline names and metadata;
- CMake `VERSION`/`SOVERSION`, Mach-O, and Windows artifact naming as selected;
- pkg-config `Version`;
- release workflow tags and archive names; and
- changelog and release notes.

Avoid encoding SemVer prerelease strings into the existing packed `uint32_t`
ABI function without a documented representation. It may be clearer for
`rumqttc_abi_version()` to identify the native ABI line while
`rumqttc_library_version()` reports the full implementation package version.
Whatever representation is chosen must distinguish compatibility decisions
unambiguously and must not claim `1.0` before stabilization.

## Header/export equality

Replace the manually duplicated exact historical export list as the definition
of the current surface. Derive the expected current public symbols from an
authoritative machine-readable source, preferably declarations explicitly
marked `RUMQTTC_API` in `include/rumqttc.h`, and require:

```text
declared current rumqttc_* functions == exported current rumqttc_* functions
```

The extraction must be robust to multiline declarations, attributes, comments,
and formatting. Do not retain the current `sed` expression as the final parser
without tests proving those cases. Acceptable approaches include a compiler AST
or another maintained C parser, a generated export map derived from structured
FFI metadata, or the authoritative mechanism selected in `TODO14.md`.

The checked-in header remains a reviewed public contract. Continue comparing it
with cbindgen output so Rust declarations cannot drift silently. If generated
metadata becomes authoritative instead, document the review flow and prove that
the shipped header, export allowlist, and Rust definitions are derived from the
same reviewed surface without circularly comparing a file to itself.

Use platform export controls where practical:

- an ELF version script or controlled symbol visibility;
- a Mach-O exported-symbols list; and
- Windows `.def`/`__declspec(dllexport)` controls.

Generating these controls from the current public contract can prevent
accidental exports before the post-build check. Still inspect the final artifact
because build configuration and linker behavior are part of the release result.

## Historical containment check

The historical check must compare more than symbol-name sets when public types
or signatures can change. Use the structural comparator selected by
`TODO14.md`, if adopted. Otherwise implement focused evidence for:

- required old symbols being present in the new artifact;
- old and new function declaration compatibility;
- public struct sizes, alignments, and field offsets on each claimed target;
- stable numeric values and typedef representations; and
- the platform shared-library identity.

Do not update a baseline merely to make CI pass. Baseline changes require one of:

- the first release of a new compatibility line;
- a compatible addition to the cumulative manifest, backed by a published
  release; or
- correction of demonstrably incorrect baseline metadata without changing the
  promised artifact, documented in the pull request.

An intentional pre-stable break must be coupled to the new minor version and
new baseline in the same change. CI should reject a detected incompatibility
when only the patch version changed.

## CI and release workflow

Split the present broad check into understandable gates:

1. **FFI/header consistency:** checked-in declarations agree with Rust/cbindgen.
2. **Current export policy:** current header declarations equal current final
   artifact exports.
3. **Historical ABI compatibility:** the previous cumulative ABI is contained
   in the new ABI when the version promises compatibility.
4. **Native consumers:** C11 and C++17 programs compile, link, load, and run.
5. **Package consumers:** CMake and pkg-config installations work and relocate.

Run gates before a release tag is published. Release jobs may repeat them
against release-profile artifacts, but must not be the first place a breaking
change is detected. Preserve comparison reports and actual/expected export
lists as failure artifacts.

Before the first release, historical comparison must report a clear “no
published baseline” status and pass only the current consistency checks. It
must not silently compare against a developer-created symbol file and imply a
public guarantee.

For patch releases, CI must identify the cumulative baseline automatically and
fail closed if required baseline artifacts cannot be authenticated or fetched.
An unavailable baseline is not evidence of compatibility.

## Documentation contract

Update the public documentation to state:

- the package version and native ABI line separately;
- which release first established the current baseline;
- that compatible additions require consumers to compile against a sufficiently
  new header and run against at least that library version;
- which targets receive structural historical comparison versus native smoke
  coverage only;
- how long pre-stable compatibility is promised within a `0.y` line;
- that a new pre-stable minor may break ABI deliberately;
- that stable incompatible changes require a new major version; and
- how users can query implementation and ABI versions at runtime.

Do not use an unconditional phrase such as “stable ABI-v1” while the wrapper is
unpublished or following the pre-stable `0.y.z` lifecycle.

## Migration from the current worktree

Because the wrapper is currently unpublished, perform the policy correction
before treating `rumqttc-v1.symbols` as a released promise:

1. verify that no `rumqttc-c-next` release artifact or tag has already been
   published; if one exists, stop and derive the actual compatibility baseline
   from that artifact;
2. choose and document independent-versus-lockstep package versioning;
3. remove or revise premature ABI 1.0 claims in the header, Rust constant,
   README, tests, changelog, and baseline filename;
4. implement robust current-header/current-export equality;
5. implement no-baseline behavior for the unpublished state;
6. establish the first immutable baseline only when `0.1.0` is published;
7. configure loader identities for future pre-stable incompatible minor lines;
8. add fixture tests proving compatible additions pass and removals/signature or
   layout changes fail; and
9. document the release procedure for advancing cumulative baselines.

Do not delete the current symbol file until equivalent current-export and
historical-containment evidence exists. It may be renamed or generated during
the transition, but coverage must not temporarily disappear.

## Acceptance criteria

This TODO is complete only when:

- unpublished, `0.y.z`, `1.x`, and future-major compatibility rules are
  documented and enforced;
- the first published wrapper release establishes an immutable baseline rather
  than inheriting a development snapshot accidentally;
- current public header declarations equal current project exports on every
  published platform;
- every ABI promised by a compatibility line is contained in later releases of
  that line;
- a deliberately added function passes without weakening accidental-export
  detection;
- removals, signature changes, layout changes, and incompatible ownership or
  loader-identity changes are rejected when the version promises compatibility;
- an intentional pre-stable break requires a new minor line and baseline;
- a stable incompatible break requires a new major version;
- package, runtime ABI, native-loader, baseline, tag, and documentation versions
  are coherent;
- CI distinguishes ABI, header, native-consumer, and packaging evidence;
- mutation tests cover both compatible additions and representative breaks;
- `rumqttc-c/README.md`, `CHANGELOG.md`, and release procedures state the exact
  guarantee without calling the pre-stable wrapper mature; and
- the final implementation incorporates the tool decision from `TODO14.md`
  without making that investigation a prerequisite for defining the policy.

