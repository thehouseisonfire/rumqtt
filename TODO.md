# Publish the JavaScript Wrapper on JSR

## Direction

Publish a small ESM-only JSR package that re-exports the released npm package.
Keep `@rumqtt-next/rumqttc` on npm as the single implementation and native-artifact
source of truth. Do not copy Node-API binaries into JSR or maintain a second
JavaScript implementation for JSR.

JSR permits ESM packages to depend on npm packages. The JSR entry point can
therefore re-export the npm package at the exact matching version:

```ts
export * from "npm:@rumqtt-next/rumqttc@0.1.0-alpha.0";
```

This arrangement does not make the native addon registry-independent. Deno
consumers still require a local `node_modules` directory, the matching optional
platform package from npm, and `--allow-ffi`.

## 1. Publish npm first

Complete `TODO6.md` before publishing to JSR. The npm release must provide the
main package and all six optional platform packages at one version, pass the
clean-install runtime matrix, and retain the required checksums and provenance.

Do not publish the corresponding JSR version until the exact npm version is
available and its post-publication verification has passed.

## 2. Add the JSR facade

Create `@rumqtt-next/rumqttc` in the `rumqtt-next` JSR scope, link it to this
GitHub repository for OIDC publishing, and add a separate
`native-wrappers/jsr/` package containing only the files needed by JSR:

- `jsr.json` with the package name, version, `mod.ts` export, and an explicit
  publication include list;
- an ESM `mod.ts` that re-exports the public npm package API; and
- `README.md` and the project license, including the native runtime
  requirements.

Use the same package version in `jsr.json`, the npm dependency specifier in
`mod.ts`, and `native-wrappers/js/package.json`. Add an automated check that
fails when any of these versions differ.

Keep every JavaScript or TypeScript file published directly to JSR ESM-only.
The npm dependency may retain its CommonJS entry point. CommonJS files and
native binaries belong in the npm packages and must not be included in the JSR
publication.

## 3. Extend the release workflow

After the npm publication and verification jobs succeed:

1. validate that the JSR and npm versions match;
2. publish the JSR package through GitHub Actions OIDC;
3. install the package in a fresh Deno project through its `jsr:` specifier;
   and
4. run a broker-backed connect, publish, and clean-close smoke test.

The smoke test must use the published JSR and npm packages, not source-tree
paths or local archives. Run Deno with `--node-modules-dir=auto`, `--allow-ffi`,
and only the additional permissions required by the fixture. Verify that the
matching optional npm platform package supplies the addon without a lifecycle
build script.

Record the JSR package version and publication URL alongside the npm release
evidence. If JSR publication fails before the version is accepted, fix the
facade and retry the same version. If the immutable JSR version is accepted but
its post-publication verification fails, fix the problem and release a new
matching npm and JSR patch or prerelease version. Never republish or mutate an
existing npm release.

## 4. Document the support boundary

Document JSR as an alternate ESM import and discovery path, not as a portable
or sandboxed implementation. State explicitly that:

- the implementation and platform binaries are obtained from npm;
- Deno requires a local `node_modules` directory and `--allow-ffi`;
- browser JavaScript, Web Workers, Deno Deploy, and other hosts that cannot load
  Node-API addons remain unsupported; and
- Node.js and Bun users should normally install the npm package directly.

Do not add an async `init()` or factory API, top-level `await`, direct
`process.dlopen` loading, or JSR-hosted native artifacts as part of this work.
The existing construction and `connect()` lifecycle remains unchanged.

## 5. Treat a true ESM implementation as separate work

The current npm ESM entry point is valid for consumers but delegates to the
CommonJS implementation. Replacing that internal arrangement is not required
for the JSR facade and must not block its publication.

If a true ESM and CommonJS dual build is pursued later, use one canonical
TypeScript or ESM implementation, load platform packages from ESM with
`createRequire()`, and generate the CommonJS entry point and declarations during
packaging. Preserve public export identity and behavior across both entry
points, and cover Node.js ESM, Node.js CommonJS, Deno, and Bun with tests that
use installed packages.

## Completion criteria

This TODO is complete when:

- `TODO6.md` is complete for the same package version;
- the JSR package contains only its ESM facade, metadata, documentation, and
  license;
- automated checks keep the JSR version, npm version, and pinned npm specifier
  identical;
- GitHub Actions publishes JSR through OIDC only after npm verification passes;
- a clean Deno project imports the published `jsr:` package and completes the
  broker-backed smoke test using the matching npm native addon; and
- the runtime requirements and unsupported environments are documented without
  implying that JSR hosts or sandboxes the native implementation.
