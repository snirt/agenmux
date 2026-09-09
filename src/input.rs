use crate::app_config::{action_for, Action, KeyChord, KeyMode, Keymap};
use crate::tmux;
use std::time::{Duration, Instant};

const KEY_SEQUENCE_TIMEOUT: Duration = Duration::from_secs(1);

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

#[derive(Clone)]
pub(crate) enum Key {
    First,
    Last,
    Sequence(char),
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
    Search,
    Backspace,
    ClearSearch,
    CycleState,
    AllStates,
    Text(String),
    Other,
}

#[derive(Default)]
pub(crate) struct KeySequence {
    pending: String,
    last: Option<Instant>,
}

pub(crate) enum SequenceResult {
    Pending,
    Match(Key),
    Miss,
}

impl KeySequence {
    pub(crate) fn push(
        &mut self,
        key: char,
        now: Instant,
        bindings: &[(&str, Key)],
    ) -> SequenceResult {
        if self
            .last
            .is_some_and(|last| now.duration_since(last) > KEY_SEQUENCE_TIMEOUT)
        {
            self.clear();
        }
        self.pending.push(key);
        self.last = Some(now);
        if let Some((_, action)) = bindings
            .iter()
            .find(|(sequence, _)| *sequence == self.pending)
        {
            let action = action.clone();
            self.clear();
            SequenceResult::Match(action)
        } else if bindings
            .iter()
            .any(|(sequence, _)| sequence.starts_with(&self.pending))
        {
            SequenceResult::Pending
        } else {
            self.clear();
            SequenceResult::Miss
        }
    }

    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.last = None;
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
        Action::Filter => Key::CycleState,
        Action::Reset | Action::Cancel => Key::AllStates,
        Action::Help => Key::Help,
        Action::Versions => Key::Versions,
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

pub(crate) fn read_key(fd: libc::c_int, keys: &Keymap) -> Key {
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
        0x01 => return Key::WheelUp,
        0x02 => return Key::WheelDown,
        0x0c => return Key::AllStates, // private clear packet used by tmux/click helpers
        // Search-table printable keys use a NUL-prefixed packet so normal-mode
        // actions such as `j`, `q`, and `f` remain query text while typing.
        0x00 => {
            return next()
                .filter(|b| (0x20..=0x7e).contains(b))
                .map(|b| Key::Text(char::from(b).to_string()))
                .unwrap_or(Key::Other)
        }
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
        // Multi-key sequence such as `gg`, delivered as one packet.
        0x06 => {
            return next()
                .filter(|b| (0x20..=0x7e).contains(b))
                .map(|b| Key::Sequence(char::from(b)))
                .unwrap_or(Key::Other)
        }
        _ => {}
    }
    // Configured chords win; the fixed edge keys are the default underneath.
    chord(b, next)
        .and_then(|chord| action_key(keys, chord))
        .unwrap_or(match b {
            b'G' => Key::Last,
            b'g' => Key::Sequence('g'),
            0x03 | 0x04 => Key::Quit, // Ctrl-C, Ctrl-D: emergency exit
            _ => Key::Other,
        })
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
pub fn send_key(name: &str) -> i32 {
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
        vec![0x06, byte]
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
            _ => return 2,
        }
    };
    send_bytes(&bytes)
}

pub fn select(index: usize) -> i32 {
    let Ok(index) = u32::try_from(index) else {
        return 2;
    };
    let mut bytes = vec![0x05];
    bytes.extend(index.to_be_bytes());
    send_bytes(&bytes)
}

fn send_bytes(bytes: &[u8]) -> i32 {
    let path = crate::tmux::runtime_dir().join("agenmux-keys");
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
    let clients = match tmux::lines(&["list-clients", "-F", "#{client_name}"]) {
        Ok(clients) => clients,
        Err(_) => return 0,
    };
    if !clients.iter().any(|name| name == client) {
        return 0;
    }
    let panes = match tmux::lines(&["list-panes", "-a", "-F", "#{pane_id}"]) {
        Ok(panes) => panes,
        Err(_) => return 0,
    };
    if !panes.iter().any(|id| id == pane) {
        return 0;
    }

    let target = y
        .checked_sub(1)
        .and_then(|line| {
            let rows = std::fs::read_to_string(tmux::runtime_dir().join("agenmux-rows")).ok()?;
            let mut fields = rows.lines().nth(line)?.split_whitespace();
            let target = fields.next()?.to_string();
            let index = fields.next()?.parse::<usize>().ok()?;
            let selected = fields.next() == Some("1");
            Some((target, index, selected))
        })
        .filter(|(target, _, _)| target.starts_with('%') && panes.iter().any(|id| id == target));

    if let Some((target, index, selected)) = target {
        if selected {
            let _ = send_key("all");
            let _ = tmux::command_status(&["switch-client", "-c", client, "-t", &target]);
            let _ = tmux::command_status(&["select-window", "-t", &target]);
            let _ = tmux::command_status(&["select-pane", "-t", &target]);
        } else if tmux::command_status(&["switch-client", "-c", client, "-t", pane]).is_ok() {
            let _ = tmux::command_status(&["switch-client", "-c", client, "-T", "agenmux"]);
            let _ = select(index);
        }
    } else if tmux::command_status(&["switch-client", "-c", client, "-t", pane]).is_ok() {
        let _ = tmux::command_status(&["switch-client", "-c", client, "-T", "agenmux"]);
    }
    0
}

pub fn wheel(pane: &str, direction: Direction) -> i32 {
    let panes = match tmux::lines(&["list-panes", "-a", "-F", "#{pane_id}"]) {
        Ok(panes) => panes,
        Err(_) => return 0,
    };
    if !panes.iter().any(|id| id == pane) {
        return 0;
    }
    send_key(match direction {
        Direction::Up => "wheel-up",
        Direction::Down => "wheel-down",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(matches!(read_key(fds[0], &keys), Key::Sequence('g')));
        feed(&[0x06, b'g']);
        assert!(matches!(read_key(fds[0], &keys), Key::Sequence('g')));
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
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn timed_key_sequences_support_arbitrary_bindings_and_expiry() {
        let bindings = [("gg", Key::First), ("gd", Key::Last)];
        let start = Instant::now();
        let mut sequence = KeySequence::default();

        assert!(matches!(
            sequence.push('g', start, &bindings),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push('d', start + Duration::from_millis(10), &bindings),
            SequenceResult::Match(Key::Last)
        ));
        assert!(matches!(
            sequence.push('g', start + Duration::from_millis(20), &bindings),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push('x', start + Duration::from_millis(30), &bindings),
            SequenceResult::Miss
        ));
        assert!(matches!(
            sequence.push('g', start + Duration::from_millis(40), &bindings),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push(
                'g',
                start + KEY_SEQUENCE_TIMEOUT + Duration::from_millis(41),
                &bindings
            ),
            SequenceResult::Pending
        ));
        assert!(matches!(
            sequence.push(
                'g',
                start + KEY_SEQUENCE_TIMEOUT + Duration::from_millis(50),
                &bindings
            ),
            SequenceResult::Match(Key::First)
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
