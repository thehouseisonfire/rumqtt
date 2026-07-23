# File session-store release checklist

- Choose independent versions for `atomic-blob-store` and the merged adapter,
  and cut each package's changelog.
- Confirm matching `rumqttc-v4-next` and `rumqttc-v5-next` versions are already
  available from crates.io.
- Cut `CHANGELOG.md` from `Unreleased` to `## [VERSION] - YYYY-MM-DD` and leave a
  new empty `Unreleased` section above it.
- Confirm both root and file-store CI workflows are green.
- Confirm the generic crate terminology audit and immutable fixture tests pass.
- Compare the persistence benchmarks with the checked-in baseline.
- Use the package-specific `publish-atomic-blob-store*` or `publish-adapter*`
  script for independent releases. The existing `publish-crates*` scripts
  validate and publish both in dependency order.
- Review the validation output, then rerun the same command with `--execute`.
- Push the resulting annotated package tags after verifying crates.io and docs.rs.
