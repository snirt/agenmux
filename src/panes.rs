use crate::tmux::{self, TmuxError};
use std::process::{Command, Stdio};

pub const IS_SIDEBAR: &str = "#{||:#{==:#{pane_title},agenmux},#{==:#{@agenmux},1}}";

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

// Restore tools respawn processless sidebar panes as idle shells; remove them before splitting.
fn kill_ghosts(win: &str, width: &str) {
    let default_shell =
        tmux::command(&["show-option", "-gqv", "default-shell"]).unwrap_or_default();
    let default_shell = default_shell.trim().rsplit('/').next().unwrap_or_default();
    let snapshot = crate::procs::Snapshot::take();
    let panes = tmux::lines(&[
        "list-panes",
        "-t",
        win,
        "-F",
        "#{pane_id}\t#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}\t#{window_height}\t#{window_panes}\t#{pane_pid}\t#{pane_current_command}\t#{pane_title}",
    ])
    .unwrap_or_default();
    for row in panes {
        let mut fields = row.split('\t');
        let (
            Some(pane),
            Some("0"),
            Some("0"),
            Some(pane_width),
            Some(pane_height),
            Some(window_height),
            Some(window_panes),
            Some(pid),
            Some(command),
            Some(title),
        ) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        )
        else {
            continue;
        };
        let command = command
            .trim_start_matches('-')
            .rsplit('/')
            .next()
            .unwrap_or(command);
        let is_shell = matches!(
            command,
            "sh" | "bash" | "zsh" | "fish" | "nu" | "dash" | "ksh" | "tcsh" | "csh"
        ) || (!default_shell.is_empty() && command == default_shell);
        let (Ok(pid), Ok(window_panes)) = (pid.parse::<u32>(), window_panes.parse::<u32>()) else {
            continue;
        };
        if window_panes > 1
            && pane_width == width
            && pane_height == window_height
            && pid != 0
            && title != "agenmux"
            && is_shell
            && !snapshot.has_children(pid)
        {
            let _ = tmux::command_status(&["kill-pane", "-t", pane]);
        }
    }
}

pub fn pane_add(window: Option<&str>) -> i32 {
    if !tmux::command(&["show-option", "-gqv", "@agenmux-on"])
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

    let lock_name = format!("agenmux-add-{}", win.trim_start_matches('@'));
    let Some(_lock) = TmuxLock::acquire(lock_name) else {
        return 0;
    };
    if !tmux::command(&["show-option", "-gqv", "@agenmux-on"])
        .is_ok_and(|value| value.trim() == "1")
    {
        return 0;
    }
    if tmux::lines(&[
        "list-panes",
        "-t",
        &win,
        "-f",
        IS_SIDEBAR,
        "-F",
        "#{pane_id}",
    ])
    .is_ok_and(|panes| !panes.is_empty())
    {
        return 0;
    }

    let width = tmux::command(&["show-option", "-gqv", "@agenmux-width"])
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "30".to_string());
    kill_ghosts(&win, &width);
    let layout = match tmux::command(&["display-message", "-p", "-t", &win, "#{window_layout}"]) {
        Ok(layout) => layout.trim_end().to_string(),
        Err(_) => return 1,
    };
    let layout_option = format!("@agenmux-layout-{win}");
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
    let _ = tmux::command_status(&["set-option", "-p", "-t", &pane, "@agenmux", "1"]);
    let _ = tmux::command_status(&["select-pane", "-t", &pane, "-T", "agenmux"]);
    0
}

pub fn pane_pin() -> i32 {
    let width = tmux::command(&["show-option", "-gqv", "@agenmux-width"])
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "30".to_string());
    let panes = tmux::lines(&["list-panes", "-a", "-f", IS_SIDEBAR, "-F", "#{pane_id}"])
        .unwrap_or_default();
    for pane in panes {
        let _ = tmux::command_status(&["resize-pane", "-t", &pane, "-x", &width]);
    }
    0
}

pub fn pane_orphan() -> i32 {
    if !tmux::command(&["show-option", "-gqv", "@agenmux-on"])
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
        if !tmux::lines(&[
            "list-panes",
            "-t",
            win,
            "-f",
            IS_SIDEBAR,
            "-F",
            "#{pane_id}",
        ])
        .is_ok_and(|panes| !panes.is_empty())
        {
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
        let option = format!("@agenmux-layout-{win}");
        let _ = tmux::command_status(&["set-option", "-gu", &option]);
        let _ = tmux::command_status(&["kill-window", "-t", win]);
    }
    0
}

fn layout_size(layout: &str) -> Option<&str> {
    layout.split_once(',')?.1.split(',').next()
}

fn restore_layout(window: &str) {
    let option = format!("@agenmux-layout-{window}");
    let legacy_option = format!("@agents-mon-layout-{window}");
    let mut layout = tmux::command(&["show-option", "-gqv", &option]).unwrap_or_default();
    if layout.trim_end().is_empty() {
        layout = tmux::command(&["show-option", "-gqv", &legacy_option]).unwrap_or_default();
    }
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
    let sidebar_filter = ["#{||:", IS_SIDEBAR, ",#{==:#{pane_title},agents-mon}}"].concat();
    let panes = tmux::lines(&[
        "list-panes",
        "-a",
        "-f",
        &sidebar_filter,
        "-F",
        "#{pane_id}\t#{window_id}",
    ])
    .unwrap_or_default();
    for row in panes {
        let Some((pane, window)) = row.split_once('\t') else {
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
        if option.starts_with("@agenmux-layout-@")
            || option.starts_with("@agenmux-winsize-@")
            || option.starts_with("@agents-mon-layout-@")
            || option.starts_with("@agents-mon-winsize-@")
        {
            let _ = tmux::command_status(&["set-option", "-gu", option]);
        }
    }
    let _ = tmux::command_status(&["set-option", "-gu", "@agenmux-on"]);
    let _ = tmux::command_status(&["set-option", "-gu", "@agenmux-control-client"]);
    let _ = tmux::command_status(&["set-option", "-gu", "@agenmux-runtime-dir"]);
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
