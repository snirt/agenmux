use crate::app_config::{action_for, Action, KeyChord, KeyMode, Keymap};
use crate::tmux;
use std::time::{Duration, Instant};

pub(crate) struct RawMode(Option<libc::termios>);

impl RawMode {
    pub(crate) fn enable() -> RawMode {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return RawMode(None); // not a tty (tests) — keys just won't work
            }
            let orig = t;
            t.c_lflag &= !(libc::ICANON | libc::ECHO);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
            libc::tcsetattr(0, libc::TCSANOW, &t);
            RawMode(Some(orig))
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(orig) = self.0 {
            unsafe { libc::tcsetattr(0, libc::TCSANOW, &orig) };
        }
    }
}

pub(crate) fn term_size() -> (usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            return (ws.ws_col as usize, ws.ws_row as usize);
        }
    }
    (30, 24)
}

/// poll one fd; returns true when readable. timeout None = wait forever.
fn poll_fd(fd: libc::c_int, timeout: Option<Duration>) -> bool {
    let mut fds = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ms = timeout.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
    unsafe { libc::poll(&mut fds, 1, ms) > 0 && fds.revents & libc::POLLIN != 0 }
}

/// Is a key already waiting on `fd`? Never blocks.
pub(crate) fn key_pending(fd: libc::c_int) -> bool {
    poll_fd(fd, Some(Duration::ZERO))
}

/// poll the key fd + the tmux control pipe; returns (key_ready, pipe_ready).
/// pipe_buffered short-circuits the wait — data is already in the BufReader.
pub(crate) fn poll_inputs(
    key_fd: libc::c_int,
    pipe_fd: libc::c_int,
    pipe_buffered: bool,
    timeout: Duration,
) -> (bool, bool) {
    let mut fds = [
        libc::pollfd {
            fd: key_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: pipe_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let ms = if pipe_buffered {
        0
    } else {
        timeout.as_millis().min(i32::MAX as u128) as i32
    };
    let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
    let key = n > 0 && fds[0].revents & libc::POLLIN != 0;
    // a dead pipe sets only HUP/ERR/NVAL — POLLIN alone never reports it, and
    // the loop then sleeps forever instead of reading its way to EOF
    let dead = libc::POLLHUP | libc::POLLERR | libc::POLLNVAL;
    let pipe = pipe_buffered || (n > 0 && fds[1].revents & (libc::POLLIN | dead) != 0);
    (key, pipe)
}

/// Read one byte; None on EOF or error.
fn read_byte(fd: libc::c_int) -> Option<u8> {
    let mut b = [0u8; 1];
    let n = unsafe { libc::read(fd, b.as_mut_ptr().cast(), 1) };
    (n == 1).then_some(b[0])
}

#[derive(Clone, Debug)]
pub(crate) enum Key {
    First,
    Last,
    Sequence(char, Option<String>),
    Owned(Box<Key>, String),
    Up,
    Select(usize),
    Down,
    WheelUp,
    WheelDown,
    Jump,
    Quit,
    Close,
    Help,
    Versions,
    Settings,
    Search,
    Backspace,
    ClearSearch,
    ToggleAttention,
    AllStates,
    TogglePanes,
    Text(String),
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SequenceAction {
    First,
    CreateWindow,
    CreateSession,
    /// `dd`: resolved to the selected record's scope when it fires.
    Delete,
    DeletePane,
    DeleteWindow,
    DeleteSession,
    Rename,
    RenamePane,
    RenameWindow,
    RenameSession,
}

#[derive(Clone, Copy)]
pub(crate) struct BuiltinSequence {
    pub sequence: &'static str,
    pub action: SequenceAction,
    pub label: &'static str,
    mutation: bool,
}

pub(crate) const BUILTIN_SEQUENCES: &[BuiltinSequence] = &[
    BuiltinSequence {
        sequence: "gg",
        action: SequenceAction::First,
        label: "first visible pane",
        mutation: false,
    },
    BuiltinSequence {
        sequence: "cc",
        action: SequenceAction::CreateWindow,
        label: "create window",
        mutation: true,
    },
    BuiltinSequence {
        sequence: "cs",
        action: SequenceAction::CreateSession,
        label: "create session",
        mutation: true,
    },
    BuiltinSequence {
        sequence: "dd",
        action: SequenceAction::Delete,
        label: "delete selected session/window/pane",
        mutation: true,
    },
    BuiltinSequence {
        sequence: "r",
        action: SequenceAction::Rename,
        label: "rename pane/window/session",
        mutation: true,
    },
];

pub(crate) fn available_sequences(
    management_enabled: bool,
) -> impl Iterator<Item = &'static BuiltinSequence> {
    BUILTIN_SEQUENCES
        .iter()
        .filter(move |binding| management_enabled || !binding.mutation)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SequenceDispatch {
    Builtin(SequenceAction),
    QuickLauncher(String),
}

#[derive(Clone, Debug)]
pub(crate) struct SequenceBinding {
    pub sequence: String,
    pub action: SequenceDispatch,
    pub label: String,
}

pub(crate) fn sequence_bindings(config: &crate::app_config::AppConfig) -> Vec<SequenceBinding> {
    let mut bindings: Vec<_> = available_sequences(config.tmux_management_enabled)
        .map(|binding| SequenceBinding {
            sequence: binding.sequence.into(),
            action: SequenceDispatch::Builtin(binding.action),
            label: binding.label.into(),
        })
        .collect();
    if config.tmux_management_enabled {
        bindings.extend(
            config
                .quick_launchers
                .iter()
                .filter(|launcher| launcher.enabled)
                .map(|launcher| SequenceBinding {
                    sequence: launcher.sequence.clone(),
                    action: SequenceDispatch::QuickLauncher(launcher.id.clone()),
                    label: launcher.label.clone(),
                }),
        );
    }
    bindings
}

#[derive(Default)]
pub(crate) struct KeySequence {
    pending: String,
    last: Option<Instant>,
    client: Option<String>,
}

pub(crate) enum SequenceResult {
    Pending,
    Match(SequenceDispatch, Option<String>),
    Miss,
}

impl KeySequence {
    pub(crate) fn push(
        &mut self,
        key: char,
        client: Option<String>,
        now: Instant,
        timeout: Duration,
        config: &crate::app_config::AppConfig,
    ) -> SequenceResult {
        if !self.pending.is_empty()
            && self.client.is_some()
            && client.is_some()
            && self.client != client
        {
            self.clear();
        }
        self.expire(now, timeout);
        self.pending.push(key);
        self.last = Some(now);
        if client.is_some() {
            self.client = client;
        }
        let bindings = sequence_bindings(config);
        if let Some(binding) = bindings
            .iter()
            .find(|binding| binding.sequence == self.pending)
        {
            let action = binding.action.clone();
            let client = self.client.take();
            self.clear();
            SequenceResult::Match(action, client)
        } else if bindings
            .iter()
            .any(|binding| binding.sequence.starts_with(&self.pending))
        {
            SequenceResult::Pending
        } else {
            self.clear();
            SequenceResult::Miss
        }
    }

    pub(crate) fn continuations(
        &self,
        config: &crate::app_config::AppConfig,
    ) -> Vec<(char, String)> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        sequence_bindings(config)
            .iter()
            .filter_map(|binding| {
                binding
                    .sequence
                    .strip_prefix(&self.pending)?
                    .chars()
                    .next()
                    .map(|key| (key, binding.label.clone()))
            })
            .collect()
    }

    pub(crate) fn deadline(&self, timeout: Duration) -> Option<Instant> {
        self.last.map(|last| last + timeout)
    }

    pub(crate) fn expire(&mut self, now: Instant, timeout: Duration) -> bool {
        let expired = self
            .last
            .is_some_and(|last| now.duration_since(last) >= timeout);
        if expired {
            self.clear();
        }
        expired
    }

    pub(crate) fn pending_prefix(&self) -> Option<char> {
        self.pending.chars().next()
    }

    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.last = None;
        self.client = None;
    }
}

/// Physical byte(s) → chord. `next` yields escape-tail bytes, None once
/// nothing more arrives.
fn chord(first: u8, next: impl FnMut() -> Option<u8>) -> Option<KeyChord> {
    Some(match first {
        0x1b => return escape_chord(next),
        0x09 => KeyChord::Tab,
        0x0a | 0x0d => KeyChord::Enter,
        0x08 | 0x7f => KeyChord::Backspace,
        0x00..=0x1f => KeyChord::Control(first),
        0x20..=0x7e => KeyChord::Printable(first),
        _ => return None,
    })
}

/// Decode the tail of an escape sequence: CSI (ESC [ A) normally, SS3
/// (ESC O A) in application-cursor mode, tilde forms for Home/End/PgUp/PgDn.
/// No tail is a bare Esc; an incomplete or unknown tail is nothing.
fn escape_chord(mut next: impl FnMut() -> Option<u8>) -> Option<KeyChord> {
    let Some(a) = next() else {
        return Some(KeyChord::Escape);
    };
    if a != b'[' && a != b'O' {
        return None;
    }
    Some(match next()? {
        b'A' => KeyChord::Up,
        b'B' => KeyChord::Down,
        b'C' => KeyChord::Right,
        b'D' => KeyChord::Left,
        b'H' => KeyChord::Home,
        b'F' => KeyChord::End,
        digit @ b'1'..=b'8' if a == b'[' && next()? == b'~' => match digit {
            b'1' | b'7' => KeyChord::Home,
            b'4' | b'8' => KeyChord::End,
            b'5' => KeyChord::PageUp,
            b'6' => KeyChord::PageDown,
            _ => return None,
        },
        _ => return None,
    })
}

/// Logical action → dispatcher key. Popup input maps chords through the
/// user's keymap; the daemon maps fixed FIFO protocol bytes through defaults.
fn action_key(keys: &Keymap, chord: KeyChord) -> Option<Key> {
    Some(match action_for(keys, chord)? {
        Action::Down => Key::Down,
        Action::Up => Key::Up,
        Action::Jump | Action::Accept => Key::Jump,
        Action::Search => Key::Search,
        Action::Filter => Key::ToggleAttention,
        Action::Reset | Action::Cancel => Key::AllStates,
        Action::Help => Key::Help,
        Action::Versions => Key::Versions,
        Action::Settings => Key::Settings,
        Action::Close => Key::Close,
        Action::Backspace => Key::Backspace,
        Action::Clear => Key::ClearSearch,
    })
}

/// Keys the daemon decodes with. Its FIFO carries the fixed `agenmux key`
/// protocol, not physical keys: the tmux tables already resolved the user's
/// chords into action names. The user's own keymap stays the one hints and
/// help are named from, so both modes advertise the keys that actually work.
pub(crate) fn protocol_keys(mode: KeyMode) -> &'static Keymap {
    static KEYS: std::sync::OnceLock<(Keymap, Keymap)> = std::sync::OnceLock::new();
    let (normal, search) = KEYS.get_or_init(|| {
        (
            crate::app_config::resolved_keys(KeyMode::Normal, None).unwrap(),
            crate::app_config::resolved_keys(KeyMode::Search, None).unwrap(),
        )
    });
    match mode {
        KeyMode::Normal => normal,
        KeyMode::Search => search,
    }
}

pub(crate) fn settings_keys() -> &'static Keymap {
    static KEYS: std::sync::OnceLock<Keymap> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        let mut keys = protocol_keys(KeyMode::Search).clone();
        if let Some(up) = keys.get_mut(&Action::Up) {
            up.push(KeyChord::Left);
        }
        if let Some(down) = keys.get_mut(&Action::Down) {
            down.push(KeyChord::Right);
        }
        keys
    })
}

fn decode_protocol_payload(
    first: u8,
    mut next: impl FnMut() -> Option<u8>,
    keys: &Keymap,
    config: &crate::app_config::AppConfig,
) -> Key {
    match first {
        0x01 => Key::WheelUp,
        0x02 => Key::WheelDown,
        0x0c => Key::AllStates,
        0x00 => next()
            .filter(|byte| (0x20..=0x7e).contains(byte))
            .map(|byte| Key::Text(char::from(byte).to_string()))
            .unwrap_or(Key::Other),
        _ => chord(first, next)
            .and_then(|chord| action_key(keys, chord))
            .unwrap_or(match first {
                b'G' => Key::Last,
                b'.' => Key::TogglePanes,
                byte if sequence_bindings(config)
                    .iter()
                    .any(|binding| binding.sequence.as_bytes()[0] == byte) =>
                {
                    Key::Sequence(char::from(byte), None)
                }
                0x03 | 0x04 => Key::Quit,
                _ => Key::Other,
            }),
    }
}

#[cfg(test)]
pub(crate) fn read_key(fd: libc::c_int, keys: &Keymap) -> Key {
    static DEFAULTS: std::sync::OnceLock<crate::app_config::AppConfig> = std::sync::OnceLock::new();
    let config = DEFAULTS.get_or_init(|| {
        crate::app_config::resolve(
            &crate::app_config::FileConfig::default(),
            &Default::default(),
        )
        .unwrap()
    });
    read_key_with_config(fd, keys, config)
}

pub(crate) fn read_key_with_config(
    fd: libc::c_int,
    keys: &Keymap,
    config: &crate::app_config::AppConfig,
) -> Key {
    let Some(b) = read_byte(fd) else {
        return Key::Quit;
    }; // EOF: explicit close
       // Every byte of the tail goes through the same polling reader. In mirror
       // mode keys arrive over a non-blocking FIFO that the key sender feeds one
       // byte at a time, so the tail is routinely still in flight; reading it
       // without polling hit EAGAIN and dropped every other arrow.
    let next = || {
        poll_fd(fd, Some(Duration::from_millis(50)))
            .then(|| read_byte(fd))
            .flatten()
    };
    match b {
        // Click target: a four-byte row index the mouse helper sends.
        0x05 => {
            let mut index = [0u8; 4];
            for byte in &mut index {
                let Some(got) = next() else {
                    return Key::Other;
                };
                *byte = got;
            }
            return Key::Select(u32::from_be_bytes(index) as usize);
        }
        // Multi-key sequence, legacy packet without client identity.
        0x06 => {
            return next()
                .filter(|b| (0x20..=0x7e).contains(b))
                .map(|b| Key::Sequence(char::from(b), None))
                .unwrap_or(Key::Other)
        }
        // Framed sequence packet: key, u16 length, invoking client UTF-8.
        0x07 => {
            let Some(key) = next().filter(|b| (0x20..=0x7e).contains(b)) else {
                return Key::Other;
            };
            let (Some(high), Some(low)) = (next(), next()) else {
                return Key::Other;
            };
            let len = u16::from_be_bytes([high, low]) as usize;
            if len == 0 || len > 255 {
                return Key::Other;
            }
            let mut client = Vec::with_capacity(len);
            for _ in 0..len {
                let Some(byte) = next() else {
                    return Key::Other;
                };
                client.push(byte);
            }
            return String::from_utf8(client)
                .map(|client| Key::Sequence(char::from(key), Some(client)))
                .unwrap_or(Key::Other);
        }
        // Framed logical key: payload length, u16 client length, payload, client.
        0x08 => {
            let Some(payload_len) = next().map(usize::from).filter(|len| (1..=16).contains(len))
            else {
                return Key::Other;
            };
            let (Some(high), Some(low)) = (next(), next()) else {
                return Key::Other;
            };
            let client_len = u16::from_be_bytes([high, low]) as usize;
            if client_len == 0 || client_len > 255 {
                return Key::Other;
            }
            let mut payload = Vec::with_capacity(payload_len);
            for _ in 0..payload_len {
                let Some(byte) = next() else {
                    return Key::Other;
                };
                payload.push(byte);
            }
            let mut client = Vec::with_capacity(client_len);
            for _ in 0..client_len {
                let Some(byte) = next() else {
                    return Key::Other;
                };
                client.push(byte);
            }
            let mut payload = payload.into_iter();
            let key =
                decode_protocol_payload(payload.next().unwrap(), || payload.next(), keys, config);
            return String::from_utf8(client)
                .map(|client| Key::Owned(Box::new(key), client))
                .unwrap_or(Key::Other);
        }
        _ => {}
    }
    decode_protocol_payload(b, next, keys, config)
}

/// Popup/tty search owns printable input. Daemon search receives printable
/// bytes through NUL-prefixed packets decoded by read_key instead.
pub(crate) fn read_search_key(fd: libc::c_int, keys: &Keymap) -> Key {
    let Some(first) = read_byte(fd) else {
        return Key::Quit;
    };
    let next = || {
        poll_fd(fd, Some(Duration::from_millis(50)))
            .then(|| read_byte(fd))
            .flatten()
    };
    if first >= 0x20 && first != 0x7f {
        let len = if first < 0x80 {
            1
        } else if first & 0xe0 == 0xc0 {
            2
        } else if first & 0xf0 == 0xe0 {
            3
        } else if first & 0xf8 == 0xf0 {
            4
        } else {
            return Key::Other;
        };
        let mut bytes = vec![first];
        for _ in 1..len {
            let Some(b) = next() else {
                return Key::Other;
            };
            bytes.push(b);
        }
        return String::from_utf8(bytes)
            .map(Key::Text)
            .unwrap_or(Key::Other);
    }
    chord(first, next)
        .and_then(|chord| action_key(keys, chord))
        .unwrap_or(match first {
            0x03 | 0x04 => Key::Quit,
            _ => Key::Other,
        })
}

/// Deliver one key-table action to the daemon without waiting for a FIFO
/// reader. Each invocation is intentionally short-lived; the daemon remains
/// the only persistent agenmux process.
pub fn send_key(name: &str, client: Option<&str>) -> i32 {
    let valid_close =
        name == "close" && client.is_none_or(|client| !client.is_empty() && client.len() <= 255);
    let _lifecycle = if valid_close {
        let lock = match crate::panes::lifecycle_lock() {
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
            "enter" => b"\r".to_vec(),
            "escape" => b"\x1b".to_vec(),
            "backspace" => vec![0x7f],
            "clear-search" => vec![0x15],
            "search" => b"/".to_vec(),
            "filter" => b"f".to_vec(),
            "all" => vec![0x0c],
            "space" => b" ".to_vec(),
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
            "versions" => b"u".to_vec(),
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
    send_bytes_to(&crate::tmux::runtime_dir(), bytes)
}

fn send_bytes_to(runtime: &std::path::Path, bytes: &[u8]) -> i32 {
    let path = runtime.join("agenmux-keys");
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return 1;
    };
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        return 1;
    }
    let wrote = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    unsafe { libc::close(fd) };
    (wrote != bytes.len() as isize) as i32
}

pub enum Direction {
    Up,
    Down,
}

pub fn click(pane: &str, y: usize, client: &str) -> i32 {
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

    let target = y
        .checked_sub(1)
        .and_then(|line| {
            let rows = std::fs::read_to_string(runtime.join("agenmux-rows")).ok()?;
            let mut fields = rows.lines().nth(line)?.split_whitespace();
            let target = fields.next()?.to_string();
            let index = fields.next()?.parse::<u32>().ok()?;
            let selected = fields.next() == Some("1");
            Some((target, index, selected))
        })
        // "=" is the clicked sidebar pane itself: overlay rows are rendered
        // into every sidebar pane, so the row map cannot name one up front.
        .filter(|(target, _, _)| {
            target == "="
                || target.starts_with('%')
                    && tmux::command(&["display-message", "-p", "-t", target, "#{pane_id}"])
                        .is_ok_and(|id| id.trim() == target)
        });

    // Every hop below is one tmux fork: chained commands, not one per step.
    if let Some((target, index, selected)) = target {
        if selected {
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
        } else if tmux::command_status(&[
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
            let mut bytes = vec![0x05];
            bytes.extend(index.to_be_bytes());
            let _ = send_bytes_to(&runtime, &bytes);
        }
    } else {
        let _ = tmux::command_status(&[
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
        ]);
    }
    0
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

    fn config(management_enabled: bool) -> crate::app_config::AppConfig {
        let source = if management_enabled {
            "[tmux_management]\nenabled = true"
        } else {
            ""
        };
        let file = crate::app_config::parse(source).unwrap();
        crate::app_config::resolve(&file, &Default::default()).unwrap()
    }

    #[test]
    fn arrows_work_in_both_cursor_key_modes() {
        // the regression: only CSI was decoded, so arrows did nothing in panes
        // tmux had put in application-cursor mode (it sends SS3 there)
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let feed = |b: &[u8]| unsafe { libc::write(fds[1], b.as_ptr().cast(), b.len()) };

        let keys = default_keys(crate::app_config::KeyMode::Normal);
        for seq in [b"\x1b[A".as_slice(), b"\x1bOA".as_slice()] {
            feed(seq);
            assert!(matches!(read_key(fds[0], &keys), Key::Up), "up: {seq:?}");
        }
        for seq in [b"\x1b[B".as_slice(), b"\x1bOB".as_slice()] {
            feed(seq);
            assert!(
                matches!(read_key(fds[0], &keys), Key::Down),
                "down: {seq:?}"
            );
        }
        feed(b"j");
        assert!(matches!(read_key(fds[0], &keys), Key::Down));
        feed(b"g");
        assert!(matches!(read_key(fds[0], &keys), Key::Sequence('g', None)));
        feed(&[0x06, b'g']);
        assert!(matches!(read_key(fds[0], &keys), Key::Sequence('g', None)));
        feed(&[0x07, b'c', 0, 7]);
        feed(b"client1");
        assert!(matches!(
            read_key(fds[0], &keys),
            Key::Sequence('c', Some(client)) if client == "client1"
        ));
        feed(b"G");
        assert!(matches!(read_key(fds[0], &keys), Key::Last));
        feed(&[0x01]);
        assert!(matches!(read_key(fds[0], &keys), Key::WheelUp));
        feed(&[0x02]);
        assert!(matches!(read_key(fds[0], &keys), Key::WheelDown));
        feed(b"u");
        assert!(matches!(read_key(fds[0], &keys), Key::Versions));
        feed(&[0, b'q']);
        assert!(matches!(read_key(fds[0], &keys), Key::Text(s) if s == "q"));
        feed(&[0x03]);
        assert!(matches!(read_key(fds[0], &keys), Key::Quit));

        // Remapped popup input: the user's chords decide, defaults are gone,
        // and search text/emergency keys stay outside the keymap.
        let file = crate::app_config::parse(
            "[keys.normal]\ndown = ['n', 'PageDown']\nup = ['p']\nclose = []\n[keys.search]\ncancel = ['C-g']\n",
        )
        .unwrap();
        let custom = crate::app_config::resolve(&file, &Default::default()).unwrap();
        feed(b"n");
        assert!(matches!(read_key(fds[0], &custom.normal), Key::Down));
        feed(b"\x1b[6~");
        assert!(matches!(read_key(fds[0], &custom.normal), Key::Down));
        feed(b"j");
        assert!(matches!(read_key(fds[0], &custom.normal), Key::Other));
        feed(b"q");
        assert!(matches!(read_key(fds[0], &custom.normal), Key::Other));
        feed(&[0x04]);
        assert!(matches!(read_key(fds[0], &custom.normal), Key::Quit));
        feed(&[0x07]);
        assert!(matches!(
            read_search_key(fds[0], &custom.search),
            Key::AllStates
        ));
        feed(&[0x03]);
        assert!(matches!(read_search_key(fds[0], &custom.search), Key::Quit));
        feed("é".as_bytes());
        assert!(matches!(read_search_key(fds[0], &custom.search), Key::Text(s) if s == "é"));
        feed(b"n");
        assert!(matches!(read_search_key(fds[0], &custom.search), Key::Text(s) if s == "n"));
        let enabled = config(true);
        let disabled = config(false);
        feed(b"e");
        assert!(matches!(
            read_key_with_config(fds[0], &enabled.normal, &enabled),
            Key::Sequence('e', None)
        ));
        feed(b"o");
        assert!(matches!(
            read_key_with_config(fds[0], &enabled.normal, &enabled),
            Key::Sequence('o', None)
        ));
        feed(b"e");
        assert!(matches!(
            read_key_with_config(fds[0], &disabled.normal, &disabled),
            Key::Other
        ));
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn built_in_sequences_share_dispatch_continuations_and_expiry() {
        let start = Instant::now();
        let timeout = Duration::from_millis(1000);
        let mut sequence = KeySequence::default();
        let enabled = config(true);
        let disabled = config(false);

        assert!(matches!(
            sequence.push('c', Some("client-a".into()), start, timeout, &enabled),
            SequenceResult::Pending
        ));
        assert_eq!(
            sequence.continuations(&enabled),
            vec![
                ('c', "create window".into()),
                ('s', "create session".into())
            ]
        );
        assert!(matches!(
            sequence.push(
                'c',
                Some("client-a".into()),
                start + Duration::from_millis(10),
                timeout,
                &enabled
            ),
            SequenceResult::Match(
                SequenceDispatch::Builtin(SequenceAction::CreateWindow),
                Some(client)
            ) if client == "client-a"
        ));

        assert!(matches!(
            sequence.push('r', Some("client-a".into()), start, timeout, &enabled),
            SequenceResult::Match(
                SequenceDispatch::Builtin(SequenceAction::Rename),
                Some(client)
            ) if client == "client-a"
        ));
        assert!(matches!(
            sequence.push('r', None, start, timeout, &disabled),
            SequenceResult::Miss
        ));

        assert!(matches!(
            sequence.push('d', None, start, timeout, &disabled),
            SequenceResult::Miss
        ));
        assert!(matches!(
            sequence.push('g', None, start, timeout, &disabled),
            SequenceResult::Pending
        ));
        assert_eq!(
            sequence.continuations(&disabled),
            vec![('g', "first visible pane".into())]
        );
        assert!(sequence.expire(start + timeout + Duration::from_millis(1), timeout));
        assert!(sequence.continuations(&enabled).is_empty());
        assert!(matches!(
            sequence.push('g', None, start + timeout, timeout, &disabled),
            SequenceResult::Pending
        ));
    }

    #[test]
    fn sequence_continuations_do_not_cross_clients() {
        let start = Instant::now();
        let timeout = Duration::from_secs(1);
        let mut sequence = KeySequence::default();
        let enabled = config(true);

        assert!(matches!(
            sequence.push('c', Some("client-a".into()), start, timeout, &enabled),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push('s', Some("client-b".into()), start, timeout, &enabled),
            SequenceResult::Miss
        ));
        assert!(matches!(
            sequence.push('c', Some("client-b".into()), start, timeout, &enabled),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push('c', Some("client-b".into()), start, timeout, &enabled),
            SequenceResult::Match(
                SequenceDispatch::Builtin(SequenceAction::CreateWindow),
                Some(client)
            )
                if client == "client-b"
        ));
    }

    #[test]
    fn quick_launcher_sequences_dispatch_only_when_management_is_enabled() {
        let source = r#"
[tmux_management]
enabled = true

[quick_launchers.terminal]
sequence = "zt"
label = "terminal"
command = "fish"
"#;
        let file = crate::app_config::parse(source).unwrap();
        let enabled = crate::app_config::resolve(&file, &Default::default()).unwrap();
        let disabled = crate::app_config::resolve(
            &crate::app_config::parse(
                "[quick_launchers.terminal]\nsequence='zt'\nlabel='terminal'\ncommand='fish'",
            )
            .unwrap(),
            &Default::default(),
        )
        .unwrap();
        let start = Instant::now();
        let timeout = Duration::from_secs(1);
        let mut sequence = KeySequence::default();

        assert!(matches!(
            sequence.push('e', None, start, timeout, &enabled),
            SequenceResult::Match(
                SequenceDispatch::QuickLauncher(id),
                None
            ) if id == "nvim"
        ));
        assert!(matches!(
            sequence.push('o', None, start, timeout, &enabled),
            SequenceResult::Pending
        ));
        assert_eq!(
            sequence.continuations(&enabled),
            vec![(('g'), "lazygit".into())]
        );
        assert!(matches!(
            sequence.push('g', None, start, timeout, &enabled),
            SequenceResult::Match(
                SequenceDispatch::QuickLauncher(id),
                None
            ) if id == "lazygit"
        ));
        assert!(matches!(
            sequence.push('z', None, start, timeout, &enabled),
            SequenceResult::Pending
        ));
        assert_eq!(
            sequence.continuations(&enabled),
            vec![(('t'), "terminal".into())]
        );
        assert!(matches!(
            sequence.push('t', None, start, timeout, &enabled),
            SequenceResult::Match(
                SequenceDispatch::QuickLauncher(id),
                None
            ) if id == "terminal"
        ));
        assert!(matches!(
            sequence.push('e', None, start, timeout, &disabled),
            SequenceResult::Miss
        ));
        assert!(matches!(
            sequence.push('z', None, start, timeout, &disabled),
            SequenceResult::Miss
        ));
    }

    /// Feed escape_chord a scripted tail. Every byte goes through the same
    /// reader, which is the point: mirror mode delivers keys over a
    /// non-blocking FIFO one byte at a time, so a tail byte that has not
    /// arrived yet must be waited for, not read blind. Reading the pair
    /// without polling hit EAGAIN and dropped every other arrow.
    fn decode(tail: &[Option<u8>]) -> Option<KeyChord> {
        let mut it = tail.iter().copied();
        escape_chord(move || it.next().flatten())
    }

    fn default_keys(mode: crate::app_config::KeyMode) -> Keymap {
        crate::app_config::resolved_keys(mode, None).unwrap()
    }

    #[test]
    fn escape_tails_decode_in_both_cursor_key_modes() {
        assert_eq!(decode(&[Some(b'['), Some(b'A')]), Some(KeyChord::Up));
        assert_eq!(decode(&[Some(b'['), Some(b'B')]), Some(KeyChord::Down));
        assert_eq!(decode(&[Some(b'O'), Some(b'A')]), Some(KeyChord::Up));
        assert_eq!(decode(&[Some(b'O'), Some(b'B')]), Some(KeyChord::Down));
        assert_eq!(decode(&[Some(b'['), Some(b'H')]), Some(KeyChord::Home));
        assert_eq!(
            decode(&[Some(b'['), Some(b'1'), Some(b'~')]),
            Some(KeyChord::Home)
        );
        assert_eq!(
            decode(&[Some(b'['), Some(b'5'), Some(b'~')]),
            Some(KeyChord::PageUp)
        );
        assert_eq!(decode(&[]), Some(KeyChord::Escape));
        assert_eq!(decode(&[None]), Some(KeyChord::Escape));
        // a tail that never completes is not a close — Esc already decided that
        assert_eq!(decode(&[Some(b'['), None]), None);
        assert_eq!(decode(&[Some(b'['), Some(b'Z')]), None);
        assert_eq!(decode(&[Some(b'['), Some(b'5'), None]), None);
        // bare Esc is the normal reset and the search cancel by default
        let normal = default_keys(crate::app_config::KeyMode::Normal);
        let search = default_keys(crate::app_config::KeyMode::Search);
        assert!(matches!(
            action_key(&normal, KeyChord::Escape),
            Some(Key::AllStates)
        ));
        assert!(matches!(
            action_key(&search, KeyChord::Escape),
            Some(Key::AllStates)
        ));
        assert!(matches!(
            action_key(&search, KeyChord::Control(21)),
            Some(Key::ClearSearch)
        ));
        assert!(action_key(&normal, KeyChord::Control(21)).is_none());
        assert!(matches!(
            action_key(settings_keys(), KeyChord::Left),
            Some(Key::Up)
        ));
        assert!(matches!(
            action_key(settings_keys(), KeyChord::Right),
            Some(Key::Down)
        ));
    }

    #[test]
    fn bare_esc_clears_filters() {
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) };
        unsafe { libc::write(fds[1], [0x1bu8].as_ptr().cast(), 1) };
        assert!(matches!(
            read_key(fds[0], &default_keys(crate::app_config::KeyMode::Normal)),
            Key::AllStates
        ));
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
