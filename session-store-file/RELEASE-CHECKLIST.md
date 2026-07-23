# File session-store release checklist

- Choose one coordinated version for the core and merged adapter.
- Confirm matching `rumqttc-v4-next` and `rumqttc-v5-next` versions are already
  available from crates.io.
- Cut `CHANGELOG.md` from `Unreleased` to `## [VERSION] - YYYY-MM-DD` and leave a
  new empty `Unreleased` section above it.
- Confirm both root and file-store CI workflows are green.
- Run `scripts/publish-crates-alpha.sh` for a prerelease or
  `scripts/publish-crates.sh` for a stable release.
- Review the validation output, then rerun the same command with `--execute`.
- Push the resulting annotated package tags after verifying crates.io and docs.rs.
