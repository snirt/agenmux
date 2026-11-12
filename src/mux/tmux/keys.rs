// tmux delivers sidebar keys, clicks and wheel turns by running agenmux from
// its key bindings; each run writes one packet into the daemon's key FIFO.
use crate::input::{send_bytes_to, write_fifo};
use crate::mux::tmux;

/// Deliver one key-table action to the daemon without waiting for a FIFO
/// reader. Each invocation is intentionally short-lived; the daemon remains
/// the only persistent agenmux process.
pub fn send_key(name: &str, client: Option<&str>) -> i32 {
    let valid_close =
        name == "close" && client.is_none_or(|client| !client.is_empty() && client.len() <= 255);
    let _lifecycle = if valid_close {
        let lock = match super::panes::lifecycle_lock() {
            Ok(lock) => lock,
            Err(error) => {
                eprintln!("agenmux: cannot acquire lifecycle lock: {error}");
                return 1;
            }
        };
        let inactive = [
            "@agenmux-on",
            "@agenmux-control-client",
            "@agenmux-runtime-dir",
            "@agenmux-generation",
        ]
        .into_iter()
        .all(|name| {
            tmux::command(&["show-option", "-gqv", name]).is_ok_and(|value| value.trim().is_empty())
        });
        if inactive {
            return 0;
        }
        // Preserve b35f644: publish close before FIFO delivery.
        let _ = tmux::command_status(&["set-option", "-gu", "@agenmux-on"]);
        Some(lock)
    } else {
        None
    };
    let status = send_key_inner(name, client);
    trace!("send key {name} for {client:?} -> {status}");
    status
}

fn send_key_inner(name: &str, client: Option<&str>) -> i32 {
    let bytes: Vec<u8> = if let Some(hex) = name.strip_prefix("text-") {
        let Ok(byte) = u8::from_str_radix(hex, 16) else {
            return 2;
        };
        if !(0x20..=0x7e).contains(&byte) {
            return 2;
        }
        vec![0, byte]
    } else if let Some(hex) = name.strip_prefix("sequence-") {
        let Ok(byte) = u8::from_str_radix(hex, 16) else {
            return 2;
        };
        if !(0x20..=0x7e).contains(&byte) {
            return 2;
        }
        if let Some(client) = client {
            if client.is_empty() || client.len() > 255 {
                return 2;
            }
            let mut packet = vec![0x07, byte, 0, client.len() as u8];
            packet.extend(client.as_bytes());
            packet
        } else {
            vec![0x06, byte]
        }
    } else {
        match name {
            "last" => b"G".to_vec(),
            "up" => b"\x1b[A".to_vec(),
            "down" => b"\x1b[B".to_vec(),
            "left" => b"\x1b[D".to_vec(),
            "right" => b"\x1b[C".to_vec(),
            "home" => b"\x1b[H".to_vec(),
            "end" => b"\x1b[F".to_vec(),
            "delete" => b"\x1b[3~".to_vec(),
            "enter" => b"\r".to_vec(),
            "escape" => b"\x1b".to_vec(),
            "backspace" => vec![0x7f],
            "clear-search" => vec![0x15],
            "search" => b"/".to_vec(),
            "filter" => b"f".to_vec(),
            "all" => vec![0x0c],
            "space" => b" ".to_vec(),
            // Unclaimed keys: any byte the protocol decodes to nothing.
            "other" => vec![0x0b],
            "h" => b"h".to_vec(),
            "collapse-all" => b"z".to_vec(),
            "expand-all" => b"Z".to_vec(),
            "restore-layout" => b"R".to_vec(),
            "dismiss-layout" => b"x".to_vec(),
            "mark" => b"v".to_vec(),
            "mark-range" => b"V".to_vec(),
            "j" => b"j".to_vec(),
            "k" => b"k".to_vec(),
            "wheel-up" => vec![0x01],
            "wheel-down" => vec![0x02],
            "l" => b"l".to_vec(),
            // Legacy alias for the popup-only quit: the daemon ignores it like
            // Ctrl-C, whereas a printable `q` would now resolve to close.
            "q" => vec![0x03],
            "close" => b"Q".to_vec(),
            "help" => b"?".to_vec(),
            "versions" => b"U".to_vec(),
            "undo" => b"u".to_vec(),
            "settings" => b"s".to_vec(),
            "toggle-panes" => b".".to_vec(),
            _ => return 2,
        }
    };
    if let Some(client) = client.filter(|client| !client.is_empty()) {
        if !name.starts_with("sequence-") {
            if client.len() > 255 || bytes.is_empty() || bytes.len() > 16 {
                return 2;
            }
            let mut packet = vec![0x08, bytes.len() as u8, 0, client.len() as u8];
            packet.extend(&bytes);
            packet.extend(client.as_bytes());
            return send_bytes(&packet);
        }
    } else if client.is_some() {
        return 2;
    }
    send_bytes(&bytes)
}

/// FIFO bytes for a key the binding delivered through a tmux buffer name.
pub(crate) fn buffer_key_bytes(action: &str) -> Option<&'static [u8]> {
    Some(match action {
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "wheel-up" => &[0x01],
        "wheel-down" => &[0x02],
        _ => return None,
    })
}

fn send_bytes(bytes: &[u8]) -> i32 {
    match write_fifo(&tmux::runtime_dir(), bytes) {
        Ok(()) => 0,
        // Named so a dropped key is diagnosable: ENXIO/ENOENT mean no daemon
        // reads the FIFO, EAGAIN that it has not drained it
        Err(error) => {
            eprintln!("agenmux: cannot deliver key: {error}");
            1
        }
    }
}

pub enum Direction {
    Up,
    Down,
}

/// `second` marks tmux's SecondClick/TripleClick: a press that follows
/// another within tmux's click timeout, wherever that one landed.
pub fn click(pane: &str, y: usize, client: &str, second: bool) -> i32 {
    if client.is_empty() {
        return 0;
    }
    // One fork validates both tmux-supplied identities: the command fails
    // for an unknown client or pane, and echoes the pane it resolved.
    let resolved = tmux::command(&[
        "display-message",
        "-p",
        "-c",
        client,
        "-t",
        pane,
        "#{pane_id}",
    ]);
    if !resolved.is_ok_and(|id| id.trim() == pane) {
        return 0;
    }
    let runtime = tmux::runtime_dir();

    let target = std::fs::read_to_string(runtime.join("agenmux-rows"))
        .ok()
        .and_then(|rows| {
            let mut fields = rows.lines().nth(y)?.split_whitespace();
            let target = fields.next()?.to_string();
            let index = fields.next()?.parse::<u32>().ok()?;
            // 2 and 3 mark a selected and an unselected branch header.
            let header = matches!(fields.next(), Some("2" | "3"));
            Some((target, index, header))
        })
        // "=" is the clicked sidebar pane itself: overlay rows are rendered
        // into every sidebar pane, so the row map cannot name one up front.
        .filter(|(target, _, _)| {
            target == "="
                || target.starts_with('%')
                    && tmux::command(&["display-message", "-p", "-t", target, "#{pane_id}"])
                        .is_ok_and(|id| id.trim() == target)
        });
    let press = match &target {
        Some((target, index, _)) => format!("{client}\t{pane}\t{target}\t{index}"),
        None => format!("{client}\t{pane}\t-"),
    };
    let record = runtime.join("agenmux-click");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let double = second
        && std::fs::read_to_string(&record).is_ok_and(|last| is_double_click(&last, &press, now));

    // Every hop below is one tmux fork: chained commands, not one per step.
    match target {
        Some((target, _, false)) if double && target.starts_with('%') => {
            let _ = send_bytes_to(&runtime, &[0x0c]); // "all"
            let _ = tmux::command_status(&[
                "switch-client",
                "-c",
                client,
                "-t",
                &target,
                ";",
                "select-window",
                "-t",
                &target,
                ";",
                "select-pane",
                "-t",
                &target,
            ]);
        }
        target => {
            if tmux::command_status(&[
                "switch-client",
                "-c",
                client,
                "-t",
                pane,
                ";",
                "switch-client",
                "-c",
                client,
                "-T",
                "agenmux",
            ])
            .is_ok()
            {
                if let Some((_, index, _)) = target {
                    let mut bytes = vec![if double { 0x0f } else { 0x05 }];
                    bytes.extend(index.to_be_bytes());
                    let _ = send_bytes_to(&runtime, &bytes);
                }
            }
        }
    }
    // Record only after this press acted, so a double click that reads it
    // cannot be overtaken by its own first press focusing the sidebar.
    let _ = std::fs::write(&record, format!("{press}\t{now}"));
    0
}

/// A second press opens only the record the previous press hit. tmux sends
/// it within its 300ms click timeout; the slack covers a slow first handler,
/// and anything older is a stale record, not this click's first press.
fn is_double_click(last: &str, press: &str, now: u128) -> bool {
    last.rsplit_once('\t').is_some_and(|(previous, at)| {
        previous == press
            && at
                .parse::<u128>()
                .is_ok_and(|at| now.saturating_sub(at) <= 500)
    })
}

pub fn wheel(pane: &str, direction: Direction) -> i32 {
    let resolved = tmux::command(&[
        "display-message",
        "-p",
        "-t",
        pane,
        "#{pane_id}|#{pane_title}",
    ]);
    if !resolved.is_ok_and(|value| value.trim() == format!("{pane}|agenmux")) {
        return 0;
    }
    send_key(
        match direction {
            Direction::Up => "wheel-up",
            Direction::Down => "wheel-down",
        },
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_press_opens_only_the_record_the_first_press_hit() {
        let press = "client\t%1\t%7\t3";
        assert!(is_double_click(&format!("{press}\t1000"), press, 1300));
        // tmux reports a press on another row as a second click too.
        assert!(!is_double_click("client\t%1\t%8\t4\t1000", press, 1300));
        assert!(!is_double_click("other\t%1\t%7\t3\t1000", press, 1300));
        assert!(!is_double_click("client\t%2\t%7\t3\t1000", press, 1300));
        assert!(!is_double_click("client\t%1\t-\t1000", press, 1300));
        // A stale record is not this click's first press.
        assert!(!is_double_click(&format!("{press}\t1000"), press, 1501));
        assert!(!is_double_click("", press, 1300));
        assert!(!is_double_click(press, press, 1300));
    }
}
