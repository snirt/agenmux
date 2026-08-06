use super::{CommandSpec, DeliveryOutcome, Payload, Runner};

pub(super) fn deliver<R: Runner>(
    runner: &R,
    payload: &Payload,
    graphical_session: bool,
) -> DeliveryOutcome {
    if !graphical_session {
        return DeliveryOutcome::Unavailable;
    }
    let command = CommandSpec {
        program: "notify-send".into(),
        args: vec!["--".into(), payload.title.clone(), payload.body.clone()],
        stdin: None,
    };
    match runner.run(&command) {
        Ok(true) => DeliveryOutcome::Delivered,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DeliveryOutcome::Unavailable,
        Ok(false) | Err(_) => DeliveryOutcome::Failed,
    }
}
