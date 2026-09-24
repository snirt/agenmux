# Release preparation (#138)

1. Replace the patch-only bump script with a patch/minor preparation command. Check release notes and version syntax before editing; update the manifest and lockfile without committing or tagging.
2. Add one release-readiness script for local preparation and CI. Check notes against the latest release tag, a valid newer manifest version, lockfile agreement, and whether the target tag is already used. Run it for version-changing PRs, untagged master, and pushed tags.
3. Make the CI version job a prerequisite for expensive jobs; keep automatic tagging and publishing after green builds. Keep `make release` as the documented manual fallback.
4. Add focused shell tests for bump arithmetic, failed preflight, successful preparation, and CI readiness modes. Update contributor and agent instructions, then run the project tests and review the exact diff for private data.
