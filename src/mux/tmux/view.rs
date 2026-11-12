// The view an update closes and reopens: an open split view's daemon holds
// the installation lease, so it must leave before the switch.
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// On the status line of the server this process can reach, else stdout.
pub fn note(message: &str) {
    if super::command_status(&["display-message", &format!("agenmux: {message}")]).is_err() {
        println!("agenmux: {message}");
    }
}

fn tmux_value(args: &[&str]) -> String {
    super::command(args)
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

fn tmux_option(name: &str) -> String {
    let canonical = tmux_value(&["show-option", "-gqv", name]);
    if !canonical.is_empty() {
        return canonical;
    }
    let legacy = name.replacen("@agenmux-", "@agents-mon-", 1);
    tmux_value(&["show-option", "-gqv", &legacy])
}

fn tmux_running() -> bool {
    super::command_status(&["info"]).is_ok()
}

fn wait_for_client(name: &str) {
    for _ in 0..80 {
        let clients = tmux_value(&["list-clients", "-F", "#{client_name}"]);
        if !clients.lines().any(|client| client == name) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Close this server's view and wait for its control client to leave.
fn teardown_runtime(old_control: &str) {
    let _ = super::panes::teardown();
    if !old_control.is_empty() {
        wait_for_client(old_control);
    }
}

/// An open split view never makes the fresh start that activates a prepared
/// update. Close it, wait for its daemon's lease, and reopen it through the
/// launch gate, which switches first. An open popup keeps its lease: leave the
/// update to its next start rather than close it under the user.
pub fn restart(plugin_dir: &Path) {
    if !tmux_running()
        || tmux_option("@agenmux-on") != "1"
        || super::runtime_dir().join("agenmux-pin").exists()
    {
        return;
    }
    teardown_runtime(&tmux_option("@agenmux-control-client"));
    // Still busy (another tmux server): the gate reopens this release.
    let _ = crate::autoupdate::exclusive(plugin_dir, Duration::from_secs(20));
    let entrypoint = plugin_dir.join(format!("{}.tmux", crate::release::runtime_name(plugin_dir)));
    let _ = Command::new("bash")
        .arg(entrypoint)
        .args(["activate", "split", ""])
        .status();
}

/// An open view, closed for an update.
pub struct Closed {
    was_open: bool,
}

/// Close this server's view; None when no tmux server runs.
pub fn close() -> Option<Closed> {
    let was_open = tmux_option("@agenmux-on") == "1"
        || !tmux_value(&["show-option", "-gqv", "@agenmux-sidebar"]).is_empty()
        || !tmux_value(&["show-option", "-gqv", "@agents-mon-sidebar"]).is_empty();
    let old_control = tmux_option("@agenmux-control-client");
    if !tmux_running() {
        return None;
    }
    teardown_runtime(&old_control);
    Some(Closed { was_open })
}

impl Closed {
    pub fn reopen(self, plugin_dir: &Path) {
        crate::release::reenter(plugin_dir, self.was_open);
    }
}
