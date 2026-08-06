use super::{CommandSpec, DeliveryOutcome, Payload, Runner};

pub(super) fn deliver<R: Runner>(
    runner: &R,
    payload: &Payload,
    click_command: Option<String>,
) -> DeliveryOutcome {
    let mut args = vec![
        "-title".into(),
        payload.title.clone(),
        "-message".into(),
        payload.body.clone(),
        "-sound".into(),
        "Glass".into(),
    ];
    if let Some(command) = click_command {
        args.extend(["-execute".into(), command]);
    }
    let terminal_notifier = CommandSpec {
        program: "terminal-notifier".into(),
        args,
        stdin: None,
    };
    if matches!(runner.run(&terminal_notifier), Ok(true)) {
        return DeliveryOutcome::Delivered;
    }

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
