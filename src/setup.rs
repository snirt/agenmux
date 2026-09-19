use crate::app_config::KeyChord;
use crate::tmux::{self, TmuxError};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

pub(crate) const NORMAL_TABLE: &str = "agenmux";
pub(crate) const SEARCH_TABLE: &str = "agenmux-search";
const SETTINGS_TABLE: &str = "agenmux-settings-edit";
pub(crate) const SEQUENCE_TABLE: &str = "agenmux-sequence";
const NAV_LAYOUT: &str = "16";
// These are trusted runtime identities, not application-file settings. q
// quotes them for the shell only when tmux executes the installed command.
const ENGINE: &str = "AGENMUX_DIR=#{q:@agenmux-plugin-dir} AGENMUX_RUNTIME_DIR=#{q:@agenmux-runtime-dir} #{q:@agenmux-runtime-bin}";

fn key_arg(key: &str) -> &str {
    if key == ";" {
        "\\;"
    } else {
        key
    }
}

/// `list-keys` with no arguments lists every table, and a table with no
/// bindings does not exist as far as tmux is concerned.
fn table_marker(table: &str) -> String {
    format!(" -T {table} ")
}
fn table_exists(table: &str) -> Result<bool, TmuxError> {
    let marker = table_marker(table);
    Ok(tmux::command(&["list-keys"])?
        .lines()
        .any(|line| line.contains(&marker)))
}
fn clear_table(table: &str) -> Result<(), TmuxError> {
    if table_exists(table)? {
        tmux::command_status(&["unbind-key", "-a", "-T", table])
    } else {
        Ok(())
    }
}
fn remove_binding(table: &str, key: &str) -> Result<(), TmuxError> {
    if table_exists(table)? {
        tmux::command_status(&["unbind-key", "-T", table, key_arg(key)])
    } else {
        Ok(())
    }
}

/// Snapshot a table as replayable `bind-key` lines, keyed by the key token.
/// `list-keys -F` would frame the fields unambiguously, but that flag is newer
/// than the tmux versions this plugin supports; the default output is already
/// a valid command and preserves `-r` and the command's own quoting. It omits
/// notes, so a user's note on a snapshotted key is not restored. The plugin
/// sets none of its own, and this only matters on a rollback path.
fn bindings(table: &str) -> Result<std::collections::BTreeMap<String, String>, TmuxError> {
    if !table_exists(table)? {
        return Ok(Default::default());
    }
    let data = tmux::command(&["list-keys", "-T", table])?;
    // A command body can also contain the marker; the first one is the real
    // table field, exactly as clone_root_table assumes when it rewrites it.
    let marker = table_marker(table);
    let mut result = std::collections::BTreeMap::new();
    for line in data.lines() {
        let Some((_, rest)) = line.split_once(&marker) else {
            continue;
        };
        let Some(key) = rest.split_whitespace().next() else {
            continue;
        };
        result.insert(key.to_owned(), format!("{line}\n"));
    }
    Ok(result)
}

fn source(text: &str) -> Result<(), TmuxError> {
    let mut child = Command::new("tmux")
        .args(["source-file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(text.as_bytes())?;
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(TmuxError::Error("restoring tmux binding failed".into()))
    }
}

struct BindingBackup {
    tables: Vec<(&'static str, String)>,
    keys: Vec<(&'static str, &'static str, String)>,
}
impl BindingBackup {
    fn capture(picker: bool) -> Result<Self, TmuxError> {
        // Check server connectivity before accepting an absent plugin table.
        tmux::command(&["list-keys", "-T", "root"])?;
        let mut tables = Vec::new();
        for table in [NORMAL_TABLE, SEARCH_TABLE, SETTINGS_TABLE, SEQUENCE_TABLE] {
            tables.push((table, bindings(table)?.into_values().collect()));
        }
        let mut keys = Vec::new();
        for (table, key) in [
            ("root", "MouseDown1Pane"),
            ("root", "WheelUpPane"),
            ("root", "WheelDownPane"),
            ("prefix", "w"),
        ] {
            if key == "w" && !picker {
                continue;
            }
            keys.push((table, key, bindings(table)?.remove(key).unwrap_or_default()));
        }
        Ok(Self { tables, keys })
    }
    fn restore(&self) -> Result<(), TmuxError> {
        let mut failures = Vec::new();
        for (table, text) in &self.tables {
            // Only plugin-owned tables may be bulk-cleared.
            if clear_table(table).is_err() {
                failures.push(format!("clear {table}"));
            }
            if !text.is_empty() && source(text).is_err() {
                failures.push(format!("restore {table}"));
            }
        }
        for (table, key, text) in &self.keys {
            if remove_binding(table, key).is_err() {
                failures.push(format!("clear {table}/{key}"));
            }
            if !text.is_empty() && source(text).is_err() {
                failures.push(format!("restore {table}/{key}"));
            }
        }
        if !failures.is_empty() {
            Err(TmuxError::Error(format!(
                "binding rollback failed: {}",
                failures.join(", ")
            )))
        } else {
            Ok(())
        }
    }
}

// Only slots setup mutates: never snapshot/replace whole user hook arrays.
const HOOKS: &[&str] = &[
    "after-select-window[42]",
    "client-session-changed[42]",
    "session-window-changed[42]",
    "pane-exited[42]",
    "after-kill-pane[45]",
    "window-pane-changed[42]",
    "window-layout-changed[42]",
    "window-resized[42]",
    "pane-mode-changed[44]",
    "after-select-window[43]",
    "session-window-changed[43]",
    "client-session-changed[43]",
    "window-layout-changed[43]",
    "after-select-pane[44]",
];

struct OptionBackup {
    window: Option<String>,
    name: String,
    value: Option<String>,
    hook: bool,
}
impl OptionBackup {
    fn capture(window: Option<&str>, name: &str, hook: bool) -> Result<Self, TmuxError> {
        let mut args = vec!["show-options", if window.is_some() { "-wq" } else { "-gq" }];
        if let Some(window) = window {
            args.extend(["-t", window]);
        }
        // Query the array, not a missing index: tmux prints "name[index] "
        // even for an absent slot when queried directly.
        args.push(if hook {
            name.split('[').next().unwrap()
        } else {
            name
        });
        let listing = tmux::command(&args)?;
        let present = if hook {
            listing.lines().any(|line| {
                line.strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with(' '))
            })
        } else {
            !listing.is_empty()
        };
        *args.last_mut().unwrap() = name;
        args[1] = if window.is_some() { "-wqv" } else { "-gqv" };
        let value = if present {
            let raw = tmux::command(&args)?;
            Some(raw.strip_suffix('\n').unwrap_or(&raw).to_owned())
        } else {
            None
        };
        Ok(Self {
            window: window.map(str::to_owned),
            name: name.into(),
            value,
            hook,
        })
    }
    fn restore(&self) -> Result<(), TmuxError> {
        let mut args = vec![
            if self.hook { "set-hook" } else { "set-option" },
            match (self.window.is_some(), self.value.is_some()) {
                (true, true) => "-w",
                (true, false) => "-wu",
                (false, true) => "-g",
                (false, false) => "-gu",
            },
        ];
        if let Some(window) = self.window.as_deref() {
            args.extend(["-t", window]);
        }
        args.push(&self.name);
        if let Some(value) = self.value.as_deref() {
            args.push(value);
        }
        tmux::command_status(&args)
    }
}

pub fn run(plugin_dir: &Path) -> i32 {
    match crate::app_config::current(None) {
        Ok(config) => run_config(plugin_dir, &config),
        Err(e) => {
            eprintln!("agenmux: {e}");
            e.exit_code()
        }
    }
}

/// Plugin-entry setup. TPM-style managers run every `*.tmux` in the plugin
/// root and the eager installer re-enters after `install-bin.sh`, so one
/// reload calls setup about four times over, each spending ~330 tmux
/// processes to reinstall what is already there. Skip when the server
/// already carries this contract; bare `setup` stays unconditional so it
/// remains the diagnostic that reinstalls.
pub fn run_if_needed(plugin_dir: &Path) -> i32 {
    let config = match crate::app_config::current(None) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    if installed_current(&config) {
        return 0;
    }
    run_config(plugin_dir, &config)
}

/// True when the tmux server already carries this binary's setup contract.
/// Same fingerprint `toggle` and config reload compare against: `nav_version`
/// folds in NAV_LAYOUT, so bumping that constant on a contract change reruns
/// setup here exactly as it already does there. The runtime binary is checked
/// too, so swapping @agenmux-bin reinstalls even when the keymaps match.
fn installed_current(config: &crate::app_config::AppConfig) -> bool {
    let Ok(bin) = std::env::current_exe() else {
        return false;
    };
    let option = |name: &str| {
        tmux::command(&["show-option", "-gqv", name])
            .map(|value| value.trim_end().to_owned())
            .unwrap_or_default()
    };
    option("@agenmux-nav-version") == nav_version(config)
        && option("@agenmux-runtime-bin") == bin.to_string_lossy()
}

pub fn run_config(plugin_dir: &Path, config: &crate::app_config::AppConfig) -> i32 {
    match setup(plugin_dir, config) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agenmux: {e}");
            1
        }
    }
}

fn migrate_legacy_options() -> Result<(), TmuxError> {
    // Bootstrap identity only. Behavioral legacy values resolve on read.
    for suffix in ["bin"] {
        let canonical = format!("@agenmux-{suffix}");
        if !tmux::command(&["show-options", "-gq", &canonical])?
            .trim()
            .is_empty()
        {
            continue;
        }
        let legacy = format!("@agents-mon-{suffix}");
        if tmux::command(&["show-options", "-gq", &legacy])?
            .trim()
            .is_empty()
        {
            continue;
        }
        let value = tmux::command(&["show-option", "-gqv", &legacy])?;
        tmux::command_status(&["set-option", "-g", &canonical, value.trim_end()])?;
    }
    Ok(())
}
fn setup(plugin_dir: &Path, config: &crate::app_config::AppConfig) -> Result<(), TmuxError> {
    let bin = std::env::current_exe()?.to_string_lossy().into_owned();
    let backup = BindingBackup::capture(config.hide_windows.is_some())?;
    let mut options = Vec::new();
    for name in [
        "@agenmux-bin",
        "@agenmux-runtime-bin",
        "@agenmux-plugin-dir",
        "@agenmux-nav-version",
        "status-left",
        "status-right",
    ] {
        options.push(OptionBackup::capture(None, name, false)?);
    }
    for hook in HOOKS {
        options.push(OptionBackup::capture(None, hook, true)?);
    }
    let windows = tmux::lines(&["list-windows", "-a", "-F", "#{window_id}"])?;
    for window in &windows {
        for name in ["@agenmux-sidebar", "@agents-mon-sidebar"] {
            options.push(OptionBackup::capture(Some(window), name, false)?);
        }
    }

    // Capture every touched slot before the first mutation; restore all on failure.
    // Opening bindings belong to tmux configuration, not application setup.
    let result = (|| {
        migrate_legacy_options()?;
        tmux::command_status(&["set-option", "-g", "@agenmux-runtime-bin", &bin])?;
        tmux::command_status(&[
            "set-option",
            "-g",
            "@agenmux-plugin-dir",
            &plugin_dir.to_string_lossy(),
        ])?;
        clear_legacy_options_and_hooks(&windows)?;
        install_hooks(&bin)?;
        // Mouse keys live in root (`bind-key -n`). Install them before cloning so
        // the plugin tables keep click behavior after tmux switches key tables.
        install_mouse(&bin)?;
        clone_root_table(NORMAL_TABLE)?;
        clone_root_table(SEARCH_TABLE)?;
        // Editing uses a fixed table so changing normal/search bindings cannot
        // strand an open TextEdit or Select control.
        clone_root_table(SETTINGS_TABLE)?;
        install_settings_keys()?;
        clear_table(SEQUENCE_TABLE)?;
        install_keys(config)?;
        install_wheel_keys(&bin)?;
        install_picker_filter(config.hide_windows.as_deref())?;
        install_status(&bin)?;
        tmux::command_status(&[
            "set-option",
            "-g",
            "@agenmux-nav-version",
            &nav_version(config),
        ])
    })();
    if let Err(error) = result {
        let binding_error = backup.restore().err();
        let mut failures = Vec::new();
        for option in &options {
            if option.restore().is_err() {
                // Names are fixed application slots, never values or raw stderr.
                failures.push(option.name.as_str());
            }
        }
        if binding_error.is_some() || !failures.is_empty() {
            return Err(TmuxError::Error(format!("setup failed: {error}; rollback failed: bindings={}, options/hooks=[{}]; inspect and rerun setup", binding_error.map(|e| e.to_string()).unwrap_or_else(|| "restored".into()), failures.join(", "))));
        }
        return Err(error);
    }
    Ok(())
}

/// Rebuilding the plugin tables drops a client that was sitting in one back to
/// root, which would leave the sidebar unresponsive until the user clicked it.
/// Put every client still focused on a sidebar pane back in the normal table.
pub fn reclaim_client_tables() {
    let Ok(clients) = tmux::command(&["list-clients", "-F", "#{client_name}|#{pane_title}"]) else {
        return;
    };
    for line in clients.lines() {
        if let Some((client, "agenmux")) = line.split_once('|') {
            let _ = tmux::command_status(&["switch-client", "-c", client, "-T", NORMAL_TABLE]);
        }
    }
}

fn clear_legacy_options_and_hooks(windows: &[String]) -> Result<(), TmuxError> {
    for window in windows {
        // Best effort: the list is a snapshot, and a window the user closed
        // mid-setup has nothing left to clear. Failing here would roll back an
        // otherwise healthy setup.
        let _ = tmux::command_status(&["set-option", "-wu", "-t", window, "@agenmux-sidebar"]);
        let _ = tmux::command_status(&["set-option", "-wu", "-t", window, "@agents-mon-sidebar"]);
    }
    for hook in [
        "after-select-window[42]",
        "client-session-changed[42]",
        "session-window-changed[42]",
    ] {
        tmux::command_status(&["set-hook", "-gu", hook])?;
    }
    Ok(())
}

fn install_hooks(_bin: &str) -> Result<(), TmuxError> {
    let bin = ENGINE;
    for (hook, command) in [
        (
            "pane-exited[42]",
            format!("run-shell \"{bin} pane-orphan; {bin} pane-pin\""),
        ),
        (
            "after-kill-pane[45]",
            format!("run-shell \"{bin} pane-pin\""),
        ),
        (
            "window-pane-changed[42]",
            format!("run-shell \"{bin} pane-orphan\""),
        ),
        (
            "window-layout-changed[42]",
            format!("run-shell \"{bin} pane-orphan\""),
        ),
        (
            "window-resized[42]",
            format!("run-shell \"{bin} pane-pin\""),
        ),
        (
            "pane-mode-changed[44]",
            "run-shell -b 'tmux if-shell -t \"#{pane_id}\" -F \"#{&&:#{||:#{==:#{pane_title},agenmux},#{==:#{pane_title},agents-mon}},#{window_zoomed_flag}}\" \"resize-pane -Z -t \\\"#{pane_id}\\\"\"'".to_string(),
        ),
    ] {
        tmux::command_status(&["set-hook", "-g", hook, &command])?;
    }

    let add = format!(
        "if -F '#{{!=:#{{@agenmux-on}},}}' {{ run-shell -b \"{bin} pane-add #{{window_id}}\" }}"
    );
    for hook in [
        "after-select-window[43]",
        "session-window-changed[43]",
        "client-session-changed[43]",
    ] {
        tmux::command_status(&["set-hook", "-g", hook, &add])?;
    }
    tmux::command_status(&["set-hook", "-gu", "window-layout-changed[43]"])?;
    tmux::command_status(&[
        "set-hook",
        "-g",
        "after-select-pane[44]",
        "if -F '#{==:#{pane_title},agenmux}' { switch-client -T agenmux }",
    ])
}

fn clone_root_table(table: &str) -> Result<(), TmuxError> {
    clear_table(table)?;
    let replacement = format!("-T {table} ");
    let source = tmux::command(&["list-keys", "-T", "root"])?
        .lines()
        // list-keys emits one leading table marker per binding. A command body
        // may also contain the literal text `-T root`; rewrite only the marker.
        .map(|line| line.replacen("-T root ", &replacement, 1))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let mut child = Command::new("tmux")
        .args(["source-file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(source.as_bytes())?;
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(TmuxError::Error(
            String::from_utf8_lossy(&output.stderr)
                .trim_end()
                .to_string(),
        ))
    }
}

fn bind(table: &str, key: &str, command: &str) -> Result<(), TmuxError> {
    tmux::command_status(&["bind-key", "-T", table, key, command])
}

/// Navigation keys skip `run-shell`: tmux spawns the user's shell per job
/// (nushell takes ~90ms here) and runs the synchronous ones back to back, so
/// a held key kept queueing presses that replayed after release. A named
/// buffer set and deleted in the binding costs no process; the daemon's
/// control pipe sees `%paste-buffer-changed agenmux.<key>` at once.
pub(crate) const BUFFER_KEYS: [&str; 4] = ["up", "down", "wheel-up", "wheel-down"];

fn buffer_key_command(action: &str) -> String {
    format!("set-buffer -b agenmux.{action} x; delete-buffer -b agenmux.{action}")
}

fn key_command(action: &str, next: &str, background: bool) -> String {
    if BUFFER_KEYS.contains(&action) {
        return format!("switch-client -T '{next}'; {}", buffer_key_command(action));
    }
    format!(
        "switch-client -T '{}'; run-shell {}\"{} key '{}' #{{q:client_name}}\"",
        next,
        if background { "-b " } else { "" },
        ENGINE,
        action
    )
}

/// Plugin-table bindings generated from the resolved keymaps. Chord names and
/// action names are closed sets: a config value never reaches a command body.
fn key_bindings(config: &crate::app_config::AppConfig) -> Vec<(&'static str, String, String)> {
    use crate::app_config::Action::*;
    let mut out = Vec::new();
    // First: later binds win, so the catch-all must not overwrite a user chord.
    // `Any` is tmux's fallback for keys nothing else claims, so it never does.
    for key in ["Space", "Any"] {
        out.push((
            NORMAL_TABLE,
            key.into(),
            key_command("space", NORMAL_TABLE, true),
        ));
    }
    // Edge navigation is fixed, not a configurable action, but it is still a
    // default: a configured chord on the same key replaces it below.
    out.push((
        NORMAL_TABLE,
        "G".into(),
        key_command("last", NORMAL_TABLE, false),
    ));
    // Live toggle between the agent list and the full tmux tree, unless the
    // user has claimed "." for a configured action.
    if crate::app_config::action_for(&config.normal, KeyChord::Printable(b'.')).is_none() {
        out.push((
            NORMAL_TABLE,
            ".".into(),
            key_command("toggle-panes", NORMAL_TABLE, true),
        ));
    }
    let mut prefixes = Vec::new();
    let mut continuations = Vec::new();
    for binding in crate::input::sequence_bindings(config) {
        let bytes = binding.sequence.as_bytes();
        let prefix = bytes[0];
        if crate::app_config::action_for(&config.normal, KeyChord::Printable(prefix)).is_some() {
            continue;
        }
        if bytes.len() == 1 {
            if !prefixes.contains(&prefix) {
                prefixes.push(prefix);
                out.push((
                    NORMAL_TABLE,
                    char::from(prefix).to_string(),
                    key_command(&format!("sequence-{prefix:02X}"), NORMAL_TABLE, false),
                ));
            }
            continue;
        }
        let continuation = bytes[1];
        if !continuations.contains(&continuation) {
            continuations.push(continuation);
            out.push((
                SEQUENCE_TABLE,
                char::from(continuation).to_string(),
                key_command(&format!("sequence-{continuation:02X}"), NORMAL_TABLE, false),
            ));
        }
        if prefixes.contains(&prefix) {
            continue;
        }
        prefixes.push(prefix);
        out.push((
            NORMAL_TABLE,
            char::from(prefix).to_string(),
            key_command(&format!("sequence-{prefix:02X}"), SEQUENCE_TABLE, false),
        ));
    }
    out.push((
        SEQUENCE_TABLE,
        "Escape".into(),
        key_command("all", NORMAL_TABLE, false),
    ));
    out.push((
        SEQUENCE_TABLE,
        "Any".into(),
        key_command("escape", NORMAL_TABLE, false),
    ));
    for (action, chords) in &config.normal {
        let (name, next, background) = match action {
            Down => ("down", NORMAL_TABLE, false),
            Up => ("up", NORMAL_TABLE, false),
            Help => ("help", NORMAL_TABLE, true),
            Versions => ("versions", NORMAL_TABLE, true),
            Settings => ("settings", NORMAL_TABLE, true),
            Jump => ("enter", "root", true),
            Close => ("close", "root", false),
            Search => ("search", SEARCH_TABLE, false),
            Filter => ("filter", NORMAL_TABLE, false),
            Reset => ("all", NORMAL_TABLE, false),
            Accept | Cancel | Backspace | Clear => continue,
        };
        for chord in chords {
            out.push((
                NORMAL_TABLE,
                chord.tmux_name(),
                key_command(name, next, background),
            ));
        }
    }
    for code in 32u8..=126 {
        let key = KeyChord::Printable(code).tmux_name();
        out.push((
            SEARCH_TABLE,
            key,
            key_command(&format!("text-{code:02X}"), SEARCH_TABLE, false),
        ));
    }
    for (action, chords) in &config.search {
        let (name, next) = match action {
            Up => ("up", SEARCH_TABLE),
            Down => ("down", SEARCH_TABLE),
            Backspace => ("backspace", SEARCH_TABLE),
            Clear => ("clear-search", SEARCH_TABLE),
            Cancel => ("escape", NORMAL_TABLE),
            Accept => ("enter", NORMAL_TABLE),
            Jump | Search | Filter | Reset | Help | Versions | Settings | Close => continue,
        };
        for chord in chords {
            out.push((
                SEARCH_TABLE,
                chord.tmux_name(),
                key_command(name, next, false),
            ));
        }
    }
    out.push((
        SEARCH_TABLE,
        "Any".into(),
        "switch-client -T agenmux-search".into(),
    ));
    out
}

fn install_settings_keys() -> Result<(), TmuxError> {
    for code in 32u8..=126 {
        bind(
            SETTINGS_TABLE,
            &KeyChord::Printable(code).tmux_name(),
            &key_command(&format!("text-{code:02X}"), SETTINGS_TABLE, false),
        )?;
    }
    // Select widgets accept arrows; Left/Right mirror Up/Down for compact panes
    // where horizontal movement is the natural dropdown gesture.
    for (key, action) in [
        ("Up", "up"),
        ("Down", "down"),
        ("Left", "up"),
        ("Right", "down"),
        ("Enter", "enter"),
        ("Escape", "escape"),
        ("BSpace", "backspace"),
        ("C-u", "clear-search"),
    ] {
        bind(
            SETTINGS_TABLE,
            key,
            &key_command(action, SETTINGS_TABLE, false),
        )?;
    }
    bind(
        SETTINGS_TABLE,
        "Any",
        "switch-client -T agenmux-settings-edit",
    )
}

fn install_keys(config: &crate::app_config::AppConfig) -> Result<(), TmuxError> {
    for (table, key, command) in key_bindings(config) {
        bind(table, &key, &command)?;
    }
    Ok(())
}

/// Table layout version plus a keymap fingerprint: toggle reruns setup when
/// either changes, so edited keys apply without a manual setup.
pub fn nav_version(config: &crate::app_config::AppConfig) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (table, key, command) in key_bindings(config) {
        for byte in [table.as_bytes(), key.as_bytes(), command.as_bytes(), b"\0"].concat() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{NAV_LAYOUT}.{hash:016x}")
}

fn install_wheel_keys(_bin: &str) -> Result<(), TmuxError> {
    for table in [NORMAL_TABLE, SEARCH_TABLE] {
        for (key, action, native) in [
            (
                "WheelUpPane",
                "wheel-up",
                r##"if -Ft= \"#{||:#{pane_in_mode},#{mouse_any_flag}}\" \"send-keys -M\" \"copy-mode -e; send-keys -M\""##,
            ),
            ("WheelDownPane", "wheel-down", "send-keys -M"),
        ] {
            let plugin = buffer_key_command(action);
            let command = format!(
                r#"if-shell -F '#{{==:#{{pane_title}},agenmux}}' "{plugin}; switch-client -T {table}" "{native}""#
            );
            bind(table, key, &command)?;
        }
    }
    Ok(())
}

fn install_mouse(_bin: &str) -> Result<(), TmuxError> {
    if tmux::command(&["show-option", "-gv", "mouse"])?.trim_end() != "on" {
        return Ok(());
    }
    let bin = ENGINE;
    for (key, plugin, native) in [
        (
            "MouseDown1Pane",
            format!(
                "run-shell -b \\\"{bin} click '#{{pane_id}}' '#{{mouse_y}}' #{{q:client_name}}\\\""
            ),
            "select-pane -t = ; send-keys -M".to_string(),
        ),
        (
            "WheelUpPane",
            buffer_key_command("wheel-up"),
            r##"if -Ft= \"#{||:#{pane_in_mode},#{mouse_any_flag}}\" \"send-keys -M\" \"copy-mode -e; send-keys -M\""##.to_string(),
        ),
        (
            "WheelDownPane",
            buffer_key_command("wheel-down"),
            "send-keys -M".to_string(),
        ),
    ] {
        tmux::command_status(&[
            "bind-key",
            "-n",
            key,
            &format!(
                "if-shell -F '#{{==:#{{pane_title}},agenmux}}' \"{plugin}\" \"{native}\""
            ),
        ])?;
    }
    Ok(())
}

fn install_picker_filter(hide: Option<&str>) -> Result<(), TmuxError> {
    let Some(hide) = hide else {
        return Ok(());
    };
    if !hide.is_empty() {
        let escaped = hide
            .replace('#', "##")
            .replace(',', "#,")
            .replace('}', "#}");
        tmux::command_status(&[
            "bind-key",
            "w",
            "choose-tree",
            "-Zw",
            "-f",
            &format!("#{{?#{{m:{escaped},#{{window_name}}}},0,1}}"),
        ])
    } else {
        tmux::command_status(&["bind-key", "w", "choose-tree", "-Zw"])
    }
}

fn install_status(_bin: &str) -> Result<(), TmuxError> {
    let segment = format!("#({ENGINE} status)");
    for option in ["status-left", "status-right"] {
        let value = tmux::command(&["show-option", "-gqv", option])?;
        let value = value.trim_end_matches(['\r', '\n']);
        let replaced = value
            .replace("#{agenmux}", &segment)
            .replace("#{agents_mon}", &segment);
        if replaced != value {
            tmux::command_status(&["set-option", "-g", option, &replaced])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(source: &str) -> crate::app_config::AppConfig {
        let file = crate::app_config::parse(source).unwrap();
        crate::app_config::resolve(&file, &Default::default()).unwrap()
    }

    #[test]
    fn bindings_follow_the_resolved_keymaps() {
        let defaults = config("");
        let keys = key_bindings(&defaults);
        let find = |table: &str, key: &str| {
            keys.iter()
                .find(|(t, k, _)| *t == table && k == key)
                .map(|(_, _, command)| command.as_str())
        };
        assert_eq!(
            find(NORMAL_TABLE, "s"),
            Some(key_command("settings", NORMAL_TABLE, true).as_str())
        );
        assert_eq!(
            find(NORMAL_TABLE, "j"),
            Some(key_command("down", NORMAL_TABLE, false).as_str())
        );
        assert_eq!(
            find(NORMAL_TABLE, "Enter"),
            Some(key_command("enter", "root", true).as_str())
        );
        assert_eq!(
            find(NORMAL_TABLE, "/"),
            Some(key_command("search", SEARCH_TABLE, false).as_str())
        );
        assert_eq!(
            find(SEARCH_TABLE, "C-u"),
            Some(key_command("clear-search", SEARCH_TABLE, false).as_str())
        );
        assert_eq!(
            find(SEARCH_TABLE, "Escape"),
            Some(key_command("escape", NORMAL_TABLE, false).as_str())
        );
        assert_eq!(
            find(SEARCH_TABLE, "\\;"),
            Some(key_command("text-3B", SEARCH_TABLE, false).as_str())
        );
        assert!(find(SEARCH_TABLE, "Any").is_some() && find(NORMAL_TABLE, "Any").is_some());
        assert!(find(NORMAL_TABLE, "g").is_some());
        assert_eq!(find(NORMAL_TABLE, "c"), None);
        assert_eq!(find(NORMAL_TABLE, "d"), None);
        assert_eq!(find(NORMAL_TABLE, "r"), None);
        let enabled = config("[tmux_management]\nenabled = true");
        let enabled_keys = key_bindings(&enabled);
        for key in ["c", "d", "g", "r"] {
            assert!(enabled_keys
                .iter()
                .any(|(table, bound, _)| *table == NORMAL_TABLE && bound == key));
        }
        assert_eq!(
            enabled_keys
                .iter()
                .find(|(table, key, _)| *table == NORMAL_TABLE && key == "d")
                .map(|(_, _, command)| command.as_str()),
            Some(key_command("sequence-64", SEQUENCE_TABLE, false).as_str())
        );
        assert_eq!(
            enabled_keys
                .iter()
                .find(|(table, key, _)| *table == NORMAL_TABLE && key == "r")
                .map(|(_, _, command)| command.as_str()),
            Some(key_command("sequence-72", NORMAL_TABLE, false).as_str())
        );
        for (key, code) in [("c", 0x63), ("s", 0x73), ("d", 0x64), ("g", 0x67)] {
            assert_eq!(
                enabled_keys
                    .iter()
                    .find(|(table, bound, _)| *table == SEQUENCE_TABLE && bound == key)
                    .map(|(_, _, command)| command.as_str()),
                Some(key_command(&format!("sequence-{code:02X}"), NORMAL_TABLE, false).as_str())
            );
        }
        assert_eq!(
            enabled_keys
                .iter()
                .find(|(table, key, _)| *table == SEQUENCE_TABLE && key == "Any")
                .map(|(_, _, command)| command.as_str()),
            Some(key_command("escape", NORMAL_TABLE, false).as_str())
        );
        let overridden = config("[tmux_management]\nenabled=true\n[keys.normal]\ndown=['c']");
        let overridden_keys = key_bindings(&overridden);
        let c_commands: Vec<_> = overridden_keys
            .iter()
            .filter(|(table, key, _)| *table == NORMAL_TABLE && key == "c")
            .collect();
        assert_eq!(c_commands.len(), 1);
        assert!(c_commands[0].2.contains("set-buffer -b agenmux.down"));
        assert!(!c_commands[0].2.contains("run-shell"));

        let custom = config(
            "[keys.normal]\ndown = ['n', 'C-k']\nclose = []\nsettings = []\n[keys.search]\ncancel = ['C-g']\nclear = ['Tab']\n",
        );
        let keys = key_bindings(&custom);
        let find = |table: &str, key: &str| {
            keys.iter()
                .find(|(t, k, _)| *t == table && k == key)
                .map(|(_, _, command)| command.as_str())
        };
        assert_eq!(
            find(NORMAL_TABLE, "n"),
            Some(key_command("down", NORMAL_TABLE, false).as_str())
        );
        assert_eq!(
            find(NORMAL_TABLE, "C-k"),
            Some(key_command("down", NORMAL_TABLE, false).as_str())
        );
        assert_eq!(find(NORMAL_TABLE, "j"), None);
        assert_eq!(find(NORMAL_TABLE, "q"), None);
        assert_eq!(find(NORMAL_TABLE, "Q"), None);
        assert_eq!(find(NORMAL_TABLE, "s"), None);
        assert_eq!(
            find(SEARCH_TABLE, "C-g"),
            Some(key_command("escape", NORMAL_TABLE, false).as_str())
        );
        assert_eq!(find(SEARCH_TABLE, "Escape"), None);
        assert_eq!(
            find(SEARCH_TABLE, "Tab"),
            Some(key_command("clear-search", SEARCH_TABLE, false).as_str())
        );
        assert_eq!(find(SEARCH_TABLE, "C-u"), None);
        // Search text keys remain the printable set regardless of overrides.
        assert!(find(SEARCH_TABLE, "n").unwrap().contains("text-6E"));
        // Only closed-set names reach command bodies.
        for (_, key, command) in &keys {
            assert!(!command.contains("private"), "{key}: {command}");
        }

        // The catch-all Space bind must not overwrite a user chord.
        let spaced = config("[keys.normal]\nversions = ['Space']\n");
        let keys = key_bindings(&spaced);
        let space: Vec<_> = keys
            .iter()
            .filter(|(t, k, _)| *t == NORMAL_TABLE && k == "Space")
            .collect();
        assert_eq!(
            space.last().map(|(_, _, command)| command.as_str()),
            Some(key_command("versions", NORMAL_TABLE, true).as_str())
        );

        assert_ne!(nav_version(&defaults), nav_version(&custom));
        assert_eq!(nav_version(&defaults), nav_version(&config("version = 1")));
        assert!(nav_version(&defaults).starts_with("16."));
    }

    #[test]
    fn launcher_bindings_are_gated_and_preserve_existing_keys() {
        fn find<'a>(
            keys: &'a [(&'static str, String, String)],
            table: &str,
            key: &str,
        ) -> Option<&'a str> {
            keys.iter()
                .find(|(listed_table, listed_key, _)| *listed_table == table && listed_key == key)
                .map(|(_, _, command)| command.as_str())
        }
        let disabled = key_bindings(&config(""));
        assert_eq!(find(&disabled, NORMAL_TABLE, "e"), None);
        assert_eq!(find(&disabled, NORMAL_TABLE, "o"), None);
        assert!(
            find(&disabled, NORMAL_TABLE, "u").is_some_and(|command| command.contains("versions"))
        );
        assert_eq!(
            find(&disabled, NORMAL_TABLE, "G"),
            Some(key_command("last", NORMAL_TABLE, false).as_str())
        );
        assert_eq!(
            find(&disabled, NORMAL_TABLE, "g"),
            Some(key_command("sequence-67", SEQUENCE_TABLE, false).as_str())
        );

        let enabled = config(
            "[tmux_management]\nenabled=true\n[quick_launchers.terminal]\nsequence='ot'\nlabel='terminal'\ncommand='fish'",
        );
        let keys = key_bindings(&enabled);
        assert_eq!(
            find(&keys, NORMAL_TABLE, "e"),
            Some(key_command("sequence-65", NORMAL_TABLE, false).as_str())
        );
        assert_eq!(
            find(&keys, NORMAL_TABLE, "o"),
            Some(key_command("sequence-6F", SEQUENCE_TABLE, false).as_str())
        );
        assert_eq!(
            find(&keys, SEQUENCE_TABLE, "g"),
            Some(key_command("sequence-67", NORMAL_TABLE, false).as_str())
        );
        assert_eq!(
            find(&keys, SEQUENCE_TABLE, "t"),
            Some(key_command("sequence-74", NORMAL_TABLE, false).as_str())
        );
        assert_eq!(
            find(&keys, NORMAL_TABLE, "u"),
            find(&disabled, NORMAL_TABLE, "u")
        );
        assert_eq!(
            find(&keys, NORMAL_TABLE, "G"),
            find(&disabled, NORMAL_TABLE, "G")
        );
        assert_eq!(
            find(&keys, NORMAL_TABLE, "g"),
            Some(key_command("sequence-67", SEQUENCE_TABLE, false).as_str())
        );

        let removed = config(
            "[tmux_management]\nenabled=true\n[quick_launchers.nvim]\nenabled=false\n[quick_launchers.lazygit]\nenabled=false",
        );
        let removed = key_bindings(&removed);
        assert_eq!(find(&removed, NORMAL_TABLE, "e"), None);
        assert_eq!(find(&removed, NORMAL_TABLE, "o"), None);
        assert_eq!(
            find(&removed, NORMAL_TABLE, "G"),
            find(&disabled, NORMAL_TABLE, "G")
        );
        assert_eq!(
            find(&removed, NORMAL_TABLE, "u"),
            find(&disabled, NORMAL_TABLE, "u")
        );
    }
}
