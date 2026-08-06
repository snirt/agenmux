use super::{CommandSpec, DeliveryOutcome, Payload, Runner};

/// Locate the installed AgentsMon.app helper. Resolved on every delivery so
/// installing the app takes effect without restarting the sidebar.
pub(super) fn helper_program() -> Option<String> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    const IN_APP: &str = "Applications/AgentsMon.app/Contents/MacOS/agents-mon-notifier";
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(std::path::Path::new(&home).join(IN_APP));
    }
    candidates.push(std::path::Path::new("/").join(IN_APP));
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .and_then(|path| path.to_str().map(String::from))
}

pub(super) fn deliver<R: Runner>(
    runner: &R,
    payload: &Payload,
    click_command: Option<String>,
    helper: Option<String>,
) -> DeliveryOutcome {
    if let Some(helper) = helper {
        // The helper detaches itself and posts through UNUserNotificationCenter
        // with the Glass sound; the click command runs when the body is clicked.
        // An installed helper is authoritative: a failure (typically denied
        // permission) means silence, never an AppleScript end-run.
        let mut args = vec![payload.title.clone(), payload.body.clone()];
        args.extend(click_command);
        let native = CommandSpec {
            program: helper,
            args,
            stdin: None,
        };
        return match runner.run(&native) {
            Ok(true) => DeliveryOutcome::Delivered,
            _ => DeliveryOutcome::Failed,
        };
    }

    // display-only fallback for installs without the app bundle
    let applescript = CommandSpec {
        program: "/usr/bin/osascript".into(),
        args: vec!["-".into(), payload.title.clone(), payload.body.clone()],
        stdin: Some(
            "on run argv\n\
             display notification (item 2 of argv) with title (item 1 of argv) sound name \"Glass\"\n\
             end run\n"
                .into(),
        ),
    };
    match runner.run(&applescript) {
        Ok(true) => DeliveryOutcome::Delivered,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DeliveryOutcome::Unavailable,
        Ok(false) | Err(_) => DeliveryOutcome::Failed,
    }
}
