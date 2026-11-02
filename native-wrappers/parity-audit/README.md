# Repository parity review

This unpublished Rust workspace tool requires the complete source checkout. It
checks explicit review decisions for discovered declarations at native Rust → wrapper-core and wrapper-core
→ C boundaries. It adds no runtime dependency or public client API.

This is a maintenance gate for the configured repository surfaces and the syntax
documented below. A successful check means the discovered inventory agrees with
the reviewed ledger. It does not certify that every API expressible in arbitrary
Rust has been discovered. Compiler, C header/export, conversion and platform
checks remain separate release evidence.

From the repository root:

```sh
cargo run --locked --manifest-path native-wrappers/Cargo.toml -p rumqttc-parity-audit -- check
cargo test --locked --manifest-path native-wrappers/Cargo.toml -p rumqttc-parity-audit
```

`check` requires every configured manifest/source tree, validates nonzero
inventories, declaration shapes, boundary decisions, mapping targets, evidence
references, supplements and generated documentation. Missing repository inputs
are errors. There is no packaged-source skip: published wrapper-core tests do
not contain this repository audit, and the tool tests verify that separation.
The repository-only JSON ledger is also excluded from the published package.

## Reviewing a declaration change

`inventory` prints current discovered declarations as JSON without accepting or
writing anything. Compare it with `wrapper-core/parity.json`, review the source
and conversions, then edit the expected declarations and explicit decisions.
Identifiers contain the surface, crate/module/type/member, declaration kind and
feature conditions. Inherent member identifiers also retain self-type arguments,
so `Options<u16>::new` and `Options<u32>::new` can have independent decisions.
Methods and fields with the same spelling remain distinct. Normalized shapes
include impl self-type arguments, generic `where` bounds, API-relevant enum
variant attributes and each payload field's feature/platform predicates.
Struct shapes retain unit/tuple/named constructor forms and the presence and
availability of private fields that block external construction, without private
field names or types. Trait method shapes retain whether a default body exists.
Synchronous/asynchronous overloads retain separate declarations even when a
reviewed decision groups their equivalent managed operation.

Each native source needs a native-to-core decision; each core source needs a
core-to-C decision. C declarations must be named targets or have a C-adapter
decision. Supported/partial decisions need existing target symbols and named
behavior evidence. Exclusions and pending capability decisions need explicit
rationale; deferred capabilities have no fabricated declaration targets.

The JSON ledger is authoritative. The initial migration has exact field
projections and broader partial managed-interface decisions. A partial group
records the listed projection and restrictions; it does not assert that each
Rust argument, field, constructor or lifetime has an identical C representation.
Newly discovered members are explicit sources, never wildcard matches. A discovered declaration change
cannot silently join an existing group.

After reviewing ledger edits, `render` prints the generated Markdown section:

```sh
cargo run --locked --manifest-path native-wrappers/Cargo.toml -p rumqttc-parity-audit -- render
```

Replace only the section between `parity-audit:begin` and `parity-audit:end` in
`wrapper-core/PARITY.md`, preserving the narrative. Then run `check`. There is
no baseline-update or automatic-accept command.

## Discovery limits

Unsupported forms reported by discovery must be rewritten using supported syntax
or receive narrowly scoped discovery support. Supplements review known generator
anchors; they cannot override discovery errors. Prioritize extensions needed by
this repository or plausible maintenance changes. A synthetic Rust counterexample
alone does not require broadening the resolver or blocking a release; assess its
effect on the documented scope and actual exported API first.

Each manifest is parsed as TOML to select `[lib].path`, relative to its package
directory, or the default `src/lib.rs`. The selected root's containing directory
controls ordinary and `#[path]` module lookup, regardless of its filename.
Unused `src/lib.rs` files are excluded. Malformed TOML, invalid source-selection
fields, missing selected sources, virtual manifests and packages without a library target fail explicitly;
`package.autolib = false` is supported when an explicit `[lib]` target exists.

The ledger lists native root types explicitly. Public methods, fields and enum
variants within those types are discovered, including disabled feature/platform
branches. Core/C discovery follows local public exports and module paths. Source
trees are traversed through inline, ordinary file and literal `#[path]` modules;
local explicit/glob reexports resolve to definitions, with separate alias entries.
On the C surface, source-level functions and statics with `no_mangle` or
`export_name` (including `unsafe(...)` and conditional forms) are independent
inventory roots, regardless of Rust visibility, module visibility or explicit
Rust observation roots. Their ABI metadata and type dependencies require review.
`link_section` alone does not make a private item an export. Built-library symbol
and header checks independently establish what each target actually exports.
Intermediate public reexports and their predicates remain review inputs when
the export passes through a private module, including unresolved external targets.
Private imports used as qualifiers along those export paths retain their predicates too.
Exported modules have separate declaration records, including empty modules and
public module ancestors of explicitly selected definitions. Their visibility and
availability require review even when a root reexport keeps the same child type
reachable. Module records omit private bodies and use both outer and file-level
conditions; known test-only modules and unexported implementation namespaces stay
outside module-path coverage.
Local enum glob and named-variant reexports retain the selected variants, payloads,
conditions, and their owning enum's metadata, methods and generator anchors.
Named variant reexports do not expose unrelated variants in the inventory.
An implicit variant also retains the preceding discriminant inputs that determine
its value. These records include source order, expressions and availability,
follow conditional alternatives recursively and stop at explicit value resets.
They retain referenced constants and aliases without exposing predecessor payloads
or methods. Already inventoried full variants need no duplicate value records;
known test-only variants remain excluded before assigning source indexes.
Traversal carries Rust's separate bases for ordinary children and explicit paths,
including path-attributed files and inline modules; arbitrary filenames do not
determine a path-attributed module's child directory.
Conditional module `path` attributes inside `cfg_attr` are rejected, including
nested conditionals and inner module attributes. Branches known to be disabled
outside tests are ignored. Express alternatives as separate `#[cfg]` module
declarations with literal `#[path]` attributes so both source trees are audited:

```rust
#[cfg(unix)]
#[path = "unix.rs"]
pub mod selected;
#[cfg(not(unix))]
#[path = "selected.rs"]
pub mod selected;
```

Grouped `self` imports bind the preceding module name, preserving explicit renames.
Absolute `::` markers survive grouped and glob import flattening, declaration
identities and shapes, and path resolution. Absolute paths start in the extern
prelude for these Rust 2024 workspaces and never resolve through a same-named
local module or import. Explicit crate aliases in that prelude remain review
inputs; external definitions remain outside discovery. Alias
targets, impl owners and API type references use the same path representation.
Source paths use ordinary local and extern-prelude lookup even when an identifier
matches the configured crate name. Only `crate` directly selects the source crate
root; explicit `extern crate self` aliases remain supported. Inventory-qualified
definition identities are used separately for observation roots and ledger entries.
Module and declaration bindings remain distinct through imports and reachability.
A public function, constant or static sharing a module's name retains its own
review decision; a public value does not expose a same-named private module.
Reachability links also distinguish type, value, module and crate declarations.
Tuple and unit struct types have separate constructor bindings. Tuple constructor
imports honor the visibility of every active field, including restricted scopes
and conditional private fields. An inaccessible constructor cannot shadow a glob
value sharing the type's name; the defining module and permitted descendants keep
their local constructor access. Restricted module declarations participate in
internal glob resolution without becoming public module exports.
Functions, constants and statics do not expose a same-named private type's methods,
associated constants, aliases or generators. Type references retain their own
members and generator inputs, while constructors imported in the value namespace
still expose the constructed type's public API. Explicit observation roots select
recorded definitions rather than unrelated hidden locals sharing their spelling.
Conditional imports retain their predicates through explicit/glob import chains,
module bindings and type aliases when resolving inherent implementation owners.
Alternative bindings to the same owner retain a symbolic union of availability.
Local declarations and named imports take precedence over glob candidates in
each type/value namespace, independently of source order. Conditional names
shadow candidates only where their predicates apply; remaining glob alternatives
retain the complementary conditions. Predicate reasoning is symbolic and does
not infer platform relationships or select the host's feature configuration.
Each named-import blocker combines the resolved target's predicates with the
import's own predicates, including conditions carried through reexports. A target
that changes namespaces across configurations shadows only its active namespace.
The import graph is rebuilt with precedence before attaching inherent members,
so discarded candidates cannot retain invented methods from an earlier pass.
An opaque dependency import overlapping a local glob candidate has an unknown
namespace. Reachable uses of that ambiguity fail explicitly; qualify the API
type or import the intended local type explicitly. Unused ambiguities and
known-disjoint availability do not fail discovery.
Conditional local type aliases retain every branch's underlying public members.
Concrete local structs, enums, unions and traits referenced by audited signatures,
fields, generic arguments/bounds and associated types retain their public members
recursively, including definitions inside private modules. Methods, callback
types, variant payloads and selecting aliases/imports participate in this closure;
cycles terminate without exposing unrelated private fields or sibling types.
An impl's owner is a selecting input rather than an exposure of every sibling
enum variant. Its generic arguments and actual public member types still
participate in recursive discovery.
Aliases referenced by audited fields, payloads, signatures, generic bounds/defaults,
associated types, constants and statics retain their definitions recursively,
including private aliases and non-path targets such as tuples and function types.
Their selecting imports and feature/platform predicates remain separate review
inputs, including restricted aliases reached through internal glob imports.
Constant, static and inherent associated-constant initializers also retain
referenced aliases and selecting imports, even when their declared types do not.
Value expressions in public types, including array lengths and const arguments,
retain referenced local constants and their initializer dependencies recursively.
Bare generic arguments such as `Bytes<N>` are checked in both type and value
namespaces because their parameter kind is ambiguous in syntax alone. Known
type parameters take precedence, and const parameters shadow value dependencies;
explicit braced const arguments keep value lookup.
Private constants and associated constants reached through type aliases or
`Self` are selecting inputs too; their visibility, predicates and generating
attributes require review. Referenced enum discriminants retain their variant
and enum metadata. Unused private constants and anonymous assertions are excluded.
Anonymous constants are inspected before being excluded. Pure computations,
local values/helper functions and proven standard assertion/literal macros remain
outside the inventory. Nested implementations, modules, types and imports require
block-scope discovery support and fail explicitly; they are never flattened into
the enclosing module. Generating/linkage attributes and unresolved macros also
fail, including macros named `assert` selected through local bindings or imports.
Assertion arguments, nested expressions and type expressions are inspected, with
known test-inactive declarations skipped before checking. Move unsupported
declarations to module scope so they can receive ordinary review and supplements.
Opaque macro blockers follow the assertion's lookup scopes and conditions.
Unrelated private modules do not block standard assertions. Absolute `::std` and
`::core` paths check crate-alias bindings first; opaque root generators remain
conservative blockers because they can introduce those bindings.
This follows source references without evaluating constants or executing calls.
Qualified inherent values such as `<Limits>::N` and `<Self>::N` retain the same
owner, selecting imports, aliases and initializer dependencies as `Limits::N`.
Generic arguments retain their own type and value dependencies. The owner is a
value qualifier, so it does not expose unrelated public fields or methods.
Trait-qualified value expressions, nested associated selections and non-path
owners are rejected when their declarations are reached; these forms require
additional resolution support. Unreachable and known test-only declarations are
excluded before that check.
Direct imports used by audited types are retained too, including trait bounds
and qualified paths, even when no local type alias or dependency definition can
be resolved. Selecting imports are review inputs; dependency internals remain
outside discovery. Unresolved external globs are retained conservatively.
Alias/import shapes distinguish restricted visibility from public exports, so
retaining a type dependency cannot hide the removal of a public alias.
Type parameters and const parameters shadow their own namespaces separately.
Array element types use type lookup, lengths use value lookup, and qualifiers
such as `N::BITS` select `N` in the type namespace. Selecting imports follow the
reference's namespace too. Unrelated private fields, bodies,
test-only declarations and unselected enum variants do not introduce dependencies.
Procedural attributes on referenced private aliases require full-input supplements;
unreferenced private aliases stay outside the inventory.
Public `extern crate` declarations retain their crate name, rename, visibility,
attributes and conditions. Private crate aliases used by audited declarations
remain selecting inputs through qualified paths, imports and internal globs;
unused private crate aliases are excluded. `extern crate self` aliases resolve
local declarations and implementations, including through absolute paths, while
dependency crate contents remain opaque.
Inherent implementations follow alias chains to the underlying type,
normalizing parenthesized/grouped owners for resolution, generic shapes and `Self`
dependencies. Parentheses alone do not change reviewed declarations; generating
attributes still retain their complete input. Other unsupported inherent owners,
including type macros, fail explicitly instead of dropping their public members.
Alias targets also normalize parentheses and groups before entering the owner
graph, including private chains, generic defaults and conditional alternatives.
Generating outer attributes on trait impls use the same full-input supplements
and provider lookup as inherent impls when their local owner is reachable.
Their headers retain selecting imports, aliases, generics and trait arguments.
Module generators enclosing those impls are retained through the owner as well.
Ordinary trait members and their bodies remain outside the declaration inventory.
Attributed impls with unresolved or unsupported owners fail explicitly until their
generator inputs can be anchored to a local implementation type.
Alias expansion preserves predicates while substituting type, lifetime and const arguments,
including defaults, after intersecting each branch with the impl's availability. Impossible branches
are discarded before substitution, so conditional aliases may have different arities.
Branch availability remains a symbolic union. Alias chains
that only cycle without reaching an underlying implementation type are errors.
Alias type arguments retain source spelling rather than compiler-resolved type
identities. Distinct local argument types with the same spelling in different
definition scopes can therefore collide in specialized member identities and fail
with a duplicate-declaration error. That pattern remains unsupported; qualify
the argument paths, for example `Container<crate::a::Arg>` and
`Container<crate::b::Arg>`, rather than depending on implicit scope resolution.
External reexports are recorded as declarations; their dependency internals are
not resolved. Shared native observation roots are inventoried separately.

The tool preserves feature/platform predicates symbolically and excludes
test-only items before assigning field and variant indexes. Inserting a test-only
field or variant does not shift public member shapes or tuple field identities.
Effective test inactivity is checked through nested `cfg_attr` before loading
module files, following imports, inspecting generators or assigning indexes.
Unknown feature/platform alternatives remain in discovery.
Ordinary private fields and symbolic feature/platform branches still count toward
those indexes. It does not establish which feature combinations compile or
which platforms execute. Conditional export aliases and definition conditions
are separate review inputs, not compiler-level availability inference.

Item macro definitions and invocations in audited modules require reviewed supplements.
Private `#[macro_use] extern crate` declarations are selecting inputs for
unqualified invocations and helper calls, including calls before the import.
Their conditions and macro-import order require review; dependency macro bodies
remain outside source discovery.
Audited item invocations also retain their lexical macro inputs, including
definitions inherited from parent scopes and private `#[macro_use]` modules.
Ordinary named, renamed, glob and qualified macro imports retain selecting use
declarations, local definitions and reachable generator attributes recursively.
Module qualifiers use type lookup; terminal macro names have separate lookup so
ordinary type/value bindings cannot suppress local macro definition discovery.
Possible import routes are retained conservatively, while dependency-internal
macro definitions remain outside this source inventory.
Literal helper calls in reachable macro rule bodies participate recursively,
including `$crate` paths, qualified private-module paths and selecting reexports.
Matcher tokens are excluded from dependency collection. Ordinary helper lookup
conservatively checks both definition and invocation scopes, and cycles terminate.
Macro-variable and synthesized expansion paths are rejected when reached; use
literal helper paths so their local definitions can be reviewed. This traversal
does not expand macros or evaluate their arguments.
Nested imports, earlier invocations that can generate macros and module generators
participate recursively. These scopes conservatively retain potential helper
definitions and conditional alternatives without expanding macro rules. Separate
`macro_use` anchors retain import order, since later imports can select a different
same-named definition. Unimported private modules and known test-only inputs remain
excluded. Associated-item invocations use the scope of their impl's source module.
Macros inside reachable types or constant expressions are rejected explicitly,
including expressions reached through private aliases and constant initializers.
The scanner does not resolve their macro namespace or lexical `macro_use` scope,
so a supplement on an item macro cannot establish these expansion dependencies.
Use explicit type and constant syntax for these declarations. Unreachable private
declarations, known test-only members and function bodies do not trigger this error.
Definitions with `macro_export` require supplements through their crate-root
export even inside private modules or outside explicit observation roots.
Their defining-module conditions, conditional export attributes and module
generator anchors remain review inputs; unrelated private sibling macros do not.
This includes associated-item macros, unknown derives and procedural attributes
on public named declarations (including aliases, unions, constants and statics),
reachable modules (including empty modules, reexported modules and ancestors of
explicit roots),
trait methods/associated types, and inherent implementations of audited types,
including empty impl blocks. Attribute anchors include the declaration kind to
distinguish names shared by type and value namespaces, and retain full macro inputs.
Generating attributes on private and restricted inherent methods also require
supplements when their owner is reachable. Their complete method inputs, signature
dependencies and selecting provider imports are retained, while ordinary private
methods remain outside the inventory. Known test-only methods and unreachable
owners do not add generator reviews.
Generating attributes on reachable reexports and imports used by audited types
require explicit supplements, including intermediate reexports in private modules,
globs, anonymous imports and empty public use trees. Each attributed use declaration
has one full-input anchor shared by its imported names; changes to sibling imports,
helper attributes or visibility invalidate its review.
Every reachable procedural attribute retains its provider imports and reexports,
including renamed, glob, qualified and raw-identifier paths through private modules.
Providers are resolved in the item's source module, including impls defined apart
from their types and attributes on modules or item macros. Nested `cfg_attr`
providers retain selecting-import predicates. Traversal continues through newly
reached module attributes before completing review. Inactive and unreachable
attributes introduce no provider dependencies.
Generating outer attributes on item macro invocations and definitions make the
entire attributed item part of its existing supplement anchor, including helper
attributes and raw enum input. Adding, removing or changing a generating attribute
requires renewed review. Without a generating attribute, benign outer documentation,
lint edits and test-inactive generators retain the existing normalized shapes.
Invocation tokens always remain complete opaque inputs, including enum, variant
and field documentation. Enum-shaped inputs use readable identities without
normalizing their reviewed shapes; editing documentation inside an invocation
therefore invalidates its supplement even when the identity stays the same.
Module attribute inputs include private declarations and bodies in inline modules;
changing those inputs invalidates the reviewed supplement. Ordinary module `path`
attributes do not require generator supplements.
Every nested attribute in `cfg_attr` is inspected
recursively; documentation and lint attributes do not exempt neighboring generators.
Impl attributes retain full block inputs and separate anchors per self type,
source module and attributed-block ordinal. Adding or reordering attributed
blocks in that scope requires reviewing any affected anchors.
Standard value/formatting/serialization/error derives are trait-implementation
support, outside the standalone method inventory, when their bindings are proven
to use the trusted providers. Derive lookup follows named imports, aliases and
local globs before applying that exemption. Imported generators named `Clone` or
other builtin spellings require full-input supplements and retain their selecting
imports. Unknown glob or macro-use namespaces are treated conservatively and also
require review. Import resolution includes declarations after the derived item
and nested `cfg_attr`; known test-only derives do not introduce anchors.
The `error`, `source`, `from` and `backtrace` helper exemptions apply only to
structs and enums where a resolved `thiserror::Error` derive registers them.
Conditional derive and import availability must cover the helper's availability.
Otherwise these names require generator supplements and retain provider imports,
just like other procedural attributes.
The public native-failure enums and native-store adapters have explicit output
declarations; private thread-local/Debug/worker generators have explicit empty
output decisions. Changing a generator or invocation invalidates its anchor.
New audited roots or unresolved generated/reexported APIs require an explicit
scope/supplement review. This is a bounded syntax audit, not a Rust compiler,
arbitrary macro expander or complete dependency API resolver.
The ledger lists additional signature dependencies explicitly. Standalone native
driver/session representations and Rust construction utilities remain excluded;
value groups without exact projection evidence carry pending boundary decisions.
Those decisions enforce declaration review without asserting runtime parity.

Trait methods and associated types are discovered directly. Other trait members,
including associated constants and item macro invocations, are rejected only
when their owning trait is reached through exports, explicit roots or public
type dependencies. Unreachable traits do not fail discovery. Known test-only
members are filtered before dispatch, including nested `cfg_attr` conditions;
feature/platform alternatives remain symbolic and still require support when
reachable. Deferred errors identify the owning trait and unsupported member.

Public foreign functions, statics and types in `extern` blocks are rejected when
reachable through audited exports or explicit roots. Foreign types referenced by
audited API types are rejected too. These declarations need explicit discovery
support before they can be reviewed; alternatively expose a reviewed Rust wrapper
around private foreign declarations. Private, known test-only and unreachable
foreign declarations do not require public declaration decisions.
Foreign-item macro invocations are also rejected in audited source scopes,
including private modules exposed through reexports or explicit observation roots.
An existing supplement for their macro definition cannot account for a new foreign
invocation. Known test-only invocations and unreachable private scopes are excluded;
unknown feature/platform branches remain subject to the error.
Public static shapes retain ABI attributes, including `unsafe(export_name = ...)`,
`unsafe(no_mangle)` and `unsafe(link_section = ...)`, alongside their type,
mutability and initializer. Conditional ABI attributes, including linker sections,
retain their complete `cfg_attr` predicates and metadata through nested conditions.
Known test-inactive attributes are ignored. Metadata classification parses attribute
syntax rather than searching documentation strings; ordinary documentation and
lint edits do not change the static's shape. Existing `doc(cfg(...))` availability
annotations remain review inputs.

## Behavior and declaration evidence

Normalized declaration shapes omit function bodies. Evidence references verify
that named Rust functions or C fixture entry points still exist; executing those
tests remains necessary. Existing C header/export checks independently establish
declaration and ABI consistency, rather than retained field values or semantics.
C evidence matches complete code identifiers followed by `(`, allowing whitespace
and comments between tokens. It ignores comments and string/character literals,
and joins escaped newlines before matching. This checks source names without
expanding preprocessor macros or parsing C declarations.

The isolated conversion regression runs a passing recovery-gate projection test,
removes its `recovery_pending` assignment in a temporary checkout, verifies that
declarations still pass, and requires the real retained-value assertion to fail:

```sh
python3 native-wrappers/parity-audit/tests/conversion_mutation.py
```

Run after building the audit tool. It writes only under `native-wrappers/target`,
keeps mutant libraries in a separate target directory, and requires the existing
offline Cargo dependency cache. Source inventory fixtures separately prove that
adding a field without a decision fails. Platform/performance evidence and the
experimental/parked/planned capability distinctions remain explicit in the ledger.
