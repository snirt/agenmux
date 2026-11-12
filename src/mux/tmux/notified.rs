// Notification clicks: `notification-open` names the tmux server by its
// socket, since the click runs outside any tmux pane.
use std::process::Command;

/// Jump the most recent real client of the tmux server at `socket` to
/// `pane`, calling `activate` once both are confirmed.
pub fn open_notified(socket: &str, pane: &str, activate: impl FnOnce()) {
    if socket.is_empty() || !valid_pane(pane) {
        return;
    }
    let mut verify = tmux_command(socket);
    let Ok(output) = verify
        .args(["display-message", "-p", "-t", pane, "#{pane_id}"])
        .output()
    else {
        return;
    };
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != pane {
        return;
    }

    let mut clients = tmux_command(socket);
    let Ok(output) = clients
        .args([
            "list-clients",
            "-F",
            "#{client_activity}|#{client_flags}|#{client_name}",
        ])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let Some(client) = most_recent_client(&String::from_utf8_lossy(&output.stdout)) else {
        return;
    };

    activate();

    let mut jump = tmux_command(socket);
    let _ = jump
        .args([
            "switch-client",
            "-c",
            &client,
            "-t",
            pane,
            ";",
            "select-window",
            "-t",
            pane,
            ";",
            "select-pane",
            "-t",
            pane,
        ])
        .status();
}

fn valid_pane(pane: &str) -> bool {
    pane.strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
}

fn most_recent_client(rows: &str) -> Option<String> {
    rows.lines()
        .filter_map(|line| {
            let separator = if line.contains('|') { '|' } else { '\t' };
            let mut fields = line.splitn(3, separator);
            let activity = fields.next()?.parse::<u64>().ok()?;
            let first = fields.next()?;
            let second = fields.next()?;
            let (name, flags) = if separator == '|' {
                (second, first)
            } else {
                (first, second)
            };
            (!flags.split(',').any(|flag| flag == "control-mode"))
                .then(|| (activity, name.to_string()))
        })
        .max_by_key(|(activity, _)| *activity)
        .map(|(_, name)| name)
}

fn tmux_command(socket: &str) -> Command {
    let mut command = super::process();
    command.args(["-S", socket]).env_remove("TMUX");
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_target_is_the_most_recent_real_client() {
        let clients = concat!(
            "20\t/dev/ttys001\tattached,utf8\n",
            "50\t/control\tattached,focused,control-mode,utf8\n",
            "40\t/dev/ttys002\tattached,focused,utf8\n",
        );
        assert_eq!(most_recent_client(clients).as_deref(), Some("/dev/ttys002"));
    }

    #[test]
    fn click_helper_accepts_only_exact_tmux_pane_ids() {
        assert!(valid_pane("%1"));
        assert!(valid_pane("%123"));
        assert!(!valid_pane(""));
        assert!(!valid_pane("%"));
        assert!(!valid_pane("work:1.2"));
        assert!(!valid_pane("%1; display-message hacked"));
    }
}
