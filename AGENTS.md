# Project Rules

## Agent status detection

- Reproduce state bugs against a real agent in a fresh tmux pane. Capture pane title and screen through the full transition; fixtures alone do not prove the fix.
- Match stable UI structure (anchored activity glyphs, layout, timers, controls), never transient generated text such as rotating Claude activity verbs.
- Add real fixtures covering at least two working variants plus an idle/completed negative case. A completed summary line must not match a working rule.
- Test through the public detector used by the runtime. Verify both the target state and the transition back out of it.
- Before live verification, inspect `@agents-mon-bin`, derive that binary's plugin root, and confirm its loaded `agents/*.conf`. Never assume the current checkout is deployed.
- Restart the active daemon after config changes, then verify the monitored pane reports `working` during activity and `idle` after completion.

## Commits

- Write concise, informative commit messages that describe the change and its intent.
- Never add AI or agent credit/attribution to commits, including `Co-authored-by`, `Generated-by`, or similar trailers.

## Repository privacy

- Never commit raw captures, logs, prompts, transcripts, agent session data, or generated diagnostics without reviewing and sanitizing them first.
- Replace usernames, home directories, hostnames, emails, IP addresses, customer/company names, private repository names, prompt content, and local workspace paths with neutral placeholders while preserving only detection-relevant structure.
- Never commit credentials, tokens, cookies, API keys, private keys, auth headers, or environment dumps. If discovered, stop, remove them from the change, and report exposure rather than reproducing the value.
- Keep `.pi/` ignored; it contains local prompts, task output, and delegated-session records.
- Before finishing, inspect `git status` and the exact diff, scan tracked and newly added files for secret-like values and private identifiers, and confirm fixtures contain only sanitized data.
