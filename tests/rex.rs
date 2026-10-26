//! The Rex backend against a fake `rex` CLI: what the scan reports, and that
//! agenmux never asks the CLI to start a server.
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

/// One scan as `rex_scan.lua` returns it: a sidebar block, a shell with a
/// label of its own and no title, and a window Lua encoded as `{}`.
const LISTING: &str = r#"{"sessions":[
 {"session_id":"session:1","label":"work","active_window_id":"window:1","windows":[
  {"window_id":"window:1","label":"Window 1","focused_block_id":"block:2","layout":{},"blocks":[
   {"block_id":"block:1","label":"agenmux","process":{"child":{"pid":11,"name":"agenmux","cwd":"/work"}},"title":{"title":""},"size":{"columns":30,"rows":40}},
   {"block_id":"block:2","label":"shell","process":{"child":{"pid":12,"name":"zsh","argv0":"-zsh","cwd":"/work/a"},"foreground":{"pid":12,"name":"zsh","argv0":"-zsh","cwd":"/work/a"}},"title":{"title":"build"},"size":{"columns":90,"rows":40}}]},
  {"window_id":"window:2","label":"logs","focused_block_id":"block:3","layout":{},"blocks":[
   {"block_id":"block:3","label":"tail","process":{"child":{"pid":13,"name":"zsh","cwd":"/work/b"},"foreground":{"pid":13,"name":"zsh","cwd":"/work/b"}},"title":{"title":""},"size":{"columns":120,"rows":40}}]},
  {"window_id":"window:3","label":"empty","layout":{},"blocks":{}}]}]}"#;

struct Fake {
    dir: PathBuf,
}

impl Fake {
    fn new(name: &str) -> Fake {
        let dir = std::env::temp_dir().join(format!("agenmux-rex-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("listing.json"), LISTING).unwrap();
        let script = dir.join("rex");
        std::fs::write(
            &script,
            format!(
                r#"#!/bin/sh
printf '%s %s %s\n' "$1" "$2" "$3" >> '{log}'
case " $* " in
  *" whoami "*) echo '{{}}' ;;
  *" do -e "*) cat '{listing}' ;;
  *" capture "*) printf '$ make\nok\n' ;;
  *) exit 1 ;;
esac
"#,
                log = dir.join("calls").display(),
                listing = dir.join("listing.json").display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake { dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_agenmux"))
            .args(args)
            .env("AGENMUX_MUX", "rex")
            .env("AGENMUX_REX", self.dir.join("rex"))
            .env("REX_SESSION", "session:1")
            .env("XDG_STATE_HOME", &self.dir)
            .env_remove("TMUX")
            .env_remove("REX_BLOCK")
            .output()
            .unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default()
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn rex_scan_lists_terminal_blocks_but_not_the_sidebar() {
    let fake = Fake::new("list");
    let listed = stdout(&fake.run(&["list", "-s", "work"]));
    assert_eq!(
        listed,
        concat!(
            "block:2\twork:1.1\t-\t-\ta\tzsh\n",
            "block:3\twork:2.1\t-\t-\tb\tzsh\n",
        )
    );
    // one batched scan, no block-by-block forks
    let calls = fake.calls();
    assert_eq!(calls.matches(" do -e").count(), 1, "{calls}");
    assert!(!calls.contains(" block "), "{calls}");
}

#[test]
fn rex_calls_never_start_a_server() {
    let fake = Fake::new("autostart");
    stdout(&fake.run(&["list"]));
    let calls = fake.calls();
    assert!(!calls.is_empty());
    for line in calls.lines() {
        assert!(line.starts_with("--autostart=false "), "{line}");
    }
}

/// Exercise the real sidebar-pane process, including FIFO framing, ioctl size
/// reads, SIGWINCH replay and terminal cleanup. No Rex server or live UI needed.
struct PtyPane {
    child: std::process::Child,
    master: std::fs::File,
    slave: std::fs::File,
    writer: std::fs::File,
    dir: PathBuf,
    output: Vec<u8>,
}

impl PtyPane {
    fn start() -> Self {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::OpenOptionsExt;
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = PathBuf::from(format!(
            "/tmp/amx-pty-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let runtime = dir.join(format!("agenmux-rex-{}", unsafe { libc::geteuid() }));
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_col: 4,
            ws_row: 2,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &mut size,
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
        let child = Command::new(env!("CARGO_BIN_EXE_agenmux"))
            .arg("sidebar-pane")
            .env("AGENMUX_MUX", "rex")
            .env("REX_BLOCK", "block:test")
            .env("TMPDIR", &dir)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(slave.try_clone().unwrap())
            .stdout(slave.try_clone().unwrap())
            .stderr(slave.try_clone().unwrap())
            .spawn()
            .unwrap();
        let path = runtime.join("agenmux-frame-block:test");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let writer = loop {
            if let Ok(file) = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&path)
            {
                break file;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pane did not create its FIFO"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let mut pane = Self {
            child,
            master,
            slave,
            writer,
            dir,
            output: Vec::new(),
        };
        pane.read_until(b"\x1b[2J");
        pane.output.clear();
        pane
    }

    fn ready(&self, milliseconds: i32) -> bool {
        use std::os::fd::AsRawFd;
        let mut p = libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut p, 1, milliseconds) > 0 }
    }

    fn read_until(&mut self, marker: &[u8]) {
        use std::io::Read;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !self.output.windows(marker.len()).any(|s| s == marker) {
            assert!(
                std::time::Instant::now() < deadline,
                "viewport timed out: {:?}",
                String::from_utf8_lossy(&self.output)
            );
            if self.ready(20) {
                let mut buf = [0u8; 8192];
                let n = self.master.read(&mut buf).unwrap();
                assert!(n > 0);
                self.output.extend_from_slice(&buf[..n]);
            }
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        use std::os::fd::AsRawFd;
        let size = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        self.output.clear();
        assert_eq!(
            unsafe { libc::ioctl(self.slave.as_raw_fd(), libc::TIOCSWINSZ, &size) },
            0
        );
        assert_eq!(
            unsafe { libc::kill(self.child.id() as i32, libc::SIGWINCH) },
            0
        );
        self.read_until(b"\x1b[?2026l");
    }
}

impl Drop for PtyPane {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn sidebar_pane_reprojects_complete_frames_on_pty_resize_without_new_input() {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    let mut p = PtyPane::start();
    let body = "\x1b[Habcdef\n123456\nthird 一❤️\n";
    p.writer
        .write_all(format!("\x1b[?2026h{body}").as_bytes())
        .unwrap();
    assert!(!p.ready(50), "partial frames must not reach the terminal");
    p.writer.write_all(b"\x1b[?2026l").unwrap();
    p.read_until(b"\x1b[?2026l");
    let out = String::from_utf8_lossy(&p.output);
    assert!(out.contains("abcd\x1b[2;1H1234"), "{out:?}");
    assert!(!out.contains("ef") && !out.contains("56") && !out.contains("third"));
    assert!(!out.contains('\n'));
    // No new daemon frame: a blocked reader must wake and replay its cache.
    for _ in 0..4 {
        p.resize(12, 3);
        let out = String::from_utf8_lossy(&p.output);
        assert!(
            out.contains("abcdef") && out.contains("third 一❤️"),
            "{out:?}"
        );
        p.resize(1, 1);
        let out = String::from_utf8_lossy(&p.output);
        assert!(out.contains("\x1b[2J\x1b[1;1Ha") && !out.contains("bc") && !out.contains("123"));
    }
    p.writer.write_all(b"\x1b[?2026htorn\x1b[").unwrap();
    assert!(!p.ready(30));
    p.writer
        .write_all(b"\x1b[?2026h\x1b[HOK\x1b[?2026l")
        .unwrap();
    p.output.clear();
    p.read_until(b"\x1b[?2026l");
    assert!(String::from_utf8_lossy(&p.output).contains("\x1b[2J\x1b[1;1HO"));
    p.output.clear();
    assert_eq!(unsafe { libc::kill(p.child.id() as i32, libc::SIGTERM) }, 0);
    p.read_until(b"\x1b[?1049l");
    assert!(String::from_utf8_lossy(&p.output).contains("\x1b[?7h"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(status) = p.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(std::time::Instant::now() < deadline, "pane failed to stop");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let mut terminal = unsafe { std::mem::zeroed::<libc::termios>() };
    assert_eq!(
        unsafe { libc::tcgetattr(p.slave.as_raw_fd(), &mut terminal) },
        0
    );
    assert_ne!(terminal.c_lflag & libc::ICANON, 0);
    assert_ne!(terminal.c_lflag & libc::ECHO, 0);
}

#[test]
fn sidebar_pane_reports_real_sizes_and_reconnects_without_another_resize() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::time::{Duration, Instant};
    let mut pane = PtyPane::start();
    pane.writer
        .write_all(b"\x1b[?2026h\x1b[Hcached\x1b[?2026l")
        .unwrap();
    pane.read_until(b"\x1b[?2026l");
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let runtime = pane
        .dir
        .join(format!("agenmux-rex-{}", unsafe { libc::geteuid() }));
    let meta = std::fs::metadata(runtime).unwrap();
    let root = PathBuf::from(format!("/tmp/agenmux-ipc-{}", unsafe { libc::geteuid() }));
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&root);
    let path = root.join(format!("{:x}-{:x}.sock", meta.dev(), meta.ino()));
    fn accept(listener: &UnixListener) -> BufReader<UnixStream> {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "pane did not reconnect");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("accept failed: {e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        BufReader::new(stream)
    }
    fn record(reader: &mut BufReader<UnixStream>) -> serde_json::Value {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        serde_json::from_str(&line).unwrap()
    }
    let listener = UnixListener::bind(&path).unwrap();
    let mut reader = accept(&listener);
    let hello = record(&mut reader);
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["pane"], "block:test");
    assert_eq!(
        (hello["cols"].as_u64(), hello["rows"].as_u64()),
        (Some(4), Some(2))
    );
    reader
        .get_mut()
        .write_all(b"{\"type\":\"welcome\",\"version\":1,\"generation\":\"first\"}\n")
        .unwrap();
    assert_eq!(record(&mut reader)["generation"], "first");
    pane.resize(20, 4);
    let resize = record(&mut reader);
    assert_eq!(resize["type"], "resize");
    assert_eq!(
        (resize["cols"].as_u64(), resize["rows"].as_u64()),
        (Some(20), Some(4))
    );
    drop(reader);
    drop(listener);
    std::fs::remove_file(&path).unwrap();
    let listener = UnixListener::bind(&path).unwrap();
    let mut reader = accept(&listener);
    let hello = record(&mut reader);
    assert_eq!(hello["type"], "hello");
    assert_eq!(
        (hello["cols"].as_u64(), hello["rows"].as_u64()),
        (Some(20), Some(4))
    );
    drop(reader);
    drop(listener);
    std::fs::remove_file(path).unwrap();
}
