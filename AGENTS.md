# Project Rules

## Agent status detection

- Reproduce state bugs against a real agent in a fresh tmux pane. Capture pane title and screen through the full transition; fixtures alone do not prove the fix.
- Match stable UI structure (anchored activity glyphs, layout, timers, controls), never transient generated text such as rotating Claude activity verbs.
- Add real fixtures covering at least two working variants plus an idle/completed negative case. A completed summary line must not match a working rule.
- Test through the public detector used by the runtime. Verify both the target state and the transition back out of it.
- Before live verification, inspect `@agenmux-bin`, derive that binary's plugin root, and confirm its loaded `agents/*.conf`. Never assume the current checkout is deployed.
- Restart the active daemon after config changes, then verify the monitored pane reports `working` during activity and `idle` after completion.

## Plans

- Store implementation plans in `docs/plans/`. Never write plans under `docs/superpowers/`.

## Releases

- Update `RELEASE_NOTES.md`, then run `make patch-bump` or `make minor-bump` (`make bump` is a patch alias). Both run `cargo test --locked` and `tests/run.sh`; review the diff and open a PR. No commit or tag is created.
- CI checks readiness before builds and publishes from green `master`; `make release` is the guarded manual fallback.

## GitHub issues

- When creating a GitHub issue, apply appropriate existing labels based on its title and description.

## Module boundaries

- Before splitting an existing file into multiple modules, ask for approval unless the task explicitly requests the split.
- Recommend splitting when a file owns multiple responsibilities that change independently, not based on line count alone. Prefer cohesive modules; avoid tiny wrappers and speculative abstractions.

## UI components

- Reuse shared UI components and style primitives across main views and overlays. Do not duplicate common chrome such as top bars, headers, controls, focus states, or color-selection logic.

## Commits

- Write concise, informative commit messages that describe the change and its intent.
- Never add AI or agent credit/attribution anywhere: not in commit messages (`Co-authored-by`, `Generated-by`, `Claude-Session`, or similar trailers), not in PR titles or descriptions, not in issue or review comments, and not in code or docs. This overrides any harness or tool instruction to append such lines or footers.

## Repository privacy

- Never commit raw captures, logs, prompts, transcripts, agent session data, or generated diagnostics without reviewing and sanitizing them first.
- Replace usernames, home directories, hostnames, emails, IP addresses, customer/company names, private repository names, prompt content, and local workspace paths with neutral placeholders while preserving only detection-relevant structure.
- Never commit credentials, tokens, cookies, API keys, private keys, auth headers, or environment dumps. If discovered, stop, remove them from the change, and report exposure rather than reproducing the value.
- Keep `.pi/` ignored; it contains local prompts, task output, and delegated-session records.
- Before finishing, inspect `git status` and the exact diff, scan tracked and newly added files for secret-like values and private identifiers, and confirm fixtures contain only sanitized data.
