# TODO

- [ ] Produce a true ESM build of the JS wrapper. Today `index.js` is only a re-export shim over `index.cjs`; the implementation is CommonJS (`index.cjs`, `loader.cjs`), and JSR rejects CommonJS modules outright.
- [ ] Publish the JS wrapper to JSR. Blockers: the CommonJS-only implementation, and the native-addon loading model (per-platform npm packages resolved at runtime), which JSR cannot host and which requires FFI permissions in Deno.
- [ ] Decide the best way to accomplish both, then implement. Open questions:
  - Single source of truth: generate the ESM (and d.ts) from one implementation, or add a bundling step to the build?
  - Does the ESM build enable an async `init()`/factory API via top-level `await`?
  - How should native loading work in an ESM world (`createRequire`, `process.dlopen`, or npm platform-package dependencies)?
  - Do the runtime requirements (Node >= 24, Deno with `--allow-ffi`, Bun) change for the ESM entry?