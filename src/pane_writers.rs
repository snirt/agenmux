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

struct PendingFrame {
    bytes: std::sync::Arc<[u8]>,
    written: usize,
}

struct PaneWriter {
    file: File,
    current: Option<PendingFrame>,
    latest: Option<std::sync::Arc<[u8]>>,
    progress: Instant,
}

impl PaneWriter {
    fn open(runtime: &Path, pane: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(frame_path(runtime, pane))?;
        if !file.metadata()?.file_type().is_fifo() {
            return Err(std::io::Error::other("sidebar frame target is not a FIFO"));
        }
        Ok(Self {
            file,
            current: None,
            latest: None,
            progress: Instant::now(),
        })
    }

    fn queue(&mut self, frame: std::sync::Arc<[u8]>, now: Instant) {
        if self.current.as_ref().is_some_and(|f| f.written > 0) {
            self.latest = Some(frame);
        } else {
            // Nothing from this frame reached the reader; replace it outright.
            if self.current.is_none() {
                self.progress = now;
            }
            self.current = Some(PendingFrame {
                bytes: frame,
                written: 0,
            });
            self.latest = None;
        }
    }

    fn pump(&mut self, now: Instant) -> std::io::Result<()> {
        // Fair, bounded work even when one pane or a frame is enormous. No poll
        // or sleep here: the daemon's common event loop owns all waiting.
        for _ in 0..8 {
            let Some(frame) = self.current.as_mut() else {
                return Ok(());
            };
            let rest = &frame.bytes[frame.written..];
            match self.file.write(&rest[..rest.len().min(4096)]) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    frame.written += n;
                    self.progress = now;
                    if frame.written == frame.bytes.len() {
                        self.current = self
                            .latest
                            .take()
                            .map(|bytes| PendingFrame { bytes, written: 0 });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }

    fn deadline(&self) -> Option<Instant> {
        self.current.as_ref().map(|_| self.progress + WRITE_BUDGET)
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
        if frame.is_empty() || frame.len() > MAX_FRAME {
            return;
        }
        let now = Instant::now();
        let bytes: std::sync::Arc<[u8]> = frame.as_bytes().into();
        for writer in self.writers.values_mut() {
            writer.queue(bytes.clone(), now);
        }
        self.pump(now);
    }

    pub(crate) fn pump(&mut self, now: Instant) {
        self.writers.retain(|_, writer| writer.pump(now).is_ok());
    }

    pub(crate) fn interests(&self) -> Vec<libc::pollfd> {
        self.writers
            .values()
            .filter(|w| w.current.is_some())
            .map(|w| libc::pollfd {
                fd: w.file.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            })
            .collect()
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.writers.values().filter_map(PaneWriter::deadline).min()
    }

    pub fn clear(&mut self) {
        self.writers.clear();
    }
}

// Viewport messages travel toward the daemon; frames and input keep their FIFOs.
const MAX_RECORD: usize = 1024;
const HANDSHAKE: Duration = Duration::from_secs(3);
const RETRY_MIN: Duration = Duration::from_millis(25);
const RETRY_MAX: Duration = Duration::from_millis(500);
type Size = (usize, usize);

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum PaneMessage {
    Hello {
        version: u8,
        pane: String,
        cols: u16,
        rows: u16,
    },
    Welcome {
        version: u8,
        generation: String,
    },
    Resize {
        generation: String,
        seq: u64,
        cols: u16,
        rows: u16,
    },
}

fn protocol_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid pane report")
}

struct Wire {
    stream: std::os::unix::net::UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    written: usize,
}

impl Wire {
    fn new(stream: std::os::unix::net::UnixStream) -> std::io::Result<Self> {
        stream.set_nonblocking(true)?;
        if unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            stream,
            input: Vec::new(),
            output: Vec::new(),
            written: 0,
        })
    }

    fn interest(&self) -> libc::pollfd {
        libc::pollfd {
            fd: self.stream.as_raw_fd(),
            events: libc::POLLIN
                | if self.output.is_empty() {
                    0
                } else {
                    libc::POLLOUT
                },
            revents: 0,
        }
    }

    fn queue(&mut self, message: &PaneMessage) -> std::io::Result<()> {
        if !self.output.is_empty() {
            return Err(protocol_error());
        }
        self.output = serde_json::to_vec(message).map_err(|_| protocol_error())?;
        if self.output.len() >= MAX_RECORD {
            return Err(protocol_error());
        }
        self.output.push(b'\n');
        self.written = 0;
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        for _ in 0..8 {
            if self.written == self.output.len() {
                self.output.clear();
                self.written = 0;
                break;
            }
            match self.stream.write(&self.output[self.written..]) {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => self.written += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn receive(&mut self) -> std::io::Result<Vec<PaneMessage>> {
        let mut messages = Vec::new();
        // At most 4 KiB per peer per turn, with at most one partial record kept.
        for _ in 0..4 {
            let mut buf = [0; MAX_RECORD];
            match self.stream.read(&mut buf) {
                Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => {
                    for &byte in &buf[..n] {
                        if byte == b'\n' {
                            messages.push(
                                serde_json::from_slice(&self.input)
                                    .map_err(|_| protocol_error())?,
                            );
                            self.input.clear();
                        } else {
                            self.input.push(byte);
                            if self.input.len() >= MAX_RECORD {
                                return Err(protocol_error());
                            }
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e),
            }
        }
        Ok(messages)
    }
}

fn private_identity(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::symlink_metadata(path)?;
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o077 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe pane runtime",
        ));
    }
    Ok((m.dev(), m.ino()))
}

/// sockaddr_un has a short fixed pathname even when the runtime directory is
/// arbitrarily deep. Device/inode identify the existing private runtime without
/// hashing paths or merging servers; the shared parent is itself owner-only.
pub(crate) fn report_path(runtime: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let (dev, ino) = private_identity(runtime)?;
    let root = PathBuf::from(format!("/tmp/agenmux-ipc-{}", unsafe { libc::geteuid() }));
    match std::fs::DirBuilder::new().mode(0o700).create(&root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    private_identity(&root)?;
    Ok(root.join(format!("{dev:x}-{ino:x}.sock")))
}

// Unlink only our own inode: the successor can publish its endpoint before we exit.
struct SocketPath {
    path: PathBuf,
    identity: (u64, u64),
}

impl Drop for SocketPath {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.file_type().is_socket() && (m.dev(), m.ino()) == self.identity)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

struct ReportPeer {
    wire: Wire,
    id: u64,
    pane: Option<String>,
    size: Size,
    seq: u64,
    expires: Instant,
}

/// Owns the listener, bounded per-pane reports, and ownership-safe endpoint cleanup.
/// Callers supply inventory; receiving a report never queries the multiplexer.
pub(crate) struct PaneReports {
    listener: std::os::unix::net::UnixListener,
    _endpoint: SocketPath,
    generation: String,
    peers: Vec<ReportPeer>,
    known: HashSet<String>,
    sizes: HashMap<String, Size>,
    next_id: u64,
}

impl PaneReports {
    pub(crate) fn bind(runtime: &Path, generation: String) -> std::io::Result<Self> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let path = report_path(runtime)?;
        let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
        for target in [&path, &temporary] {
            match std::fs::symlink_metadata(target) {
                Ok(m) if !m.file_type().is_socket() || m.uid() != unsafe { libc::geteuid() } => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "unsafe pane endpoint",
                    ));
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        // The caller already owns the daemon claim. Publish atomically without
        // destroying a working predecessor's pathname if bind/configuration fails.
        let _ = std::fs::remove_file(&temporary);
        let listener = std::os::unix::net::UnixListener::bind(&temporary)?;
        let m = std::fs::symlink_metadata(&temporary)?;
        let mut endpoint = SocketPath {
            path: temporary,
            identity: (m.dev(), m.ino()),
        };
        listener.set_nonblocking(true)?;
        if unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        std::fs::set_permissions(&endpoint.path, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&endpoint.path, &path)?;
        endpoint.path = path;
        Ok(Self {
            listener,
            _endpoint: endpoint,
            generation,
            peers: Vec::new(),
            known: HashSet::new(),
            sizes: HashMap::new(),
            next_id: 0,
        })
    }

    pub(crate) fn reconcile(&mut self, panes: impl IntoIterator<Item = String>) {
        self.known = panes.into_iter().collect();
        self.peers
            .retain(|p| p.pane.as_ref().is_none_or(|pane| self.known.contains(pane)));
        self.sizes.retain(|pane, _| self.known.contains(pane));
    }

    pub(crate) fn size(&self, pane: &str) -> Option<Size> {
        self.sizes.get(pane).copied()
    }

    pub(crate) fn interests(&self) -> Vec<libc::pollfd> {
        let mut fds = vec![libc::pollfd {
            fd: self.listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        fds.extend(self.peers.iter().map(|p| p.wire.interest()));
        fds
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.peers
            .iter()
            .filter(|p| p.pane.is_none())
            .map(|p| p.expires)
            .min()
    }

    pub(crate) fn pump(&mut self, now: Instant) -> bool {
        for _ in 0..16 {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if self.peers.len() >= (self.known.len() * 2 + 8).min(256) {
                        continue;
                    }
                    if let Ok(wire) = Wire::new(stream) {
                        self.next_id += 1;
                        self.peers.push(ReportPeer {
                            wire,
                            id: self.next_id,
                            pane: None,
                            size: (0, 0),
                            seq: 0,
                            expires: now + HANDSHAKE,
                        });
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        self.peers.retain_mut(|peer| {
            let result = (|| -> std::io::Result<()> {
                if peer.pane.is_none() && now >= peer.expires {
                    return Err(protocol_error());
                }
                for message in peer.wire.receive()? {
                    match message {
                        PaneMessage::Hello {
                            version: 1,
                            pane,
                            cols,
                            rows,
                        } if peer.pane.is_none()
                            && self.known.contains(&pane)
                            && cols > 0
                            && rows > 0 =>
                        {
                            peer.pane = Some(pane);
                            peer.size = (cols as usize, rows as usize);
                            peer.wire.queue(&PaneMessage::Welcome {
                                version: 1,
                                generation: self.generation.clone(),
                            })?;
                        }
                        PaneMessage::Resize {
                            generation,
                            seq,
                            cols,
                            rows,
                        } if peer.pane.is_some()
                            && generation == self.generation
                            && cols > 0
                            && rows > 0 =>
                        {
                            if seq > peer.seq {
                                peer.seq = seq;
                                peer.size = (cols as usize, rows as usize);
                                trace!("pane viewport received {cols}x{rows} seq={seq}");
                            }
                        }
                        _ => return Err(protocol_error()),
                    }
                }
                peer.wire.flush()
            })();
            result.is_ok()
        });
        // A replacement connection wins; late data from the retired one cannot
        // overwrite it. Disconnection removes authority, allowing backend fallback.
        let mut latest = HashMap::new();
        for p in &self.peers {
            if let Some(pane) = &p.pane {
                latest
                    .entry(pane.clone())
                    .and_modify(|id: &mut u64| *id = (*id).max(p.id))
                    .or_insert(p.id);
            }
        }
        self.peers.retain(|p| {
            p.pane
                .as_ref()
                .is_none_or(|pane| latest.get(pane) == Some(&p.id))
        });
        let sizes = self
            .peers
            .iter()
            .filter_map(|p| Some((p.pane.clone()?, p.size)))
            .collect();
        let changed = self.sizes != sizes;
        self.sizes = sizes;
        changed
    }
}

// A local Unix connect can block when the accept backlog fills. NONBLOCK must
// be set before connect, not merely on the returned stream.
fn connect_report(path: &Path) -> std::io::Result<(Wire, bool)> {
    use std::os::unix::net::UnixStream;
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        return Err(protocol_error());
    }
    address.sun_family = libc::AF_UNIX as _;
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        address.sun_len = len as _;
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let wire = Wire::new(unsafe { UnixStream::from_raw_fd(fd) })?;
    if unsafe { libc::connect(fd, (&address as *const libc::sockaddr_un).cast(), len) } == 0 {
        return Ok((wire, false));
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::EINPROGRESS) {
        Ok((wire, true))
    } else {
        Err(error)
    }
}

struct PaneReporter {
    runtime: PathBuf,
    pane: String,
    size: Option<Size>,
    wire: Option<Wire>,
    connecting: bool,
    generation: Option<String>,
    seq: u64,
    dirty: bool,
    retry: Instant,
    backoff: Duration,
    handshake: Option<Instant>,
}

impl PaneReporter {
    fn new(runtime: &Path, pane: String, size: Option<Size>, now: Instant) -> Self {
        Self {
            runtime: runtime.to_path_buf(),
            pane,
            size,
            wire: None,
            connecting: false,
            generation: None,
            seq: 0,
            dirty: true,
            retry: now,
            backoff: RETRY_MIN,
            handshake: None,
        }
    }

    fn observe(&mut self, size: Option<Size>) {
        if let Some(size) = size
            .filter(|&(w, h)| w > 0 && h > 0 && w <= u16::MAX as usize && h <= u16::MAX as usize)
        {
            if self.size != Some(size) {
                self.size = Some(size);
                trace!(
                    "pane viewport measured {}x{} pane={}",
                    size.0,
                    size.1,
                    self.pane
                );
                self.dirty = true;
            }
        }
    }

    fn disconnect(&mut self, now: Instant) {
        self.wire = None;
        self.generation = None;
        self.seq = 0;
        self.connecting = false;
        self.handshake = None;
        self.dirty = true;
        self.retry = now + self.backoff;
        self.backoff = (self.backoff * 2).min(RETRY_MAX);
    }

    fn interest(&self) -> libc::pollfd {
        let mut p = self.wire.as_ref().map_or(
            libc::pollfd {
                fd: -1,
                events: 0,
                revents: 0,
            },
            Wire::interest,
        );
        if self.connecting {
            p.events |= libc::POLLOUT;
        }
        p
    }

    fn deadline(&self) -> Option<Instant> {
        if self.size.is_none() {
            None
        } else if self.wire.is_none() {
            Some(self.retry)
        } else {
            self.handshake
        }
    }

    fn pump(&mut self, now: Instant) {
        if self.wire.is_none() && self.size.is_some() && now >= self.retry {
            match report_path(&self.runtime).and_then(|path| connect_report(&path)) {
                Ok((wire, connecting)) => {
                    self.wire = Some(wire);
                    self.connecting = connecting;
                    self.handshake = Some(now + HANDSHAKE);
                }
                Err(_) => {
                    self.disconnect(now);
                    return;
                }
            }
        }
        let result = self.exchange(now);
        if result.is_err() {
            self.disconnect(now);
        }
    }

    fn exchange(&mut self, now: Instant) -> std::io::Result<()> {
        let Some(wire) = self.wire.as_mut() else {
            return Ok(());
        };
        if self.handshake.is_some_and(|deadline| now >= deadline) {
            return Err(protocol_error());
        }
        if self.connecting {
            let mut fd = wire.interest();
            fd.events |= libc::POLLOUT;
            if unsafe { libc::poll(&mut fd, 1, 0) } <= 0 {
                return Ok(());
            }
            if let Some(error) = wire.stream.take_error()? {
                return Err(error);
            }
            self.connecting = false;
        }
        // The first outbound record always carries a real snapshot.
        if self.generation.is_none() && self.seq == 0 && self.dirty && wire.output.is_empty() {
            let (cols, rows) = self.size.ok_or_else(protocol_error)?;
            wire.queue(&PaneMessage::Hello {
                version: 1,
                pane: self.pane.clone(),
                cols: cols as u16,
                rows: rows as u16,
            })?;
            self.seq = 1; // hello queued, wait for welcome before sending sizes
        }
        wire.flush()?;
        for message in wire.receive()? {
            match message {
                PaneMessage::Welcome {
                    version: 1,
                    generation,
                } if self.generation.is_none()
                    && !generation.is_empty()
                    && generation.len() <= 128 =>
                {
                    self.generation = Some(generation);
                    self.handshake = None;
                    self.backoff = RETRY_MIN;
                    self.seq = 0;
                    self.dirty = true;
                }
                _ => return Err(protocol_error()),
            }
        }
        if let Some(generation) = &self.generation {
            if self.dirty && wire.output.is_empty() {
                self.seq = self.seq.checked_add(1).ok_or_else(protocol_error)?;
                let (cols, rows) = self.size.ok_or_else(protocol_error)?;
                wire.queue(&PaneMessage::Resize {
                    generation: generation.clone(),
                    seq: self.seq,
                    cols: cols as u16,
                    rows: rows as u16,
                })?;
                self.dirty = false;
            }
        }
        wire.flush()
    }
}

const FRAME_START: &[u8] = b"\x1b[?2026h";
const FRAME_END: &[u8] = b"\x1b[?2026l";
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// FIFO reads are not messages. Only complete synchronized frames may reach
/// the terminal; a fresh start marker also recovers from a dropped writer.
/// Clipboard escapes are lifted out: a resize replays the frame, not the copy.
#[derive(Default)]
struct FrameBuffer(Vec<u8>, usize, String);

const CLIPBOARD: &str = "\x1b]52;";

impl FrameBuffer {
    fn take_clipboard(&mut self) -> String {
        std::mem::take(&mut self.2)
    }

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
                        let mut frame = frame.to_string();
                        while let Some(at) = frame.find(CLIPBOARD) {
                            let Some(len) = frame[at..].find('\x07') else {
                                break;
                            };
                            self.2.extend(frame.drain(at..at + len + 1));
                        }
                        latest = Some(frame);
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
    let mut reporter = PaneReporter::new(
        &runtime,
        pane.clone(),
        crate::input::measured_size(0),
        Instant::now(),
    );
    while !PANE_STOP.load(Ordering::Relaxed) {
        reporter.pump(Instant::now());
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
            reporter.interest(),
        ];
        let timeout = reporter.deadline().map_or(-1, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(i32::MAX as u128) as i32
        });
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) } < 0 {
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
            reporter.observe(crate::input::measured_size(0));
            reporter.pump(Instant::now());
        }
        if PANE_STOP.load(Ordering::Relaxed) {
            break;
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 8192];
            match frames.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let frame = decoder.push(&buf[..n]);
                    let clipboard = decoder.take_clipboard();
                    if !clipboard.is_empty()
                        && stdout
                            .write_all(clipboard.as_bytes())
                            .and_then(|_| stdout.flush())
                            .is_err()
                    {
                        break;
                    }
                    if let Some(frame) = frame {
                        last_frame = Some(frame);
                        trace!("pane frame received pane={pane}");
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

    struct Runtime(PathBuf);
    impl Runtime {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt;
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            // Unique private runtimes also exercise per-server endpoint identity.
            let path = PathBuf::from(format!(
                "/tmp/amx-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn server(&self, generation: &str) -> PaneReports {
            let mut server = PaneReports::bind(&self.0, generation.into()).unwrap();
            server.reconcile(["%1".into(), "%2".into()]);
            server
        }
    }
    impl Drop for Runtime {
        fn drop(&mut self) {
            if let Ok(path) = report_path(&self.0) {
                let _ = std::fs::remove_file(path);
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn exchange(server: &mut PaneReports, reporter: &mut PaneReporter, now: Instant) {
        for _ in 0..8 {
            reporter.pump(now);
            server.pump(now);
        }
    }

    #[test]
    fn viewport_wire_survives_every_record_split_and_rejects_oversize() {
        let bytes = b"{\"type\":\"hello\",\"version\":1,\"pane\":\"%1\",\"cols\":35,\"rows\":46}\n";
        for split in 0..bytes.len() {
            let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
            let mut wire = Wire::new(reader).unwrap();
            writer.write_all(&bytes[..split]).unwrap();
            assert!(wire.receive().unwrap().is_empty());
            writer.write_all(&bytes[split..]).unwrap();
            let records = wire.receive().unwrap();
            assert!(matches!(
                &records[..],
                [PaneMessage::Hello {
                    version: 1,
                    cols: 35,
                    rows: 46,
                    ..
                }]
            ));
        }
        let (reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut wire = Wire::new(reader).unwrap();
        writer.write_all(&vec![b'x'; MAX_RECORD]).unwrap();
        assert!(wire.receive().is_err());
        assert!(wire.input.len() <= MAX_RECORD);
    }

    #[test]
    fn viewport_reports_coalesce_and_reconnect_with_latest_size_without_a_signal() {
        let runtime = Runtime::new();
        let now = Instant::now();
        let mut reporter = PaneReporter::new(&runtime.0, "%1".into(), Some((35, 46)), now);
        reporter.pump(now); // listener does not exist yet
        assert!(reporter.wire.is_none());
        let mut first = runtime.server("first");
        exchange(&mut first, &mut reporter, now + RETRY_MAX);
        assert_eq!(first.size("%1"), Some((35, 46)));
        for width in 1..500 {
            reporter.observe(Some((width, 24)));
        }
        exchange(&mut first, &mut reporter, now + RETRY_MAX);
        assert_eq!(first.size("%1"), Some((499, 24)));
        assert_eq!(reporter.interest().events, libc::POLLIN);
        assert!(
            reporter.deadline().is_none(),
            "connected idle panes must sleep"
        );
        let mut next = runtime.server("second");
        drop(first); // must not unlink the newly published endpoint
        assert!(report_path(&runtime.0).unwrap().exists());
        exchange(&mut next, &mut reporter, now + RETRY_MAX * 2);
        exchange(&mut next, &mut reporter, now + RETRY_MAX * 3);
        assert_eq!(next.size("%1"), Some((499, 24)));
        assert_eq!(reporter.generation.as_deref(), Some("second"));
        drop(reporter);
        next.pump(now + RETRY_MAX * 4);
        assert_eq!(
            next.size("%1"),
            None,
            "disconnected peers lose geometry authority"
        );
    }

    #[test]
    fn viewport_reports_reject_unknown_and_stale_peers_without_harming_others() {
        let runtime = Runtime::new();
        let mut server = runtime.server("live");
        let now = Instant::now();
        let mut old = PaneReporter::new(&runtime.0, "%1".into(), Some((30, 40)), now);
        exchange(&mut server, &mut old, now);
        let mut replacement = PaneReporter::new(&runtime.0, "%1".into(), Some((50, 60)), now);
        exchange(&mut server, &mut replacement, now);
        assert_eq!(server.size("%1"), Some((50, 60)));
        // A retired connection cannot publish a newer sequence over its replacement.
        old.observe(Some((99, 99)));
        old.pump(now);
        server.pump(now);
        assert_eq!(server.size("%1"), Some((50, 60)));
        let mut unknown = PaneReporter::new(&runtime.0, "%999".into(), Some((99, 99)), now);
        exchange(&mut server, &mut unknown, now);
        assert_eq!(server.size("%999"), None);
        assert_eq!(server.size("%1"), Some((50, 60)));
        let wire = &mut replacement.wire.as_mut().unwrap();
        wire.queue(&PaneMessage::Resize {
            generation: "stale".into(),
            seq: 100,
            cols: 99,
            rows: 99,
        })
        .unwrap();
        wire.flush().unwrap();
        server.pump(now);
        assert_eq!(server.size("%1"), None);
    }

    #[test]
    fn viewport_endpoint_refuses_regular_files_and_symlinks() {
        let runtime = Runtime::new();
        let path = report_path(&runtime.0).unwrap();
        std::fs::write(&path, b"leave intact").unwrap();
        assert!(PaneReports::bind(&runtime.0, "test".into()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"leave intact");
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(runtime.0.join("absent"), &path).unwrap();
        assert!(PaneReports::bind(&runtime.0, "test".into()).is_err());
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
    }

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
    fn clipboard_escapes_leave_the_replayed_frame() {
        let mut b = FrameBuffer::default();
        let frame = b.push(&envelope("\x1b]52;c;YQ==\x07\x1b[Hrow"));
        assert_eq!(frame.as_deref(), Some("\x1b[Hrow"));
        assert_eq!(b.take_clipboard(), "\x1b]52;c;YQ==\x07");
        assert_eq!(b.take_clipboard(), "", "a copy is written once");
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
            let mut writer = PaneWriter::open(&runtime, "%1").unwrap();
            writer.queue(sent.into(), Instant::now());
            let deadline = Instant::now() + Duration::from_secs(2);
            while writer.current.is_some() {
                writer.pump(Instant::now()).unwrap();
                if writer.current.is_some() {
                    assert!(crate::tmux::poll_by(
                        writer.file.as_raw_fd(),
                        libc::POLLOUT,
                        deadline
                    ));
                }
            }
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
    fn viewport_reports_bound_backpressure_and_ignore_old_sequences() {
        let runtime = Runtime::new();
        let now = Instant::now();
        let mut server = runtime.server("live");
        let mut reporter = PaneReporter::new(&runtime.0, "%1".into(), Some((30, 40)), now);
        exchange(&mut server, &mut reporter, now);
        let small: libc::c_int = 1024;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    reporter.wire.as_ref().unwrap().stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&small as *const libc::c_int).cast(),
                    std::mem::size_of_val(&small) as _,
                )
            },
            0
        );
        for width in 1..5000 {
            reporter.observe(Some((width, 40)));
            reporter.pump(now); // daemon deliberately does not read
        }
        assert!(
            !reporter.wire.as_ref().unwrap().output.is_empty(),
            "test must fill socket capacity"
        );
        assert!(reporter.wire.as_ref().unwrap().output.len() <= MAX_RECORD);
        for _ in 0..64 {
            exchange(&mut server, &mut reporter, now);
        }
        assert_eq!(server.size("%1"), Some((4999, 40)));
        let seq = reporter.seq + 1;
        let wire = reporter.wire.as_mut().unwrap();
        wire.queue(&PaneMessage::Resize {
            generation: "live".into(),
            seq,
            cols: 45,
            rows: 50,
        })
        .unwrap();
        wire.flush().unwrap();
        server.pump(now);
        wire.queue(&PaneMessage::Resize {
            generation: "live".into(),
            seq: seq - 1,
            cols: 99,
            rows: 99,
        })
        .unwrap();
        wire.flush().unwrap();
        server.pump(now);
        assert_eq!(server.size("%1"), Some((45, 50)));
    }

    #[test]
    fn long_runtime_paths_and_distinct_servers_have_short_isolated_endpoints() {
        use std::os::unix::fs::DirBuilderExt;
        let runtime = Runtime::new();
        let deep = runtime.0.join("deep".repeat(40));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&deep)
            .unwrap();
        assert!(deep.as_os_str().as_encoded_bytes().len() > 104);
        let mut first = runtime.server("first");
        let mut second = PaneReports::bind(&deep, "second".into()).unwrap();
        second.reconcile(["%1".into()]);
        let path = report_path(&deep).unwrap();
        assert!(path.as_os_str().as_encoded_bytes().len() < 100);
        assert_ne!(path, report_path(&runtime.0).unwrap());
        let now = Instant::now();
        let mut a = PaneReporter::new(&runtime.0, "%1".into(), Some((30, 40)), now);
        let mut b = PaneReporter::new(&deep, "%1".into(), Some((50, 60)), now);
        exchange(&mut first, &mut a, now);
        exchange(&mut second, &mut b, now);
        assert_eq!(first.size("%1"), Some((30, 40)));
        assert_eq!(second.size("%1"), Some((50, 60)));
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
        assert!(started.elapsed() < WRITE_BUDGET, "{:?}", started.elapsed());
        assert!(
            !writers.writers.is_empty(),
            "emit queues rather than waiting for the reader"
        );
        writers.pump(Instant::now() + WRITE_BUDGET);
        assert!(writers.writers.is_empty());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(runtime).unwrap();
    }

    #[test]
    fn a_stalled_fifo_does_not_block_a_healthy_pane_and_pending_frames_coalesce() {
        let runtime = Runtime::new();
        let open = |pane| {
            let path = frame_path(&runtime.0, pane);
            let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
            let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
            assert!(fd >= 0);
            unsafe { File::from_raw_fd(fd) }
        };
        let _stalled = open("%1");
        let mut healthy = open("%2");
        let mut writers = PaneWriters {
            writers: HashMap::new(),
            runtime: runtime.0.clone(),
        };
        assert!(writers.reconcile(["%1".into(), "%2".into()]));
        let old = format!("\x1b[?2026h{}\x1b[?2026l", "x".repeat(1 << 20));
        writers.emit(&old);
        writers.emit("\x1b[?2026hmiddle\x1b[?2026l");
        writers.emit("\x1b[?2026hlatest\x1b[?2026l");
        assert!(writers
            .writers
            .values()
            .all(|w| w.latest.as_deref() == Some(b"\x1b[?2026hlatest\x1b[?2026l")));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut received = Vec::new();
        let mut decoder = FrameBuffer::default();
        loop {
            writers.pump(Instant::now());
            let mut buf = [0; 8192];
            match healthy.read(&mut buf) {
                Ok(n) if n > 0 => {
                    received.extend_from_slice(&buf[..n]);
                    if decoder.push(&buf[..n]).as_deref() == Some("latest") {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                other => panic!("healthy FIFO failed: {other:?}"),
            }
            assert!(
                Instant::now() < deadline,
                "healthy reader starved behind stalled reader"
            );
        }
        assert!(!received.windows(6).any(|s| s == b"middle"));
        writers.pump(Instant::now() + WRITE_BUDGET);
        assert!(!writers.writers.contains_key("%1"));
        assert!(writers.writers.contains_key("%2"));
        assert!(
            writers.interests().is_empty(),
            "idle writers must not spin on writability"
        );
    }
}
