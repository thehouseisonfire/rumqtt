# File session-store release checklist

- Confirm `atomic-blob-store` 0.1.0 is available from crates.io.
- Confirm matching `rumqttc-v4-next` and `rumqttc-v5-next` versions are already
  available from crates.io.
- Cut `CHANGELOG.md` from `Unreleased` to `## [VERSION] - YYYY-MM-DD` and leave a
  new empty `Unreleased` section above it.
- Confirm both root and file-store CI workflows are green.
- Compare the persistence benchmarks with the checked-in baseline.
- Use the package-specific adapter publication script.
- Review the validation output, then rerun the same command with `--execute`.
- Push the resulting annotated package tags after verifying crates.io and docs.rs.
