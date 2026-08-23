use crate::tmux::{self, TmuxError};
use std::process::{Command, Stdio};

struct TmuxLock(String);

impl TmuxLock {
    fn acquire(name: String) -> Option<Self> {
        tmux::command_status(&["wait-for", "-L", &name])
            .ok()
            .map(|()| Self(name))
    }
}

impl Drop for TmuxLock {
    fn drop(&mut self) {
        let _ = tmux::command_status(&["wait-for", "-U", &self.0]);
    }
}

#[allow(dead_code)] // native toggle consumes this in the next migration task
pub fn newest_real_client(format: &str) -> Result<Option<String>, TmuxError> {
    let output_format = format!("#{{client_activity}}\t{format}");
    let rows = tmux::lines(&[
        "list-clients",
        "-f",
        "#{?#{m:*control-mode*,#{client_flags}},0,1}",
        "-F",
        &output_format,
    ])?;
    Ok(newest_value(&rows))
}

fn newest_value(rows: &[String]) -> Option<String> {
    rows.iter()
        .filter_map(|row| {
            let (activity, value) = row.split_once('\t')?;
            Some((activity.parse::<u64>().ok()?, value.to_string()))
        })
        .max_by_key(|(activity, _)| *activity)
        .map(|(_, value)| value)
}

pub fn pane_add(window: Option<&str>) -> i32 {
    if !tmux::command(&["show-option", "-gqv", "@agents-mon-on"])
        .is_ok_and(|value| value.trim() == "1")
    {
        return 0;
    }
    let win = match window {
        Some(win) => win.to_string(),
        None => match tmux::command(&["display-message", "-p", "#{window_id}"]) {
            Ok(win) => win.trim().to_string(),
            Err(_) => return 0,
        },
    };
    if tmux::command(&["display-message", "-p", "-t", &win, "#{session_name}"])
        .is_ok_and(|session| session.trim() == "pi")
    {
        return 0;
    }

    let lock_name = format!("agents-mon-add-{}", win.trim_start_matches('@'));
    let Some(_lock) = TmuxLock::acquire(lock_name) else {
        return 0;
    };
    if !tmux::command(&["show-option", "-gqv", "@agents-mon-on"])
        .is_ok_and(|value| value.trim() == "1")
    {
        return 0;
    }
    if tmux::lines(&["list-panes", "-t", &win, "-F", "#{pane_title}"])
        .is_ok_and(|titles| titles.iter().any(|title| title == "agents-mon"))
    {
        return 0;
    }

    let width = tmux::command(&["show-option", "-gqv", "@agents-mon-width"])
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "30".to_string());
    let layout = match tmux::command(&["display-message", "-p", "-t", &win, "#{window_layout}"]) {
        Ok(layout) => layout.trim_end().to_string(),
        Err(_) => return 1,
    };
    let layout_option = format!("@agents-mon-layout-{win}");
    if tmux::command_status(&["set-option", "-g", &layout_option, &layout]).is_err() {
        return 1;
    }

    let output = Command::new("tmux")
        .args([
            "split-window",
            "-I",
            "-hbf",
            "-d",
            "-l",
            &width,
            "-t",
            &win,
            "-P",
            "-F",
            "#{pane_id}",
        ])
        .stdin(Stdio::null())
        .output();
    let pane = match output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => {
            let _ = tmux::command_status(&["set-option", "-gu", &layout_option]);
            return 1;
        }
    };
    if pane.is_empty() {
        let _ = tmux::command_status(&["set-option", "-gu", &layout_option]);
        return 1;
    }
    let _ = tmux::command_status(&["set-option", "-p", "-t", &pane, "allow-rename", "off"]);
    let _ = tmux::command_status(&["select-pane", "-t", &pane, "-T", "agents-mon"]);
    0
}

pub fn pane_pin() -> i32 {
    let width = tmux::command(&["show-option", "-gqv", "@agents-mon-width"])
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "30".to_string());
    let panes =
        tmux::lines(&["list-panes", "-a", "-F", "#{pane_id}\t#{pane_title}"]).unwrap_or_default();
    for row in panes {
        let Some((pane, "agents-mon")) = row.split_once('\t') else {
            continue;
        };
        let _ = tmux::command_status(&["resize-pane", "-t", pane, "-x", &width]);
    }
    0
}

pub fn pane_orphan() -> i32 {
    if !tmux::command(&["show-option", "-gqv", "@agents-mon-on"])
        .is_ok_and(|value| value.trim() == "1")
    {
        return 0;
    }
    let windows = tmux::lines(&[
        "list-windows",
        "-a",
        "-F",
        "#{window_id}\t#{window_panes}\t#{session_id}",
    ])
    .unwrap_or_default();
    for row in windows {
        let mut fields = row.split('\t');
        let (Some(win), Some("1"), Some(session)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let title =
            tmux::command(&["list-panes", "-t", win, "-F", "#{pane_title}"]).unwrap_or_default();
        if title.trim() != "agents-mon" {
            continue;
        }

        let candidates = tmux::lines(&[
            "list-windows",
            "-t",
            session,
            "-F",
            "#{window_id}\t#{window_last_flag}",
        ])
        .unwrap_or_default();
        let target = candidates
            .iter()
            .find_map(|row| {
                let (candidate, last) = row.split_once('\t')?;
                (candidate != win && last == "1").then(|| candidate.to_string())
            })
            .or_else(|| {
                candidates.iter().find_map(|row| {
                    let candidate = row.split_once('\t').map_or(row.as_str(), |(id, _)| id);
                    (candidate != win).then(|| candidate.to_string())
                })
            });
        let clients = tmux::lines(&[
            "list-clients",
            "-f",
            "#{?#{m:*control-mode*,#{client_flags}},0,1}",
            "-F",
            "#{client_name}",
        ])
        .unwrap_or_default();
        for client in clients.into_iter().filter(|client| !client.is_empty()) {
            let current = tmux::command(&["display-message", "-p", "-c", &client, "#{window_id}"])
                .unwrap_or_default();
            if current.trim() != win {
                continue;
            }
            if let Some(target) = &target {
                let _ = tmux::command_status(&["switch-client", "-c", &client, "-t", target]);
            } else if tmux::command_status(&["switch-client", "-c", &client, "-l"]).is_err() {
                let _ = tmux::command_status(&["switch-client", "-c", &client, "-p"]);
            }
        }
        let option = format!("@agents-mon-layout-{win}");
        let _ = tmux::command_status(&["set-option", "-gu", &option]);
        let _ = tmux::command_status(&["kill-window", "-t", win]);
    }
    0
}

fn layout_size(layout: &str) -> Option<&str> {
    layout.split_once(',')?.1.split(',').next()
}

fn restore_layout(window: &str) {
    let option = format!("@agents-mon-layout-{window}");
    let layout = tmux::command(&["show-option", "-gqv", &option]).unwrap_or_default();
    let layout = layout.trim_end();
    if layout.is_empty() {
        return;
    }
    let current = tmux::command(&[
        "display-message",
        "-p",
        "-t",
        window,
        "#{window_width}x#{window_height}",
    ])
    .unwrap_or_default();
    if layout_size(layout) == Some(current.trim()) {
        let _ = tmux::command_status(&["select-layout", "-t", window, layout]);
    }
}

pub fn teardown() -> i32 {
    let panes = tmux::lines(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id}\t#{pane_title}\t#{window_id}",
    ])
    .unwrap_or_default();
    for row in panes {
        let mut fields = row.split('\t');
        let (Some(pane), Some("agents-mon"), Some(window)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let _ = tmux::command_status(&["kill-pane", "-t", pane]);
        restore_layout(window);
    }

    let options = tmux::lines(&["show-options", "-g"]).unwrap_or_default();
    for row in options {
        let Some(option) = row.split_whitespace().next() else {
            continue;
        };
        if option.starts_with("@agents-mon-layout-@") || option.starts_with("@agents-mon-winsize-@")
        {
            let _ = tmux::command_status(&["set-option", "-gu", option]);
        }
    }
    let _ = tmux::command_status(&["set-option", "-gu", "@agents-mon-on"]);
    let _ = tmux::command_status(&["set-option", "-gu", "@agents-mon-control-client"]);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_client_ignores_invalid_rows_and_keeps_the_format_value() {
        let rows = vec![
            "10\tfirst".to_string(),
            "bad\tignored".to_string(),
            "20\tsecond value".to_string(),
        ];
        assert_eq!(newest_value(&rows).as_deref(), Some("second value"));
    }

    #[test]
    fn layout_size_is_the_absolute_window_size() {
        assert_eq!(layout_size("abcd,100x30,0,0[100x30,0,0,1]"), Some("100x30"));
        assert_eq!(layout_size("invalid"), None);
    }
}
