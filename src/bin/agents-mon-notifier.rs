//! AgentsMon.app helper: posts native macOS notifications through
//! UNUserNotificationCenter and runs the click command when the body is
//! clicked. Must run from inside the installed, signed AgentsMon.app bundle
//! (see scripts/install-app.sh); macOS refuses notifications otherwise.
//!
//! usage: agents-mon-notifier [--spawned] <title> <body> [click-command]
//!        agents-mon-notifier --setup
//!
//! Without --spawned it re-executes itself detached and returns immediately,
//! so the caller (the sidebar) never blocks on the click window. The spawned
//! instance keeps the main run loop alive until the notification is clicked,
//! dismissed, or the click window elapses. Denied permission exits 4 without
//! posting — denial means silence, never a fallback.
//!
//! --setup is the install-time flow: it requests permission, waits for the
//! user's answer to the prompt, and posts a test notification when granted;
//! exit 0 = granted, 4 = denied.

type Parsed = (bool, String, String, Option<String>);

fn parse(args: &[String]) -> Option<Parsed> {
    let (spawned, rest) = match args.split_first() {
        Some((flag, rest)) if flag == "--spawned" => (true, rest),
        _ => (false, args),
    };
    match rest {
        [title, body] => Some((spawned, title.clone(), body.clone(), None)),
        [title, body, click] => Some((spawned, title.clone(), body.clone(), Some(click.clone()))),
        _ => None,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["--setup"] {
        std::process::exit(setup());
    }
    let Some(parsed) = parse(&args) else {
        eprintln!(
            "usage: agents-mon-notifier [--setup] [--spawned] <title> <body> [click-command]"
        );
        std::process::exit(2);
    };
    std::process::exit(run(parsed));
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
fn run((spawned, title, body, click): Parsed) -> i32 {
    use std::process::{Command, Stdio};

    if !spawned {
        // denied permission means silence: report failure, spawn nothing —
        // NotDetermined still spawns so the first delivery can prompt
        if let Ok(settings) = noti::blocking::get_notification_settings() {
            if settings.authorization_status == noti::AuthorizationStatus::Denied {
                return 4;
            }
        }
        let Ok(exe) = std::env::current_exe() else {
            return 1;
        };
        let mut detached = Command::new(exe);
        detached.arg("--spawned").arg(&title).arg(&body);
        if let Some(click) = &click {
            detached.arg(click);
        }
        detached
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        return match detached.spawn() {
            Ok(_) => 0,
            Err(_) => 1,
        };
    }

    use mac_usernotifications as noti;

    if noti::check_bundle().is_err() {
        return 3;
    }
    match noti::blocking::request_auth() {
        Ok(true) => {}
        Ok(false) | Err(_) => return 4,
    }

    let notification = noti::Notification::new()
        .title(&title)
        .message(&body)
        .sound(noti::sound::GLASS);

    let Some(click) = click else {
        return match notification.send_blocking() {
            Ok(_) => 0,
            Err(_) => 5,
        };
    };

    // ponytail: one waiting helper process per notification; after 24h the
    // notification is closed and later clicks are no-ops. A shared long-lived
    // helper is the upgrade path if that ever matters.
    let response = noti::block_on_main(async {
        notification
            .timeout(std::time::Duration::from_secs(24 * 60 * 60))
            .send()
            .await?
            .response()
            .await
    });
    match response {
        Ok(response) if response.is_default_action() => {
            let ran = Command::new("/bin/sh")
                .args(["-c", &click])
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if ran {
                0
            } else {
                6
            }
        }
        Ok(_) => 0,
        Err(_) => 5,
    }
}

#[cfg(not(target_os = "macos"))]
fn run(_: Parsed) -> i32 {
    eprintln!("agents-mon-notifier is macOS-only");
    2
}

#[cfg(test)]
mod tests {
    use super::parse;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_accepts_title_body_and_optional_click_command() {
        assert_eq!(
            parse(&args(&["Codex finished", "subject · dir"])),
            Some((false, "Codex finished".into(), "subject · dir".into(), None))
        );
        assert_eq!(
            parse(&args(&["t", "b", "'agents-mon' 'notification-open'"])),
            Some((
                false,
                "t".into(),
                "b".into(),
                Some("'agents-mon' 'notification-open'".into())
            ))
        );
    }

    #[test]
    fn parse_recognizes_the_spawned_marker() {
        assert_eq!(
            parse(&args(&["--spawned", "t", "b"])),
            Some((true, "t".into(), "b".into(), None))
        );
    }

    #[test]
    fn parse_rejects_wrong_arity() {
        assert_eq!(parse(&args(&[])), None);
        assert_eq!(parse(&args(&["only-title"])), None);
        assert_eq!(parse(&args(&["--spawned", "only-title"])), None);
        assert_eq!(parse(&args(&["t", "b", "c", "extra"])), None);
    }
}
