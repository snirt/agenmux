# Default-on auto-update (#181)

## Goal

Discover a newer stable release in the background, download and verify it
without touching the running install, and switch to it on the next fresh
Agenmux start. Manual update/rollback keeps its immediate restart.

## Decisions

- Six steps ship on one branch, one commit each; every step keeps current
  launch/update behavior working.
- Persistent state lives in a hidden sibling of the plugin directory:
  `<plugin-parent>/.agenmux-state/<plugin-dir-name>/`. It shares the plugin's
  filesystem (tarball activation is a rename), survives TPM clean (dot
  directory) and keeps a stable lock identity across source replacement.
- Existing tarball installs establish a baseline once: preparation also fetches
  the installed version's verified package and compares file hashes. A match
  records `baseline`; a mismatch skips with "local changes".
- `install.sh`: fresh clones check out the latest stable tag (detached).
  `AGENMUX_REF=<branch|tag>` opts into another ref. Re-running on a detached
  exact-tag checkout fetches tags and checks out the newest stable tag; branch
  checkouts keep `pull --ff-only`. If the latest tag cannot be resolved the
  installer warns and keeps the default branch.
- Policy: `[behavior] auto_update = true` (missing = true). False stops
  discovery/preparation and blocks activation. An invalid/unreadable config
  counts as false for automatic mutation.
- Manual version switch writes `auto_update = false` through the config
  document writer before switching and restores the previous document if the
  switch fails. Success also clears pending and failed state. Turning
  `auto_update` back on in settings clears a failed-target record.
- The start/daily check shares one throttle: at most one attempt per 24 hours,
  recorded in `last-attempt` before any network call. Manual picker refresh is
  unthrottled and never prepares.

## State directory

```
install.lock     flock: runtimes hold LOCK_SH, activation/manual switch LOCK_EX
prepare.lock     flock: one preparation worker at a time
state.lock       flock: short critical sections around pending/failed/txn files
last-attempt     unix seconds of the last automatic check
status           one-line skip/failure reason for the version picker
pending          ready marker (key=value), published by rename after validation
pkg-<tag>/       verified package tree (+ .agenmux-sha256 manifest)
work-*/          temporary preparation work, removed on exit
baseline         tarball installs: "<tag>" then the package sha256 manifest
failed           target tag whose activation failed (no automatic retry)
transaction      activation in progress (key=value)
backup/          previous engine/notifier/version state, or the old tarball tree
```

`pending` fields: `target`, `base`, `kind` (`git`|`tarball`), `base_rev`,
`target_rev` (git), `package` (relative dir), `manifest` (sha256 of the package
manifest), `prepared` (unix seconds).

## Lock order

server lifecycle lock → `install.lock` → `prepare.lock` → `state.lock`.
Preparation never takes `install.lock`. Activation holds `install.lock`
exclusively and takes `state.lock` briefly. Lock files use `O_CLOEXEC`; the only
deliberate inheritance is the lease handed from `toggle` to the daemon it starts
and the transaction handoff to the target entrypoint (`AGENMUX_INSTALL_LOCK_FD`).

## Eligibility (shared by preparation and activation)

- Engine is the default `target/release/agenmux` of this plugin (no
  `@agenmux-bin`/debug override) and matches `.agenmux-version`.
- Git: clean tree, HEAD exactly at the manifest's stable tag.
- Tarball: `baseline` for the manifest tag matches the tree.
- Failures/skips are written to `status`, never shown as tmux popups.

## Steps

1. **Config and installer:** `behavior.auto_update`, report row/diagnostics,
   settings row, example config, docs; installer tag checkout + `AGENMUX_REF`.
2. **Updater phases:** split `release::update` into resolve → fetch → install →
   restart helpers inside `release.rs`; existing tests unchanged.
3. **Preparation:** `agenmux internal auto-update` worker (detached, throttled,
   timeouts on curl/git), state dir, staging, validation (stable, newer,
   checksum, manifest version, files, engine version, no escaping paths/links),
   atomic `pending`, git tag fetched locally without moving HEAD. Scheduler
   thread in daemon and sidebar. Picker shows `vX.Y.Z ready · next start` and
   the `status` reason; header keeps the running version.
4. **Coordination:** `install.lock` leases in toggle/daemon/sidebar, inherited
   by the daemon from toggle; manual update tears down its own runtime, then
   needs `install.lock` exclusively (bounded wait) or restores and refuses.
5. **Activation:** gate at the start of `toggle` and of direct
   `sidebar`/`daemon` (not popup children or toggle-spawned daemons): recover
   an interrupted transaction, revalidate, swap source/engine/notifier offline,
   run target setup + notifier sync, re-enter the target entrypoint with the
   lock fd; the target commits after readiness or rolls back, records `failed`
   and re-enters the old version once.
6. **Manual pause and docs:** pause/cancel on manual switch with rollback of the
   preference; docs for default, opt-out, next-start meaning, skips, recovery.

## Hardening (review follow-up)

- Removal order: the transaction marker goes before the backup, so a crash
  mid-cleanup never pairs a marker with a partial backup.
- Tarball swap uses an atomic directory exchange (`renamex_np(RENAME_SWAP)`,
  `renameat2(RENAME_EXCHANGE)`), so the plugin path never goes missing.
- A launch that recovers an unconfirmed activation while running the target
  engine re-enters the restored release instead of continuing as the target.
- Target setup is bounded (60 s). The daemon downgrades an inherited exclusive
  hold to shared once it is up. Lock conversion never blocks.
- A launch that waited on the lock re-enters the installed release if the
  engine changed meanwhile.
- Popups confirm the activation when the target sidebar draws its first frame
  (`AGENMUX_READY`), not before.
- Only readiness failures mark a target `failed`; local switch errors keep
  the package for the next fresh start.
- Branch checkouts are skipped (they belong to `git pull`/TPM).
- Manual switches download before closing anything or taking the lock, wait up
  to 20 s for their own daemon to exit, and clear a dangling transaction.
- `install-bin.sh` re-checks the source version and revision before writing
  an engine or state, so a switch during its download wins.
- The worker runs in its own session (no tty prompts) with an ssh timeout.

## Verification

Extend `tests/release.rs` (fake curl/git/tmux on PATH), `tests/run.sh` and
`tests/install-script.sh`; crate tests cover transaction fault injection and
recovery. Full suites: `cargo test --locked`, `bash tests/run.sh`.
