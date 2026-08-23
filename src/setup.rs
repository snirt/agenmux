use crate::tmux::{self, TmuxError};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const NORMAL_TABLE: &str = "agents-mon";
const SEARCH_TABLE: &str = "agents-mon-search";

pub fn run(plugin_dir: &Path) -> i32 {
    match setup(plugin_dir) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agents-mon: {e}");
            1
        }
    }
}

fn setup(plugin_dir: &Path) -> Result<(), TmuxError> {
    let default_bin = plugin_dir.join("target/release/agents-mon");
    let configured_bin = tmux::command(&["show-option", "-gqv", "@agents-mon-bin"])?;
    let bin = configured_bin
        .trim_end()
        .is_empty()
        .then(|| default_bin.to_string_lossy().into_owned())
        .unwrap_or_else(|| configured_bin.trim_end().to_string());

    clear_legacy_options_and_hooks()?;
    install_hooks(&bin)?;
    // Mouse keys live in root (`bind-key -n`). Install them before cloning so
    // the plugin tables keep click behavior after tmux switches key tables.
    install_mouse(&bin)?;
    clone_root_table(NORMAL_TABLE)?;
    clone_root_table(SEARCH_TABLE)?;
    install_normal_keys(&bin)?;
    install_search_keys(&bin)?;
    install_wheel_keys(&bin)?;
    install_picker_filter()?;
    install_status(&bin)?;
    tmux::command_status(&["set-option", "-g", "@agents-mon-nav-version", "12"])
}

fn clear_legacy_options_and_hooks() -> Result<(), TmuxError> {
    for window in tmux::lines(&["list-windows", "-a", "-F", "#{window_id}"])? {
        let _ = tmux::command_status(&["set-option", "-wu", "-t", &window, "@agents-mon-sidebar"]);
    }
    for hook in [
        "after-select-window[42]",
        "client-session-changed[42]",
        "session-window-changed[42]",
    ] {
        let _ = tmux::command_status(&["set-hook", "-gu", hook]);
    }
    Ok(())
}

fn install_hooks(bin: &str) -> Result<(), TmuxError> {
    let bin = tmux::quote(bin);
    for (hook, command) in [
        (
            "pane-exited[42]",
            format!("run-shell \"{bin} pane-orphan\""),
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
            "run-shell -b 'tmux if-shell -t \"#{pane_id}\" -F \"#{&&:#{==:#{pane_title},agents-mon},#{window_zoomed_flag}}\" \"resize-pane -Z -t \\\"#{pane_id}\\\"\"'".to_string(),
        ),
    ] {
        tmux::command_status(&["set-hook", "-g", hook, &command])?;
    }

    let add = format!(
        "if -F '#{{!=:#{{@agents-mon-on}},}}' {{ run-shell -b \"{bin} pane-add #{{window_id}}\" }}"
    );
    for hook in [
        "after-select-window[43]",
        "session-window-changed[43]",
        "client-session-changed[43]",
    ] {
        tmux::command_status(&["set-hook", "-g", hook, &add])?;
    }
    let _ = tmux::command_status(&["set-hook", "-gu", "window-layout-changed[43]"]);
    tmux::command_status(&[
        "set-hook",
        "-g",
        "after-select-pane[44]",
        "if -F '#{==:#{pane_title},agents-mon}' { switch-client -T agents-mon }",
    ])
}

fn clone_root_table(table: &str) -> Result<(), TmuxError> {
    let _ = tmux::command_status(&["unbind-key", "-a", "-T", table]);
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

fn key_command(bin: &str, action: &str, next: &str, background: bool) -> String {
    format!(
        "run-shell {}\"{} key '{}'\"; switch-client -T '{}'",
        if background { "-b " } else { "" },
        tmux::quote(bin),
        action,
        next
    )
}

fn install_normal_keys(bin: &str) -> Result<(), TmuxError> {
    for (key, action, next) in [
        ("j", "j", NORMAL_TABLE),
        ("k", "k", NORMAL_TABLE),
        ("Down", "down", NORMAL_TABLE),
        ("Up", "up", NORMAL_TABLE),
        ("?", "help", NORMAL_TABLE),
        ("u", "versions", NORMAL_TABLE),
        ("Space", "space", NORMAL_TABLE),
        ("Any", "space", NORMAL_TABLE),
        ("Enter", "enter", "root"),
        ("l", "l", "root"),
        ("q", "close", "root"),
        ("Q", "close", "root"),
    ] {
        bind(NORMAL_TABLE, key, &key_command(bin, action, next, true))?;
    }
    bind(
        NORMAL_TABLE,
        "/",
        &key_command(bin, "search", SEARCH_TABLE, false),
    )?;
    bind(
        NORMAL_TABLE,
        "f",
        &key_command(bin, "filter", NORMAL_TABLE, false),
    )?;
    bind(
        NORMAL_TABLE,
        "Escape",
        &key_command(bin, "all", NORMAL_TABLE, false),
    )
}

fn install_search_keys(bin: &str) -> Result<(), TmuxError> {
    for code in 32u8..=126 {
        let key = match code {
            b' ' => "Space".to_string(),
            b';' => "\\;".to_string(),
            _ => char::from(code).to_string(),
        };
        bind(
            SEARCH_TABLE,
            &key,
            &key_command(bin, &format!("text-{code:02X}"), SEARCH_TABLE, false),
        )?;
    }
    for (key, action, next) in [
        ("Up", "up", SEARCH_TABLE),
        ("Down", "down", SEARCH_TABLE),
        ("C-p", "up", SEARCH_TABLE),
        ("C-n", "down", SEARCH_TABLE),
        ("BSpace", "backspace", SEARCH_TABLE),
        ("C-u", "clear-search", SEARCH_TABLE),
        ("Escape", "escape", NORMAL_TABLE),
        ("C-c", "escape", NORMAL_TABLE),
        ("Enter", "enter", NORMAL_TABLE),
    ] {
        bind(SEARCH_TABLE, key, &key_command(bin, action, next, false))?;
    }
    bind(SEARCH_TABLE, "Any", "switch-client -T agents-mon-search")
}

fn install_wheel_keys(bin: &str) -> Result<(), TmuxError> {
    let bin = tmux::quote(bin);
    for table in [NORMAL_TABLE, SEARCH_TABLE] {
        for (key, direction, native) in [
            (
                "WheelUpPane",
                "up",
                "if -Ft= \\\"#{||:#{pane_in_mode},#{mouse_any_flag}}\\\" \\\"send-keys -M\\\" \\\"copy-mode -e; send-keys -M\\\"",
            ),
            ("WheelDownPane", "down", "send-keys -M"),
        ] {
            let command = format!(
                "if-shell -F '#{{==:#{{pane_title}},agents-mon}}' \"run-shell -b \\\"{bin} wheel '#{{pane_id}}' {direction}\\\" ; switch-client -T {table}\" \"{native}\""
            );
            bind(table, key, &command)?;
        }
    }
    Ok(())
}

fn install_mouse(bin: &str) -> Result<(), TmuxError> {
    if tmux::command(&["show-option", "-gv", "mouse"])?.trim_end() != "on" {
        return Ok(());
    }
    let bin = tmux::quote(bin);
    for (key, plugin, native) in [
        (
            "MouseDown1Pane",
            format!(
                "run-shell -b \\\"{bin} click '#{{pane_id}}' '#{{mouse_y}}' '#{{client_name}}'\\\""
            ),
            "select-pane -t = ; send-keys -M".to_string(),
        ),
        (
            "WheelUpPane",
            format!("run-shell -b \\\"{bin} wheel '#{{pane_id}}' up\\\""),
            "if -Ft= \\\"#{||:#{pane_in_mode},#{mouse_any_flag}}\\\" \\\"send-keys -M\\\" \\\"copy-mode -e; send-keys -M\\\"".to_string(),
        ),
        (
            "WheelDownPane",
            format!("run-shell -b \\\"{bin} wheel '#{{pane_id}}' down\\\""),
            "send-keys -M".to_string(),
        ),
    ] {
        tmux::command_status(&[
            "bind-key",
            "-n",
            key,
            &format!(
                "if-shell -F '#{{==:#{{pane_title}},agents-mon}}' \"{plugin}\" \"{native}\""
            ),
        ])?;
    }
    Ok(())
}

fn install_picker_filter() -> Result<(), TmuxError> {
    let hide = tmux::command(&["show-option", "-gqv", "@agents-mon-hide-windows"])?;
    let hide = hide.trim_end();
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
    } else if !tmux::command(&["show-options", "-gq", "@agents-mon-hide-windows"])?
        .trim_end()
        .is_empty()
    {
        tmux::command_status(&["bind-key", "w", "choose-tree", "-Zw"])
    } else {
        Ok(())
    }
}

fn install_status(bin: &str) -> Result<(), TmuxError> {
    let segment = format!("#({bin} status)");
    for option in ["status-left", "status-right"] {
        let value = tmux::command(&["show-option", "-gqv", option])?;
        let value = value.trim_end_matches(['\r', '\n']);
        if value.contains("#{agents_mon}") {
            tmux::command_status(&[
                "set-option",
                "-g",
                option,
                &value.replace("#{agents_mon}", &segment),
            ])?;
        }
    }
    Ok(())
}
