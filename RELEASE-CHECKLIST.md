# Release checklist

Complete these human checks before running `./scripts/publish-crates.sh` or
`./scripts/publish-crates-alpha.sh`. The scripts handle coordinated version
updates (including known documentation references), stale-version warnings,
changelog cutting, formatting and link checks, release verification, crate
publication, tags, and the GitHub Release.

- Decide the exact version and whether it is a stable or prerelease. The
  scripts reject the wrong release channel; when manifests already contain the
  intended version, select `current` at the prompt.
- Review the complete `Unreleased` changelog section for accuracy, useful
  migration detail, correct v4/v5 scope, and clearly identified breaking
  changes.
- Confirm every user-facing API or behavior change is reflected in rustdoc,
  examples, recipes, and `MIGRATION.md` where applicable.
- Rewrite `RELEASE-NOTES.md` for this release, or remove it when no custom
  introduction is wanted. Never leave notes from the previous release in place.
- Confirm the full CI and feature matrix are green for the commit being
  released, especially Clippy and each-feature tests beyond the script's
  preflight checks.
- Confirm no extra documents need to be included on the release scripts'
  list of documents to automatically update the semver version of.
