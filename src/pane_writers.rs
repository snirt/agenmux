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

/// Raw terminal input from sidebar panes, for the daemon to decode.
pub(crate) fn input_path(runtime: &Path) -> PathBuf {
    runtime.join("agenmux-input")
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
                        && crate::tmux::poll_by(self.0.as_raw_fd(), libc::POLLOUT, deadline) => {}
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
            runtime: crate::tmux::runtime_dir(),
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

const FRAME_START: &[u8] = b"\x1b[?2026h";
const FRAME_END: &[u8] = b"\x1b[?2026l";
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// FIFO reads are not messages. Only complete synchronized frames may reach
/// the terminal; a fresh start marker also recovers from a dropped writer.
#[derive(Default)]
struct FrameBuffer(Vec<u8>, usize);

impl FrameBuffer {
    fn push(&mut self, bytes: &[u8]) -> Option<String> {
        self.0.extend_from_slice(bytes);
        let mut latest = None;
        loop {
            let Some(start) = self
                .0
                .windows(FRAME_START.len())
                .position(|w| w == FRAME_START)
            else {
                let keep = self.0.len().min(FRAME_START.len() - 1);
                self.0.drain(..self.0.len() - keep);
                self.1 = 0;
                break;
            };
            self.0.drain(..start);
            let tail = &self.0[FRAME_START.len()..];
            let scan = self.1.min(tail.len());
            let end = tail[scan..]
                .windows(FRAME_END.len())
                .position(|w| w == FRAME_END)
                .map(|p| scan + p);
            let restart = tail[scan..]
                .windows(FRAME_START.len())
                .position(|w| w == FRAME_START)
                .map(|p| scan + p);
            if let Some(restart) = restart.filter(|&r| end.is_none_or(|end| r < end)) {
                self.0.drain(..FRAME_START.len() + restart);
                self.1 = 0;
                continue;
            }
            if let Some(end) = end {
                let end = FRAME_START.len() + end;
                if end <= MAX_FRAME {
                    if let Ok(frame) = std::str::from_utf8(&self.0[FRAME_START.len()..end]) {
                        latest = Some(frame.to_string());
                    }
                }
                self.0.drain(..end + FRAME_END.len());
                self.1 = 0;
            } else {
                // Keep only a possible marker prefix in the next scan, making
                // incremental FIFO decoding linear even for very large frames.
                self.1 = tail.len().saturating_sub(FRAME_START.len() - 1);
                if self.0.len() > MAX_FRAME {
                    let keep = FRAME_START.len() - 1;
                    self.0.drain(..self.0.len() - keep);
                    self.1 = 0;
                }
                break;
            }
        }
        latest
    }
}

// Self-pipe closes the signal-before-poll race without polling an idle pane.
static PANE_WAKE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn wake_pane(_: libc::c_int) {
    let fd = PANE_WAKE.load(Ordering::Relaxed);
    if fd >= 0 {
        unsafe {
            libc::write(fd, b"r".as_ptr().cast(), 1);
        }
    }
}

fn wake_pipe() -> std::io::Result<(File, File)> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let files = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    for fd in fds {
        if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } < 0
            || unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(files)
}

static PANE_STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop_pane(_: libc::c_int) {
    PANE_STOP.store(true, Ordering::Relaxed);
    wake_pane(0);
}

/// One idle reader per split pane: tmux sends terminal paste to its PTY, not
/// to key bindings. Frames arrive on a separate FIFO from the shared daemon.
pub(crate) fn run_pane() -> i32 {
    let Some(pane) = crate::mux::own_pane() else {
        return 1;
    };
    // tmux key tables send the daemon its keys; elsewhere this pane does
    let forward = !crate::mux::routes_keys();
    let runtime = crate::tmux::runtime_dir();
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
    let Ok((mut wake, wake_writer)) = wake_pipe() else {
        let _ = std::fs::remove_file(&path);
        return 1;
    };
    PANE_STOP.store(false, Ordering::Relaxed);
    PANE_WAKE.store(wake_writer.as_raw_fd(), Ordering::Relaxed);
    unsafe {
        libc::signal(libc::SIGHUP, stop_pane as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, stop_pane as *const () as libc::sighandler_t);
        libc::signal(libc::SIGWINCH, wake_pane as *const () as libc::sighandler_t);
    }
    let mut decoder = FrameBuffer::default();
    let mut last_frame = None;
    let _raw = crate::input::RawMode::enable();
    let keys = crate::input::protocol_keys(crate::app_config::KeyMode::Search);
    let mut stdout = std::io::stdout().lock();
    if forward {
        // The alternate screen keeps frames out of the scrollback, where a
        // resize would reflow them into junk.
        let _ = stdout.write_all(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[2J");
        let _ = stdout.flush();
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
            libc::pollfd {
                fd: wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(fds.as_mut_ptr(), 3, -1) } < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
        if fds[0].revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            break;
        }
        let mut repaint = fds[2].revents & libc::POLLIN != 0;
        if repaint {
            let mut buf = [0u8; 128];
            while wake.read(&mut buf).is_ok() {}
        }
        if PANE_STOP.load(Ordering::Relaxed) {
            break;
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 8192];
            match frames.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Some(frame) = decoder.push(&buf[..n]) {
                        last_frame = Some(frame);
                        repaint = true;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) => {}
                Err(_) => break,
            }
        }
        if repaint {
            if let Some(frame) = &last_frame {
                let (cols, rows) = crate::input::term_size();
                let frame = crate::sidebar::viewport_frame(frame, cols, rows);
                if stdout
                    .write_all(frame.as_bytes())
                    .and_then(|_| stdout.flush())
                    .is_err()
                {
                    break;
                }
            }
        }
        if forward && fds[0].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 4096];
            match std::io::stdin().read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => forward_input(&runtime, &buf[..n]),
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
    PANE_WAKE.store(-1, Ordering::Relaxed);
    let _ = stdout.write_all(b"\x1b[?2026l\x1b[?7h");
    if forward {
        let _ = stdout.write_all(b"\x1b[?1006l\x1b[?1000l\x1b[?1049l");
        let _ = stdout.flush();
    }
    let _ = stdout.flush();
    drop(stdout);
    drop(frames);
    let _ = std::fs::remove_file(path);
    0
}

/// One write per read, so a key's bytes reach the daemon together. Open the
/// current FIFO each time: a daemon handover replaces its inode, and retaining
/// a writer would silently send keys to the retired daemon or drop the first key.
fn forward_input(runtime: &Path, bytes: &[u8]) {
    if let Ok(mut input) = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(input_path(runtime))
    {
        if input.metadata().is_ok_and(|m| m.file_type().is_fifo()) {
            let _ = input.write_all(bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_input_reopens_the_daemons_fifo_after_handover() {
        let runtime =
            std::env::temp_dir().join(format!("agenmux-input-rebind-{}", std::process::id()));
        std::fs::create_dir_all(&runtime).unwrap();
        let path = input_path(&runtime);
        let open = || {
            let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let fd = unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
            assert!(fd >= 0);
            unsafe { File::from_raw_fd(fd) }
        };
        let mut retired = open();
        forward_input(&runtime, b"j");
        let mut buf = [0; 1];
        assert_eq!(retired.read(&mut buf).unwrap(), 1);
        assert_eq!(&buf, b"j");
        std::fs::remove_file(&path).unwrap();
        let mut current = open();
        // The old daemon can still have its FIFO open during takeover.
        forward_input(&runtime, b"k");
        assert_eq!(current.read(&mut buf).unwrap(), 1);
        assert_eq!(&buf, b"k");
        assert_eq!(
            retired.read(&mut buf).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&runtime).unwrap();
    }

    fn envelope(body: &str) -> Vec<u8> {
        [FRAME_START, body.as_bytes(), FRAME_END].concat()
    }

    #[test]
    fn frame_boundaries_survive_every_fifo_chunk_and_utf8_split() {
        let bytes = envelope("\x1b[Hrow 一❤️\nnext");
        for split in 0..bytes.len() {
            let mut b = FrameBuffer::default();
            assert_eq!(b.push(&bytes[..split]), None);
            assert_eq!(
                b.push(&bytes[split..]).as_deref(),
                Some("\x1b[Hrow 一❤️\nnext")
            );
            assert!(b.0.len() < FRAME_START.len());
        }
        let mut b = FrameBuffer::default();
        let mut result = None;
        for byte in bytes {
            result = b.push(&[byte]).or(result);
        }
        assert_eq!(result.as_deref(), Some("\x1b[Hrow 一❤️\nnext"));
    }

    #[test]
    fn incomplete_and_abandoned_frames_never_reach_the_screen() {
        let mut b = FrameBuffer::default();
        assert_eq!(b.push(b"junk\x1b[?2026htorn\x1b["), None);
        assert_eq!(b.push(&envelope("good")).as_deref(), Some("good"));
        let bytes = [envelope("old"), envelope("new")].concat();
        assert_eq!(b.push(&bytes).as_deref(), Some("new"));
        assert_eq!(
            b.push(&[FRAME_START, &vec![b'x'; MAX_FRAME]].concat()),
            None
        );
        assert!(b.0.len() < FRAME_START.len());
        assert_eq!(b.push(&envelope("recovered")).as_deref(), Some("recovered"));
    }

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
