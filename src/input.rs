use crate::app_config::{action_for, Action, KeyChord, KeyMode, Keymap};
use crate::tmux;
use std::time::{Duration, Instant};

pub(crate) struct RawMode(bool);

static SAVED_TERMINAL: std::sync::Mutex<Option<libc::termios>> = std::sync::Mutex::new(None);
static TERMINAL_HOOK: std::sync::Once = std::sync::Once::new();

// Panic cleanup must work even when another thread holds Rust's stdout lock.
fn terminal_write(mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let n = unsafe { libc::write(1, bytes.as_ptr().cast(), bytes.len()) };
        if n > 0 {
            bytes = &bytes[n as usize..];
        } else if n == 0
            || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
        {
            break;
        }
    }
}

fn restore_terminal() {
    // These critical sections contain only non-panicking state changes and syscalls.
    let mut saved = SAVED_TERMINAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(original) = saved.take() {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &original) };
        terminal_write(b"\x1b[?2004l\x1b[?25h");
    }
}

/// True while a `RawMode` holds the terminal, i.e. a TUI frame is on screen.
pub(crate) fn terminal_owned() -> bool {
    SAVED_TERMINAL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some()
}

impl RawMode {
    pub(crate) fn enable() -> RawMode {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return RawMode(false); // not a tty (tests) — keys just won't work
            }
            TERMINAL_HOOK.call_once(|| {
                let previous = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    restore_terminal();
                    previous(info);
                }));
            });
            let mut saved = SAVED_TERMINAL.lock().unwrap_or_else(|e| e.into_inner());
            if saved.is_some() {
                return RawMode(false); // one terminal owner per process
            }
            *saved = Some(t);
            t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
            t.c_iflag &= !(libc::IXON | libc::IXOFF);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(0, libc::TCSANOW, &t) != 0 {
                *saved = None;
                return RawMode(false);
            }
            terminal_write(b"\x1b[?2004h");
            RawMode(true)
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        if self.0 {
            restore_terminal();
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
    Left,
    Right,
    Home,
    End,
    Delete,
    Select(usize),
    /// Double click: select the row, then activate it like Enter.
    Open(usize),
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
    ToggleBranch,
    CollapseAll,
    ExpandAll,
    RestoreLayout,
    Undo,
    DismissLayout,
    Mark,
    MarkRange,
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
    /// `yy`: copy the selected record's tmux reference.
    Copy,
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
    BuiltinSequence {
        sequence: "yy",
        action: SequenceAction::Copy,
        label: "copy selected item reference",
        mutation: false,
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
        0x1b => {
            return match escape_input(next)? {
                EscapeInput::Chord(chord) => Some(chord),
                EscapeInput::Paste => None,
            }
        }
        0x09 => KeyChord::Tab,
        0x0a | 0x0d => KeyChord::Enter,
        0x08 | 0x7f => KeyChord::Backspace,
        0x00..=0x1f => KeyChord::Control(first),
        0x20..=0x7e => KeyChord::Printable(first),
        _ => return None,
    })
}

enum EscapeInput {
    Chord(KeyChord),
    Paste,
}

/// Consume the entire CSI/SS3 tail before deciding whether it is supported.
/// No tail is a bare Esc; an incomplete or unknown tail is nothing.
fn escape_input(mut next: impl FnMut() -> Option<u8>) -> Option<EscapeInput> {
    'escape: loop {
        let Some(prefix) = next() else {
            return Some(EscapeInput::Chord(KeyChord::Escape));
        };
        if prefix == 0x1b {
            continue;
        }
        if prefix != b'[' && prefix != b'O' {
            return None;
        }
        // Supported tails fit in four bytes; still drain longer unsupported tails.
        let mut tail = [0u8; 4];
        let mut len = 0usize;
        loop {
            let byte = next()?;
            // ESC starts a new sequence; CAN/SUB cancel the current sequence.
            match byte {
                0x1b => continue 'escape,
                0x18 | 0x1a => return None,
                _ => {}
            }
            if len < tail.len() {
                tail[len] = byte;
            }
            len += 1;
            if (0x40..=0x7e).contains(&byte) {
                break;
            }
        }
        let tail = tail.get(..len)?;
        return Some(EscapeInput::Chord(match tail {
            b"A" => KeyChord::Up,
            b"B" => KeyChord::Down,
            b"C" => KeyChord::Right,
            b"D" => KeyChord::Left,
            b"H" => KeyChord::Home,
            b"F" => KeyChord::End,
            b"200~" if prefix == b'[' => return Some(EscapeInput::Paste),
            [digit, b'~'] if prefix == b'[' => match digit {
                b'1' | b'7' => KeyChord::Home,
                b'3' => KeyChord::Delete,
                b'4' | b'8' => KeyChord::End,
                b'5' => KeyChord::PageUp,
                b'6' => KeyChord::PageDown,
                _ => return None,
            },
            _ => return None,
        }));
    }
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
        _ => key_for_chord(first, chord(first, next), keys, config),
    }
}

fn key_for_chord(
    first: u8,
    chord: Option<KeyChord>,
    keys: &Keymap,
    config: &crate::app_config::AppConfig,
) -> Key {
    chord
        .and_then(|chord| {
            if keys.contains_key(&Action::Accept) {
                match chord {
                    KeyChord::Left => return Some(Key::Left),
                    KeyChord::Right => return Some(Key::Right),
                    KeyChord::Home => return Some(Key::Home),
                    KeyChord::End => return Some(Key::End),
                    KeyChord::Delete => return Some(Key::Delete),
                    _ => {}
                }
            }
            action_key(keys, chord)
        })
        .unwrap_or(match first {
            b'G' => Key::Last,
            b'.' => Key::TogglePanes,
            byte if sequence_bindings(config)
                .iter()
                .any(|binding| binding.sequence.as_bytes()[0] == byte) =>
            {
                Key::Sequence(char::from(byte), None)
            }
            // Fixed tree keys, like `G` and `.`: a configured chord wins.
            b' ' => Key::ToggleBranch,
            byte if byte == crate::app_config::fold_key(&config.normal) => Key::Left,
            b'z' => Key::CollapseAll,
            b'Z' => Key::ExpandAll,
            b'R' => Key::RestoreLayout,
            b'u' => Key::Undo,
            b'x' => Key::DismissLayout,
            b'v' => Key::Mark,
            b'V' => Key::MarkRange,
            0x03 | 0x04 => Key::Quit,
            0x1b if chord == Some(KeyChord::Left) => Key::Left,
            0x1b if chord == Some(KeyChord::Right) => Key::Right,
            _ => Key::Other,
        })
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
        0x1b => {
            return match escape_input(next) {
                Some(EscapeInput::Paste) => read_paste(fd, false),
                Some(EscapeInput::Chord(chord)) => key_for_chord(b, Some(chord), keys, config),
                None => key_for_chord(b, None, keys, config),
            };
        }
        // Click target: a four-byte row index the mouse helper sends, as a
        // single (0x05) or double (0x0f) click.
        0x05 | 0x0f => {
            let mut index = [0u8; 4];
            for byte in &mut index {
                let Some(got) = next() else {
                    return Key::Other;
                };
                *byte = got;
            }
            let index = u32::from_be_bytes(index) as usize;
            return if b == 0x05 {
                Key::Select(index)
            } else {
                Key::Open(index)
            };
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
        // Literal text from a split pane's PTY. Keep each packet <= PIPE_BUF
        // so it cannot interleave with a tmux key-table packet on the FIFO.
        0x09 => {
            let (Some(high), Some(low)) = (next(), next()) else {
                return Key::Other;
            };
            let len = u16::from_be_bytes([high, low]) as usize;
            if len == 0 || len > 480 {
                return Key::Other;
            }
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                let Some(byte) = next() else {
                    return Key::Other;
                };
                bytes.push(byte);
            }
            return String::from_utf8(bytes)
                .map(Key::Text)
                .unwrap_or(Key::Other);
        }
        _ => {}
    }
    decode_protocol_payload(b, next, keys, config)
}

/// Consume a terminal's bracketed paste as one edit; never interpret pasted
/// Enter/Escape as commands. Drain oversized pastes before the next key.
fn read_paste(fd: libc::c_int, collect: bool) -> Key {
    let mut bytes = Vec::new();
    let mut tail = [0u8; 6];
    let mut count = 0usize;
    // A 1s idle gap ends draining; slower pastes need persistent state to stay isolated.
    while poll_fd(fd, Some(Duration::from_secs(1))) {
        let Some(byte) = read_byte(fd) else { break };
        tail[count % 6] = byte;
        count += 1;
        if count >= 6
            && (0..6)
                .map(|i| tail[(count + i) % 6])
                .eq(b"\x1b[201~".iter().copied())
        {
            bytes.truncate(count - 6);
            return if collect {
                Key::Text(String::from_utf8_lossy(&bytes).into_owned())
            } else {
                Key::Other
            };
        }
        if collect && bytes.len() < 65536 {
            bytes.push(byte);
        }
    }
    Key::Other
}

/// Popup/tty search owns printable input. Daemon search receives printable
/// bytes through NUL-prefixed packets decoded by read_key instead.
pub(crate) fn read_search_key(fd: libc::c_int, keys: &Keymap) -> Key {
    let Some(first) = read_byte(fd) else {
        return Key::Quit;
    };
    let next = || {
        poll_fd(fd, Some(Duration::from_millis(100)))
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
    let chord = if first == 0x1b {
        match escape_input(next) {
            Some(EscapeInput::Paste) => return read_paste(fd, true),
            Some(EscapeInput::Chord(chord)) => Some(chord),
            None => None,
        }
    } else {
        chord(first, next)
    };
    chord
        .and_then(|chord| match chord {
            KeyChord::Left => Some(Key::Left),
            KeyChord::Right => Some(Key::Right),
            KeyChord::Home => Some(Key::Home),
            KeyChord::End => Some(Key::End),
            KeyChord::Delete => Some(Key::Delete),
            _ => action_key(keys, chord),
        })
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
    match write_fifo(&crate::tmux::runtime_dir(), bytes) {
        Ok(()) => 0,
        // Named so a dropped key is diagnosable: ENXIO/ENOENT mean no daemon
        // reads the FIFO, EAGAIN that it has not drained it
        Err(error) => {
            eprintln!("agenmux: cannot deliver key: {error}");
            1
        }
    }
}

/// Relay terminal paste to the daemon without interpreting its bytes as keys.
pub(crate) fn send_text_to(runtime: &std::path::Path, text: &str) -> i32 {
    let mut limit = text.len().min(2048);
    while !text.is_char_boundary(limit) {
        limit -= 1;
    }
    let mut remaining = &text[..limit];
    while !remaining.is_empty() {
        let mut end = remaining.len().min(480);
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, rest) = remaining.split_at(end);
        let mut packet = vec![0x09, (chunk.len() >> 8) as u8, chunk.len() as u8];
        packet.extend_from_slice(chunk.as_bytes());
        if send_bytes_to(runtime, &packet) != 0 {
            return 1;
        }
        remaining = rest;
    }
    0
}

fn send_bytes_to(runtime: &std::path::Path, bytes: &[u8]) -> i32 {
    write_fifo(runtime, bytes).is_err() as i32
}

fn write_fifo(runtime: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let path = runtime.join("agenmux-keys");
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let wrote = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    let error = std::io::Error::last_os_error();
    unsafe { libc::close(fd) };
    if wrote != bytes.len() as isize {
        return Err(error);
    }
    Ok(())
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

    // Run terminal ownership in a separate process: hooks and stdin are global.
    #[test]
    fn terminal_child() {
        let Ok(mode) = std::env::var("AGENMUX_TEST_TERMINAL") else {
            return;
        };
        if mode == "panic" || mode == "after-drop" {
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) }, 0);
            std::panic::set_hook(Box::new(|_| {
                let message = b"previous panic hook\n";
                unsafe { libc::write(2, message.as_ptr().cast(), message.len()) };
                // Abort inside the previous hook so unwinding cannot mask missing cleanup.
                std::process::abort();
            }));
        }
        let raw = RawMode::enable();
        if mode == "non-tty" {
            drop(raw);
            return;
        }
        if mode == "diagnostic" {
            crate::diag::error("during the frame");
            drop(raw);
            crate::diag::error("after the frame");
            return;
        }
        if mode == "after-drop" {
            drop(raw);
            let mut changed: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::tcgetattr(0, &mut changed) }, 0);
            changed.c_lflag &= !libc::ECHO;
            assert_eq!(unsafe { libc::tcsetattr(0, libc::TCSANOW, &changed) }, 0);
            panic!("panic after terminal ownership ended");
        }
        use std::io::Write;
        std::io::stdout().write_all(b"TERMINAL_READY\n").unwrap();
        std::io::stdout().flush().unwrap();
        if mode == "panic" {
            let (send, ready) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _stdout = std::io::stdout().lock();
                send.send(()).unwrap();
                loop {
                    std::thread::park();
                }
            });
            ready.recv().unwrap();
            panic!("terminal panic with stdout locked by another thread");
        }
        let config = config(true);
        for _ in 0..5 {
            assert!(matches!(read_search_key(0, &config.search), Key::Other));
            std::io::stdout().write_all(b"CONTROL_READ\n").unwrap();
            std::io::stdout().flush().unwrap();
        }
        assert!(matches!(read_search_key(0, &config.search), Key::AllStates));
        std::io::stdout().write_all(b"CANCEL_READ\n").unwrap();
        std::io::stdout().flush().unwrap();
        assert!(matches!(read_search_key(0, &config.search), Key::Quit));
        drop(raw);
    }

    struct TerminalChild {
        child: std::process::Child,
        master: std::fs::File,
        slave: std::fs::File,
        original: libc::termios,
        output: Vec<u8>,
    }

    impl TerminalChild {
        fn start(mode: &str) -> Self {
            Self::spawn(terminal_command(mode))
        }

        fn spawn(mut command: std::process::Command) -> Self {
            use std::os::fd::{AsRawFd, FromRawFd};
            let (mut master, mut slave) = (-1, -1);
            assert_eq!(
                unsafe {
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                    )
                },
                0
            );
            let master = unsafe { std::fs::File::from_raw_fd(master) };
            let slave = unsafe { std::fs::File::from_raw_fd(slave) };
            for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
                assert_eq!(
                    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                    0
                );
            }
            let mut original: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut original) },
                0
            );
            original.c_lflag |= libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN;
            original.c_iflag |= libc::IXON | libc::IXOFF;
            assert_eq!(
                unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &original) },
                0
            );
            let child = command
                .stdin(slave.try_clone().unwrap())
                .stdout(slave.try_clone().unwrap())
                .stderr(slave.try_clone().unwrap())
                .spawn()
                .unwrap();
            Self {
                child,
                master,
                slave,
                original,
                output: Vec::new(),
            }
        }

        fn settings(&self) -> libc::termios {
            use std::os::fd::AsRawFd;
            let mut settings = unsafe { std::mem::zeroed() };
            assert_eq!(
                unsafe { libc::tcgetattr(self.slave.as_raw_fd(), &mut settings) },
                0
            );
            settings
        }

        fn read_until(&mut self, marker: &[u8]) {
            use std::io::Read;
            use std::os::fd::AsRawFd;
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.output.windows(marker.len()).any(|part| part == marker) {
                assert!(
                    Instant::now() < deadline,
                    "terminal timed out: {:?}",
                    String::from_utf8_lossy(&self.output)
                );
                if poll_fd(self.master.as_raw_fd(), Some(Duration::from_millis(20))) {
                    let mut buf = [0; 4096];
                    let n = self.master.read(&mut buf).unwrap();
                    assert_ne!(n, 0);
                    self.output.extend_from_slice(&buf[..n]);
                }
            }
        }

        fn finish(&mut self) -> std::process::ExitStatus {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                assert!(Instant::now() < deadline, "terminal child did not exit");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for TerminalChild {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn terminal_command(mode: &str) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "input::tests::terminal_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("AGENMUX_TEST_TERMINAL", mode);
        command
    }

    fn assert_termios_eq(actual: &libc::termios, expected: &libc::termios) {
        assert_eq!(actual.c_iflag, expected.c_iflag, "input flags");
        assert_eq!(actual.c_oflag, expected.c_oflag, "output flags");
        assert_eq!(actual.c_cflag, expected.c_cflag, "control flags");
        // BSD sets PENDIN when restoring canonical input; it is transient queue state.
        assert_eq!(
            actual.c_lflag & !libc::PENDIN,
            expected.c_lflag & !libc::PENDIN,
            "local flags"
        );
        assert_eq!(actual.c_cc, expected.c_cc, "control characters");
        assert_eq!(unsafe { libc::cfgetispeed(actual) }, unsafe {
            libc::cfgetispeed(expected)
        });
        assert_eq!(unsafe { libc::cfgetospeed(actual) }, unsafe {
            libc::cfgetospeed(expected)
        });
    }

    #[test]
    fn terminal_controls_and_normal_drop() {
        use std::io::Write;
        let mut child = TerminalChild::start("normal");
        child.read_until(b"TERMINAL_READY");
        let active = child.settings();
        assert_eq!(
            active.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN),
            0
        );
        assert_eq!(active.c_iflag & (libc::IXON | libc::IXOFF), 0);
        for byte in [0x1a, 0x1c, 0x13, 0x11, 0x0f] {
            child.output.clear();
            child.master.write_all(&[byte]).unwrap();
            child.read_until(b"CONTROL_READ");
        }
        child.master.write_all(&[0x03]).unwrap();
        child.read_until(b"CANCEL_READ");
        child.master.write_all(&[0x04]).unwrap();
        child.read_until(b"\x1b[?2004l\x1b[?25h");
        assert!(child.finish().success());
        assert_termios_eq(&child.settings(), &child.original);
    }

    #[test]
    fn terminal_panic_restores_before_abort() {
        use std::os::unix::process::ExitStatusExt;
        let mut child = TerminalChild::start("panic");
        child.read_until(b"previous panic hook");
        assert_eq!(child.finish().signal(), Some(libc::SIGABRT));
        assert_termios_eq(&child.settings(), &child.original);
        let output = String::from_utf8_lossy(&child.output);
        assert!(output.contains("\x1b[?2004l\x1b[?25h"), "{output:?}");
    }

    #[test]
    fn terminal_hook_does_not_restore_after_drop() {
        use std::os::unix::process::ExitStatusExt;
        let mut child = TerminalChild::start("after-drop");
        child.read_until(b"previous panic hook");
        assert_eq!(child.finish().signal(), Some(libc::SIGABRT));
        let mut expected = child.original;
        expected.c_lflag &= !libc::ECHO;
        assert_termios_eq(&child.settings(), &expected);
    }

    #[test]
    fn diagnostics_skip_the_owned_terminal() {
        let state = std::env::temp_dir().join(format!("agenmux-tui-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let mut command = terminal_command("diagnostic");
        command.env("XDG_STATE_HOME", &state);
        let mut child = TerminalChild::spawn(command);
        child.read_until(b"agenmux: after the frame");
        assert!(child.finish().success());
        let output = String::from_utf8_lossy(&child.output);
        assert!(!output.contains("during the frame"), "{output:?}");
        let log = std::fs::read_to_string(state.join("agenmux").join(crate::diag::DAEMON_LOG));
        let _ = std::fs::remove_dir_all(&state);
        assert_eq!(log.unwrap(), "agenmux: during the frame\n");
    }

    #[test]
    fn terminal_non_tty_is_inactive() {
        let output = terminal_command("non-tty")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!output.stdout.contains(&0x1b));
    }

    fn config(management_enabled: bool) -> crate::app_config::AppConfig {
        let source = if management_enabled {
            ""
        } else {
            "[tmux_management]\nenabled = false"
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
        // Tree keys: Space toggles, h/Left collapse, Right expands, z/Z all.
        for (bytes, expected) in [
            (b" ".as_slice(), "toggle"),
            (b"h", "left"),
            (b"\x1b[D", "left"),
            (b"\x1b[C", "right"),
            (b"z", "collapse-all"),
            (b"Z", "expand-all"),
            (b"R", "restore-layout"),
            (b"u", "undo"),
            (b"x", "dismiss-layout"),
            (b"v", "mark"),
            (b"V", "mark-range"),
            (&[0x0b], "other"),
        ] {
            feed(bytes);
            let key = read_key(fds[0], &keys);
            assert!(
                matches!(
                    (&key, expected),
                    (Key::ToggleBranch, "toggle")
                        | (Key::Left, "left")
                        | (Key::Right, "right")
                        | (Key::CollapseAll, "collapse-all")
                        | (Key::ExpandAll, "expand-all")
                        | (Key::RestoreLayout, "restore-layout")
                        | (Key::Undo, "undo")
                        | (Key::DismissLayout, "dismiss-layout")
                        | (Key::Mark, "mark")
                        | (Key::MarkRange, "mark-range")
                        | (Key::Other, "other")
                ),
                "{bytes:?}: {key:?}"
            );
        }
        feed(&[0x01]);
        assert!(matches!(read_key(fds[0], &keys), Key::WheelUp));
        feed(&[0x02]);
        assert!(matches!(read_key(fds[0], &keys), Key::WheelDown));
        feed(b"U");
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
        feed(b"o");
        assert!(matches!(
            read_key_with_config(fds[0], &enabled.normal, &enabled),
            Key::Sequence('o', None)
        ));
        feed(b"o");
        assert!(matches!(
            read_key_with_config(fds[0], &disabled.normal, &disabled),
            Key::Other
        ));
        // Beside a right-hand sidebar, l folds and h jumps.
        let right = crate::app_config::resolve(
            &crate::app_config::parse("[display]\nsidebar_position = 'right'").unwrap(),
            &Default::default(),
        )
        .unwrap();
        feed(b"l");
        assert!(matches!(
            read_key_with_config(fds[0], &right.normal, &right),
            Key::Left
        ));
        feed(b"h");
        assert!(matches!(
            read_key_with_config(fds[0], &right.normal, &right),
            Key::Jump
        ));
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    fn with_input(bytes: &[u8], check: impl FnOnce(libc::c_int)) {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let bytes = bytes.to_vec();
        let sender = std::thread::spawn(move || writer.write_all(&bytes).unwrap());
        check(reader.as_raw_fd());
        drop(reader);
        sender.join().unwrap();
    }

    #[test]
    fn normal_paste_does_not_dispatch_commands() {
        let config = config(true);
        for body in [
            b"xuR\rdd\x03\x04".as_slice(),
            b"",
            b"\x1b[20x\x1b[201x\x1b[A",
        ] {
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend_from_slice(body);
            bytes.extend_from_slice(b"\x1b[201~j");
            with_input(&bytes, |fd| {
                assert!(matches!(
                    read_key_with_config(fd, &config.normal, &config),
                    Key::Other
                ));
                assert!(matches!(read_key(fd, &config.normal), Key::Down));
                assert!(matches!(read_key(fd, &config.normal), Key::Quit));
            });
        }
    }

    #[test]
    fn unknown_escape_sequences_do_not_leak_into_either_input_mode() {
        let normal = default_keys(KeyMode::Normal);
        let search = default_keys(KeyMode::Search);
        for sequence in [
            b"\x1b[1;5A".as_slice(),
            b"\x1b[1;2B",
            b"\x1b[1;3C",
            b"\x1b[15;2~",
            b"\x1b[200;1~",
            b"\x1b[1 q",
            b"\x1b[?25h",
            b"\x1b[Z",
        ] {
            let mut bytes = sequence.to_vec();
            bytes.push(b'j');
            with_input(&bytes, |fd| {
                assert!(matches!(read_key(fd, &normal), Key::Other), "{sequence:?}");
                assert!(matches!(read_key(fd, &normal), Key::Down), "{sequence:?}");
                assert!(matches!(read_key(fd, &normal), Key::Quit), "{sequence:?}");
            });
            with_input(&bytes, |fd| {
                assert!(
                    matches!(read_search_key(fd, &search), Key::Other),
                    "{sequence:?}"
                );
                assert!(
                    matches!(read_search_key(fd, &search), Key::Text(text) if text == "j"),
                    "{sequence:?}"
                );
                assert!(
                    matches!(read_search_key(fd, &search), Key::Quit),
                    "{sequence:?}"
                );
            });
        }
    }

    #[test]
    fn interrupted_escape_restarts_without_leaking_bytes() {
        let normal = default_keys(KeyMode::Normal);
        let search = default_keys(KeyMode::Search);
        for bytes in [
            b"\x1b[1;\x1b[Aj".as_slice(),
            b"\x1bO1;\x1bOAj",
            b"\x1b[1;\x1b\x1b[Aj",
            b"\x1b\x1b[Aj",
        ] {
            with_input(bytes, |fd| {
                assert!(matches!(read_key(fd, &normal), Key::Up));
                assert!(matches!(read_key(fd, &normal), Key::Down));
                assert!(matches!(read_key(fd, &normal), Key::Quit));
            });
            with_input(bytes, |fd| {
                assert!(matches!(read_search_key(fd, &search), Key::Up));
                assert!(matches!(read_search_key(fd, &search), Key::Text(text) if text == "j"));
                assert!(matches!(read_search_key(fd, &search), Key::Quit));
            });
        }
        for bytes in [b"\x1b[1;\x1b[Rj".as_slice(), b"\x1b[1;\x1b[1;5Aj"] {
            with_input(bytes, |fd| {
                assert!(matches!(read_key(fd, &normal), Key::Other));
                assert!(matches!(read_key(fd, &normal), Key::Down));
                assert!(matches!(read_key(fd, &normal), Key::Quit));
            });
            with_input(bytes, |fd| {
                assert!(matches!(read_search_key(fd, &search), Key::Other));
                assert!(matches!(read_search_key(fd, &search), Key::Text(text) if text == "j"));
                assert!(matches!(read_search_key(fd, &search), Key::Quit));
            });
        }
        let paste = b"\x1b[1;\x1b[200~xuR\rdd\x18\x1a\x1b[201~j";
        with_input(paste, |fd| {
            assert!(matches!(read_key(fd, &normal), Key::Other));
            assert!(matches!(read_key(fd, &normal), Key::Down));
            assert!(matches!(read_key(fd, &normal), Key::Quit));
        });
        with_input(paste, |fd| {
            assert!(
                matches!(read_search_key(fd, &search), Key::Text(text) if text == "xuR\rdd\x18\x1a")
            );
            assert!(matches!(read_search_key(fd, &search), Key::Text(text) if text == "j"));
            assert!(matches!(read_search_key(fd, &search), Key::Quit));
        });
    }

    #[test]
    fn cancelled_escape_preserves_the_next_key() {
        let normal = default_keys(KeyMode::Normal);
        let search = default_keys(KeyMode::Search);
        for bytes in [
            b"\x1b[1;\x18j".as_slice(),
            b"\x1b[1;\x1aj",
            b"\x1bO1;\x18j",
            b"\x1bO1;\x1aj",
        ] {
            with_input(bytes, |fd| {
                assert!(matches!(read_key(fd, &normal), Key::Other));
                assert!(matches!(read_key(fd, &normal), Key::Down));
                assert!(matches!(read_key(fd, &normal), Key::Quit));
            });
            with_input(bytes, |fd| {
                assert!(matches!(read_search_key(fd, &search), Key::Other));
                assert!(matches!(read_search_key(fd, &search), Key::Text(text) if text == "j"));
                assert!(matches!(read_search_key(fd, &search), Key::Quit));
            });
        }
    }

    #[test]
    fn oversized_paste_is_drained_in_both_modes() {
        let normal = default_keys(KeyMode::Normal);
        let search = default_keys(KeyMode::Search);
        for len in [65530, 65531, 65535, 65536, 65537, 70000] {
            let mut bytes = b"\x1b[200~".to_vec();
            bytes.extend(std::iter::repeat_n(b'x', len));
            bytes.extend_from_slice(b"\x1b[201~j");
            with_input(&bytes, |fd| {
                assert!(matches!(read_key(fd, &normal), Key::Other));
                assert!(matches!(read_key(fd, &normal), Key::Down));
                assert!(matches!(read_key(fd, &normal), Key::Quit));
            });
            with_input(&bytes, |fd| {
                assert!(
                    matches!(
                        read_search_key(fd, &search),
                        Key::Text(text) if text == "x".repeat(len.min(65536))
                    ),
                    "length: {len}"
                );
                assert!(matches!(read_search_key(fd, &search), Key::Text(text) if text == "j"));
                assert!(matches!(read_search_key(fd, &search), Key::Quit));
            });
        }
    }

    #[test]
    fn escape_parsing_stays_inside_logical_key_packets() {
        let normal = default_keys(KeyMode::Normal);
        for payload in [
            b"\x1b[200~".as_slice(),
            b"\x1b[1;5A",
            b"\x1b[1;",
            b"\x1b[1;\x1b[R",
            b"\x1b[1;\x18",
            b"\x1b[1;\x1a",
        ] {
            let mut bytes = vec![0x08, payload.len() as u8, 0, 7];
            bytes.extend_from_slice(payload);
            bytes.extend_from_slice(b"client1\x08\x01\x00\x07jclient1");
            with_input(&bytes, |fd| {
                assert!(matches!(
                    read_key(fd, &normal),
                    Key::Owned(key, client) if matches!(*key, Key::Other) && client == "client1"
                ));
                assert!(matches!(
                    read_key(fd, &normal),
                    Key::Owned(key, client) if matches!(*key, Key::Down) && client == "client1"
                ));
                assert!(matches!(read_key(fd, &normal), Key::Quit));
            });
        }
    }

    #[test]
    fn incomplete_input_ends_without_an_action() {
        let normal = default_keys(KeyMode::Normal);
        let search = default_keys(KeyMode::Search);
        for bytes in [b"\x1b[".as_slice(), b"\x1b[1;", b"\x1b[200~xuR\rdd"] {
            with_input(bytes, |fd| {
                assert!(matches!(read_key(fd, &normal), Key::Other));
                assert!(matches!(read_key(fd, &normal), Key::Quit));
            });
            with_input(bytes, |fd| {
                assert!(matches!(read_search_key(fd, &search), Key::Other));
                assert!(matches!(read_search_key(fd, &search), Key::Quit));
            });
        }
    }

    #[test]
    fn incomplete_input_times_out_and_next_key_still_works() {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        for text_mode in [false, true] {
            let keys = default_keys(if text_mode {
                KeyMode::Search
            } else {
                KeyMode::Normal
            });
            for bytes in [b"\x1b[".as_slice(), b"\x1b[1;", b"\x1b[200~xuR\rdd"] {
                let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
                let read = || {
                    if text_mode {
                        read_search_key(reader.as_raw_fd(), &keys)
                    } else {
                        read_key(reader.as_raw_fd(), &keys)
                    }
                };
                writer.write_all(bytes).unwrap();
                assert!(matches!(read(), Key::Other));
                writer.write_all(b"j").unwrap();
                if text_mode {
                    assert!(matches!(read(), Key::Text(text) if text == "j"));
                } else {
                    assert!(matches!(read(), Key::Down));
                }
            }
        }
    }

    #[test]
    fn long_unknown_csi_is_drained_without_growing_the_capture() {
        let normal = default_keys(KeyMode::Normal);
        let mut bytes = b"\x1b[".to_vec();
        bytes.extend(std::iter::repeat_n(b'1', 10000));
        bytes.extend_from_slice(b"Aj");
        with_input(&bytes, |fd| {
            assert!(matches!(read_key(fd, &normal), Key::Other));
            assert!(matches!(read_key(fd, &normal), Key::Down));
            assert!(matches!(read_key(fd, &normal), Key::Quit));
        });
    }

    #[test]
    fn chunked_escape_and_paste_wait_for_in_flight_bytes() {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        for text_mode in [false, true] {
            let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
            let sender = std::thread::spawn(move || {
                for byte in b"\x1b[1;5A\x1b[200~" {
                    writer.write_all(&[*byte]).unwrap();
                    std::thread::sleep(Duration::from_millis(2));
                }
                std::thread::sleep(Duration::from_millis(150));
                for byte in "xuR\rddé\x1b[201~j".as_bytes() {
                    writer.write_all(&[*byte]).unwrap();
                    std::thread::sleep(Duration::from_millis(2));
                }
            });
            let fd = reader.as_raw_fd();
            let keys = default_keys(if text_mode {
                KeyMode::Search
            } else {
                KeyMode::Normal
            });
            let read = || {
                if text_mode {
                    read_search_key(fd, &keys)
                } else {
                    read_key(fd, &keys)
                }
            };
            assert!(matches!(read(), Key::Other));
            let paste = read();
            if text_mode {
                assert!(matches!(paste, Key::Text(text) if text == "xuR\rddé"));
                assert!(matches!(read(), Key::Text(text) if text == "j"));
            } else {
                assert!(matches!(paste, Key::Other));
                assert!(matches!(read(), Key::Down));
            }
            assert!(matches!(read(), Key::Quit));
            sender.join().unwrap();
        }
    }

    #[test]
    fn popup_text_keys_and_bracketed_paste_are_atomic() {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let keys = default_keys(crate::app_config::KeyMode::Search);
        let feed = |bytes: &[u8]| {
            assert_eq!(
                unsafe { libc::write(fds[1], bytes.as_ptr().cast(), bytes.len()) },
                bytes.len() as isize
            )
        };
        for (bytes, expected) in [
            (b"\x1b[D".as_slice(), "left"),
            (b"\x1bOC", "right"),
            (b"\x1b[H", "home"),
            (b"\x1b[4~", "end"),
            (b"\x1b[3~", "delete"),
        ] {
            feed(bytes);
            let key = read_search_key(fds[0], &keys);
            assert!(matches!(
                (key, expected),
                (Key::Left, "left")
                    | (Key::Right, "right")
                    | (Key::Home, "home")
                    | (Key::End, "end")
                    | (Key::Delete, "delete")
            ));
        }
        feed(b"\x1b[200~hello\n\x1b[A\xc3\xa9\x1b[201~x");
        assert!(
            matches!(read_search_key(fds[0], &keys), Key::Text(text) if text == "hello\n\x1b[Aé")
        );
        assert!(matches!(read_search_key(fds[0], &keys), Key::Text(text) if text == "x"));
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
    fn copy_sequence_works_without_tmux_management() {
        let start = Instant::now();
        let timeout = Duration::from_secs(1);
        let mut sequence = KeySequence::default();
        let disabled = config(false);

        assert!(matches!(
            sequence.push('y', Some("client-a".into()), start, timeout, &disabled),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push('y', Some("client-a".into()), start, timeout, &disabled),
            SequenceResult::Match(
                SequenceDispatch::Builtin(SequenceAction::Copy),
                Some(client)
            ) if client == "client-a"
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
                "[tmux_management]\nenabled=false\n[quick_launchers.terminal]\nsequence='zt'\nlabel='terminal'\ncommand='fish'",
            )
            .unwrap(),
            &Default::default(),
        )
        .unwrap();
        let start = Instant::now();
        let timeout = Duration::from_secs(1);
        let mut sequence = KeySequence::default();

        assert!(matches!(
            sequence.push('o', None, start, timeout, &enabled),
            SequenceResult::Pending
        ));
        assert_eq!(
            sequence.continuations(&enabled),
            vec![(('g'), "lazygit".into()), (('e'), "nvim".into())]
        );
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
            sequence.push('o', None, start, timeout, &disabled),
            SequenceResult::Miss
        ));
        assert!(matches!(
            sequence.push('z', None, start, timeout, &disabled),
            SequenceResult::Miss
        ));
    }

    /// Feed escape_input a scripted tail. Every byte goes through the same
    /// reader, which is the point: mirror mode delivers keys over a
    /// non-blocking FIFO one byte at a time, so a tail byte that has not
    /// arrived yet must be waited for, not read blind. Reading the pair
    /// without polling hit EAGAIN and dropped every other arrow.
    fn decode(tail: &[Option<u8>]) -> Option<KeyChord> {
        let mut it = tail.iter().copied();
        chord(0x1b, move || it.next().flatten())
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
    fn split_text_packets_preserve_unicode_across_fifo_chunks() {
        let runtime =
            std::env::temp_dir().join(format!("agenmux-paste-test-{}", std::process::id()));
        std::fs::create_dir_all(&runtime).unwrap();
        let path = runtime.join("agenmux-keys");
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
        assert!(fd >= 0);
        let text = "é".repeat(245);
        assert_eq!(send_text_to(&runtime, &text), 0);
        let keys = default_keys(crate::app_config::KeyMode::Search);
        let mut received = String::new();
        for _ in 0..2 {
            match read_key(fd, &keys) {
                Key::Text(chunk) => received.push_str(&chunk),
                key => panic!("unexpected paste packet: {key:?}"),
            }
        }
        assert_eq!(received, text);
        unsafe { libc::close(fd) };
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(runtime).unwrap();
    }

    #[test]
    fn click_packets_decode_single_and_double_clicks() {
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) };
        let packets = [0x05u8, 0, 0, 1, 2, 0x0f, 0, 0, 0, 7];
        unsafe { libc::write(fds[1], packets.as_ptr().cast(), packets.len()) };
        let keys = default_keys(crate::app_config::KeyMode::Normal);
        assert!(matches!(read_key(fds[0], &keys), Key::Select(258)));
        assert!(matches!(read_key(fds[0], &keys), Key::Open(7)));
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

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
