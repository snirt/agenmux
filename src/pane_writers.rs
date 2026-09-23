//! Frame delivery and terminal input for split sidebar panes.
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) fn frame_path(runtime: &Path, pane: &str) -> PathBuf {
    runtime.join(format!("agenmux-frame-{pane}"))
}

struct PaneWriter(File);

impl PaneWriter {
    fn open(runtime: &Path, pane: &str) -> std::io::Result<Self> {
        // Nonblocking open avoids hanging while tmux starts the pane reader.
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
            .open(frame_path(runtime, pane))?;
        if !file.metadata()?.file_type().is_fifo() {
            return Err(std::io::Error::other("sidebar frame target is not a FIFO"));
        }
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK) }
                < 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(file))
    }

    fn emit(&mut self, frame: &[u8]) -> std::io::Result<()> {
        self.0.write_all(frame)
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
            runtime: std::env::temp_dir(),
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
        self.writers
            .retain(|_, writer| writer.emit(frame.as_bytes()).is_ok());
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
