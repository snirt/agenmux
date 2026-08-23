use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

static NEXT_SERVER: AtomicUsize = AtomicUsize::new(0);

struct TestTmux {
    socket: String,
    tmp: PathBuf,
}

impl TestTmux {
    fn new(name: &str) -> Self {
        let serial = NEXT_SERVER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "agents-mon-plugin-{name}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let socket = format!("agents-mon-plugin-{name}-{}-{serial}", std::process::id());
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
            "exec sleep 60",
        ]);
        server
    }

    fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .args(["-L", &self.socket])
            .args(args)
            .output()
            .unwrap()
    }

    fn tmux_env(&self) -> String {
        format!(
            "{},0,0",
            self.text(&["display-message", "-p", "#{socket_path}"])
        )
    }

    fn bin(&self, args: &[&str]) -> Output {
        self.bin_command(args).output().unwrap()
    }

    fn bin_command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agents-mon"));
        command
            .args(args)
            .env("TMPDIR", &self.tmp)
            .env("TMUX", self.tmux_env())
            .env("AGENTS_MON_DIR", env!("CARGO_MANIFEST_DIR"));
        command
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
            .to_string()
    }

    fn assert_tmux(&self, args: &[&str]) {
        let output = self.tmux(args);
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn wait_for(&self, timeout: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + timeout;
        while !condition() {
            assert!(Instant::now() < deadline, "condition timed out");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn attach(&self) -> Child {
        let program = format!(
            "log_user 0; set timeout -1; spawn tmux -L {{{}}} attach-session -t plugin; expect eof",
            self.socket
        );
        Command::new("expect")
            .args(["-c", &program])
            .env("TERM", "xterm")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn attach_control(&self) -> Child {
        Command::new("tmux")
            .args(["-L", &self.socket, "-C", "attach-session", "-t", "plugin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}

impl Drop for TestTmux {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn assert_success(output: Output, context: &str) {
    assert!(
        output.status.success(),
        "{context}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn teardown_discards_a_layout_after_window_size_changes() {
    let tmux = TestTmux::new("restore-size");
    let window = tmux.text(&["display-message", "-p", "#{window_id}"]);
    let saved = tmux.text(&["display-message", "-p", "#{window_layout}"]);
    let option = format!("@agents-mon-layout-{window}");
    tmux.assert_tmux(&["set-option", "-g", &option, &saved]);
    tmux.assert_tmux(&["resize-window", "-t", &window, "-x", "100", "-y", "30"]);
    let resized = tmux.text(&["display-message", "-p", "-t", &window, "#{window_layout}"]);
    assert_ne!(saved, resized);

    assert_success(tmux.bin(&["teardown"]), "agents-mon teardown");

    assert_eq!(
        tmux.text(&["display-message", "-p", "-t", &window, "#{window_layout}"]),
        resized
    );
    assert_eq!(tmux.text(&["show-option", "-gqv", &option]), "");
}

#[test]
fn mirror_add_is_idempotent_under_concurrent_calls() {
    let tmux = TestTmux::new("mirror-race");
    let window = tmux.text(&["display-message", "-p", "#{window_id}"]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-on", "1"]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-width", "30"]);
    let original_panes = tmux
        .text(&["list-panes", "-t", &window, "-F", "#{pane_id}"])
        .lines()
        .count();

    let mut children = (0..8)
        .map(|_| tmux.bin_command(&["pane-add", &window]).spawn().unwrap())
        .collect::<Vec<_>>();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }

    let panes = tmux.text(&[
        "list-panes",
        "-t",
        &window,
        "-F",
        "#{pane_title}\t#{pane_pid}\t#{pane_width}",
    ]);
    assert_eq!(panes.lines().count(), original_panes + 1, "{panes}");
    let mirrors = panes
        .lines()
        .filter(|line| line.starts_with("agents-mon\t"))
        .collect::<Vec<_>>();
    assert_eq!(mirrors.len(), 1, "{panes}");
    assert_eq!(mirrors[0], "agents-mon\t0\t30");
}

#[test]
fn wheel_cli_uses_reserved_packets() {
    let tmux = TestTmux::new("wheel-packets");
    let pane = tmux.text(&["display-message", "-p", "#{pane_id}"]);
    let fifo = tmux.tmp.join("agents-mon-keys");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let mut fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fifo)
        .unwrap();

    assert_success(tmux.bin(&["wheel", &pane, "down"]), "wheel down");
    assert_success(tmux.bin(&["wheel", &pane, "up"]), "wheel up");
    let mut packets = [0; 2];
    fifo.read_exact(&mut packets).unwrap();
    assert_eq!(packets, [0x02, 0x01]);
    assert!(!tmux.tmp.join("agents-mon-wheel").exists());
}

#[test]
fn newest_non_control_client_wins() {
    let tmux = TestTmux::new("newest-client");
    let mut first_process = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let first = tmux.text(&["list-clients", "-F", "#{client_name}"]);
    thread::sleep(Duration::from_secs(1));

    let mut second_process = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        tmux.text(&["list-clients", "-F", "#{client_name}"])
            .lines()
            .count()
            == 2
    });
    let clients = tmux
        .text(&["list-clients", "-F", "#{client_name}"])
        .lines()
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    let second = clients.iter().find(|name| *name != &first).unwrap().clone();
    thread::sleep(Duration::from_secs(1));

    let mut control_process = tmux.attach_control();
    tmux.wait_for(Duration::from_secs(2), || {
        tmux.text(&["list-clients", "-F", "#{client_name}"])
            .lines()
            .count()
            == 3
    });
    let newest = tmux
        .text(&[
            "list-clients",
            "-F",
            "#{client_activity}\t#{client_flags}\t#{client_name}",
        ])
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            Some((
                fields.next()?.parse::<u64>().ok()?,
                fields.next()?.to_owned(),
            ))
        })
        .max_by_key(|(activity, _)| *activity)
        .unwrap();
    assert!(
        newest.1.contains("control-mode"),
        "newest flags: {}",
        newest.1
    );

    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agents-mon-bin",
        env!("CARGO_BIN_EXE_agents-mon"),
    ]);
    assert_success(tmux.bin(&["toggle", "split"]), "toggle newest real client");
    tmux.wait_for(Duration::from_secs(3), || {
        tmux.text(&[
            "display-message",
            "-p",
            "-c",
            &second,
            "#{client_key_table}",
        ]) == "agents-mon"
    });
    assert_eq!(
        tmux.text(&["display-message", "-p", "-c", &first, "#{client_key_table}",]),
        "root"
    );

    let _ = first_process.kill();
    let _ = second_process.kill();
    let _ = control_process.kill();
    let _ = first_process.wait();
    let _ = second_process.wait();
    let _ = control_process.wait();
}

#[test]
fn stale_click_origin_is_a_noop() {
    let tmux = TestTmux::new("stale-click");
    let mut viewer_process = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let viewer = tmux.text(&["list-clients", "-F", "#{client_name}"]);
    let client_before = tmux.text(&[
        "display-message",
        "-p",
        "-c",
        &viewer,
        "#{window_id}\t#{pane_id}\t#{client_key_table}",
    ]);
    let selected_before = client_before.split('\t').nth(1).unwrap().to_owned();
    let clicked = tmux.text(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "plugin:",
        "exec sleep 60",
    ]);
    assert_ne!(clicked, selected_before);
    let viewer_window = client_before.split('\t').next().unwrap();
    let target_window = tmux.text(&["display-message", "-p", "-t", &clicked, "#{window_id}"]);
    assert_ne!(target_window, viewer_window);

    let rows = tmux.tmp.join("agents-mon-rows");
    // If the handler guessed the attached viewer after rejecting the stale
    // origin, this valid row would visibly move it to the other window.
    std::fs::write(rows, format!("{clicked}\n")).unwrap();
    assert_success(
        tmux.bin(&["click", &clicked, "1", "vanished-client"]),
        "agents-mon click",
    );

    assert_eq!(
        tmux.text(&[
            "display-message",
            "-p",
            "-c",
            &viewer,
            "#{window_id}\t#{pane_id}\t#{client_key_table}",
        ]),
        client_before
    );

    let _ = viewer_process.kill();
    let _ = viewer_process.wait();
}

#[test]
fn setup_preserves_root_bindings_and_installs_plugin_tables() {
    let tmux = TestTmux::new("setup");
    let bin = env!("CARGO_BIN_EXE_agents-mon");
    tmux.assert_tmux(&[
        "bind-key",
        "-T",
        "root",
        "C-g",
        "display-message",
        "custom-root -T root body",
    ]);
    tmux.assert_tmux(&["set-option", "-g", "mouse", "off"]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-bin", bin]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-key", "A"]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-popup-key", "e"]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-hide-windows", "agents*"]);
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "status-right",
        "#{agents_mon} | %H:%M \t  ",
    ]);

    assert_success(tmux.bin(&["setup"]), "agents-mon setup with mouse off");

    let root_mouse = tmux.text(&["list-keys", "-T", "root"]);
    assert!(!root_mouse.contains(" click '#{pane_id}'"), "{root_mouse}");
    let mut installed_hooks = String::new();
    for (hook, expected) in [
        ("pane-exited", "pane-exited[42]"),
        ("window-pane-changed", "window-pane-changed[42]"),
        ("window-layout-changed", "window-layout-changed[42]"),
        ("window-resized", "window-resized[42]"),
        ("after-select-window", "after-select-window[43]"),
        ("session-window-changed", "session-window-changed[43]"),
        ("client-session-changed", "client-session-changed[43]"),
        ("pane-mode-changed", "pane-mode-changed[44]"),
        ("after-select-pane", "after-select-pane[44]"),
    ] {
        let installed = tmux.text(&["show-hooks", "-g", hook]);
        assert!(
            installed.contains(expected),
            "missing {expected}: {installed}"
        );
        installed_hooks.push_str(&installed);
        installed_hooks.push('\n');
    }
    assert!(!installed_hooks.contains("/scripts/"), "{installed_hooks}");
    for command in ["pane-orphan", "pane-pin", "pane-add"] {
        assert!(installed_hooks.contains(command), "{installed_hooks}");
    }
    let normal = tmux.text(&["list-keys", "-T", "agents-mon"]);
    assert!(
        normal.contains("C-g") && normal.contains("custom-root -T root body"),
        "{normal}"
    );
    assert!(
        !normal.contains("custom-root -T agents-mon body"),
        "{normal}"
    );
    assert!(
        normal.contains("run-shell -b") && normal.contains(" key \'j\'"),
        "{normal}"
    );
    let search_action = normal
        .lines()
        .find(|line| line.contains(" key \'search\'"))
        .unwrap();
    let filter_action = normal
        .lines()
        .find(|line| line.contains(" key \'filter\'"))
        .unwrap();
    assert!(
        search_action.contains("agents-mon-search"),
        "{search_action}"
    );
    assert!(!search_action.contains("run-shell -b"), "{search_action}");
    assert!(!filter_action.contains("run-shell -b"), "{filter_action}");
    assert!(
        normal.contains("WheelUpPane")
            && normal.contains("copy-mode -e; send-keys -M")
            && normal.contains("WheelDownPane")
            && normal.contains(" wheel "),
        "{normal}"
    );
    let search = tmux.text(&["list-keys", "-T", "agents-mon-search"]);
    for code in 32u8..=126 {
        assert!(
            search.contains(&format!("text-{code:02X}")),
            "missing {code:02X}"
        );
    }
    let text_action = search
        .lines()
        .find(|line| line.contains("text-6A"))
        .unwrap();
    assert!(!text_action.contains("run-shell -b"), "{text_action}");
    assert_eq!(
        tmux.text(&["show-option", "-gqv", "@agents-mon-nav-version"]),
        "12"
    );
    let status = tmux.tmux(&["show-option", "-gqv", "status-right"]);
    assert_success(status.clone(), "show status-right");
    assert_eq!(
        String::from_utf8(status.stdout)
            .unwrap()
            .trim_end_matches(['\r', '\n']),
        format!("#({bin} status) | %H:%M \t  ")
    );
    let prefix = tmux.text(&["list-keys", "-T", "prefix"]);
    assert!(
        prefix.contains(" w ") && prefix.contains("agents*"),
        "{prefix}"
    );

    tmux.assert_tmux(&["set-option", "-g", "mouse", "on"]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-hide-windows", ""]);
    assert_success(tmux.bin(&["setup"]), "agents-mon setup with mouse on");
    let root_mouse = tmux.text(&["list-keys", "-T", "root"]);
    assert!(root_mouse.contains(" click '#{pane_id}'"), "{root_mouse}");
    for table in ["agents-mon", "agents-mon-search"] {
        let keys = tmux.text(&["list-keys", "-T", table]);
        assert!(keys.contains("MouseDown1Pane"), "{table}: {keys}");
        assert!(keys.contains(" click '#{pane_id}'"), "{table}: {keys}");
    }
    let prefix = tmux.text(&["list-keys", "-T", "prefix"]);
    let picker = prefix
        .lines()
        .find(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields
                .iter()
                .position(|field| *field == "prefix")
                .and_then(|i| fields.get(i + 1))
                == Some(&"w")
        })
        .unwrap();
    assert!(picker.contains("choose-tree -Zw"), "{picker}");
    assert!(!picker.contains("agents*"), "{picker}");
}

#[test]
fn native_toggle_preserves_split_and_popup_behavior() {
    let tmux = TestTmux::new("native-toggle");
    let bin = env!("CARGO_BIN_EXE_agents-mon");
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-bin", bin]);
    tmux.assert_tmux(&["new-window", "-d", "-n", "other", "exec sleep 60"]);
    let mut viewer_process = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);

    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "first native split toggle",
    );
    tmux.wait_for(Duration::from_secs(3), || {
        !tmux
            .text(&["show-option", "-gqv", "@agents-mon-control-client"])
            .is_empty()
    });
    assert_eq!(tmux.text(&["show-option", "-gqv", "@agents-mon-on"]), "1");
    let sidebars = tmux.text(&[
        "list-panes",
        "-a",
        "-F",
        "#{window_id}\t#{pane_title}\t#{pane_pid}",
    ]);
    let windows = tmux.text(&["list-windows", "-a", "-F", "#{window_id}"]);
    for window in windows.lines() {
        assert_eq!(
            sidebars
                .lines()
                .filter(|line| *line == format!("{window}\tagents-mon\t0"))
                .count(),
            1,
            "{sidebars}"
        );
    }
    let selected = tmux.text(&[
        "display-message",
        "-p",
        "-c",
        &client,
        "#{pane_title}\t#{client_key_table}",
    ]);
    assert_eq!(selected, "agents-mon\tagents-mon");

    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "repeated native split toggle",
    );
    let repeated = tmux.text(&["list-panes", "-a", "-F", "#{pane_title}"]);
    assert_eq!(
        repeated
            .lines()
            .filter(|title| *title == "agents-mon")
            .count(),
        windows.lines().count()
    );

    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agents-mon-control-client",
        "stale-control-client",
    ]);
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "stale native split toggle",
    );
    tmux.wait_for(Duration::from_secs(3), || {
        let control = tmux.text(&["show-option", "-gqv", "@agents-mon-control-client"]);
        !control.is_empty() && control != "stale-control-client"
    });

    let pin = tmux.tmp.join("agents-mon-pin");
    std::fs::write(&pin, "").unwrap();
    assert_success(
        tmux.bin(&["toggle", "popup", &client]),
        "existing popup pin closes",
    );
    assert!(!pin.exists());

    let _ = viewer_process.kill();
    let _ = viewer_process.wait();
}

#[test]
fn binary_helper_uses_private_server() {
    let tmux = TestTmux::new("binary-helper");
    assert_success(tmux.bin(&["status"]), "agents-mon status");
}
