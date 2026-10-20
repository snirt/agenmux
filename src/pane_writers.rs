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

static PANE_STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop_pane(_: libc::c_int) {
    PANE_STOP.store(true, Ordering::Relaxed);
}

/// One idle reader per split pane: tmux sends terminal paste to its PTY, not
/// to key bindings. Frames arrive on a separate FIFO from the shared daemon.
pub(crate) fn run_pane() -> i32 {
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return 1;
    };
    if !pane
        .strip_prefix('%')
        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
    {
        return 1;
    }
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
    unsafe {
        libc::signal(libc::SIGHUP, stop_pane as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, stop_pane as *const () as libc::sighandler_t);
    }
    let _raw = crate::input::RawMode::enable();
    let keys = crate::input::protocol_keys(crate::app_config::KeyMode::Search);
    let mut stdout = std::io::stdout().lock();
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
                Ok(n)
                    if stdout
                        .write_all(&buf[..n])
                        .and_then(|_| stdout.flush())
                        .is_err() =>
                {
                    break
                }
                _ => {}
            }
        }
        if fds[0].revents & libc::POLLIN != 0 {
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
    drop(stdout);
    drop(frames);
    let _ = std::fs::remove_file(path);
    0
}

#[cfg(test)]
mod tests {
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
