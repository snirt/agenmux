use crate::{sidebar, tmux};

pub enum Direction {
    Up,
    Down,
}

pub fn click(pane: &str, y: usize, client: &str) -> i32 {
    if client.is_empty() {
        return 0;
    }
    let clients = match tmux::lines(&["list-clients", "-F", "#{client_name}"]) {
        Ok(clients) => clients,
        Err(_) => return 0,
    };
    if !clients.iter().any(|name| name == client) {
        return 0;
    }
    let panes = match tmux::lines(&["list-panes", "-a", "-F", "#{pane_id}"]) {
        Ok(panes) => panes,
        Err(_) => return 0,
    };
    if !panes.iter().any(|id| id == pane) {
        return 0;
    }

    let target = y
        .checked_sub(1)
        .and_then(|line| {
            std::fs::read_to_string(std::env::temp_dir().join("agents-mon-rows"))
                .ok()?
                .lines()
                .nth(line)
                .and_then(|row| row.split_whitespace().next())
                .map(str::to_string)
        })
        .filter(|target| target.starts_with('%') && panes.iter().any(|id| id == target));

    if let Some(target) = target {
        let _ = sidebar::send_key("all");
        let _ = tmux::command_status(&["switch-client", "-c", client, "-t", &target]);
        let _ = tmux::command_status(&["select-window", "-t", &target]);
        let _ = tmux::command_status(&["select-pane", "-t", &target]);
    } else if tmux::command_status(&["switch-client", "-c", client, "-t", pane]).is_ok() {
        let _ = tmux::command_status(&["switch-client", "-c", client, "-T", "agents-mon"]);
    }
    0
}

pub fn wheel(pane: &str, direction: Direction) -> i32 {
    let panes = match tmux::lines(&["list-panes", "-a", "-F", "#{pane_id}"]) {
        Ok(panes) => panes,
        Err(_) => return 0,
    };
    if !panes.iter().any(|id| id == pane) {
        return 0;
    }
    sidebar::send_key(match direction {
        Direction::Up => "wheel-up",
        Direction::Down => "wheel-down",
    })
}
