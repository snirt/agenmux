# Launcher configuration boundary — implementation amendment

Supersedes launcher ownership sections of the issue #36 app-configuration draft. User approved implementation. Base: 8f363fa. Agent configuration remains unchanged.

## Contract

Tmux configuration owns opening Agenmux. `@agenmux-key` and `@agenmux-popup-key` (legacy aliases accepted) are bootstrap/tmux preferences, not application settings. Defaults remain A/e per explicit user decision; an explicitly empty option disables that launcher binding. Canonical presence wins over legacy, including empty. Direct user-written tmux bindings may invoke the verified plugin activation entrypoint.

`agenmux.tmux` installs launcher bindings using native tmux binding semantics (last binding wins). Document load order and disabling plugin launchers before manually binding keys. No collision registry, no global collision preflight, no prefix ownership migration. Rust app setup and toggle must not install/remove/validate opening bindings. No runtime cleanup of previously installed user opening bindings; changing bindings follows normal tmux unbind/reload behavior.

Application TOML retains display, behavior, themes, normal/search keymaps. Reject `keys.prefix` as unknown. Agenmux remains allowed to manage panes, private navigation tables, hooks, and existing app-specific tmux behavior. Do not move all tmux integration into shell or redesign picker behavior in this change.

## Implementation

1. Remove Prefix key mode, Toggle/Popup actions used only for launchers, schema/resolver/source entries, and compatibility option parsing for launcher keys from app_config.rs. Remove PrefixPlan, opening-binding ownership metadata and preflight calls from setup/toggle. Retain safety/rollback for remaining app-managed bindings.
2. Restore bootstrap ownership of opening bindings. Read only tmux options, never app TOML. Use fixed activation actions and tmux argv; encode path/client across shell and tmux format layers safely. Preserve install lock, stale-engine verification, custom binary selection, verified activation, and error reporting. Invalid app TOML cannot prevent launcher installation, though activation reports validation errors before mutation.
3. Tests: TOML prefix rejected; tmux options configure/default/disable launchers including canonical empty; app setup/toggle preserve manually assigned opening bindings; no ownership metadata written; installed launchers invoke verified activation on private tmux with adversarial paths/client handling. Replace obsolete collision-policy tests with ownership-boundary assertions; retain unrelated geometry/notification/navigation/injection checks.
4. README: separate tmux launcher options from app TOML; show defaults A/e, disabling both with empty options before TPM, manual activation bindings, reload/unbind semantics. Remove claims Rust tracks opening bindings or rejects collisions. Do not claim unfinished theme/keymap application is complete.
5. Run targeted Rust/CLI/plugin and shell bootstrap tests, full Cargo tests, diff/privacy checks. Commit focused change without attribution. Independently review before continuing remaining #36 tasks.

No active-user tmux changes, tracker updates, agent-rule migration, new config framework, or generic file split.
