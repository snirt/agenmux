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
            "agenmux-plugin-{name}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let socket = format!("agenmux-plugin-{name}-{}-{serial}", std::process::id());
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_agenmux"));
        command
            .args(args)
            .env("TMPDIR", &self.tmp)
            .env("XDG_CONFIG_HOME", self.tmp.join("config"))
            .env("TMUX", self.tmux_env())
            .env("AGENMUX_DIR", env!("CARGO_MANIFEST_DIR"));
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

    fn binding(&self, table: &str, key: &str) -> String {
        self.text(&[
            "list-keys",
            "-T",
            table,
            "-F",
            "#{key_string}\t#{key_command}",
        ])
        .lines()
        .find_map(|line| {
            line.split_once('\t')
                .filter(|(k, _)| *k == key)
                .map(|(_, command)| command.to_owned())
        })
        .unwrap_or_default()
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
        // kill-server leaves the socket file behind, so ask the live server
        // where it is before taking it down. Thousands of dead sockets
        // otherwise pile up in the shared tmux socket directory.
        let socket_path = self.tmux(&["display-message", "-p", "#{socket_path}"]);
        let _ = self.tmux(&["kill-server"]);
        if socket_path.status.success() {
            let path = String::from_utf8_lossy(&socket_path.stdout)
                .trim_end()
                .to_string();
            if !path.is_empty() {
                let _ = std::fs::remove_file(path);
            }
        }
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
    let option = format!("@agenmux-layout-{window}");
    tmux.assert_tmux(&["set-option", "-g", &option, &saved]);
    tmux.assert_tmux(&["resize-window", "-t", &window, "-x", "100", "-y", "30"]);
    let resized = tmux.text(&["display-message", "-p", "-t", &window, "#{window_layout}"]);
    assert_ne!(saved, resized);

    assert_success(tmux.bin(&["teardown"]), "agenmux teardown");

    assert_eq!(
        tmux.text(&["display-message", "-p", "-t", &window, "#{window_layout}"]),
        resized
    );
    assert_eq!(tmux.text(&["show-option", "-gqv", &option]), "");
}

#[test]
fn teardown_removes_legacy_sidebar_panes() {
    let tmux = TestTmux::new("legacy-teardown");
    let pane = tmux.text(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "exec sleep 60",
    ]);
    tmux.assert_tmux(&["select-pane", "-t", &pane, "-T", "agents-mon"]);

    assert_success(tmux.bin(&["teardown"]), "legacy agenmux teardown");

    let panes = tmux.text(&["list-panes", "-a", "-F", "#{pane_id}"]);
    assert!(!panes.lines().any(|candidate| candidate == pane));
}

#[test]
fn pane_add_kills_restored_ghost_shell() {
    let tmux = TestTmux::new("restored-ghost");
    let window = tmux.text(&["display-message", "-p", "#{window_id}"]);
    let original_panes = tmux
        .text(&["list-panes", "-t", &window, "-F", "#{pane_id}"])
        .lines()
        .count();
    let ghost = tmux.text(&[
        "split-window",
        "-hbf",
        "-l",
        "30",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "exec sh",
    ]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-on", "1"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-width", "30"]);

    assert_success(tmux.bin(&["pane-add", &window]), "pane-add restored ghost");

    let panes = tmux.text(&[
        "list-panes",
        "-t",
        &window,
        "-F",
        "#{pane_id}\t#{pane_title}\t#{@agenmux}",
    ]);
    assert!(
        !panes.lines().any(|line| line.starts_with(&ghost)),
        "{panes}"
    );
    assert_eq!(panes.lines().count(), original_panes + 1, "{panes}");
    assert_eq!(
        panes
            .lines()
            .filter(|line| line.ends_with("\tagenmux\t1"))
            .count(),
        1,
        "{panes}"
    );
}

#[test]
fn pane_add_keeps_leftmost_non_shell_pane() {
    let tmux = TestTmux::new("leftmost-non-shell");
    let window = tmux.text(&["display-message", "-p", "#{window_id}"]);
    let pane = tmux.text(&[
        "split-window",
        "-hbf",
        "-l",
        "30",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "exec sleep 60",
    ]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-on", "1"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-width", "30"]);

    assert_success(tmux.bin(&["pane-add", &window]), "pane-add non-shell");

    let panes = tmux.text(&[
        "list-panes",
        "-t",
        &window,
        "-F",
        "#{pane_id}\t#{pane_title}\t#{@agenmux}",
    ]);
    assert!(panes.lines().any(|line| line.starts_with(&pane)), "{panes}");
    assert_eq!(
        panes
            .lines()
            .filter(|line| line.ends_with("\tagenmux\t1"))
            .count(),
        1,
        "{panes}"
    );
}

#[test]
fn teardown_finds_sidebar_by_pane_option() {
    let tmux = TestTmux::new("option-teardown");
    let window = tmux.text(&["display-message", "-p", "#{window_id}"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-on", "1"]);
    assert_success(tmux.bin(&["pane-add", &window]), "pane-add tagged sidebar");
    let pane = tmux.text(&[
        "list-panes",
        "-t",
        &window,
        "-f",
        "#{==:#{@agenmux},1}",
        "-F",
        "#{pane_id}",
    ]);
    tmux.assert_tmux(&["select-pane", "-t", &pane, "-T", "retitled"]);

    assert_success(tmux.bin(&["teardown"]), "teardown tagged sidebar");

    let panes = tmux.text(&["list-panes", "-t", &window, "-F", "#{pane_id}"]);
    assert!(!panes.lines().any(|candidate| candidate == pane));
}

#[test]
fn mirror_add_is_idempotent_under_concurrent_calls() {
    let tmux = TestTmux::new("mirror-race");
    let window = tmux.text(&["display-message", "-p", "#{window_id}"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-on", "1"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-width", "30"]);
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
        .filter(|line| line.starts_with("agenmux\t"))
        .collect::<Vec<_>>();
    assert_eq!(mirrors.len(), 1, "{panes}");
    assert_eq!(mirrors[0], "agenmux\t0\t30");
}

#[test]
fn wheel_cli_uses_reserved_packets() {
    let tmux = TestTmux::new("wheel-packets");
    let pane = tmux.text(&["display-message", "-p", "#{pane_id}"]);
    let fifo = tmux.tmp.join("agenmux-keys");
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
    assert!(!tmux.tmp.join("agenmux-wheel").exists());
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
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    assert_success(tmux.bin(&["toggle", "split"]), "toggle newest real client");
    tmux.wait_for(Duration::from_secs(3), || {
        tmux.text(&[
            "display-message",
            "-p",
            "-c",
            &second,
            "#{client_key_table}",
        ]) == "agenmux"
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

    let rows = tmux.tmp.join("agenmux-rows");
    // If the handler guessed the attached viewer after rejecting the stale
    // origin, this valid row would visibly move it to the other window.
    std::fs::write(rows, format!("{clicked}\n")).unwrap();
    assert_success(
        tmux.bin(&["click", &clicked, "1", "vanished-client"]),
        "agenmux click",
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
    let bin = env!("CARGO_BIN_EXE_agenmux");
    tmux.assert_tmux(&[
        "bind-key",
        "-T",
        "root",
        "C-g",
        "display-message",
        "custom-root -T root body",
    ]);
    tmux.assert_tmux(&["set-option", "-g", "mouse", "off"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-bin", bin]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-key", "A"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-popup-key", "e"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-hide-windows", "agents*"]);
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "status-right",
        "#{agenmux} | %H:%M \t  ",
    ]);

    assert_success(tmux.bin(&["setup"]), "agenmux setup with mouse off");

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
    let normal = tmux.text(&["list-keys", "-T", "agenmux"]);
    assert!(
        normal.contains("C-g") && normal.contains("custom-root -T root body"),
        "{normal}"
    );
    assert!(!normal.contains("custom-root -T agenmux body"), "{normal}");
    assert!(
        normal.contains("run-shell -b") && normal.contains("agenmux j ") && normal.contains(" key \'down\'"),
        "{normal}"
    );
    assert!(normal.contains(" key \'sequence-67\'"), "{normal}");
    assert!(normal.contains(" key \'last\'"), "{normal}");
    let search_action = normal
        .lines()
        .find(|line| line.contains(" key \'search\'"))
        .unwrap();
    let filter_action = normal
        .lines()
        .find(|line| line.contains(" key \'filter\'"))
        .unwrap();
    assert!(search_action.contains("agenmux-search"), "{search_action}");
    assert!(!search_action.contains("run-shell -b"), "{search_action}");
    assert!(!filter_action.contains("run-shell -b"), "{filter_action}");
    assert!(
        normal.contains("WheelUpPane")
            && normal.contains("copy-mode -e; send-keys -M")
            && normal.contains("WheelDownPane")
            && normal.contains(" wheel "),
        "{normal}"
    );
    let search = tmux.text(&["list-keys", "-T", "agenmux-search"]);
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
    let nav_version = tmux.text(&["show-option", "-gqv", "@agenmux-nav-version"]);
    assert!(
        nav_version.starts_with("14.") && nav_version.len() == 19,
        "{nav_version}"
    );
    let status = tmux.tmux(&["show-option", "-gqv", "status-right"]);
    assert_success(status.clone(), "show status-right");
    assert_eq!(
        String::from_utf8(status.stdout)
            .unwrap()
            .trim_end_matches(['\r', '\n']),
        "#(AGENMUX_DIR=#{q:@agenmux-plugin-dir} #{q:@agenmux-runtime-bin} status) | %H:%M \t  "
    );
    let prefix = tmux.text(&["list-keys", "-T", "prefix"]);
    assert!(
        prefix.contains(" w ") && prefix.contains("agents*"),
        "{prefix}"
    );

    tmux.assert_tmux(&["set-option", "-g", "mouse", "on"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-hide-windows", ""]);
    assert_success(tmux.bin(&["setup"]), "agenmux setup with mouse on");
    let root_mouse = tmux.text(&["list-keys", "-T", "root"]);
    assert!(root_mouse.contains(" click '#{pane_id}'"), "{root_mouse}");
    for table in ["agenmux", "agenmux-search"] {
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
fn setup_resolves_legacy_options_without_copying_behavior() {
    let tmux = TestTmux::new("legacy-options");
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-width", "41"]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-notifications", "off"]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-key", "L"]);
    tmux.assert_tmux(&["unbind-key", "L"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-width", "50"]);
    tmux.assert_tmux(&["set-option", "-g", "status-right", "#{agents_mon}"]);

    assert_success(tmux.bin(&["setup"]), "legacy option migration");

    assert_eq!(tmux.text(&["show-option", "-gqv", "@agenmux-width"]), "50");
    assert_eq!(
        tmux.text(&["show-option", "-gqv", "@agenmux-notifications"]),
        ""
    );
    assert_eq!(tmux.text(&["show-option", "-gqv", "@agenmux-key"]), "");
    assert!(tmux.binding("prefix", "L").is_empty());
    let effective = tmux.bin(&["config", "check", "--effective"]);
    assert!(effective.status.success());
    let effective = String::from_utf8(effective.stdout).unwrap();
    // The winning option is named, so the fix for a surprise is obvious.
    // Columns are padded to the widest cell, so match per row, not on spacing.
    let row = |name: &str| {
        effective
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap_or_else(|| panic!("no {name} row in:\n{effective}"))
            .split_whitespace()
            .skip(1)
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(row("display.sidebar_width"), "50 tmux @agenmux-width");
    assert_eq!(
        row("behavior.notifications"),
        "false tmux @agents-mon-notifications"
    );
    assert!(tmux
        .text(&["show-option", "-gqv", "status-right"])
        .contains("#{q:@agenmux-runtime-bin} status"));
}

#[test]
fn native_toggle_preserves_split_and_popup_behavior() {
    let tmux = TestTmux::new("native-toggle");
    let bin = env!("CARGO_BIN_EXE_agenmux");
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-bin", bin]);
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
            .text(&["show-option", "-gqv", "@agenmux-control-client"])
            .is_empty()
    });
    assert_eq!(tmux.text(&["show-option", "-gqv", "@agenmux-on"]), "1");
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
                .filter(|line| *line == format!("{window}\tagenmux\t0"))
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
    assert_eq!(selected, "agenmux\tagenmux");

    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "repeated native split toggle",
    );
    let repeated = tmux.text(&["list-panes", "-a", "-F", "#{pane_title}"]);
    assert_eq!(
        repeated.lines().filter(|title| *title == "agenmux").count(),
        windows.lines().count()
    );

    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-control-client",
        "stale-control-client",
    ]);
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "stale native split toggle",
    );
    tmux.wait_for(Duration::from_secs(3), || {
        let control = tmux.text(&["show-option", "-gqv", "@agenmux-control-client"]);
        !control.is_empty() && control != "stale-control-client"
    });

    let pin = tmux.tmp.join("agenmux-pin");
    std::fs::write(&pin, "").unwrap();
    assert_success(
        tmux.bin(&["toggle", "popup", &client]),
        "existing popup pin closes",
    );
    assert!(!pin.exists());

    let _ = viewer_process.kill();
    let _ = viewer_process.wait();
}

fn app_file(tmux: &TestTmux, text: &str) {
    let path = tmux.tmp.join("config/agenmux/config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn setup_and_toggle_preserve_manual_launchers_and_old_metadata() {
    let tmux = TestTmux::new("manual-launchers");
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    for key in ["A", "e", "L", "w"] {
        tmux.assert_tmux(&["bind-key", key, "display-message", "manual binding"]);
    }
    // Even an exact old bootstrap form is no longer migrated or cleaned up.
    let old = format!(
        "bash '{}/agenmux.tmux' activate '' '#{{client_name}}'",
        env!("CARGO_MANIFEST_DIR")
    );
    tmux.assert_tmux(&["bind-key", "L", "run-shell", "-b", &old]);
    let before = tmux.text(&["list-keys", "-T", "prefix"]);
    // Launcher preferences are not application settings, even malformed ones.
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-key", "not a key"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-popup-key", "A"]);
    for metadata in [None, Some("old registry: leave untouched")] {
        if let Some(value) = metadata {
            tmux.assert_tmux(&["set-option", "-g", "@agenmux-prefix-owned", value]);
        }
        for args in [&["setup"][..], &["toggle", "split"], &["teardown"]] {
            assert_success(
                tmux.bin(args),
                &format!("manual launcher preservation {args:?}"),
            );
            assert_eq!(tmux.text(&["list-keys", "-T", "prefix"]), before);
            assert_eq!(
                tmux.text(&["show-option", "-gqv", "@agenmux-prefix-owned"]),
                metadata.unwrap_or("")
            );
            if metadata.is_none() {
                assert_eq!(tmux.text(&["show-options", "-gq", "@agenmux-prefix-owned"]), "");
            }
        }
    }
    app_file(&tmux, "[keys.prefix]\ntoggle=['A']");
    for args in [&["config", "check"][..], &["setup"], &["toggle", "split"]] {
        assert_eq!(tmux.bin(args).status.code(), Some(2));
        assert_eq!(tmux.text(&["list-keys", "-T", "prefix"]), before);
    }
}

#[test]
fn invalid_layers_do_not_mutate_and_recovery_remains_available() {
    let tmux = TestTmux::new("invalid-settings");
    let before = tmux.text(&["list-keys"]);
    let layout = tmux.text(&["display-message", "-p", "#{window_layout}"]);
    app_file(&tmux, "[display]\nsidebar_width=0");
    for args in [
        &["setup"][..],
        &["toggle", "split"],
        &["toggle", "popup"],
        &["pane-add"],
        &["pane-pin"],
        &["sidebar"],
        &["daemon"],
    ] {
        let result = tmux.bin(args);
        assert_eq!(result.status.code(), Some(2), "{args:?}");
        assert_eq!(tmux.text(&["list-keys"]), before);
        assert_eq!(
            tmux.text(&["display-message", "-p", "#{window_layout}"]),
            layout
        );
        assert!(!tmux.tmp.join("agenmux-pin").exists());
        assert!(!tmux.tmp.join("agenmux-keys").exists());
    }
    std::fs::write(tmux.tmp.join("agenmux-pin"), "").unwrap();
    assert_success(tmux.bin(&["toggle", "popup"]), "broken file popup close");
    assert_success(tmux.bin(&["teardown"]), "broken file teardown");
    // No listener means transport failure (1), not config rejection (2).
    assert_eq!(tmux.bin(&["key", "close"]).status.code(), Some(1));
    app_file(&tmux, "");
    assert_eq!(tmux.bin(&["toggle", "nonsense"]).status.code(), Some(2));
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-width", "bad"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-width", "44"]);
    assert_eq!(tmux.bin(&["setup"]).status.code(), Some(2));
}

// Exercise bootstrap independently of app setup, with real tmux argv/format
// parsing and actual key delivery. All executable fixtures are synthetic.
#[test]
fn bootstrap_launchers_use_tmux_options_and_verified_literal_activation() {
    use std::os::unix::fs::PermissionsExt;
    let tmux = TestTmux::new("bootstrap-launchers");
    let plugin = tmux
        .tmp
        .join("plugin '\"$; #{pane_id} #(touch INJECTED) (literal)");
    std::fs::create_dir_all(plugin.join("scripts")).unwrap();
    for file in ["agenmux.tmux", "scripts/version.sh", "Cargo.toml"] {
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(file),
            plugin.join(file),
        )
        .unwrap();
    }
    let engine = plugin.join("engine '\"$; # (literal)");
    let script = format!(
        r#"#!/usr/bin/env bash
base="$(dirname "$0")"
case "$1" in
--version) printf 'agenmux {}\n' ;;
setup) [ ! -e "$base/fail-setup" ] || exit 2 ;;
toggle) printf '%s\0' "$@" "$AGENMUX_DIR" >"$base/activation" ;;
*) exit 64 ;;
esac
"#,
        env!("CARGO_PKG_VERSION")
    );
    std::fs::write(&engine, script).unwrap();
    std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o755)).unwrap();
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-bin", engine.to_str().unwrap()]);
    let bootstrap = || {
        Command::new("bash")
            .arg(plugin.join("agenmux.tmux"))
            .env("TMUX", tmux.tmux_env())
            .env("TMPDIR", &tmux.tmp)
            .env("XDG_CONFIG_HOME", tmux.tmp.join("config"))
            .env("AGENMUX_INSTALL_REFRESH", "1")
            .output()
            .unwrap()
    };
    let clear = || {
        for key in ["A", "e", "L", "P"] {
            tmux.assert_tmux(&["unbind-key", key]);
        }
    };
    clear();
    app_file(&tmux, "[keys.prefix]\ntoggle=['invalid']");
    assert_success(bootstrap(), "launchers do not parse TOML");
    assert!(tmux.binding("prefix", "A").contains("activate ''"));
    assert!(tmux.binding("prefix", "e").contains("activate 'popup'"));
    assert_eq!(
        tmux.text(&["show-options", "-gq", "@agenmux-prefix-owned"]),
        ""
    );
    clear();
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-key", "L"]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-popup-key", "P"]);
    assert_success(bootstrap(), "legacy launchers");
    assert!(tmux.binding("prefix", "L").contains("activate ''"));
    assert!(tmux.binding("prefix", "P").contains("activate 'popup'"));
    clear();
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-key", ""]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-popup-key", ""]);
    let before = tmux.text(&["list-keys", "-T", "prefix"]);
    assert_success(bootstrap(), "legacy empty disables defaults");
    assert_eq!(tmux.text(&["list-keys", "-T", "prefix"]), before);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-key", "L"]);
    tmux.assert_tmux(&["set-option", "-g", "@agents-mon-popup-key", "P"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-key", ""]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-popup-key", ""]);
    tmux.assert_tmux(&["bind-key", "A", "display-message", "manual"]);
    let before = tmux.text(&["list-keys", "-T", "prefix"]);
    assert_success(
        bootstrap(),
        "canonical empty disables both, preserves manual",
    );
    assert_eq!(tmux.text(&["list-keys", "-T", "prefix"]), before);
    for key in [
        ";", "'", "\"", "\\", "$", "#", "(", ")", "-", "~", "Space", "C-@", "PageUp", "PageDown",
    ] {
        tmux.assert_tmux(&[
            "set-option",
            "-g",
            "@agenmux-popup-key",
            if key == ";" { "\\;" } else { key },
        ]);
        assert_success(bootstrap(), "native launcher key syntax");
        let native = match key {
            "PageUp" => "PPage",
            "PageDown" => "NPage",
            _ => key,
        };
        let binding = tmux.binding("prefix", native);
        assert!(binding.contains("activate 'popup'"), "{key}: {binding}");
        tmux.assert_tmux(&["set-option", "-g", "@agenmux-popup-key", ""]);
        assert_success(bootstrap(), "disabling never unbinds an existing launcher");
        assert_eq!(tmux.binding("prefix", native), binding);
    }
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-key", "A"]);
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-popup-key", "A"]);
    assert_success(
        bootstrap(),
        "native last writer wins, even between launchers",
    );
    assert!(tmux.binding("prefix", "A").contains("activate 'popup'"));
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-popup-key", "e"]);
    std::fs::write(plugin.join("fail-setup"), "").unwrap();
    assert_eq!(bootstrap().status.code(), Some(2));
    assert!(tmux.binding("prefix", "A").contains("activate ''"));
    assert!(tmux.binding("prefix", "e").contains("activate 'popup'"));
    std::fs::remove_file(plugin.join("fail-setup")).unwrap();
    let mut viewer = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);
    let fire = |key: &str| {
        tmux.assert_tmux(&["switch-client", "-c", &client, "-T", "prefix"]);
        tmux.assert_tmux(&["send-keys", "-K", "-c", &client, key]);
    };
    for (key, mode) in [("A", ""), ("e", "popup")] {
        fire(key);
        let expected = format!("toggle\0{mode}\0{client}\0{}\0", plugin.display());
        tmux.wait_for(Duration::from_secs(3), || {
            std::fs::read(plugin.join("activation")).ok().as_deref() == Some(expected.as_bytes())
        });
        std::fs::remove_file(plugin.join("activation")).unwrap();
    }
    // Real client names above are PTY paths. Also exercise the same deferred
    // q format boundary with a synthetic adversarial client value.
    let hostile_client = "client '\"$; #{pane_id} #(touch INJECTED) (literal)";
    tmux.assert_tmux(&["set-option", "-g", "@test-client", hostile_client]);
    let command = tmux
        .binding("prefix", "e")
        .replace("#{q:client_name}", "#{q:@test-client}");
    let mut source = Command::new("tmux")
        .args(["-L", &tmux.socket, "source-file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    source
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{command}\n").as_bytes())
        .unwrap();
    assert_success(
        source.wait_with_output().unwrap(),
        "adversarial client format",
    );
    let expected = format!("toggle\0popup\0{hostile_client}\0{}\0", plugin.display());
    tmux.wait_for(Duration::from_secs(3), || {
        std::fs::read(plugin.join("activation")).ok().as_deref() == Some(expected.as_bytes())
    });
    std::fs::remove_file(plugin.join("activation")).unwrap();

    // Replacing a custom binary with an incompatible one must not execute it.
    std::fs::write(&engine, "#!/bin/sh\nbase=\"$(dirname \"$0\")\"\nif [ \"$1\" = --version ]; then touch \"$base/rejected-version\"; else touch \"$base/activation\"; fi\nexit 64\n").unwrap();
    fire("A");
    tmux.wait_for(Duration::from_secs(3), || {
        plugin.join("rejected-version").exists()
    });
    thread::sleep(Duration::from_millis(200));
    assert!(!plugin.join("activation").exists());

    // Default-engine installation failure is also reached through the actual
    // launcher, with the real private-server install lock (no network access).
    tmux.assert_tmux(&["set-option", "-gu", "@agenmux-bin"]);
    std::fs::write(
        plugin.join("scripts/install-bin.sh"),
        "#!/bin/sh\ntouch \"$(dirname \"$0\")/../install-attempted\"\nexit 1\n",
    )
    .unwrap();
    fire("e");
    tmux.wait_for(Duration::from_secs(3), || {
        plugin.join("install-attempted").exists()
    });
    // Taking the same lock waits for the failed activation to release it.
    tmux.assert_tmux(&["wait-for", "-L", "agenmux-install"]);
    tmux.assert_tmux(&["wait-for", "-U", "agenmux-install"]);
    assert!(!plugin.join("activation").exists());

    // The freshly built real engine rejects TOML at activation before runtime
    // mutation, but its setup failure cannot prevent launcher installation.
    std::fs::copy(env!("CARGO_BIN_EXE_agenmux"), &engine).unwrap();
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-bin", engine.to_str().unwrap()]);
    assert_eq!(bootstrap().status.code(), Some(2));
    assert!(tmux.binding("prefix", "A").contains("activate ''"));
    assert!(tmux.binding("prefix", "e").contains("activate 'popup'"));
    let before = tmux.text(&["list-keys"]);
    let hooks = tmux.text(&["show-hooks", "-g"]);
    let layout = tmux.text(&["display-message", "-p", "#{window_layout}"]);
    let rejected = Command::new("bash")
        .arg(plugin.join("agenmux.tmux"))
        .args(["activate", "split", "client '\"$; #{pane_id} (literal)"])
        .env("TMUX", tmux.tmux_env())
        .env("TMPDIR", &tmux.tmp)
        .env("XDG_CONFIG_HOME", tmux.tmp.join("config"))
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(tmux.text(&["list-keys"]), before);
    assert_eq!(tmux.text(&["show-hooks", "-g"]), hooks);
    assert_eq!(
        tmux.text(&["display-message", "-p", "#{window_layout}"]),
        layout
    );
    assert_eq!(tmux.text(&["show-options", "-gq", "@agenmux-on"]), "");
    assert!(!tmux.tmp.join("agenmux-pin").exists());
    assert!(!plugin.join("INJECTED").exists());
    assert!(!PathBuf::from("INJECTED").exists());
    assert_eq!(
        tmux.text(&["show-options", "-gq", "@agenmux-prefix-owned"]),
        ""
    );
    let _ = viewer.kill();
    let _ = viewer.wait();
}

#[test]
fn runtime_binary_path_uses_tmux_shell_argument_quoting() {
    let tmux = TestTmux::new("literal-engine");
    let executable = tmux.tmp.join("engine '\"$; #{pane_id} (literal)");
    std::fs::copy(env!("CARGO_BIN_EXE_agenmux"), &executable).unwrap();
    let result = Command::new(&executable)
        .arg("setup")
        .env("TMUX", tmux.tmux_env())
        .env("TMPDIR", &tmux.tmp)
        .env("XDG_CONFIG_HOME", tmux.tmp.join("config"))
        .env("AGENMUX_DIR", env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert_success(result, "setup from literal engine path");
    assert_eq!(
        tmux.text(&["show-option", "-gqv", "@agenmux-runtime-bin"]),
        executable.to_string_lossy()
    );
    assert_eq!(
        tmux.text(&["show-option", "-gqv", "@agenmux-plugin-dir"]),
        env!("CARGO_MANIFEST_DIR")
    );
    // Exercise the same nested command text as a binding without spawning a
    // daemon. q expansion happens after parsing, so shell/path text stays data.
    let binding = tmux.binding("agenmux", "k");
    let command = binding
        .split(" \\; ")
        .next()
        .unwrap()
        .split(" ; ")
        .next()
        .unwrap()
        .replace("key 'k'", "--version");
    let mut process = Command::new("tmux")
        .args(["-L", &tmux.socket, "source-file", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    process
        .stdin
        .take()
        .unwrap()
        .write_all(format!("{command}\n").as_bytes())
        .unwrap();
    let output = process.wait_with_output().unwrap();
    assert_success(output, "execute literal nested path");
    // Synchronous execution additionally checks the actual executable output.
    assert_eq!(
        tmux.text(&["run-shell", "#{q:@agenmux-runtime-bin} --version"]),
        format!("agenmux {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn daemon_theme_is_a_startup_snapshot_without_global_color_mutation() {
    for base in ["light", "terminal"] {
        let tmux = TestTmux::new(&format!("theme-{base}"));
        app_file(&tmux, &format!("[theme]\nbase='{base}'\n[theme.colors]\nheader_fg='#123456'\nmuted_fg=99\n[behavior]\nnotifications=false"));
        tmux.assert_tmux(&[
            "set-option",
            "-g",
            "@agenmux-bin",
            env!("CARGO_BIN_EXE_agenmux"),
        ]);
        let style = tmux.text(&["show-options", "-g", "window-style"]);
        assert_success(tmux.bin(&["toggle", "split"]), "start themed daemon");
        let pane = tmux.text(&[
            "list-panes",
            "-f",
            "#{==:#{pane_title},agenmux}",
            "-F",
            "#{pane_id}",
        ]);
        let capture = || tmux.text(&["capture-pane", "-p", "-e", "-t", &pane]);
        tmux.wait_for(Duration::from_secs(4), || {
            let frame = capture();
            frame.contains("38;2;18;52;86")
                && frame.contains("38;5;99")
                && frame.contains("no agents")
        });
        let before = capture();
        app_file(&tmux, "[theme.colors]\nheader_fg='#abcdef'\nmuted_fg=100");
        assert_success(tmux.bin(&["key", "search"]), "force themed redraw");
        tmux.wait_for(Duration::from_secs(4), || capture().contains("clear"));
        let after = capture();
        assert!(after.contains("38;2;18;52;86"), "startup header: {after:?}");
        assert!(after.contains("38;5;99"), "startup hints: {after:?}");
        assert!(!after.contains("38;2;171;205;239"));
        assert_eq!(tmux.text(&["show-options", "-g", "window-style"]), style);
        assert!(before.contains("agenmux"));
        assert_success(tmux.bin(&["key", "close"]), "close themed daemon");
    }
}

#[test]
fn daemon_live_width_keeps_startup_file_and_last_valid_overrides() {
    let tmux = TestTmux::new("live-config");
    app_file(
        &tmux,
        "[display]\nsidebar_width=26\n[behavior]\nnotifications=false",
    );
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    assert_success(tmux.bin(&["toggle", "split"]), "start configured daemon");
    let pane = tmux.text(&[
        "list-panes",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}",
    ]);
    let width = || tmux.text(&["display-message", "-p", "-t", &pane, "#{pane_width}"]);
    tmux.wait_for(Duration::from_secs(4), || {
        width() == "26"
            && !tmux
                .text(&["show-option", "-gqv", "@agenmux-control-client"])
                .is_empty()
    });
    // File edits do not reload in an existing process, even invalid ones.
    app_file(&tmux, "invalid");
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-width", "38"]);
    tmux.wait_for(Duration::from_secs(4), || width() == "38");
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-width", "invalid"]);
    thread::sleep(Duration::from_millis(800));
    assert_eq!(width(), "38");
    tmux.assert_tmux(&["set-option", "-gu", "@agenmux-width"]);
    tmux.wait_for(Duration::from_secs(4), || width() == "26");
    thread::sleep(Duration::from_millis(500));
    tmux.assert_tmux(&["resize-pane", "-t", &pane, "-x", "35"]);
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&["show-option", "-gqv", "@agenmux-width"]) == "35"
    });
    assert_success(tmux.bin(&["key", "close"]), "broken file daemon close");
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&[
            "list-panes",
            "-f",
            "#{==:#{pane_title},agenmux}",
            "-F",
            "#{pane_id}",
        ])
        .is_empty()
    });
}

#[test]
fn setup_restores_touched_bindings_and_reports_rollback_failure() {
    use std::os::unix::fs::PermissionsExt;
    for (activation, fail_rollback) in [(false, false), (false, true), (true, false)] {
        let tmux = TestTmux::new("rollback");
        assert_success(tmux.bin(&["setup"]), "initial setup");
        tmux.assert_tmux(&[
            "bind-key",
            "-n",
            "-N",
            "synthetic é  note '$\\\"\nsecond line",
            "MouseDown1Pane",
            "display-message",
            "literal note",
        ]);
        tmux.assert_tmux(&["set-option", "-gu", "@agenmux-runtime-bin"]);
        tmux.assert_tmux(&["set-option", "-g", "@agenmux-plugin-dir", ""]);
        tmux.assert_tmux(&["set-option", "-gu", "@agenmux-bin"]);
        tmux.assert_tmux(&["set-option", "-g", "@agents-mon-bin", "/synthetic/engine"]);
        tmux.assert_tmux(&["set-option", "-g", "status-left", "#{agenmux} literal\n"]);
        tmux.assert_tmux(&["set-option", "-g", "status-right", "#{agents_mon}"]);
        tmux.assert_tmux(&["set-option", "-w", "@agenmux-sidebar", ""]);
        tmux.assert_tmux(&["set-option", "-w", "@agents-mon-sidebar", "synthetic"]);
        tmux.assert_tmux(&["set-hook", "-g", "after-select-window[42]", "display-message 'synthetic' ; display-message 'second'"]);
        tmux.assert_tmux(&["set-hook", "-g", "after-select-window[99]", "display-message untouched"]);
        if activation {
            tmux.assert_tmux(&["set-option", "-g", "@agenmux-bin", env!("CARGO_BIN_EXE_agenmux")]);
        }
        let panes_before = tmux.text(&["list-panes", "-F", "#{pane_id}"]);
        let layout_before = tmux.text(&["display-message", "-p", "#{window_layout}"]);
        let options_before = tmux.text(&["show-options", "-g"]);
        let windows_before = tmux.text(&["show-options", "-w"]);
        let hooks_before = tmux.text(&["show-hooks", "-g"]);
        let window_hooks_before = tmux.text(&["show-hooks", "-gw"]);
        let before = tmux.text(&["list-keys"]);
        let notes_before = tmux.text(&[
            "list-keys",
            "-F",
            "#{key_table}:#{key_string}:#{key_note}:#{key_repeat}",
        ]);
        assert_eq!(tmux.text(&["show-options", "-gq", "@agenmux-prefix-owned"]), "");
        app_file(
            &tmux,
            "[behavior]\nhide_windows='hidden*'",
        );
        let stubs = tmux.tmp.join("stubs");
        std::fs::create_dir_all(&stubs).unwrap();
        let real = Command::new("which").arg("tmux").output().unwrap();
        let real = String::from_utf8(real.stdout).unwrap();
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\"'\"'"));
        let marker = tmux.tmp.join("failed");
        let script = format!("#!/bin/sh\nif [ ! -e {marker} ] && [ \"$1 $2 $3\" = 'set-option -g @agenmux-nav-version' ]; then touch {marker}; echo 'injected setup failure' >&2; exit 1; fi\n{}\nexec {} \"$@\"\n",
            if fail_rollback { format!("if [ -e {} ]; then case \"$1 $2 $3\" in 'source-file - '*|'set-option -gu @agenmux-runtime-bin'|'set-option -g status-left'|'set-hook -g after-select-window[42]') echo 'injected rollback failure' >&2; exit 1;; esac; fi", quote(&marker.to_string_lossy())) } else { String::new() }, quote(real.trim()), marker = quote(&marker.to_string_lossy()));
        std::fs::write(stubs.join("tmux"), script).unwrap();
        std::fs::set_permissions(stubs.join("tmux"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let result = tmux
            .bin_command(if activation { &["toggle", "split"] } else { &["setup"] })
            .env(
                "PATH",
                format!("{}:{}", stubs.display(), std::env::var("PATH").unwrap()),
            )
            .output()
            .unwrap();
        assert!(!result.status.success());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains("injected setup failure"), "{error}");
        if fail_rollback {
            assert!(error.contains("rollback failed"), "{error}");
            for slot in ["restore agenmux", "@agenmux-runtime-bin", "status-left", "after-select-window[42]"] {
                assert!(error.contains(slot), "{error}");
            }
            // Later restores still execute after binding, identity, status and hook errors.
            assert_eq!(tmux.text(&["show-option", "-gqv", "status-right"]), "#{agents_mon}");
            assert_eq!(tmux.text(&["show-options", "-w"]), windows_before);
            assert_eq!(tmux.text(&["show-options", "-gq", "@agenmux-plugin-dir"]), "@agenmux-plugin-dir ''");
        } else {
            assert!(!error.contains("rollback failed"), "{error}");
            if activation {
                for name in ["agenmux-keys", "agenmux-rows", "agenmux-scan-cache"] {
                    assert!(!tmux.tmp.join(name).exists(), "{name}");
                }
                tmux.wait_for(Duration::from_secs(3), || tmux.text(&["list-clients", "-F", "#{client_name}"]).is_empty());
            }
            assert_eq!(tmux.text(&["list-panes", "-F", "#{pane_id}"]), panes_before);
            assert_eq!(tmux.text(&["display-message", "-p", "#{window_layout}"]), layout_before);
            assert_eq!(tmux.text(&["show-options", "-g"]), options_before);
            assert_eq!(tmux.text(&["show-options", "-w"]), windows_before);
            assert_eq!(tmux.text(&["show-hooks", "-g"]), hooks_before);
            assert_eq!(tmux.text(&["show-hooks", "-gw"]), window_hooks_before);
            assert_eq!(tmux.text(&["list-keys"]), before);
            assert_eq!(
                tmux.text(&[
                    "list-keys",
                    "-F",
                    "#{key_table}:#{key_string}:#{key_note}:#{key_repeat}"
                ]),
                notes_before
            );
            assert_eq!(
                tmux.text(&["show-option", "-gqv", "@agenmux-prefix-owned"]),
                ""
            );
        }
    }
}

#[test]
fn file_width_pin_and_notification_eligibility_share_resolution() {
    let tmux = TestTmux::new("file-behavior");
    app_file(
        &tmux,
        "[display]\nsidebar_width=26\n[behavior]\nnotifications=false",
    );
    assert_eq!(
        tmux.bin(&["internal", "notification-eligible"])
            .status
            .code(),
        Some(3)
    );
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-on", "1"]);
    assert_success(tmux.bin(&["pane-add"]), "file width new pane");
    let pane = tmux.text(&[
        "list-panes",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}",
    ]);
    assert_eq!(
        tmux.text(&["display-message", "-p", "-t", &pane, "#{pane_width}"]),
        "26"
    );
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-width", "34"]);
    assert_success(tmux.bin(&["pane-pin"]), "live width pin");
    assert_eq!(
        tmux.text(&["display-message", "-p", "-t", &pane, "#{pane_width}"]),
        "34"
    );
    tmux.assert_tmux(&["set-option", "-gu", "@agenmux-width"]);
    assert_success(tmux.bin(&["pane-pin"]), "unset width returns to file");
    assert_eq!(
        tmux.text(&["display-message", "-p", "-t", &pane, "#{pane_width}"]),
        "26"
    );
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-notifications", ""]);
    assert_success(
        tmux.bin(&["internal", "notification-eligible"]),
        "empty canonical enables notifications",
    );
    app_file(&tmux, "invalid");
    assert_eq!(
        tmux.bin(&["internal", "notification-eligible"])
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn binary_helper_uses_private_server() {
    let tmux = TestTmux::new("binary-helper");
    assert_success(tmux.bin(&["status"]), "agenmux status");
}

#[test]
fn public_toggle_observes_daemon_failure_and_cleans_new_resources() {
    use std::os::unix::fs::PermissionsExt;
    for mode in ["exit", "changed-config", "timeout", "missing"] {
        let tmux = TestTmux::new("startup-failure");
        app_file(&tmux, "[display]\nsidebar_width=26\n");
        tmux.assert_tmux(&["split-window", "-h", "exec sleep 60"]);
        let before = tmux.text(&["list-panes", "-F", "#{pane_id}"]);
        let layout = tmux.text(&["display-message", "-p", "#{window_layout}"]);
        let bin = tmux.tmp.join("daemon-stub");
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\"'\"'"));
        let body = match mode {
            "exit" => "echo 'synthetic private diagnostic' >&2; exit 2".to_owned(),
            "changed-config" => format!(
                "printf '[invalid' > {}; exec {} daemon",
                quote(&tmux.tmp.join("config/agenmux/config.toml").to_string_lossy()),
                quote(env!("CARGO_BIN_EXE_agenmux"))),
            _ => "exec sleep 60".to_owned(),
        };
        if mode != "missing" {
            std::fs::write(&bin, format!("#!/bin/sh\necho $$ > {}\n{body}\n", quote(&tmux.tmp.join("child-pid").to_string_lossy()))).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        tmux.assert_tmux(&["set-option", "-g", "@agenmux-bin", &bin.to_string_lossy()]);
        let start = Instant::now();
        let output = tmux.bin(&["toggle", "split"]);
        assert_eq!(output.status.code(), Some(1), "{mode}");
        assert!(start.elapsed() < Duration::from_secs(12), "{mode}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("daemon"), "{mode}: {error}");
        assert!(!error.contains("synthetic private"));
        if mode == "timeout" { assert!(error.contains("timed out"), "{error}"); }
        assert_eq!(tmux.text(&["list-panes", "-F", "#{pane_id}"]), before, "{mode}");
        assert_eq!(tmux.text(&["display-message", "-p", "#{window_layout}"]), layout, "{mode}");
        for name in ["@agenmux-on", "@agenmux-control-client", "@agenmux-runtime-dir"] {
            assert_eq!(tmux.text(&["show-options", "-gq", name]), "", "{mode}: {name}");
        }
        assert!(!tmux.tmp.join("agenmux-keys").exists());
        if mode != "missing" {
            let pid: i32 = std::fs::read_to_string(tmux.tmp.join("child-pid")).unwrap().trim().parse().unwrap();
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "startup child survived: {mode}");
        }
    }
}

#[test]
fn public_popup_launch_errors_remove_pin_and_jump() {
    let tmux = TestTmux::new("popup-failure");
    let mut client = tmux.attach();
    tmux.wait_for(Duration::from_secs(3), || !tmux.text(&["list-clients", "-F", "#{client_name}"]).is_empty());
    for owner in [Some("missing-client"), None] {
        tmux.assert_tmux(&["set-option", "-g", "@agenmux-bin", "/synthetic/missing-executable"]);
        std::fs::write(tmux.tmp.join("agenmux-pin.jump"), "synthetic").unwrap();
        let args = if let Some(owner) = owner { vec!["toggle", "popup", owner] } else { vec!["toggle", "popup"] };
        let output = tmux.bin(&args);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("popup launch failed"));
        assert!(!tmux.tmp.join("agenmux-pin").exists());
        assert!(!tmux.tmp.join("agenmux-pin.jump").exists());
    }
    let _ = client.kill();
    let _ = client.wait();
}

/// A keymap edit must reach the private tables without a manual `setup`: the
/// nav-version option carries a fingerprint of the generated bindings, and
/// activation reinstalls them whenever the installed one is stale.
#[test]
fn toggle_reinstalls_key_tables_after_a_keymap_change() {
    let tmux = TestTmux::new("keymap-rerun");
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    let mut viewer_process = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);

    app_file(&tmux, "[keys.normal]\ndown = ['n']\n");
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "open with the first keymap",
    );
    tmux.wait_for(Duration::from_secs(3), || {
        !tmux
            .text(&["show-option", "-gqv", "@agenmux-control-client"])
            .is_empty()
    });
    let first = tmux.text(&["show-option", "-gqv", "@agenmux-nav-version"]);
    assert!(first.starts_with("14."), "{first}");
    assert!(tmux.binding("agenmux", "n").contains("key 'down'"));
    assert!(tmux.binding("agenmux", "j").is_empty());

    // Same session, edited file: activation alone must replace the tables.
    app_file(&tmux, "[keys.normal]\ndown = ['x']\n");
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "reopen with the second keymap",
    );
    let second = tmux.text(&["show-option", "-gqv", "@agenmux-nav-version"]);
    assert_ne!(first, second, "fingerprint did not follow the keymap");
    assert!(tmux.binding("agenmux", "x").contains("key 'down'"));
    assert!(tmux.binding("agenmux", "n").is_empty());

    // An unchanged file leaves the fingerprint alone, so toggling stays cheap.
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "toggle with an unchanged keymap",
    );
    assert_eq!(
        tmux.text(&["show-option", "-gqv", "@agenmux-nav-version"]),
        second
    );

    let _ = viewer_process.kill();
    let _ = viewer_process.wait();
}
