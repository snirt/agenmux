# agenmux 0.3.1 Version Bump Plan

**Goal:** Prepare Agenmux v0.3.1 and submit version metadata plus curated release notes for review.

**Approach:** Reuse Cargo and existing test commands. Keep publication and tagging out of pull request; tag merged commit when release is approved.

## Files

- `Cargo.toml` — bump package version to `0.3.1`.
- `Cargo.lock` — record package version `0.3.1`.
- `RELEASE_NOTES.md` — summarize merged changes from `v0.3.0` through `origin/master`.
- `docs/plans/2026-09-03-version-bump-0.3.1.md` — record the reviewed bump procedure and publication handoff.

## Checklist

- [ ] Create `release/v0.3.1` from current `origin/master`.
- [ ] Set package version to `0.3.1` in `Cargo.toml` and refresh `Cargo.lock` with Cargo.
- [ ] Replace `RELEASE_NOTES.md` with v0.3.1 fixes, website updates, assets, and `v0.3.0...v0.3.1` comparison link.
- [ ] Run `cargo test` and `./tests/run.sh`.
- [ ] Verify `scripts/version.sh` prints `0.3.1` and `scripts/version.sh check-tag v0.3.1` passes.
- [ ] Inspect exact diff and confirm no unrelated files, private data, or secret-like values are staged.
- [ ] Commit as `chore: bump version to 0.3.1`, push `release/v0.3.1`, and open pull request against `master`.

## Publication handoff

After pull request merge, tag merged `master` commit as `v0.3.1` and push tag so release workflow builds and publishes assets. Do not tag pull-request commit.
