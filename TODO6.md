# Publish the Node-API JavaScript and TypeScript Wrapper

## Completion target

Publish `@rumqtt-next/rumqttc` and its optional native platform packages to the
npm registry, then verify clean registry installations under Node.js 24, Deno
2.9.5, and Bun 1.3.14.

## 1. Finish the release workflow

Use Bun for the entire publishing flow. Replace the remaining npm CLI publish
commands in `.github/workflows/js-release.yml` with `bun publish --access
public --tag next` while preserving this order:

1. publish all optional platform packages;
2. publish the main package; and
3. run the post-publication verification jobs.

Keep npm Trusted Publishing through GitHub Actions OIDC. Do not introduce a
long-lived npm token.

Before publishing, make the workflow fail unless all of the following agree:

- the `rumqttc-js-v<version>` tag;
- the main package version;
- every platform package version; and
- every optional dependency version in the main package.

The release must also fail if any advertised platform archive, native binary,
SHA-256 checksum, or build-provenance attestation is missing or invalid.

## 2. Publish the packages

Create and push the release tag for the intended version. Publish these
packages, with the platform packages preceding the main package:

- `@rumqtt-next/rumqttc-linux-x64-gnu`;
- `@rumqtt-next/rumqttc-linux-x64-musl`;
- `@rumqtt-next/rumqttc-linux-arm64-gnu`;
- `@rumqtt-next/rumqttc-darwin-x64`;
- `@rumqtt-next/rumqttc-darwin-arm64`;
- `@rumqtt-next/rumqttc-win32-x64-msvc`; and
- `@rumqtt-next/rumqttc`.

Retain the generated checksums and attestations with the GitHub release.

## 3. Verify the published release

Install the published main package from the npm registry into fresh projects;
do not use source-tree paths, local tarballs, or a local registry for these
checks. Use a deterministic local MQTT broker for runtime verification.

Verify the following hosts:

- Node.js 24 on each executable release host, using both ESM `import` and
  CommonJS `require`;
- Deno 2.9.5 using `npm:@rumqtt-next/rumqttc`,
  `--node-modules-dir=auto`, `--allow-ffi`, and only the additional permissions
  required by the fixture; and
- Bun 1.3.14 using the package's public entry point.

Each verification must load the native addon through the main package, connect
to the broker, publish a message, and close cleanly. Confirm that the matching
optional platform package was selected without an npm lifecycle build script.

Record the following release evidence:

- the exact package version and dist-tag;
- runtime versions;
- host operating systems, architectures, and Linux libc variants;
- commands and results for every clean-install smoke test;
- package archive checksums; and
- links to the build-provenance attestations.

## Completion criteria

This TODO is complete when:

- the release workflow publishes exclusively through Bun;
- the main and all six platform packages are available from the npm registry at
  the same version;
- all release archives, binaries, checksums, and attestations are present and
  validated;
- clean registry installations pass broker-backed smoke tests under Node.js
  24, Deno 2.9.5, and Bun 1.3.14; and
- the release evidence is retained with the corresponding GitHub release.
