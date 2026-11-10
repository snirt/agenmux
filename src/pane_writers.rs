//! Frame delivery and terminal input for split sidebar panes.
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A live reader drains a frame in microseconds. One that stopped draining
/// gets dropped after this; the next reconcile reopens it and replays a full
/// frame, which repaints over any torn one.
const WRITE_BUDGET: Duration = Duration::from_millis(100);

pub(crate) fn frame_path(runtime: &Path, pane: &str) -> PathBuf {
    runtime.join(format!("agenmux-frame-{pane}"))
}

struct PaneWriter(File);

impl PaneWriter {
    fn open(runtime: &Path, pane: &str) -> std::io::Result<Self> {
        // Nonblocking: neither a reader still starting nor one that stopped
        // draining may stall the daemon.
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(frame_path(runtime, pane))?;
        if !file.metadata()?.file_type().is_fifo() {
            return Err(std::io::Error::other("sidebar frame target is not a FIFO"));
        }
        Ok(Self(file))
    }

    fn emit(&mut self, frame: &[u8], deadline: Instant) -> std::io::Result<()> {
        let mut rest = frame;
        while !rest.is_empty() {
            // Chunks fit the 8 KiB FIFO buffer macOS starts with.
            match self.0.write(&rest[..rest.len().min(4096)]) {
                Ok(n) => rest = &rest[n..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        && crate::input::poll_by(self.0.as_raw_fd(), libc::POLLOUT, deadline) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

pub struct PaneWriters {
    writers: HashMap<String, PaneWriter>,
    runtime: PathBuf,
}

impl PaneWriters {
    pub fn new() -> PaneWriters {
        PaneWriters {
            writers: HashMap::new(),
            runtime: crate::mux::runtime_dir(),
        }
    }

    /// Match writers to visible panes; new targets need the last frame replayed.
    pub fn reconcile(&mut self, panes: impl IntoIterator<Item = String>) -> bool {
        let wanted: HashSet<String> = panes.into_iter().collect();
        let before = self.writers.len();
        self.writers.retain(|pane, _| wanted.contains(pane));
        let mut changed = self.writers.len() != before;
        for pane in &wanted {
            if !self.writers.contains_key(pane) {
                if let Ok(writer) = PaneWriter::open(&self.runtime, pane) {
                    self.writers.insert(pane.clone(), writer);
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn emit(&mut self, frame: &str) {
        let deadline = Instant::now() + WRITE_BUDGET;
        self.writers
            .retain(|_, writer| writer.emit(frame.as_bytes(), deadline).is_ok());
    }

    pub fn clear(&mut self) {
        self.writers.clear();
    }
}

static PANE_STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop_pane(_: libc::c_int) {
    PANE_STOP.store(true, Ordering::Relaxed);
}

/// Terminal input for a daemon whose server routes no keys (Rex): typed
/// bytes pass through for the daemon to decode with the user's keymap, mouse
/// reports become the click and wheel packets tmux bindings send.
#[derive(Default)]
struct TtyInput {
    /// An escape sequence split across reads.
    partial: Vec<u8>,
    /// Row and time of the last press, for double clicks.
    last_press: Option<(usize, Instant)>,
}

const DOUBLE_CLICK: Duration = Duration::from_millis(300);
/// Mouse reports on, in SGR encoding; the frame takes the alternate screen.
/// Frames hide the cursor: Rex's cursor shader leaves an outline wherever a
/// cursor was last shown, which no escape can clear or recolor.
const TTY_ON: &str = "\x1b[?1049h\x1b[?1000h\x1b[?1006h";
const TTY_OFF: &str = "\x1b[?25h\x1b[?1006l\x1b[?1000l\x1b[?1049l";

impl TtyInput {
    /// Packets for the key FIFO. `row_index`: the list index a frame row
    /// shows, from the daemon's row map.
    fn translate(
        &mut self,
        bytes: &[u8],
        now: Instant,
        row_index: impl Fn(usize) -> Option<u32>,
    ) -> Vec<Vec<u8>> {
        let mut input = std::mem::take(&mut self.partial);
        input.extend_from_slice(bytes);
        let mut packets = Vec::new();
        let mut typed = Vec::new();
        let mut at = 0;
        while at < input.len() {
            if input[at..].starts_with(b"\x1b[<") {
                let Some(end) = input[at..].iter().position(|b| matches!(b, b'M' | b'm')) else {
                    if input.len() - at < 32 {
                        self.partial = input[at..].to_vec();
                    }
                    break;
                };
                let report = &input[at + 3..at + end];
                let press = input[at + end] == b'M';
                at += end + 1;
                let mut fields = std::str::from_utf8(report)
                    .unwrap_or_default()
                    .split(';')
                    .map(|f| f.parse::<usize>().ok());
                let (Some(Some(button)), Some(Some(_x)), Some(Some(y))) =
                    (fields.next(), fields.next(), fields.next())
                else {
                    continue;
                };
                if !typed.is_empty() {
                    packets.push(std::mem::take(&mut typed));
                }
                match (button, press) {
                    (64, true) => packets.push(vec![0x01]),
                    (65, true) => packets.push(vec![0x02]),
                    (0, true) => {
                        let row = y.saturating_sub(1);
                        let Some(index) = row_index(row) else {
                            continue;
                        };
                        let double = self
                            .last_press
                            .is_some_and(|(last, at)| last == row && now - at < DOUBLE_CLICK);
                        self.last_press = (!double).then_some((row, now));
                        let mut packet = vec![if double { 0x0f } else { 0x05 }];
                        packet.extend_from_slice(&index.to_be_bytes());
                        packets.push(packet);
                    }
                    _ => {}
                }
                continue;
            }
            let byte = input[at];
            at += 1;
            // Control bytes the key protocol reads as packets (wheel, click,
            // text) never pass as typed input.
            if byte >= 0x20 || matches!(byte, 0x03 | 0x0a | 0x0d | 0x1b) {
                typed.push(byte);
            }
        }
        if !typed.is_empty() {
            packets.push(typed);
        }
        packets
    }
}

/// The list index on `row` of the daemon's row map.
fn row_index(runtime: &Path, row: usize) -> Option<u32> {
    let rows = std::fs::read_to_string(runtime.join("agenmux-rows")).ok()?;
    rows.lines()
        .nth(row)?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

/// One idle reader per split pane: tmux sends terminal paste to its PTY, not
/// to key bindings. Frames arrive on a separate FIFO from the shared daemon.
/// Where the server routes no keys, the reader forwards its terminal input.
pub(crate) fn run_pane() -> i32 {
    let Some(pane) = crate::mux::own_pane() else {
        return 1;
    };
    let runtime = crate::mux::runtime_dir();
    let path = frame_path(&runtime, &pane);
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return 1;
    };
    let _ = std::fs::remove_file(&path); // stale FIFO from a killed pane
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
        return 1;
    }
    // O_RDWR keeps poll asleep across daemon restarts (no POLLHUP spin).
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
    if fd < 0 {
        let _ = std::fs::remove_file(&path);
        return 1;
    }
    let mut frames = unsafe { File::from_raw_fd(fd) };
    crate::mux::revive_daemon();
    unsafe {
        libc::signal(libc::SIGHUP, stop_pane as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, stop_pane as *const () as libc::sighandler_t);
    }
    let _raw = crate::input::RawMode::enable();
    let keys = crate::input::protocol_keys(crate::app_config::KeyMode::Search);
    let mut stdout = std::io::stdout().lock();
    let mut tty = (!crate::mux::routes_keys()).then(TtyInput::default);
    if tty.is_some() {
        let _ = stdout
            .write_all(TTY_ON.as_bytes())
            .and_then(|_| stdout.flush());
    }
    while !PANE_STOP.load(Ordering::Relaxed) {
        let mut fds = [
            libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            break;
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 8192];
            match frames.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if stdout
                        .write_all(&buf[..n])
                        .and_then(|_| stdout.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        if let Some(tty) = tty.as_mut().filter(|_| fds[0].revents & libc::POLLIN != 0) {
            let mut buf = [0u8; 4096];
            match std::io::stdin().lock().read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let packets =
                        tty.translate(&buf[..n], Instant::now(), |row| row_index(&runtime, row));
                    for packet in packets {
                        // PIPE_BUF bounds an atomic FIFO write
                        for chunk in packet.chunks(512) {
                            let _ = crate::input::send_bytes_to(&runtime, chunk);
                        }
                    }
                }
            }
        } else if fds[0].revents & libc::POLLIN != 0 {
            // Attached tmux clients strip paste markers and forward raw text;
            // tmux paste-buffer -p instead sends the bracketed sequence intact.
            match crate::input::read_search_key(0, keys) {
                crate::input::Key::Text(text) => {
                    let _ = crate::input::send_text_to(&runtime, &text);
                }
                crate::input::Key::Quit => break,
                _ => {} // tmux key tables handle commands and cursor movement
            }
        }
    }
    if tty.is_some() {
        let _ = stdout
            .write_all(TTY_OFF.as_bytes())
            .and_then(|_| stdout.flush());
    }
    drop(stdout);
    drop(frames);
    let _ = std::fs::remove_file(path);
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn tty_input_turns_mouse_reports_into_packets_and_drops_packet_bytes() {
        let mut tty = super::TtyInput::default();
        let t0 = std::time::Instant::now();
        let rows = |row: usize| (row == 4).then_some(7u32);
        assert_eq!(
            tty.translate(b"j\x01\nk\x1b[A\x1b[<64;3;2M\x1b[<65;3;2M", t0, rows),
            [b"j\nk\x1b[A".to_vec(), vec![0x01], vec![0x02]]
        );
        // press on row 5 (index 7), its release, then a quick second press
        let click = tty.translate(b"\x1b[<0;2;5M\x1b[<0;2;5m", t0, rows);
        assert_eq!(click, [vec![0x05, 0, 0, 0, 7]]);
        let later = t0 + std::time::Duration::from_millis(100);
        assert_eq!(
            tty.translate(b"\x1b[<0;2;5M", later, rows),
            [vec![0x0f, 0, 0, 0, 7]]
        );
        // a row the map does not list is no click
        assert!(tty.translate(b"\x1b[<0;2;9M", later, rows).is_empty());
        // a report split across reads completes on the next one
        assert!(tty.translate(b"\x1b[<64;1", later, rows).is_empty());
        assert_eq!(tty.translate(b";1M", later, rows), [vec![0x01]]);
    }

    use super::*;

    #[test]
    fn large_frame_reaches_polling_fifo_reader() {
        let runtime = std::env::temp_dir().join(format!(
            "agenmux-large-frame-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&runtime).unwrap();
        let path = frame_path(&runtime, "%1");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let fd = unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
        assert!(fd >= 0);
        let mut reader = unsafe { File::from_raw_fd(fd) };

        let frame = vec![b'x'; 20_000];
        let sent = frame.clone();
        let writer = std::thread::spawn(move || {
            PaneWriter::open(&runtime, "%1")
                .unwrap()
                .emit(&sent, Instant::now() + Duration::from_secs(2))
                .unwrap();
            std::fs::remove_file(path).unwrap();
            std::fs::remove_dir(runtime).unwrap();
        });
        let mut received = Vec::new();
        while received.len() < frame.len() {
            let mut event = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            assert_eq!(
                unsafe { libc::poll(&mut event, 1, 2_000) },
                1,
                "FIFO stalled"
            );
            let mut buf = [0; 8192];
            let n = reader.read(&mut buf).unwrap();
            received.extend_from_slice(&buf[..n]);
        }
        writer.join().unwrap();
        assert_eq!(received, frame);
    }

    #[test]
    fn a_reader_that_stops_draining_drops_its_writer_within_the_budget() {
        let runtime = std::env::temp_dir().join(format!(
            "agenmux-stuck-reader-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&runtime).unwrap();
        let path = frame_path(&runtime, "%1");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // Opened like the pane reader, then never read.
        let fd = unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
        assert!(fd >= 0);
        let _reader = unsafe { File::from_raw_fd(fd) };

        let mut writers = PaneWriters {
            writers: HashMap::new(),
            runtime: runtime.clone(),
        };
        assert!(writers.reconcile(["%1".to_string()]));
        let started = Instant::now();
        writers.emit(&"x".repeat(1 << 20));
        assert!(
            started.elapsed() < WRITE_BUDGET * 3,
            "{:?}",
            started.elapsed()
        );
        assert!(writers.writers.is_empty());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(runtime).unwrap();
    }
}
