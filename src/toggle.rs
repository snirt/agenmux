use crate::{panes, setup, tmux};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn run(plugin_dir: &Path, requested_mode: Option<&str>, requested_client: Option<&str>) -> i32 {
    let mode = requested_mode
        .filter(|mode| !mode.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| option("@agents-mon-display"));
    let client = requested_client
        .filter(|client| !client.is_empty())
        .map(str::to_string)
        .or_else(|| panes::newest_real_client("#{client_name}").ok().flatten());
    if matches!(mode.as_str(), "popup" | "float") {
        popup(plugin_dir, client)
    } else {
        split(plugin_dir, client)
    }
}

fn option(name: &str) -> String {
    tmux::command(&["show-option", "-gqv", name])
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

fn binary(plugin_dir: &Path) -> PathBuf {
    let configured = option("@agents-mon-bin");
    if configured.is_empty() {
        plugin_dir.join("target/release/agents-mon")
    } else {
        configured.into()
    }
}

fn control_alive() -> bool {
    let control = option("@agents-mon-control-client");
    !control.is_empty()
        && tmux::lines(&["list-clients", "-F", "#{client_name}"])
            .is_ok_and(|clients| clients.iter().any(|client| client == &control))
}

fn split(plugin_dir: &Path, client: Option<String>) -> i32 {
    let window = client.as_deref().and_then(client_window);
    let mut reuse = option("@agents-mon-on") == "1" && control_alive();
    if reuse {
        if panes::pane_add(window.as_deref()) != 0 {
            return 1;
        }
        // A close can remove its panes just before publishing @agents-mon-on=off.
        // Let that short teardown finish instead of attaching to its dying daemon.
        std::thread::sleep(std::time::Duration::from_millis(100));
        reuse = option("@agents-mon-on") == "1"
            && control_alive()
            && window.as_deref().is_none_or(window_has_sidebar);
    }
    if !reuse {
        panes::teardown();
        if tmux::command_status(&["set-option", "-g", "@agents-mon-on", "1"]).is_err() {
            return 1;
        }
        let bin = binary(plugin_dir);
        if Command::new(&bin)
            .arg("daemon")
            .env("AGENTS_MON_DIR", plugin_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_err()
        {
            panes::teardown();
            return 1;
        }
        for window in tmux::lines(&["list-windows", "-a", "-F", "#{window_id}"]).unwrap_or_default()
        {
            if panes::pane_add(Some(&window)) != 0 {
                panes::teardown();
                return 1;
            }
        }
        if setup::run(plugin_dir) != 0 {
            panes::teardown();
            return 1;
        }
    }
    if option("@agents-mon-nav-version") != "12" && setup::run(plugin_dir) != 0 {
        return 1;
    }
    select_sidebar(client.as_deref());
    0
}

fn window_has_sidebar(window: &str) -> bool {
    tmux::lines(&["list-panes", "-t", window, "-F", "#{pane_title}"])
        .is_ok_and(|titles| titles.iter().any(|title| title == "agents-mon"))
}

fn client_window(client: &str) -> Option<String> {
    tmux::command(&["display-message", "-p", "-c", client, "#{window_id}"])
        .ok()
        .map(|window| window.trim().to_string())
        .filter(|window| !window.is_empty())
}

fn select_sidebar(client: Option<&str>) {
    let Some(client) = client else { return };
    let Some(window) = client_window(client) else {
        return;
    };
    let pane = tmux::lines(&[
        "list-panes",
        "-t",
        &window,
        "-f",
        "#{==:#{pane_title},agents-mon}",
        "-F",
        "#{pane_id}",
    ])
    .ok()
    .and_then(|panes| panes.into_iter().next());
    if let Some(pane) = pane.filter(|pane| !pane.is_empty()) {
        let _ = tmux::command_status(&["select-pane", "-t", &pane]);
    }
    let _ = tmux::command_status(&["switch-client", "-c", client, "-T", "agents-mon"]);
}

fn popup(plugin_dir: &Path, mut client: Option<String>) -> i32 {
    let pin = std::env::temp_dir().join("agents-mon-pin");
    if pin.exists() {
        let _ = std::fs::remove_file(pin);
        return 0;
    }
    if std::fs::File::create(&pin).is_err() {
        return 1;
    }

    let width = nonempty_option("@agents-mon-width").unwrap_or_else(|| "40".to_string());
    let height = nonempty_option("@agents-mon-height")
        .unwrap_or_else(|| popup_height(&scan_cache(), client.as_deref()).to_string());
    let bin = binary(plugin_dir);
    let command = format!(
        "bash -c {}",
        tmux::quote(&format!("{} sidebar", tmux::quote(&bin.to_string_lossy())))
    );
    let jump = PathBuf::from(format!("{}.jump", pin.to_string_lossy()));

    while pin.exists() {
        let pin_env = format!("AGENTS_MON_PIN={}", pin.to_string_lossy());
        let mut args = vec![
            "display-popup".to_string(),
            "-E".to_string(),
            "-w".to_string(),
            width.clone(),
            "-h".to_string(),
            height.clone(),
            "-e".to_string(),
            pin_env,
        ];
        if let Some(owner) = client.as_deref() {
            args.extend([
                "-c".to_string(),
                owner.to_string(),
                "-e".to_string(),
                format!("AGENTS_MON_POPUP_CLIENT={owner}"),
            ]);
        }
        args.push(command.clone());
        let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
        let _ = tmux::command_status(&refs);

        if jump.exists() {
            let target = std::fs::read_to_string(&jump).unwrap_or_default();
            let target = target.trim();
            let _ = std::fs::remove_file(&jump);
            client = panes::newest_real_client("#{client_name}").ok().flatten();
            if !target.is_empty() {
                if let Some(owner) = client.as_deref() {
                    let _ = tmux::command_status(&["switch-client", "-c", owner, "-t", target]);
                }
                let _ = tmux::command_status(&["select-window", "-t", target]);
                let _ = tmux::command_status(&["select-pane", "-t", target]);
            }
        } else {
            let _ = std::fs::remove_file(&pin);
            break;
        }
    }
    0
}

fn nonempty_option(name: &str) -> Option<String> {
    let value = option(name);
    (!value.is_empty()).then_some(value)
}

fn scan_cache() -> PathBuf {
    std::env::temp_dir().join("agents-mon-scan-cache")
}

fn popup_height(cache: &Path, client: Option<&str>) -> usize {
    let text = std::fs::read_to_string(cache).unwrap_or_default();
    let Some(height) = cache_height(&text) else {
        return 15;
    };
    let client_height = client
        .and_then(|client| {
            tmux::command(&["display-message", "-p", "-c", client, "#{client_height}"]).ok()
        })
        .or_else(|| tmux::command(&["display-message", "-p", "#{client_height}"]).ok())
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(height + 2);
    cap_popup_height(height, client_height)
}

fn cap_popup_height(height: usize, client_height: usize) -> usize {
    height.min(client_height.saturating_sub(2)).max(15)
}

fn cache_height(text: &str) -> Option<usize> {
    if text.is_empty() {
        return None;
    }
    let mut sessions = HashSet::new();
    let mut rows = 0usize;
    let mut subjects = 0usize;
    for line in text.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        rows += 1;
        if fields.get(5).is_some_and(|subject| !subject.is_empty()) {
            subjects += 1;
        }
        if let Some(session) = fields
            .get(1)
            .and_then(|location| location.split(':').next())
        {
            sessions.insert(session);
        }
    }
    Some(rows + subjects + sessions.len() + 5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_height_counts_rows_subjects_and_sessions() {
        let text = (0..10)
            .map(|i| format!("%{i}\ts{}:0.{i}\tcodex\tidle\t/tmp\tsubject", i % 2))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(cache_height(""), None);
        assert_eq!(cache_height(&text), Some(27));
    }

    #[test]
    fn popup_height_caps_to_client_and_keeps_help_floor() {
        assert_eq!(cap_popup_height(40, 22), 20);
        assert_eq!(cap_popup_height(10, 40), 15);
        assert_eq!(cap_popup_height(40, 10), 15);
    }
}
