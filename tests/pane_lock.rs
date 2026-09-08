use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct TestTmux {
    socket: String,
    tmp: PathBuf,
}
impl TestTmux {
    fn new(name: &str) -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let tmp = PathBuf::from(format!("/tmp/agenmux-{name}-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&tmp).unwrap();
        let socket = tmp.join("socket").to_str().unwrap().to_owned();
        let server = Self { socket, tmp };
        server.assert_tmux(&[
            "-f",
            "/dev/null",
            "new-session",
            "-d",
            "-s",
            "plugin",
            "-x",
            "120",
            "-y",
            "40",
            "/bin/bash -c 'exec sleep 60'",
        ]);
        server
    }
    fn tmux(&self, args: &[&str]) -> Output {
        let mut command = Command::new("tmux");
        command.args(["-S", &self.socket]).args(args);
        let mut child = PaneLockChild::spawn(command);
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "private tmux command blocked");
            thread::sleep(Duration::from_millis(10));
        }
        child.0.take().unwrap().wait_with_output().unwrap()
    }
    fn text(&self, args: &[&str]) -> String {
        let output = self.tmux(args);
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .trim_end()
            .to_owned()
    }
    fn assert_tmux(&self, args: &[&str]) {
        self.text(args);
    }
    fn tmux_env(&self) -> String {
        format!("{},0,0", self.socket)
    }
    fn wait_for(&self, timeout: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + timeout;
        while !condition() {
            assert!(Instant::now() < deadline, "condition timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for TestTmux {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

// Process-group cleanup also kills the deliberately paused tmux child on a
// failed assertion. All commands below target TestTmux's disposable servers.
struct PaneLockChild(Option<Child>);
impl PaneLockChild {
    fn spawn(mut command: Command) -> Self {
        use std::os::unix::process::CommandExt;
        Self(Some(
            command
                .process_group(0)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }

    fn finish(mut self, code: i32) -> String {
        let child = self.0.as_mut().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "public pane command blocked");
            thread::sleep(Duration::from_millis(10));
        }
        let output = self.0.take().unwrap().wait_with_output().unwrap();
        let error = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(code), "{error}");
        error
    }
}
impl Drop for PaneLockChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
        }
    }
}

fn pane_lock_command(server: &TestTmux, args: &[&str], temp: &str) -> Command {
    // Optional saved pre-fix executable makes this exact test red-capable.
    let bin = std::env::var_os("AGENMUX_PANE_LOCK_TEST_BIN")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_agenmux").into());
    let mut command = Command::new(bin);
    let tmp = server.tmp.join(temp);
    std::fs::create_dir_all(&tmp).unwrap();
    command
        .args(args)
        .env("TMUX", server.tmux_env())
        .env("TMPDIR", tmp)
        .env("HOME", &server.tmp)
        .env("XDG_CONFIG_HOME", server.tmp.join("config"))
        .env("AGENMUX_DIR", env!("CARGO_MANIFEST_DIR"))
        .env_remove("PANE_LOCK_TEST_PAUSE");
    command
}

fn pane_lock_run(server: &TestTmux, args: &[&str], code: i32) -> String {
    PaneLockChild::spawn(pane_lock_command(server, args, "other-temp")).finish(code)
}

fn pause_pane_add(server: &TestTmux, window: &str, name: &str) -> (PaneLockChild, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let bin = server.tmp.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let real_tmux = Command::new("/bin/bash")
        .args(["-c", "command -v tmux"])
        .output()
        .unwrap();
    assert!(real_tmux.status.success());
    let wrapper = bin.join("tmux");
    // First list-panes is inside the actual public pane-add critical section.
    // Keep the child alive after parent death to verify descriptor CLOEXEC.
    std::fs::write(
        &wrapper,
        r#"#!/bin/bash
if [ "$1" = list-panes ]; then
    printf '%s\n' "$$" > "$PANE_LOCK_TEST_PAUSE"
    for ((i = 0; i < 1500; i++)); do
        [ ! -e "$PANE_LOCK_TEST_PAUSE.release" ] || break
        sleep 0.01
    done
fi
exec "$PANE_LOCK_TEST_TMUX" "$@"
"#,
    )
    .unwrap();
    std::fs::set_permissions(wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let marker = server.tmp.join(name);
    let mut command = pane_lock_command(server, &["pane-add", window], "owner-temp");
    command
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env(
            "PANE_LOCK_TEST_TMUX",
            String::from_utf8(real_tmux.stdout).unwrap().trim(),
        )
        .env("PANE_LOCK_TEST_PAUSE", &marker);
    let mut child = PaneLockChild::spawn(command);
    server.wait_for(Duration::from_secs(3), || {
        assert!(
            child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "holder exited before entering critical section"
        );
        std::fs::read_to_string(&marker)
            .is_ok_and(|value| value.trim().parse::<u32>().is_ok())
    });
    (child, marker)
}

fn pane_lock_path(server: &TestTmux, window: &str) -> PathBuf {
    let identity = server.text(&[
        "display-message",
        "-p",
        "-t",
        window,
        "#{pid}-#{start_time}-#{window_id}",
    ]);
    PathBuf::from(format!("/tmp/agenmux-pane-locks-{}", unsafe {
        libc::geteuid()
    }))
    .join(format!("{identity}.lock"))
}

fn pane_lock_sidebar_count(server: &TestTmux, window: &str) -> usize {
    server
        .text(&[
            "list-panes",
            "-t",
            window,
            "-f",
            "#{==:#{@agenmux},1}",
            "-F",
            "#{pane_id}",
        ])
        .lines()
        .count()
}

fn pane_lock_clear(server: &TestTmux) {
    for pane in server
        .text(&[
            "list-panes",
            "-a",
            "-f",
            "#{==:#{@agenmux},1}",
            "-F",
            "#{pane_id}",
        ])
        .lines()
    {
        server.assert_tmux(&["kill-pane", "-t", pane]);
    }
}

#[test]
fn public_pane_lock_recovers_bounds_contention_and_rolls_back() {
    let server = TestTmux::new("pane-lock");
    server.assert_tmux(&["set-option", "-g", "@agenmux-on", "1"]);
    server.assert_tmux(&["wait-for", "-L", "agenmux-add-0"]);
    let mut wait = Command::new("tmux");
    wait.args(["-S", &server.socket, "wait-for", "-L", "agenmux-add-0"]);
    let mut waiter = PaneLockChild::spawn(wait);
    thread::sleep(Duration::from_millis(200));
    assert!(waiter.0.as_mut().unwrap().try_wait().unwrap().is_none());
    drop(waiter);
    server.assert_tmux(&["wait-for", "-U", "agenmux-add-0"]);
    // The old implementation blocks here until the test's outer deadline.
    pane_lock_run(&server, &["pane-add", "@0"], 0);
    assert_eq!(pane_lock_sidebar_count(&server, "@0"), 1);
    pane_lock_clear(&server);

    let (mut owner, marker) = pause_pane_add(&server, "@0", "killed-owner");
    owner.0.as_mut().unwrap().kill().unwrap();
    owner.0.as_mut().unwrap().wait().unwrap();
    let shim = std::fs::read_to_string(marker)
        .unwrap()
        .trim()
        .parse::<i32>()
        .unwrap();
    assert_eq!(
        unsafe { libc::kill(shim, 0) },
        0,
        "tmux child must still be alive"
    );
    pane_lock_run(&server, &["pane-add", "plugin:0"], 0);
    assert_eq!(pane_lock_sidebar_count(&server, "@0"), 1);
    drop(owner);
    pane_lock_clear(&server);

    let (owner, marker) = pause_pane_add(&server, "@0", "contended-owner");
    let began = Instant::now();
    let error = pane_lock_run(&server, &["pane-add", "plugin:0"], 1);
    assert!(error.contains("pane lock acquisition timed out"), "{error}");
    assert!((Duration::from_millis(1800)..Duration::from_secs(4)).contains(&began.elapsed()));
    assert_eq!(pane_lock_sidebar_count(&server, "@0"), 0);
    // A held @0 lock must not serialize other windows or another server's @0.
    let other = server.text(&[
        "new-window",
        "-d",
        "-t",
        "plugin",
        "-P",
        "-F",
        "#{window_id}",
        "/bin/bash -c 'exec sleep 60'",
    ]);
    pane_lock_run(&server, &["pane-add", &other], 0);
    let second = TestTmux::new("pane-lock-other");
    second.assert_tmux(&["set-option", "-g", "@agenmux-on", "1"]);
    pane_lock_run(&second, &["pane-add", "@0"], 0);
    assert_eq!(pane_lock_sidebar_count(&server, &other), 1);
    assert_eq!(pane_lock_sidebar_count(&second, "@0"), 1);
    std::fs::write(marker.with_extension("release"), "").unwrap();
    owner.finish(0);
    pane_lock_clear(&server);

    // Startup creates @0 then times out on @1. It must roll back only its
    // own split, restore layout, and clear activation/daemon options.
    let (owner, _) = pause_pane_add(&server, &other, "rollback-owner");
    let layout = server.text(&["display-message", "-p", "-t", "@0", "#{window_layout}"]);
    let error = pane_lock_run(&server, &["toggle", "split"], 1);
    assert!(
        error.contains("pane lock acquisition timed out")
            && error.contains("cannot create startup pane"),
        "{error}"
    );
    assert_eq!(pane_lock_sidebar_count(&server, "@0"), 0);
    assert_eq!(pane_lock_sidebar_count(&server, &other), 0);
    assert_eq!(
        server.text(&["display-message", "-p", "-t", "@0", "#{window_layout}"]),
        layout
    );
    for option in [
        "@agenmux-on",
        "@agenmux-control-client",
        "@agenmux-runtime-dir",
        "@agenmux-layout-@0",
        &format!("@agenmux-layout-{other}"),
    ] {
        assert_eq!(
            server.text(&["show-option", "-gqv", option]),
            "",
            "{option}"
        );
    }
    assert_eq!(
        std::fs::read_dir(server.tmp.join("other-temp"))
            .unwrap()
            .count(),
        0
    );
    drop(owner);

    server.assert_tmux(&["set-option", "-g", "@agenmux-on", "1"]);
    // Exercise the production synchronous layout hooks too.
    pane_lock_run(&server, &["setup"], 0);
    let (owner, marker) = pause_pane_add(&server, "@0", "racing-owner");
    let racers = (0..8)
        .map(|i| {
            PaneLockChild::spawn(pane_lock_command(
                &server,
                &["pane-add", if i % 2 == 0 { "@0" } else { "plugin:0" }],
                &format!("racer-{i}"),
            ))
        })
        .collect::<Vec<_>>();
    std::fs::write(marker.with_extension("release"), "").unwrap();
    owner.finish(0);
    for racer in racers {
        racer.finish(0);
    }
    assert_eq!(pane_lock_sidebar_count(&server, "@0"), 1);
    let error = pane_lock_run(&server, &["pane-add", "@999999999"], 1);
    assert!(error.contains("cannot resolve pane lock identity"));

    let paths = [
        pane_lock_path(&server, "@0"),
        pane_lock_path(&server, &other),
        pane_lock_path(&second, "@0"),
    ];
    drop(second);
    drop(server);
    for path in paths {
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn public_pane_lock_keeps_secure_inode_and_rejects_unsafe_objects() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    let server = TestTmux::new("pane-lock-files");
    server.assert_tmux(&["set-option", "-g", "@agenmux-on", "1"]);
    pane_lock_run(&server, &["pane-add", "@0"], 0);
    let path = pane_lock_path(&server, "@0");
    let inode = path.metadata().unwrap().ino();
    pane_lock_clear(&server);
    pane_lock_run(&server, &["pane-add", "@0"], 0);
    assert_eq!(path.metadata().unwrap().ino(), inode);
    assert_eq!(path.metadata().unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        path.parent().unwrap().metadata().unwrap().mode() & 0o777,
        0o700
    );
    pane_lock_clear(&server);
    let target = server.tmp.join("untouched");
    std::fs::write(&target, "unchanged").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    for kind in ["symlink", "fifo", "directory", "permissions", "hardlink"] {
        std::fs::remove_file(&path).unwrap();
        match kind {
            "symlink" => symlink(&target, &path).unwrap(),
            "fifo" => {
                let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            "directory" => std::fs::create_dir(&path).unwrap(),
            "permissions" => {
                std::fs::write(&path, "").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            "hardlink" => std::fs::hard_link(&target, &path).unwrap(),
            _ => unreachable!(),
        }
        let error = pane_lock_run(&server, &["pane-add", "@0"], 1);
        assert!(error.contains("cannot acquire pane-add lock"), "{error}");
        assert_eq!(pane_lock_sidebar_count(&server, "@0"), 0);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "unchanged");
        if kind == "directory" {
            std::fs::remove_dir(&path).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
        std::fs::write(&path, "").unwrap();
    }
    drop(server);
    std::fs::remove_file(path).unwrap();
}
