use crate::attention::{AttentionEvent, AttentionKind};
use std::io::Write;
use std::process::{Command, Stdio};

#[cfg(any(target_os = "linux", test))]
mod linux;
#[cfg(any(target_os = "macos", test))]
mod macos;

#[derive(Debug, Eq, PartialEq)]
struct Payload {
    title: String,
    body: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandSpec {
    program: String,
    args: Vec<String>,
    stdin: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeliveryOutcome {
    Delivered,
    Disabled,
    Unavailable,
    Failed,
}

trait Runner {
    fn run(&self, command: &CommandSpec) -> std::io::Result<bool>;
}

struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, command: &CommandSpec) -> std::io::Result<bool> {
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .stdin(if command.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let write_result =
            if let (Some(input), Some(mut stdin)) = (&command.stdin, child.stdin.take()) {
                stdin.write_all(input.as_bytes())
            } else {
                Ok(())
            };
        let success = child.wait()?.success();
        write_result?;
        Ok(success)
    }
}

fn payload(event: &AttentionEvent) -> Payload {
    let outcome = match event.kind {
        AttentionKind::Finished => "finished",
        AttentionKind::NeedsAttention => "needs attention",
    };
    let agent = sentence_case(&sanitize(&event.agent));
    let title = truncate(&format!("{agent} {outcome}"), 80);
    let body = [&event.title, &event.cwd, &event.loc]
        .into_iter()
        .map(|part| sanitize(part))
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    Payload {
        title,
        body: truncate(&body, 240),
    }
}

fn sentence_case(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return "Agent".into();
    };
    first.to_uppercase().chain(chars).collect()
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    text.chars()
        .take(limit.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

fn sanitize(text: &str) -> String {
    let mut chars = text.chars().peekable();
    let mut clean = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.next() {
                Some('[') => skip_csi(&mut chars),
                Some(']') | Some('P') | Some('X') | Some('^') | Some('_') => {
                    skip_control_string(&mut chars)
                }
                _ => {}
            }
        } else if ch == '\u{9b}' {
            skip_csi(&mut chars);
        } else if ch.is_whitespace() {
            clean.push(' ');
        } else if !ch.is_control() {
            clean.push(ch);
        }
    }
    clean.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn skip_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for ch in chars.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&ch) {
            break;
        }
    }
}

fn skip_control_string(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(ch) = chars.next() {
        if ch == '\u{7}' {
            break;
        }
        if ch == '\u{1b}' && chars.next_if_eq(&'\\').is_some() {
            break;
        }
    }
}

fn notifications_enabled(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "off" | "false" | "0"
    )
}

fn build_click_command(exe: &str, socket: &str, pane: &str, bundle: &str) -> String {
    [exe, "notification-open", socket, pane, bundle]
        .into_iter()
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn most_recent_client(rows: &str) -> Option<String> {
    rows.lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let activity = fields.next()?.parse::<u64>().ok()?;
            let name = fields.next()?;
            let flags = fields.next()?;
            (!flags.split(',').any(|flag| flag == "control-mode"))
                .then(|| (activity, name.to_string()))
        })
        .max_by_key(|(activity, _)| *activity)
        .map(|(_, name)| name)
}

pub fn deliver(tmux: &mut crate::tmux::Tmux, event: &AttentionEvent) -> DeliveryOutcome {
    let option = tmux
        .run("show-option -gqv @agents-mon-notifications")
        .unwrap_or_default();
    let outcome = deliver_if_enabled(&option, event, |payload| {
        #[cfg(target_os = "macos")]
        return macos::deliver(&SystemRunner, payload, click_command(event));
        #[cfg(target_os = "linux")]
        return linux::deliver(
            &SystemRunner,
            payload,
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some(),
        );
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        DeliveryOutcome::Unavailable
    });

    crate::tmux::debug_note(&format!(
        "notification {} {:?}: {outcome:?}",
        event.pane, event.kind
    ));
    outcome
}

fn deliver_if_enabled<F>(option: &str, event: &AttentionEvent, adapter: F) -> DeliveryOutcome
where
    F: FnOnce(&Payload) -> DeliveryOutcome,
{
    if !notifications_enabled(option) {
        DeliveryOutcome::Disabled
    } else {
        adapter(&payload(event))
    }
}

#[cfg(target_os = "macos")]
fn click_command(event: &AttentionEvent) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_str()?;
    let tmux = std::env::var("TMUX").ok()?;
    let socket = tmux.split(',').next().filter(|socket| !socket.is_empty())?;
    let bundle = terminal_bundle()?;
    Some(build_click_command(exe, socket, &event.pane, bundle))
}

#[cfg(target_os = "macos")]
fn terminal_bundle() -> Option<&'static str> {
    if let Ok(bundle) = std::env::var("__CFBundleIdentifier") {
        if let Some(bundle) = known_bundle(&bundle) {
            return Some(bundle);
        }
    }
    match std::env::var("TERM_PROGRAM")
        .ok()?
        .to_ascii_lowercase()
        .as_str()
    {
        "ghostty" => Some("com.mitchellh.ghostty"),
        "iterm.app" | "iterm2" => Some("com.googlecode.iterm2"),
        "wezterm" => Some("com.github.wez.wezterm"),
        "apple_terminal" => Some("com.apple.Terminal"),
        "kitty" => Some("net.kovidgoyal.kitty"),
        "alacritty" => Some("org.alacritty"),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn known_bundle(bundle: &str) -> Option<&'static str> {
    match bundle {
        "com.mitchellh.ghostty" => Some("com.mitchellh.ghostty"),
        "com.googlecode.iterm2" => Some("com.googlecode.iterm2"),
        "com.github.wez.wezterm" => Some("com.github.wez.wezterm"),
        "com.apple.Terminal" => Some("com.apple.Terminal"),
        "net.kovidgoyal.kitty" => Some("net.kovidgoyal.kitty"),
        "org.alacritty" => Some("org.alacritty"),
        _ => None,
    }
}

pub fn open_pane(socket: &str, pane: &str, bundle: &str) -> i32 {
    if socket.is_empty() || !valid_pane(pane) {
        return 0;
    }
    let mut verify = tmux_command(socket);
    let Ok(output) = verify
        .args(["display-message", "-p", "-t", pane, "#{pane_id}"])
        .output()
    else {
        return 0;
    };
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != pane {
        return 0;
    }

    let mut clients = tmux_command(socket);
    let Ok(output) = clients
        .args([
            "list-clients",
            "-F",
            "#{client_activity}\t#{client_name}\t#{client_flags}",
        ])
        .output()
    else {
        return 0;
    };
    if !output.status.success() {
        return 0;
    }
    let Some(client) = most_recent_client(&String::from_utf8_lossy(&output.stdout)) else {
        return 0;
    };

    let mut jump = tmux_command(socket);
    let Ok(status) = jump
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
        .status()
    else {
        return 0;
    };
    if !status.success() {
        return 0;
    }

    #[cfg(target_os = "macos")]
    if let Some(command) = activation_command(bundle) {
        let _ = SystemRunner.run(&command);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = bundle;
    0
}

fn valid_pane(pane: &str) -> bool {
    pane.strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(any(target_os = "macos", test))]
fn activation_command(bundle: &str) -> Option<CommandSpec> {
    if bundle.is_empty() {
        return None;
    }
    Some(CommandSpec {
        program: "/usr/bin/open".into(),
        args: vec!["-b".into(), bundle.into()],
        stdin: None,
    })
}

fn tmux_command(socket: &str) -> Command {
    let mut command = Command::new("tmux");
    command.args(["-S", socket]).env_remove("TMUX");
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    struct FakeRunner {
        commands: RefCell<Vec<CommandSpec>>,
        results: RefCell<VecDeque<std::io::Result<bool>>>,
    }

    impl FakeRunner {
        fn new(results: Vec<std::io::Result<bool>>) -> Self {
            Self {
                commands: RefCell::new(Vec::new()),
                results: RefCell::new(results.into()),
            }
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, command: &CommandSpec) -> std::io::Result<bool> {
            self.commands.borrow_mut().push(command.clone());
            self.results.borrow_mut().pop_front().unwrap()
        }
    }

    fn event(kind: AttentionKind) -> AttentionEvent {
        AttentionEvent {
            kind,
            pane: "%7".into(),
            loc: "DOTFILES:3.2".into(),
            agent: "codex".into(),
            cwd: "tmux-agents-mon".into(),
            title: "Implement\u{1b}]0;secret\u{7}\u{1b}[31m notifications\u{1b}]8;;https://example.test\u{1b}\\\nnow".into(),
        }
    }

    #[test]
    fn payload_has_agent_outcome_and_sanitized_context() {
        assert_eq!(
            payload(&event(AttentionKind::Finished)),
            Payload {
                title: "Codex finished".into(),
                body: "Implement notifications now · tmux-agents-mon · DOTFILES:3.2".into(),
            }
        );
        assert_eq!(
            payload(&event(AttentionKind::NeedsAttention)).title,
            "Codex needs attention"
        );
    }

    #[test]
    fn macos_falls_back_to_argument_safe_applescript() {
        let runner = FakeRunner::new(vec![
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing")),
            Ok(true),
        ]);
        let outcome = macos::deliver(
            &runner,
            &payload(&event(AttentionKind::Finished)),
            Some("'agents-mon' 'notification-open'".into()),
        );

        assert_eq!(outcome, DeliveryOutcome::Delivered);
        let commands = runner.commands.borrow();
        assert_eq!(commands[0].program, "terminal-notifier");
        assert!(commands[0]
            .args
            .windows(2)
            .any(|pair| { pair == ["-execute", "'agents-mon' 'notification-open'"] }));
        assert_eq!(commands[1].program, "/usr/bin/osascript");
        assert_eq!(commands[1].args[0], "-");
        assert!(commands[1]
            .stdin
            .as_deref()
            .unwrap()
            .contains("on run argv"));
        assert!(!commands[1].stdin.as_deref().unwrap().contains("Codex"));
    }

    #[test]
    fn linux_uses_notify_send_without_treating_payload_as_options() {
        let runner = FakeRunner::new(vec![Ok(true)]);
        let outcome = linux::deliver(
            &runner,
            &payload(&event(AttentionKind::NeedsAttention)),
            true,
        );

        assert_eq!(outcome, DeliveryOutcome::Delivered);
        let commands = runner.commands.borrow();
        assert_eq!(commands[0].program, "notify-send");
        assert_eq!(commands[0].args[0], "--");
        assert!(!commands[0].args.iter().any(|arg| arg == "--wait"));
        assert!(!commands[0].args.iter().any(|arg| arg == "--action"));
    }

    #[test]
    fn linux_without_a_graphical_session_is_a_silent_noop() {
        let runner = FakeRunner::new(Vec::new());
        let outcome = linux::deliver(&runner, &payload(&event(AttentionKind::Finished)), false);

        assert_eq!(outcome, DeliveryOutcome::Unavailable);
        assert!(runner.commands.borrow().is_empty());
    }

    #[test]
    fn notifier_failure_does_not_escape_the_delivery_boundary() {
        let runner = FakeRunner::new(vec![Ok(false)]);
        assert_eq!(
            linux::deliver(
                &runner,
                &payload(&event(AttentionKind::NeedsAttention)),
                true,
            ),
            DeliveryOutcome::Failed
        );
    }

    #[test]
    fn payload_limits_are_unicode_safe() {
        let mut long = event(AttentionKind::Finished);
        long.agent = "界".repeat(100);
        long.title = "🙂".repeat(300);
        let payload = payload(&long);

        assert_eq!(payload.title.chars().count(), 80);
        assert_eq!(payload.body.chars().count(), 240);
        assert!(payload.title.ends_with('…'));
        assert!(payload.body.ends_with('…'));
    }

    #[test]
    fn notifications_default_on_and_accept_common_off_values() {
        assert!(notifications_enabled(""));
        assert!(notifications_enabled("on"));
        assert!(notifications_enabled("anything-else"));
        assert!(!notifications_enabled("off"));
        assert!(!notifications_enabled(" FALSE "));
        assert!(!notifications_enabled("0"));
    }

    #[test]
    fn disabled_option_never_invokes_a_platform_adapter() {
        let invoked = std::cell::Cell::new(false);
        let outcome = deliver_if_enabled("off", &event(AttentionKind::Finished), |_| {
            invoked.set(true);
            DeliveryOutcome::Delivered
        });

        assert_eq!(outcome, DeliveryOutcome::Disabled);
        assert!(!invoked.get());
    }

    #[test]
    fn click_command_shell_quotes_every_internal_argument() {
        assert_eq!(
            build_click_command(
                "/tmp/agent's mon",
                "/tmp/tmux socket",
                "%7",
                "com.mitchellh.ghostty",
            ),
            "'/tmp/agent'\"'\"'s mon' 'notification-open' '/tmp/tmux socket' '%7' 'com.mitchellh.ghostty'"
        );
    }

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

    #[test]
    fn terminal_activation_targets_the_detected_bundle() {
        assert_eq!(
            activation_command("com.mitchellh.ghostty"),
            Some(CommandSpec {
                program: "/usr/bin/open".into(),
                args: vec!["-b".into(), "com.mitchellh.ghostty".into()],
                stdin: None,
            })
        );
        assert_eq!(activation_command(""), None);
    }
}
