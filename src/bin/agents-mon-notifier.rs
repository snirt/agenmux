//! AgentsMon.app helper: posts native macOS notifications through
//! UNUserNotificationCenter and runs the click command when the body is
//! clicked. Must run from inside the installed, signed AgentsMon.app bundle
//! (see scripts/install-app.sh); macOS refuses notifications otherwise.
//!
//! usage: agents-mon-notifier [--setup|--broker] | <title> <body> [click-command]
//!
//! Normal invocations submit a request to the shared broker and return
//! immediately. Denied permission exits 4 without posting — denial means
//! silence, never a fallback.
//!
//! --setup is the install-time flow: it requests permission, waits for the
//! user's answer to the prompt, and posts a test notification when granted;
//! exit 0 = granted, 4 = denied.

#[path = "agents-mon-notifier/broker.rs"]
mod broker;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotificationRequest {
    pub title: String,
    pub body: String,
    pub click: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    Setup,
    Broker,
    Notify(NotificationRequest),
}

fn parse(args: &[String]) -> Option<Mode> {
    match args {
        [flag] if flag == "--setup" => Some(Mode::Setup),
        [flag] if flag == "--broker" => Some(Mode::Broker),
        [title, body] => Some(Mode::Notify(NotificationRequest {
            title: title.clone(),
            body: body.clone(),
            click: None,
        })),
        [title, body, click] => Some(Mode::Notify(NotificationRequest {
            title: title.clone(),
            body: body.clone(),
            click: Some(click.clone()),
        })),
        _ => None,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match parse(&args) {
        Some(Mode::Setup) => setup(),
        Some(Mode::Broker) => broker::serve(),
        Some(Mode::Notify(request)) => run(request),
        None => {
            eprintln!(
                "usage: agents-mon-notifier [--setup|--broker] | <title> <body> [click-command]"
            );
            2
        }
    };
    std::process::exit(code);
}

#[cfg(target_os = "macos")]
fn setup() -> i32 {
    use mac_usernotifications as noti;

    if noti::check_bundle().is_err() {
        return 3;
    }
    match noti::blocking::request_auth() {
        Ok(true) => {}
        Ok(false) | Err(_) => return 4,
    }
    match noti::Notification::new()
        .title("AgentsMon")
        .message("Notifications are ready.")
        .sound(noti::sound::GLASS)
        .send_blocking()
    {
        Ok(_) => 0,
        Err(_) => 5,
    }
}

#[cfg(not(target_os = "macos"))]
fn setup() -> i32 {
    eprintln!("agents-mon-notifier is macOS-only");
    2
}

#[cfg(target_os = "macos")]
fn run(request: NotificationRequest) -> i32 {
    use mac_usernotifications as noti;

    // Denied permission means silence: report failure without starting a
    // broker. NotDetermined is left to the broker's first-request flow.
    if let Ok(settings) = noti::blocking::get_notification_settings() {
        if settings.authorization_status == noti::AuthorizationStatus::Denied {
            return 4;
        }
    }
    match broker::submit(&request) {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

#[cfg(not(target_os = "macos"))]
fn run(_: NotificationRequest) -> i32 {
    eprintln!("agents-mon-notifier is macOS-only");
    2
}

#[cfg(test)]
mod tests {
    use super::{parse, Mode, NotificationRequest};

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_accepts_title_body_and_optional_click_command() {
        assert_eq!(
            parse(&args(&["Codex finished", "subject · dir"])),
            Some(Mode::Notify(NotificationRequest {
                title: "Codex finished".into(),
                body: "subject · dir".into(),
                click: None,
            }))
        );
        assert_eq!(
            parse(&args(&["t", "b", "'agents-mon' 'notification-open'"])),
            Some(Mode::Notify(NotificationRequest {
                title: "t".into(),
                body: "b".into(),
                click: Some("'agents-mon' 'notification-open'".into()),
            }))
        );
    }

    #[test]
    fn parse_recognizes_setup_and_broker_modes() {
        assert_eq!(parse(&args(&["--setup"])), Some(Mode::Setup));
        assert_eq!(parse(&args(&["--broker"])), Some(Mode::Broker));
    }

    #[test]
    fn parse_rejects_wrong_arity() {
        assert_eq!(parse(&args(&[])), None);
        assert_eq!(parse(&args(&["only-title"])), None);
        assert_eq!(parse(&args(&["t", "b", "c", "extra"])), None);
    }
}
