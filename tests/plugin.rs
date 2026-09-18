use std::collections::HashSet;
use std::io::Read;
use std::os::fd::AsRawFd;
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
            "exec sleep 3600",
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
            .env("XDG_STATE_HOME", self.tmp.join("state"))
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

    /// The command bound to `key`, from the default `list-keys` output.
    /// `-F` is newer than the tmux versions CI runs, so parse the plain form.
    fn binding(&self, table: &str, key: &str) -> String {
        let marker = format!(" -T {table} ");
        self.text(&["list-keys", "-T", table])
            .lines()
            .find_map(|line| {
                let (_, rest) = line.split_once(&marker)?;
                let mut fields = rest.splitn(2, char::is_whitespace);
                let listed = fields.next()?;
                // tmux escapes the command separator in its own output.
                (listed == key || listed.strip_prefix('\\') == Some(key))
                    .then(|| fields.next().unwrap_or("").trim_start().to_owned())
            })
            .unwrap_or_default()
    }

    /// `list-keys` equals `before`. Re-read first: a loaded server sometimes
    /// prints one line garbled or missing, and only a real binding change
    /// stays different.
    #[track_caller]
    fn assert_keys_unchanged(&self, before: &str) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut keys = self.text(&["list-keys"]);
        while keys != before && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
            keys = self.text(&["list-keys"]);
        }
        assert_eq!(keys, before);
    }

    fn assert_tmux(&self, args: &[&str]) {
        let output = self.tmux(args);
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[track_caller]
    fn wait_for(&self, timeout: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + timeout;
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "condition timed out after {timeout:?}\n{}",
                self.diagnostics()
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Panes, clients, runtime files and the daemon trace tail, for a failed wait.
    fn diagnostics(&self) -> String {
        let listing = |args: &[&str]| {
            let output = self.tmux(args);
            String::from_utf8_lossy(if output.status.success() {
                &output.stdout
            } else {
                &output.stderr
            })
            .trim_end()
            .to_string()
        };
        let mut out = format!(
            "panes:\n{}\nclients:\n{}\n",
            listing(&[
                "list-panes",
                "-a",
                "-F",
                "#{session_name}:#{window_index} #{pane_id} pid=#{pane_pid} title=#{pane_title}",
            ]),
            listing(&[
                "list-clients",
                "-F",
                "#{client_name} pid=#{client_pid} flags=#{client_flags}",
            ]),
        );
        for name in ["agenmux-rows", "agenmux-scan-cache"] {
            if let Ok(text) = std::fs::read_to_string(self.tmp.join(name)) {
                out.push_str(&format!("{name}:\n{}\n", text.trim_end()));
            }
        }
        let mut traces = std::fs::read_dir(&self.tmp)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| {
                        path.file_name()
                            .is_some_and(|name| name.to_string_lossy().ends_with("-debug"))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        traces.sort();
        for path in traces {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let lines = text.lines().collect::<Vec<_>>();
            let tail = &lines[lines.len().saturating_sub(60)..];
            out.push_str(&format!(
                "{} (last {} of {} lines):\n{}\n",
                path.display(),
                tail.len(),
                lines.len(),
                tail.join("\n")
            ));
        }
        out
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

fn latency_stats(samples: &[Duration]) -> (u128, u128, u128) {
    let mut micros = samples.iter().map(Duration::as_micros).collect::<Vec<_>>();
    micros.sort_unstable();
    (
        micros[0],
        micros[micros.len() / 2],
        micros[micros.len() - 1],
    )
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
    tmux.assert_tmux(&["select-pane", "-t", &pane, "-T", "agenmux"]);
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
fn close_key_marks_split_off_before_fifo_delivery() {
    let tmux = TestTmux::new("close-packet");
    let fifo = tmux.tmp.join("agenmux-keys");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let mut fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fifo)
        .unwrap();
    tmux.assert_tmux(&["set-option", "-g", "@agenmux-on", "1"]);

    assert_success(tmux.bin(&["key", "close"]), "close key");

    assert_eq!(tmux.text(&["show-option", "-gqv", "@agenmux-on"]), "");
    let mut packet = [0];
    fifo.read_exact(&mut packet).unwrap();
    assert_eq!(packet, [b'Q']);
}

#[test]
fn concurrent_opens_converge_on_one_activation() {
    let tmux = TestTmux::new("concurrent-open");
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    let mut viewer = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);

    let mut opens = (0..4)
        .map(|_| {
            tmux.bin_command(&["toggle", "split", &client])
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for open in &mut opens {
        assert!(open.wait().unwrap().success());
    }

    let generation = tmux.text(&["show-option", "-gqv", "@agenmux-generation"]);
    assert!(!generation.is_empty());
    let controls = tmux.text(&[
        "list-clients",
        "-f",
        "#{m:*control-mode*,#{client_flags}}",
        "-F",
        "#{client_name}",
    ]);
    assert_eq!(controls.lines().count(), 1, "{controls}");
    let sidebars = tmux.text(&[
        "list-panes",
        "-a",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}",
    ]);
    assert_eq!(sidebars.lines().count(), 1, "{sidebars}");
    assert!(tmux.tmp.join("agenmux-keys").exists());

    assert_success(tmux.bin(&["key", "close", &client]), "final close");
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&["show-option", "-gqv", "@agenmux-generation"])
            .is_empty()
            && tmux
                .text(&[
                    "list-panes",
                    "-a",
                    "-f",
                    "#{==:#{pane_title},agenmux}",
                    "-F",
                    "#{pane_id}",
                ])
                .is_empty()
            && !tmux.tmp.join("agenmux-keys").exists()
    });
    let _ = viewer.kill();
    let _ = viewer.wait();
}

#[test]
#[ignore = "diagnostic lifecycle latency report"]
fn lifecycle_latency_report() {
    for windows in [1usize, 40] {
        let tmux = TestTmux::new(&format!("lifecycle-latency-{windows}"));
        tmux.assert_tmux(&[
            "set-option",
            "-g",
            "@agenmux-bin",
            env!("CARGO_BIN_EXE_agenmux"),
        ]);
        for index in 1..windows {
            tmux.assert_tmux(&[
                "new-window",
                "-d",
                "-t",
                "plugin:",
                "-n",
                &format!("latency-{index}"),
                "exec sleep 3600",
            ]);
        }
        let mut viewer = tmux.attach();
        tmux.wait_for(Duration::from_secs(2), || {
            !tmux
                .text(&["list-clients", "-F", "#{client_name}"])
                .is_empty()
        });
        let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);
        let mut opens = Vec::new();
        let mut closes = Vec::new();

        for sample in 0..5 {
            let started = Instant::now();
            assert_success(
                tmux.bin(&["toggle", "split", &client]),
                &format!("open sample {sample} with {windows} windows"),
            );
            opens.push(started.elapsed());

            let sidebars = tmux.text(&[
                "list-panes",
                "-a",
                "-f",
                "#{==:#{pane_title},agenmux}",
                "-F",
                "#{pane_id}",
            ]);
            assert_eq!(sidebars.lines().count(), 1, "{sidebars}");
            let sidebar = sidebars.lines().next().unwrap();
            assert!(
                !tmux.text(&["capture-pane", "-p", "-t", sidebar]).is_empty(),
                "startup acknowledged before a live frame"
            );

            let started = Instant::now();
            assert_success(
                tmux.bin(&["key", "close", &client]),
                &format!("close sample {sample} with {windows} windows"),
            );
            tmux.wait_for(Duration::from_secs(4), || {
                tmux.text(&["show-option", "-gqv", "@agenmux-generation"])
                    .is_empty()
                    && tmux
                        .text(&[
                            "list-panes",
                            "-a",
                            "-f",
                            "#{==:#{pane_title},agenmux}",
                            "-F",
                            "#{pane_id}",
                        ])
                        .is_empty()
                    && !tmux.tmp.join("agenmux-keys").exists()
            });
            closes.push(started.elapsed());
        }

        let cold = opens[0].as_micros();
        let reopen = latency_stats(&opens[1..]);
        let close = latency_stats(&closes);
        eprintln!(
            "lifecycle-latency windows={windows} cold_open_us={cold} reopen_us[min/median/max]={}/{}/{} close_us[min/median/max]={}/{}/{}",
            reopen.0, reopen.1, reopen.2, close.0, close.1, close.2
        );
        let _ = viewer.kill();
        let _ = viewer.wait();
    }
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
fn overlay_click_rows_target_the_clicked_pane() {
    // Split daemons have no pane of their own, so overlay rows name the
    // clicked pane as "=" and the handler must still deliver the packet.
    let tmux = TestTmux::new("overlay-click");
    tmux.assert_tmux(&["bind-key", "-T", "agenmux", "Escape", "kill-pane"]);
    let mut viewer_process = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let viewer = tmux.text(&["list-clients", "-F", "#{client_name}"]);
    let pane = tmux.text(&["display-message", "-p", "-c", &viewer, "#{pane_id}"]);
    let fifo = tmux.tmp.join("agenmux-keys");
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
    let mut fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fifo)
        .unwrap();
    std::fs::write(tmux.tmp.join("agenmux-rows"), "-\n=\t3\t0\n").unwrap();

    assert_success(tmux.bin(&["click", &pane, "2", &viewer]), "agenmux click");
    // Non-blocking read: a dropped packet fails here instead of hanging.
    assert_eq!(
        unsafe { libc::fcntl(fifo.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    thread::sleep(Duration::from_millis(200));
    let mut packet = [0; 5];
    assert_eq!(fifo.read(&mut packet).ok(), Some(5), "no select packet");
    assert_eq!(packet, [0x05, 0, 0, 0, 3]);
    assert_eq!(
        tmux.text(&[
            "display-message",
            "-p",
            "-c",
            &viewer,
            "#{client_key_table}"
        ]),
        "agenmux"
    );

    let _ = viewer_process.kill();
    let _ = viewer_process.wait();
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
    app_file(&tmux, "[tmux_management]\nenabled=true");
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
        ("after-kill-pane", "after-kill-pane[45]"),
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
    let down_action = normal
        .lines()
        .find(|line| line.contains("set-buffer -b agenmux.down"))
        .unwrap();
    assert!(down_action.contains("agenmux j "), "{down_action}");
    assert!(!down_action.contains("run-shell"), "{down_action}");
    assert!(
        down_action.find("switch-client").unwrap() < down_action.find("set-buffer").unwrap(),
        "key table must switch before the producer runs: {down_action}"
    );
    // keys that still go through the engine must find the daemon runtime dir
    let sequence_action = normal
        .lines()
        .find(|line| line.contains(" key \'sequence-67\'"))
        .unwrap();
    assert!(
        sequence_action.contains("AGENMUX_RUNTIME_DIR=#{q:@agenmux-runtime-dir}"),
        "{sequence_action}"
    );
    assert!(normal.contains(" key \'sequence-72\'"), "{normal}");
    let delete_prefix = normal
        .lines()
        .find(|line| line.contains(" key \'sequence-64\'"))
        .unwrap();
    assert!(
        delete_prefix.contains("switch-client -T agenmux-sequence"),
        "{delete_prefix}"
    );
    assert!(!delete_prefix.contains("run-shell -b"), "{delete_prefix}");
    let sequence = tmux.text(&["list-keys", "-T", "agenmux-sequence"]);
    for code in ["63", "64", "67", "73"] {
        assert!(
            sequence.contains(&format!("key \'sequence-{code}\'")),
            "missing {code}: {sequence}"
        );
    }
    assert!(
        tmux.binding("agenmux-sequence", "Any")
            .contains("key 'escape'"),
        "{sequence}"
    );
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
            && normal.contains("agenmux.wheel-up")
            && normal.contains("agenmux.wheel-down"),
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
        nav_version.starts_with("16.") && nav_version.len() == 19,
        "{nav_version}"
    );
    let status = tmux.tmux(&["show-option", "-gqv", "status-right"]);
    assert_success(status.clone(), "show status-right");
    assert_eq!(
        String::from_utf8(status.stdout)
            .unwrap()
            .trim_end_matches(['\r', '\n']),
        "#(AGENMUX_DIR=#{q:@agenmux-plugin-dir} AGENMUX_RUNTIME_DIR=#{q:@agenmux-runtime-dir} #{q:@agenmux-runtime-bin} status) | %H:%M \t  "
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
    let effective = tmux.bin(&["config", "check", "--effective", "--all"]);
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
    assert_eq!(row("display.show_all_panes"), "false default");
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
    let focused_window = tmux.text(&["display-message", "-p", "-c", &client, "#{window_id}"]);

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
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{window_id}\t#{pane_pid}",
    ]);
    assert_eq!(sidebars, format!("{focused_window}\t0"), "{sidebars}");
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
    let repeated = tmux.text(&[
        "list-panes",
        "-a",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{window_id}",
    ]);
    assert_eq!(repeated.lines().count(), 1, "{repeated}");

    tmux.assert_tmux(&["switch-client", "-c", &client, "-t", ":other"]);
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "lazy second-window split toggle",
    );
    let lazy = tmux.text(&[
        "list-panes",
        "-a",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{window_id}\t#{pane_pid}",
    ]);
    assert_eq!(lazy.lines().count(), 2, "{lazy}");
    assert!(lazy.lines().all(|line| line.ends_with("\t0")), "{lazy}");
    assert_eq!(
        tmux.text(&[
            "display-message",
            "-p",
            "-c",
            &client,
            "#{pane_title}\t#{client_key_table}",
        ]),
        "agenmux\tagenmux"
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
fn quick_launchers_open_selected_panes_safely_and_reload_transactionally() {
    use std::os::unix::fs::PermissionsExt;

    let tmux = TestTmux::new("quick-launchers");
    let tool = tmux.tmp.join("launcher tool/record runner");
    std::fs::create_dir_all(tool.parent().unwrap()).unwrap();
    std::fs::write(
        &tool,
        "#!/bin/sh\nlog=$1\nshift\n{ printf '%s\\n' \"$PWD\"; printf '%s\\n' \"$@\"; } > \"$log\"\nexec sleep 300\n",
    )
    .unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();

    let ordinary_cwd = tmux.tmp.join("ordinary selected cwd");
    let agent_cwd = tmux.tmp.join("agent selected cwd");
    std::fs::create_dir_all(&ordinary_cwd).unwrap();
    std::fs::create_dir_all(&agent_cwd).unwrap();
    let ordinary = tmux.text(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "plugin:",
        "-n",
        "ordinary-target",
        "-c",
        ordinary_cwd.to_str().unwrap(),
        "exec sleep 3600",
    ]);
    let codex = tmux.tmp.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nwhile :; do sleep 60; done\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let agent = tmux.text(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "plugin:",
        "-n",
        "agent-target",
        "-c",
        agent_cwd.to_str().unwrap(),
        codex.to_str().unwrap(),
    ]);

    let nvim_log = tmux.tmp.join("nvim arguments.log");
    let lazygit_log = tmux.tmp.join("lazygit arguments.log");
    let terminal_log = tmux.tmp.join("terminal arguments.log");
    let session_cwd = tmux.text(&["display-message", "-p", "-t", "plugin", "#{session_path}"]);
    let marker = tmux.tmp.join("injected-command-ran");
    let shell_expression = format!("$(touch {})", marker.display());
    let suspicious_arg = format!("; touch {}; #", marker.display());
    let nvim_args = vec![
        nvim_log.to_string_lossy().to_string(),
        "two words".into(),
        "quote's preserved".into(),
        shell_expression.clone(),
        suspicious_arg.clone(),
    ];
    let lazygit_args = vec![
        lazygit_log.to_string_lossy().to_string(),
        "ordinary arg".into(),
    ];
    let terminal_args = vec![
        terminal_log.to_string_lossy().to_string(),
        "tmux default cwd".into(),
    ];
    let toml_string = |value: &str| toml_edit::Value::from(value).to_string();
    let toml_array = |values: &[String]| {
        let mut array = toml_edit::Array::new();
        for value in values {
            array.push(value.as_str());
        }
        toml_edit::Value::Array(array)
            .to_string()
            .trim()
            .to_string()
    };
    let config = |lazygit_sequence: &str, management_enabled: bool, nvim_enabled: bool| {
        format!(
            "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false\n[tmux_management]\nenabled={management_enabled}\n[quick_launchers.nvim]\nsequence='e'\nlabel='nvim'\ncommand={}\nargs={}\nworking_directory='selected'\nenabled={nvim_enabled}\n[quick_launchers.lazygit]\nsequence={}\nlabel='lazygit'\ncommand={}\nargs={}\nworking_directory='selected'\nenabled=true\n[quick_launchers.terminal]\nsequence='ov'\nlabel='terminal'\ncommand={}\nargs={}\nworking_directory='tmux'\nenabled=true\n",
            toml_string(&tool.to_string_lossy()),
            toml_array(&nvim_args),
            toml_string(lazygit_sequence),
            toml_string(&tool.to_string_lossy()),
            toml_array(&lazygit_args),
            toml_string(&tool.to_string_lossy()),
            toml_array(&terminal_args),
        )
    };
    app_file(&tmux, &config("og", true, true));
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    let mut viewer = tmux.attach();
    tmux.wait_for(Duration::from_secs(3), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "start quick launcher sidebar",
    );
    let initial_sidebar = tmux.text(&[
        "list-panes",
        "-a",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}",
    ]);
    assert!(!initial_sidebar.is_empty(), "sidebar pane not installed");
    let initial_sidebar_window = tmux.text(&[
        "display-message",
        "-p",
        "-t",
        &initial_sidebar,
        "#{window_id}",
    ]);
    tmux.wait_for(Duration::from_secs(8), || {
        std::fs::read_to_string(tmux.tmp.join("agenmux-scan-cache"))
            .unwrap_or_default()
            .lines()
            .any(|line| line.starts_with(&format!("{agent}\t")))
            && std::fs::read_to_string(tmux.tmp.join("agenmux-rows"))
                .unwrap_or_default()
                .lines()
                .any(|line| line.starts_with(&format!("{ordinary}\t")))
    });

    let selected = || {
        std::fs::read_to_string(tmux.tmp.join("agenmux-rows"))
            .unwrap_or_default()
            .lines()
            .find_map(|line| {
                let mut fields = line.split('\t');
                let pane = fields.next()?;
                let _ordinal = fields.next()?;
                (fields.next() == Some("1")).then(|| pane.to_string())
            })
            .unwrap_or_default()
    };
    let send_sequence = |sequence: &str| {
        for byte in sequence.bytes() {
            assert_success(
                tmux.bin(&["key", &format!("sequence-{byte:02X}"), &client]),
                sequence,
            );
            thread::sleep(Duration::from_millis(100));
        }
    };
    let select = |target: &str| {
        send_sequence("gg");
        for _ in 0..16 {
            if selected() == target {
                return;
            }
            assert_success(tmux.bin(&["key", "down", &client]), "select pane row");
            thread::sleep(Duration::from_millis(90));
        }
        panic!("could not select pane {target}; selected {}", selected());
    };
    let client_value = |field: &str| {
        let filter = format!("#{{==:#{{client_name}},{client}}}");
        tmux.text(&["list-clients", "-f", &filter, "-F", field])
    };
    let return_to_sidebar = || {
        tmux.assert_tmux(&["select-window", "-t", &initial_sidebar_window]);
        tmux.assert_tmux(&["select-pane", "-t", &initial_sidebar]);
        tmux.assert_tmux(&["switch-client", "-c", &client, "-T", "agenmux"]);
    };
    let assert_launch = |log: &std::path::Path, cwd: &std::path::Path, expected: &[String]| {
        tmux.wait_for(Duration::from_secs(5), || log.is_file());
        let lines = std::fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let actual_cwd = std::fs::canonicalize(lines.first().unwrap()).unwrap();
        let expected_cwd = std::fs::canonicalize(cwd).unwrap();
        assert_eq!(actual_cwd, expected_cwd);
        assert_eq!(&lines[1..], expected);
        tmux.wait_for(Duration::from_secs(5), || {
            let focused = client_value("#{pane_id}");
            let current_path = tmux.text(&[
                "display-message",
                "-p",
                "-t",
                &focused,
                "#{pane_current_path}",
            ]);
            client_value("#{pane_id}|#{client_key_table}") == format!("{focused}|root")
                && std::fs::canonicalize(current_path).ok().as_ref() == Some(&expected_cwd)
        });
        let focused = client_value("#{pane_id}");
        tmux.wait_for(Duration::from_secs(5), || {
            std::fs::read_to_string(tmux.tmp.join("agenmux-rows"))
                .unwrap_or_default()
                .lines()
                .any(|line| line.starts_with(&format!("{focused}\t")))
        });
    };

    let initial_version = tmux.text(&["show-option", "-gqv", "@agenmux-nav-version"]);
    assert!(tmux.binding("agenmux", "e").contains("sequence-65"));
    assert!(tmux.binding("agenmux", "o").contains("sequence-6F"));
    assert!(tmux
        .binding("agenmux-sequence", "g")
        .contains("sequence-67"));
    assert!(tmux
        .binding("agenmux-sequence", "v")
        .contains("sequence-76"));
    assert!(tmux.binding("agenmux", "G").contains("last"));
    assert!(tmux.binding("agenmux", "u").contains("versions"));

    // The first target is a real detected agent in all-pane mode. Its path,
    // the executable path, and each argument contain spaces or shell syntax.
    select(&agent);
    send_sequence("e");
    assert_launch(&nvim_log, &agent_cwd, &nvim_args[1..].to_vec());
    assert!(
        !marker.exists(),
        "launcher arguments were interpreted by a shell"
    );
    return_to_sidebar();

    // Tmux mode uses the target session's default working directory instead
    // of the selected agent pane's current directory.
    select(&agent);
    send_sequence("ov");
    assert_launch(
        &terminal_log,
        std::path::Path::new(&session_cwd),
        &["tmux default cwd".into()],
    );
    return_to_sidebar();

    // Reload changes and removes sequences in the live tmux tables.
    app_file(&tmux, &config("ot", true, false));
    assert_success(tmux.bin(&["config", "reload"]), "reload launcher bindings");
    let reloaded_version = tmux.text(&["show-option", "-gqv", "@agenmux-nav-version"]);
    assert_ne!(initial_version, reloaded_version);
    assert!(tmux.binding("agenmux", "e").is_empty());
    assert!(tmux.binding("agenmux", "o").contains("sequence-6F"));
    assert!(tmux
        .binding("agenmux-sequence", "t")
        .contains("sequence-74"));
    // Live views read the published token on the two-second periodic scan.
    thread::sleep(Duration::from_secs(3));
    select(&ordinary);
    let window_count = tmux
        .text(&["list-windows", "-t", "plugin", "-F", "#{window_id}"])
        .lines()
        .count();
    send_sequence("og");
    thread::sleep(Duration::from_millis(300));
    assert!(
        !lazygit_log.exists(),
        "removed sequence still launched lazygit: {}",
        std::fs::read_to_string(&lazygit_log).unwrap_or_default()
    );
    assert_eq!(
        tmux.text(&["list-windows", "-t", "plugin", "-F", "#{window_id}"])
            .lines()
            .count(),
        window_count,
        "removed sequence created a window"
    );
    send_sequence("ot");
    assert_launch(&lazygit_log, &ordinary_cwd, &["ordinary arg".into()]);

    // An invalid conflicting reload keeps the currently installed tables and
    // resolved generation intact.
    app_file(&tmux, &config("u", true, false));
    let invalid = tmux.bin(&["config", "reload"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(
        tmux.text(&["show-option", "-gqv", "@agenmux-nav-version"]),
        reloaded_version
    );
    assert!(tmux
        .binding("agenmux-sequence", "t")
        .contains("sequence-74"));

    // Turning the management gate off removes launcher and mutation bindings;
    // the legacy packet path is gated by the daemon's freshly loaded config too.
    app_file(&tmux, &config("ot", false, false));
    assert_success(tmux.bin(&["config", "reload"]), "disable quick launchers");
    assert!(tmux.binding("agenmux", "e").is_empty());
    assert!(tmux.binding("agenmux", "o").is_empty());
    assert!(tmux.binding("agenmux", "c").is_empty());
    assert!(tmux.binding("agenmux", "g").contains("sequence-67"));
    assert!(tmux.binding("agenmux", "G").contains("last"));
    assert!(tmux.binding("agenmux", "u").contains("versions"));
    return_to_sidebar();
    assert_success(
        tmux.bin(&["key", "help", &client]),
        "open help while disabling quick launchers",
    );
    tmux.wait_for(Duration::from_secs(5), || {
        let frame = tmux.text(&["capture-pane", "-p", "-t", &initial_sidebar]);
        frame.contains("this help")
            && !frame.contains("nvim")
            && !frame.contains("lazygit")
            && !frame.contains("optional launchers")
    });
    assert_success(
        tmux.bin(&["key", "escape", &client]),
        "close help after disabling quick launchers",
    );
    let windows_before_legacy = tmux
        .text(&["list-windows", "-t", "plugin", "-F", "#{window_id}"])
        .lines()
        .count();
    send_sequence("e");
    send_sequence("ot");
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        tmux.text(&["list-windows", "-t", "plugin", "-F", "#{window_id}"])
            .lines()
            .count(),
        windows_before_legacy
    );

    let _ = viewer.kill();
    let _ = viewer.wait();
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
                assert_eq!(
                    tmux.text(&["show-options", "-gq", "@agenmux-prefix-owned"]),
                    ""
                );
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
        tmux.assert_keys_unchanged(&before);
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
    // Close is idempotent across partially completed teardown.
    assert_eq!(tmux.bin(&["key", "close"]).status.code(), Some(0));
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
            .env("XDG_STATE_HOME", tmux.tmp.join("state"))
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
        let native: &[&str] = match key {
            "PageUp" => &["PPage"],
            "PageDown" => &["NPage"],
            "C-@" => &["C-@", "C-Space"],
            _ => &[key],
        };
        let binding = native
            .iter()
            .map(|name| tmux.binding("prefix", name))
            .find(|found| !found.is_empty())
            .unwrap_or_default();
        assert!(
            binding.contains("activate 'popup'"),
            "{key} (as {native:?}): {binding}\nprefix table:\n{}",
            tmux.text(&["list-keys", "-T", "prefix"])
        );
        tmux.assert_tmux(&["set-option", "-g", "@agenmux-popup-key", ""]);
        assert_success(bootstrap(), "disabling never unbinds an existing launcher");
        assert_eq!(
            native
                .iter()
                .map(|name| tmux.binding("prefix", name))
                .find(|found| !found.is_empty())
                .unwrap_or_default(),
            binding
        );
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
        .env("XDG_STATE_HOME", tmux.tmp.join("state"))
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    tmux.assert_keys_unchanged(&before);
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
        .env("XDG_STATE_HOME", tmux.tmp.join("state"))
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
    let binding = tmux.binding("agenmux", "q");
    let command = binding
        .split(" \\; ")
        .find(|segment| segment.trim_start().starts_with("run-shell"))
        .unwrap()
        .replace("key 'close'", "--version");
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
    // `run-shell` only returns its command's output to the client on newer
    // tmux, so expand the same format and run it through a shell instead: that
    // checks the quoting and the executable without depending on where tmux
    // chooses to deliver output.
    let quoted = tmux.text(&["display-message", "-p", "#{q:@agenmux-runtime-bin}"]);
    let version = Command::new("sh")
        .arg("-c")
        .arg(format!("{quoted} --version"))
        .output()
        .unwrap();
    assert_success(version.clone(), "run the expanded runtime binary");
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim_end(),
        format!("agenmux {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn default_header_inherits_tmux_active_border_contrast() {
    let tmux = TestTmux::new("tmux-header");
    app_file(&tmux, "[behavior]\nnotifications=false");
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "pane-active-border-style",
        "fg=#f5a97f,bg=colour236",
    ]);
    assert_eq!(
        tmux.text(&["show-option", "-gv", "pane-active-border-style"]),
        "fg=#f5a97f,bg=colour236"
    );
    let mut viewer = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "start inherited header",
    );
    assert_eq!(
        tmux.text(&["show-option", "-gv", "pane-active-border-style"]),
        "fg=#f5a97f,bg=colour236"
    );
    let pane = tmux.text(&[
        "list-panes",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}",
    ]);
    let capture = || tmux.text(&["capture-pane", "-p", "-e", "-t", &pane]);
    let mut frame = String::new();
    for _ in 0..80 {
        frame = capture();
        if frame.contains("48;2;245;169;127") && frame.contains("38;5;236") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        frame.contains("48;2;245;169;127"),
        "header background: {frame:?}"
    );
    assert!(frame.contains("38;5;236"), "header foreground: {frame:?}");
    assert!(
        frame
            .lines()
            .next()
            .is_some_and(|line| line.contains("38;2;245;169;127")),
        "focused frame should use tmux active border color: {frame:?}"
    );
    let ordinary = tmux.text(&[
        "list-panes",
        "-f",
        "#{!=:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}",
    ]);
    tmux.assert_tmux(&["switch-client", "-c", &client, "-t", &ordinary]);
    for _ in 0..80 {
        frame = capture();
        if frame.contains("38;2;245;169;127") && !frame.contains("48;2;245;169;127") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        frame.contains("38;2;245;169;127"),
        "unfocused header foreground: {frame:?}"
    );
    assert!(
        !frame.contains("48;2;245;169;127"),
        "unfocused header background: {frame:?}"
    );
    let border = frame.lines().next().unwrap_or("");
    assert!(
        border.contains("\x1b[34m") && !border.contains("38;2;245;169;127"),
        "unfocused frame should retain the theme accent: {frame:?}"
    );

    // The frame switch belongs to Display settings and applies immediately.
    tmux.assert_tmux(&["switch-client", "-c", &client, "-t", &pane]);
    tmux.assert_tmux(&["switch-client", "-c", &client, "-T", "agenmux"]);
    for key in ["settings", "down", "down"] {
        assert_success(tmux.bin(&["key", key, &client]), "open frame setting");
    }
    tmux.wait_for(Duration::from_secs(3), || capture().contains("show frame"));
    for key in ["enter", "down", "enter"] {
        assert_success(tmux.bin(&["key", key, &client]), "disable pane frame");
    }
    let config = tmux.tmp.join("config/agenmux/config.toml");
    tmux.wait_for(Duration::from_secs(3), || {
        std::fs::read_to_string(&config).is_ok_and(|source| source.contains("show_frame = false"))
    });
    tmux.wait_for(Duration::from_secs(15), || capture().contains("saved"));
    assert_success(tmux.bin(&["key", "close", &client]), "close settings");
    tmux.wait_for(Duration::from_secs(5), || {
        let frame = capture();
        !frame.contains("— settings") && !frame.contains('┌') && frame.contains("s settings")
    });
    let unframed = capture();
    assert!(
        unframed
            .trim_end()
            .lines()
            .last()
            .is_some_and(|line| line.contains("s settings")),
        "unframed footer should occupy the bottom row: {unframed:?}"
    );
    assert_success(tmux.bin(&["key", "close"]), "close inherited header");
    let _ = viewer.kill();
    let _ = viewer.wait();
}

#[test]
fn daemon_theme_is_a_startup_snapshot_without_global_color_mutation() {
    for base in ["light", "terminal"] {
        let tmux = TestTmux::new(&format!("theme-{base}"));
        app_file(&tmux, &format!("[theme]\nbase='{base}'\n[theme.colors]\nheader_fg='#123456'\nheader_bg='default'\nmuted_fg=99\n[behavior]\nnotifications=false"));
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
fn tmux_management_creates_and_deletes_stable_targets() {
    let tmux = TestTmux::new("tmux-management");
    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false\n[tmux_management]\nenabled=true\nconfirm_delete=true",
    );
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    let mut viewer = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);
    let initial = tmux.text(&["display-message", "-p", "-c", &client, "#{pane_id}"]);
    let cwd = tmux.tmp.join("selected-cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    tmux.assert_tmux(&[
        "respawn-pane",
        "-k",
        "-t",
        &initial,
        "-c",
        &cwd.to_string_lossy(),
        "exec sleep 300",
    ]);
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "start management daemon",
    );
    let sidebar = tmux.text(&[
        "list-panes",
        "-a",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}",
    ]);
    tmux.wait_for(Duration::from_secs(4), || {
        std::fs::read_to_string(tmux.tmp.join("agenmux-rows"))
            .unwrap_or_default()
            .lines()
            .any(|line| line.starts_with(&format!("{initial}\t")))
    });
    let send_sequence = |sequence: &str| {
        for byte in sequence.bytes() {
            assert_success(
                tmux.bin(&["key", &format!("sequence-{byte:02X}"), &client]),
                sequence,
            );
            thread::sleep(Duration::from_millis(80));
        }
    };
    let send_text = |text: &str| {
        for byte in text.bytes() {
            assert_success(tmux.bin(&["key", &format!("text-{byte:02X}")]), text);
            thread::sleep(Duration::from_millis(80));
        }
    };
    let selected = || {
        std::fs::read_to_string(tmux.tmp.join("agenmux-rows"))
            .unwrap_or_default()
            .lines()
            .find_map(|line| {
                let mut fields = line.split('\t');
                let pane = fields.next()?;
                let _index = fields.next()?;
                (fields.next() == Some("1")).then(|| pane.to_string())
            })
            .unwrap_or_default()
    };
    let client_value = |field: &str| {
        let filter = format!("#{{==:#{{client_name}},{client}}}");
        tmux.text(&["list-clients", "-f", &filter, "-F", field])
    };

    // Mutations never hand the client off: it stays on a sidebar pane, in the
    // plugin key table, so the next key keeps navigating agenmux.
    let assert_on_sidebar = || {
        tmux.wait_for(Duration::from_secs(10), || {
            client_value("#{pane_title}\t#{client_key_table}") == "agenmux\tagenmux"
        });
    };
    let created_pane = |window_name: &str| {
        tmux.text(&[
            "list-panes",
            "-a",
            "-f",
            &format!(
                "#{{&&:#{{==:#{{window_name}},{window_name}}},#{{!=:#{{pane_title}},agenmux}}}}"
            ),
            "-F",
            "#{pane_id}",
        ])
    };
    // Prompts render on the daemon's next pass; a loaded runner has needed
    // several seconds. Name the needle and show the frame on failure.
    let sidebar_shows = |needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut frame = String::new();
        while Instant::now() < deadline {
            frame = tmux.text(&["capture-pane", "-p", "-t", &sidebar]);
            if frame.contains(needle) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "sidebar never showed {needle:?}: {frame:?}\n{}",
            tmux.diagnostics()
        );
    };
    send_sequence("cc");
    sidebar_shows("\u{eb7f} ▏");
    for _ in 0..2 {
        assert_success(
            tmux.bin(&["key", "sequence-63", "missing-client"]),
            "second client create sequence",
        );
    }
    assert_eq!(
        client_value("#{client_key_table}"),
        "agenmux-search",
        "stray client sequence must preserve the owner's create prompt"
    );
    assert_success(tmux.bin(&["key", "escape"]), "cancel owned create");
    tmux.wait_for(Duration::from_secs(2), || {
        client_value("#{client_key_table}") == "agenmux"
    });

    let original_window_name =
        tmux.text(&["display-message", "-p", "-t", &initial, "#{window_name}"]);
    let original_session_name =
        tmux.text(&["display-message", "-p", "-t", &initial, "#{session_name}"]);

    // r renames the record under the cursor in place: a collapsed window row
    // is the window, a pane inside a split window is the pane, a session row
    // is the session.
    send_sequence("r");
    sidebar_shows(&format!("{original_window_name}▏"));
    send_text("x");
    sidebar_shows(&format!("{original_window_name}x▏"));
    assert_success(
        tmux.bin(&["key", "backspace"]),
        "delete appended name character",
    );
    sidebar_shows(&format!("{original_window_name}▏"));
    send_text("-renamed");
    assert_success(tmux.bin(&["key", "enter"]), "rename window");
    let renamed_window = format!("{original_window_name}-renamed");
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&["display-message", "-p", "-t", &initial, "#{window_name}"]) == renamed_window
    });

    let sibling = tmux.text(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &initial,
        "exec sleep 60",
    ]);
    // A short, known pane title so the preloaded edit is fully visible in the
    // narrow sidebar (a long title clips to its tail near the cursor).
    tmux.assert_tmux(&["select-pane", "-t", &initial, "-T", "edit"]);
    tmux.assert_tmux(&["select-pane", "-t", &initial]);
    tmux.wait_for(Duration::from_secs(4), || selected() == initial);
    sidebar_shows("edit");
    send_sequence("r");
    sidebar_shows("edit▏");
    send_text("-renamed");
    assert_success(tmux.bin(&["key", "enter"]), "rename pane");
    let renamed_pane = "edit-renamed".to_string();
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&["display-message", "-p", "-t", &initial, "#{pane_title}"]) == renamed_pane
    });
    tmux.assert_tmux(&["kill-pane", "-t", &sibling]);
    tmux.wait_for(Duration::from_secs(4), || selected() == initial);
    let _ = &original_session_name; // session-scope rename is covered by unit tests

    let windows = tmux
        .text(&["list-windows", "-a", "-F", "#{window_id}"])
        .lines()
        .count();
    send_sequence("cc");
    sidebar_shows("\u{eb7f} ▏");
    send_text("w");
    sidebar_shows("\u{eb7f} w▏");
    assert_success(tmux.bin(&["key", "enter"]), "accept window name");
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&["list-windows", "-a", "-F", "#{window_id}"])
            .lines()
            .count()
            == windows + 1
    });
    tmux.wait_for(Duration::from_secs(4), || {
        client_value("#{window_name}") == "w"
    });
    assert_on_sidebar();
    let window_pane = created_pane("w");
    assert!(window_pane.starts_with('%'), "{window_pane:?}");
    assert_eq!(
        tmux.text(&[
            "display-message",
            "-p",
            "-t",
            &window_pane,
            "#{pane_current_path}"
        ]),
        cwd.canonicalize().unwrap().to_string_lossy()
    );
    tmux.wait_for(Duration::from_secs(4), || selected() == window_pane);

    let sessions = tmux
        .text(&["list-sessions", "-F", "#{session_id}"])
        .lines()
        .count();
    send_sequence("cs");
    send_text("s");
    assert_success(tmux.bin(&["key", "enter"]), "accept session name");
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&["list-sessions", "-F", "#{session_id}"])
            .lines()
            .count()
            == sessions + 1
    });
    tmux.wait_for(Duration::from_secs(4), || {
        client_value("#{session_name}") == "s"
    });
    assert_on_sidebar();
    let session_pane = tmux.text(&[
        "list-panes",
        "-a",
        "-f",
        "#{&&:#{==:#{session_name},s},#{!=:#{pane_title},agenmux}}",
        "-F",
        "#{pane_id}",
    ]);
    assert!(session_pane.starts_with('%'), "{session_pane:?}");
    tmux.wait_for(Duration::from_secs(4), || selected() == session_pane);
    let guarded_session = tmux.text(&[
        "display-message",
        "-p",
        "-t",
        &session_pane,
        "#{session_id}",
    ]);
    send_sequence("dd");
    assert_success(
        tmux.bin(&["key", "text-79", &client]),
        "confirm guarded last-window delete",
    );
    tmux.wait_for(Duration::from_secs(10), || {
        tmux.text(&[
            "list-clients",
            "-f",
            &format!("#{{==:#{{client_name}},{client}}}"),
            "-F",
            "#{client_key_table}",
        ]) == "agenmux"
    });
    assert!(
        tmux.text(&["list-sessions", "-F", "#{session_id}"])
            .lines()
            .any(|session| session == guarded_session),
        "window deletion must not implicitly destroy its session"
    );
    let marked_sidebar = tmux.text(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &session_pane,
        "exec sleep 60",
    ]);
    tmux.assert_tmux(&["set-option", "-p", "-t", &marked_sidebar, "@agenmux", "1"]);
    tmux.assert_tmux(&[
        "select-pane",
        "-t",
        &marked_sidebar,
        "-T",
        "sidebar-fixture",
    ]);
    send_sequence("dd");
    assert_success(
        tmux.bin(&["key", "text-79", &client]),
        "confirm guarded last-pane delete",
    );
    tmux.wait_for(Duration::from_secs(10), || {
        client_value("#{client_key_table}") == "agenmux"
    });
    assert!(
        tmux.text(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|pane| pane == session_pane),
        "pane deletion must not implicitly destroy its session"
    );
    tmux.wait_for(Duration::from_secs(4), || selected() == session_pane);
    // The session row and its only pane share a pane id in the row map; the
    // session row is the first line with it. A scan's focus follower can
    // undo an `up` that lands mid-scan, so retry until the row map agrees.
    let session_row_selected = || {
        std::fs::read_to_string(tmux.tmp.join("agenmux-rows"))
            .unwrap_or_default()
            .lines()
            .find(|line| line.starts_with(&format!("{session_pane}\t")))
            .is_some_and(|line| line.ends_with("\t1"))
    };
    for _ in 0..5 {
        assert_success(tmux.bin(&["key", "up", &client]), "move to session row");
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline && !session_row_selected() {
            thread::sleep(Duration::from_millis(50));
        }
        if session_row_selected() {
            break;
        }
    }
    assert!(
        session_row_selected(),
        "cursor never reached the session row"
    );
    send_sequence("dd");
    // dd on a session row must confirm the whole session, inline.
    // Only sidebars in the client's session are repainted: read the one
    // the client sits on.
    let confirm_pane = client_value("#{pane_id}");
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut session_prompt = String::new();
    while Instant::now() < deadline {
        session_prompt = tmux.text(&["capture-pane", "-p", "-t", &confirm_pane]);
        if session_prompt.contains("delete session? y/N") {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        session_prompt.contains("delete session? y/N"),
        "dd on a session row must confirm the whole session: {session_prompt:?}"
    );
    assert_success(
        tmux.bin(&["key", "text-79", &client]),
        "confirm session delete",
    );
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&["list-sessions", "-F", "#{session_id}"])
            .lines()
            .count()
            == sessions
    });
    assert_on_sidebar();

    tmux.wait_for(Duration::from_secs(4), || selected() == window_pane);
    send_sequence("dd");
    assert_success(
        tmux.bin(&["key", "text-79", &client]),
        "confirm window delete",
    );
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&["list-windows", "-a", "-F", "#{window_id}"])
            .lines()
            .count()
            == windows
    });
    assert_on_sidebar();

    let extra = tmux.text(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &initial,
        "exec sleep 60",
    ]);
    tmux.assert_tmux(&["select-pane", "-t", &extra]);
    tmux.wait_for(Duration::from_secs(4), || selected() == extra);
    send_sequence("dd");
    assert_success(
        tmux.bin(&["key", "text-79", "missing-client"]),
        "ignore non-owner confirmation",
    );
    thread::sleep(Duration::from_millis(150));
    assert!(
        tmux.text(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|pane| pane == extra),
        "non-owner y must not confirm deletion"
    );
    assert_eq!(
        client_value("#{client_key_table}"),
        "agenmux-search",
        "non-owner input must leave the owner's confirmation active"
    );
    assert_success(
        tmux.bin(&["key", "sequence-67", "missing-client"]),
        "ignore non-owner sequence during confirmation",
    );
    thread::sleep(Duration::from_millis(150));
    assert_eq!(
        client_value("#{client_key_table}"),
        "agenmux-search",
        "non-owner sequence must leave confirmation active"
    );
    assert_success(
        tmux.bin(&["key", "text-59", &client]),
        "reject uppercase delete",
    );
    tmux.wait_for(Duration::from_secs(10), || {
        client_value("#{client_key_table}") == "agenmux"
    });
    assert!(
        tmux.text(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|pane| pane == extra),
        "uppercase Y must cancel deletion"
    );
    tmux.assert_tmux(&["select-pane", "-t", &extra]);
    tmux.wait_for(Duration::from_secs(10), || selected() == extra);
    send_sequence("dd");
    assert_success(tmux.bin(&["key", "enter", &client]), "cancel pane delete");
    tmux.wait_for(Duration::from_secs(10), || {
        client_value("#{client_key_table}") == "agenmux"
    });
    assert!(
        tmux.text(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|pane| pane == extra),
        "Enter must safely cancel deletion"
    );
    tmux.assert_tmux(&["select-pane", "-t", &extra]);
    tmux.wait_for(Duration::from_secs(10), || selected() == extra);
    send_sequence("dd");
    assert_success(
        tmux.bin(&["key", "text-79", &client]),
        "confirm pane delete",
    );
    tmux.wait_for(Duration::from_secs(10), || {
        !tmux
            .text(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|pane| pane == extra)
    });
    assert_on_sidebar();

    let stale = tmux.text(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &initial,
        "exec sleep 60",
    ]);
    tmux.wait_for(Duration::from_secs(4), || {
        std::fs::read_to_string(tmux.tmp.join("agenmux-rows"))
            .unwrap_or_default()
            .lines()
            .any(|line| line.starts_with(&format!("{stale}\t")))
    });
    assert_success(tmux.bin(&["key", "down", &client]), "select stale pane");
    tmux.wait_for(Duration::from_secs(4), || selected() == stale);
    send_sequence("dd");
    tmux.assert_tmux(&["kill-pane", "-t", &stale]);
    assert_success(
        tmux.bin(&["key", "text-79", &client]),
        "confirm stale pane delete",
    );
    tmux.wait_for(Duration::from_secs(4), || selected() == initial);
    assert!(
        tmux.text(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|pane| pane == initial),
        "stale target must not fall back to the active pane"
    );

    let immediate = tmux.text(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &initial,
        "exec sleep 60",
    ]);
    tmux.assert_tmux(&["select-pane", "-t", &immediate]);
    tmux.wait_for(Duration::from_secs(4), || selected() == immediate);
    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false\n[tmux_management]\nenabled=true\nconfirm_delete=false",
    );
    assert_success(tmux.bin(&["config", "reload"]), "disable confirmation");
    thread::sleep(Duration::from_millis(2200));
    send_sequence("dd");
    tmux.wait_for(Duration::from_secs(4), || {
        !tmux
            .text(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|pane| pane == immediate)
    });
    assert_on_sidebar();
    tmux.wait_for(Duration::from_secs(4), || selected() == initial);

    let protected = tmux.text(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &initial,
        "exec sleep 60",
    ]);
    tmux.assert_tmux(&["select-pane", "-t", &protected]);
    tmux.wait_for(Duration::from_secs(4), || selected() == protected);
    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false\n[tmux_management]\nenabled=true\nconfirm_delete=true",
    );
    assert_success(tmux.bin(&["config", "reload"]), "enable confirmation");
    thread::sleep(Duration::from_millis(2200));
    send_sequence("dd");
    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false\n[tmux_management]\nenabled=false",
    );
    assert_success(
        tmux.bin(&["config", "reload"]),
        "disable management during confirmation",
    );
    tmux.wait_for(Duration::from_secs(4), || {
        !tmux
            .text(&["capture-pane", "-p", "-t", &sidebar])
            .contains("? y/N")
    });
    assert_success(
        tmux.bin(&["key", "text-79"]),
        "ignore confirmation after disable",
    );
    assert!(
        tmux.text(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|pane| pane == protected),
        "management disable must cancel an open mutation"
    );

    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false\n[tmux_management]\nenabled=true\nconfirm_delete=true",
    );
    assert_success(tmux.bin(&["config", "reload"]), "re-enable management");
    thread::sleep(Duration::from_millis(2200));
    send_sequence("d");
    thread::sleep(Duration::from_millis(150));
    assert!(
        tmux.text(&["capture-pane", "-p", "-t", &sidebar])
            .contains("d delete selected"),
        "delete prefix should be pending before override"
    );
    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false\n[tmux_management]\nenabled=true\nconfirm_delete=true\n[keys.normal]\ndown=['d']",
    );
    assert_success(tmux.bin(&["config", "reload"]), "override pending prefix");
    tmux.wait_for(Duration::from_secs(4), || {
        !tmux
            .text(&["capture-pane", "-p", "-t", &sidebar])
            .contains("delete selected")
    });
    assert_success(
        tmux.bin(&["key", "sequence-64", &client]),
        "ignore continuation after prefix override",
    );
    assert!(
        tmux.text(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|pane| pane == protected),
        "a live prefix override must clear the pending sequence"
    );
    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false\n[tmux_management]\nenabled=true\nconfirm_delete=true",
    );
    assert_success(tmux.bin(&["config", "reload"]), "remove prefix override");
    thread::sleep(Duration::from_millis(2200));
    send_sequence("d");
    thread::sleep(Duration::from_millis(150));
    let pending = tmux.text(&["capture-pane", "-p", "-t", &sidebar]);
    assert!(pending.contains("d delete selected"), "{pending:?}");
    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false\n[tmux_management]\nenabled=false",
    );
    assert_success(tmux.bin(&["config", "reload"]), "disable management");
    tmux.wait_for(Duration::from_secs(4), || {
        !tmux
            .text(&["capture-pane", "-p", "-t", &sidebar])
            .contains("delete selected")
    });
    assert_success(
        tmux.bin(&["key", "sequence-64", &client]),
        "ignored disabled delete continuation",
    );
    thread::sleep(Duration::from_millis(150));
    assert!(
        tmux.text(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .any(|pane| pane == initial),
        "disabled management must not mutate"
    );

    assert_success(tmux.bin(&["key", "close"]), "close management daemon");
    let _ = viewer.kill();
    let _ = viewer.wait();
}

#[test]
fn split_sidebar_redraws_after_pane_resize_before_the_next_periodic_tick() {
    let tmux = TestTmux::new("resize-frame");
    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\nshow_frame=false\nsidebar_width=30\n[behavior]\nnotifications=false",
    );
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    let content = tmux.text(&["display-message", "-p", "#{pane_id}"]);
    let sibling = tmux.text(&[
        "split-window",
        "-h",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &content,
        "exec sleep 3600",
    ]);
    tmux.assert_tmux(&["select-pane", "-t", &sibling, "-T", "stable sibling"]);
    tmux.assert_tmux(&[
        "select-pane",
        "-t",
        &content,
        "-T",
        "stable geometry title long enough to clip",
    ]);

    let mut viewer = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "start resize test sidebar",
    );
    let sidebar = tmux.text(&[
        "list-panes",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}",
    ]);
    let width = || tmux.text(&["display-message", "-p", "-t", &sidebar, "#{pane_width}"]);
    let frame = || tmux.text(&["capture-pane", "-p", "-t", &sidebar]);
    let wide_title = "stable geometry title l";
    tmux.wait_for(Duration::from_secs(4), || {
        width() == "30" && frame().contains(wide_title)
    });

    tmux.assert_tmux(&["resize-pane", "-t", &sidebar, "-x", "18"]);
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&["show-option", "-gqv", "@agenmux-width"]) == "18"
    });
    thread::sleep(Duration::from_millis(100));
    assert!(
        !frame().contains(wide_title),
        "narrow render should clip the long title"
    );

    // `@agenmux-width` is adopted only during periodic reconciliation. Keep it
    // at 18 while waiting for the restored frame to prove the layout event
    // redraws before the next tick.
    tmux.assert_tmux(&["resize-pane", "-t", &sidebar, "-x", "30"]);
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut restored_frame = frame();
    let mut pane_width = width();
    let mut configured_width = tmux.text(&["show-option", "-gqv", "@agenmux-width"]);
    while !(pane_width == "30" && restored_frame.contains(wide_title) && configured_width == "18")
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(20));
        restored_frame = frame();
        pane_width = width();
        configured_width = tmux.text(&["show-option", "-gqv", "@agenmux-width"]);
    }
    let restored =
        pane_width == "30" && restored_frame.contains(wide_title) && configured_width == "18";
    assert!(
        restored,
        "sidebar geometry did not redraw before periodic reconciliation: pane_width={pane_width}, @agenmux-width={configured_width}, frame_contains_wide_title={}, frame:\n{restored_frame}",
        restored_frame.contains(wide_title)
    );

    assert_success(
        tmux.bin(&["key", "close", &client]),
        "close resize test sidebar",
    );
    let _ = viewer.kill();
    let _ = viewer.wait();
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
    // Killing the sidebar's neighbour hands it the freed columns; the daemon
    // gives them back instead of reading the growth as a drag.
    let neighbour = tmux.text(&[
        "list-panes",
        "-f",
        "#{!=:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}",
    ]);
    tmux.assert_tmux(&["split-window", "-h", "-t", &neighbour]);
    thread::sleep(Duration::from_millis(2500)); // past one periodic tick
    assert_eq!(width(), "35");
    tmux.assert_tmux(&["kill-pane", "-t", &neighbour]);
    tmux.wait_for(Duration::from_millis(1500), || width() == "35");
    thread::sleep(Duration::from_millis(500));
    assert_eq!(tmux.text(&["show-option", "-gqv", "@agenmux-width"]), "35");
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
        tmux.assert_tmux(&[
            "set-hook",
            "-g",
            "after-select-window[42]",
            "display-message 'synthetic' ; display-message 'second'",
        ]);
        tmux.assert_tmux(&[
            "set-hook",
            "-g",
            "after-select-window[99]",
            "display-message untouched",
        ]);
        if activation {
            tmux.assert_tmux(&[
                "set-option",
                "-g",
                "@agenmux-bin",
                env!("CARGO_BIN_EXE_agenmux"),
            ]);
        }
        let panes_before = tmux.text(&["list-panes", "-F", "#{pane_id}"]);
        let layout_before = tmux.text(&["display-message", "-p", "#{window_layout}"]);
        let options_before = tmux.text(&["show-options", "-g"]);
        let windows_before = tmux.text(&["show-options", "-w"]);
        let hooks_before = tmux.text(&["show-hooks", "-g"]);
        let window_hooks_before = tmux.text(&["show-hooks", "-gw"]);
        let before = tmux.text(&["list-keys"]);
        assert_eq!(
            tmux.text(&["show-options", "-gq", "@agenmux-prefix-owned"]),
            ""
        );
        app_file(&tmux, "[behavior]\nhide_windows='hidden*'");
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
            .bin_command(if activation {
                &["toggle", "split"]
            } else {
                &["setup"]
            })
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
            for slot in [
                "restore agenmux",
                "@agenmux-runtime-bin",
                "status-left",
                "after-select-window[42]",
            ] {
                assert!(error.contains(slot), "{error}");
            }
            // Later restores still execute after binding, identity, status and hook errors.
            assert_eq!(
                tmux.text(&["show-option", "-gqv", "status-right"]),
                "#{agents_mon}"
            );
            assert_eq!(tmux.text(&["show-options", "-w"]), windows_before);
            assert_eq!(
                tmux.text(&["show-options", "-gq", "@agenmux-plugin-dir"]),
                "@agenmux-plugin-dir ''"
            );
        } else {
            assert!(!error.contains("rollback failed"), "{error}");
            if activation {
                for name in ["agenmux-keys", "agenmux-rows", "agenmux-scan-cache"] {
                    assert!(!tmux.tmp.join(name).exists(), "{name}");
                }
                tmux.wait_for(Duration::from_secs(3), || {
                    tmux.text(&["list-clients", "-F", "#{client_name}"])
                        .is_empty()
                });
            }
            assert_eq!(tmux.text(&["list-panes", "-F", "#{pane_id}"]), panes_before);
            assert_eq!(
                tmux.text(&["display-message", "-p", "#{window_layout}"]),
                layout_before
            );
            assert_eq!(tmux.text(&["show-options", "-g"]), options_before);
            assert_eq!(tmux.text(&["show-options", "-w"]), windows_before);
            assert_eq!(tmux.text(&["show-hooks", "-g"]), hooks_before);
            assert_eq!(tmux.text(&["show-hooks", "-gw"]), window_hooks_before);
            tmux.assert_keys_unchanged(&before);
            // Notes are deliberately not compared: `list-keys -F` is newer than
            // tmux 3.4, so the snapshot reads the default output, which omits
            // them. The keys above still carry notes to prove a noted binding
            // round-trips its key, flags and command intact.
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
fn scan_keeps_inventory_separate_from_agent_output() {
    use std::os::unix::fs::PermissionsExt;

    let tmux = TestTmux::new("scan-inventory");
    let ordinary = tmux.text(&["display-message", "-p", "#{pane_id}"]);
    let codex = tmux.tmp.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nwhile :; do sleep 60; done\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let agent = tmux.text(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-n",
        "agent",
        codex.to_str().unwrap(),
    ]);
    let sidebar = tmux.text(&[
        "split-window",
        "-I",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &agent,
    ]);
    tmux.assert_tmux(&["select-pane", "-t", &sidebar, "-T", "agenmux"]);
    let marked_sidebar = tmux.text(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &ordinary,
        "exec sleep 60",
    ]);
    tmux.assert_tmux(&["set-option", "-p", "-t", &marked_sidebar, "@agenmux", "1"]);
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux.bin(&["scan"]).stdout.is_empty()
    });

    let debug = tmux.tmp.join("scan-debug");
    let scan = tmux
        .bin_command(&["scan"])
        .env("AGENMUX_DEBUG", &debug)
        .output()
        .unwrap();
    assert_success(scan.clone(), "agent scan");
    let list = tmux.bin(&["list"]);
    assert_success(list.clone(), "agent list");
    assert_eq!(scan.stdout, list.stdout);
    let rows = String::from_utf8(scan.stdout).unwrap();
    let fields = rows.trim_end().split('\t').collect::<Vec<_>>();
    assert_eq!(fields.len(), 6, "{rows:?}");
    assert_eq!(fields[0], agent);
    assert_eq!(fields[2], "codex");
    assert!(!rows.contains(&ordinary));
    assert!(!rows.contains(&sidebar));
    assert!(!rows.contains(&marked_sidebar));

    let debug = std::fs::read_to_string(debug).unwrap();
    assert!(debug.contains("# snapshot panes=2 agents=1"), "{debug}");
    assert_eq!(
        String::from_utf8(tmux.bin(&["status"]).stdout).unwrap(),
        "#[fg=green]⣿#[default]1"
    );
}

#[test]
fn all_panes_reload_preserves_daemon_and_selection() {
    use std::os::unix::fs::PermissionsExt;

    let tmux = TestTmux::new("all-panes-reload");
    tmux.assert_tmux(&["rename-window", "-t", "plugin:0", "ordinary-single"]);
    let single = tmux.text(&["display-message", "-p", "-t", "plugin:0", "#{pane_id}"]);
    let codex = tmux.tmp.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nwhile :; do sleep 60; done\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let agent = tmux.text(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "plugin:",
        "-n",
        "mixed",
        codex.to_str().unwrap(),
    ]);
    let ordinary = tmux.text(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "plugin:mixed",
        "exec sleep 60",
    ]);
    let ordinary_only = tmux.text(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "ordinary-only",
        "-x",
        "120",
        "-y",
        "40",
        "exec sleep 60",
    ]);
    app_file(
        &tmux,
        "[display]\nshow_all_panes=false\n[behavior]\nnotifications=false",
    );
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    let debug = tmux.tmp.join("reload-debug");
    let mut viewer = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);
    assert_success(
        tmux.bin_command(&["toggle", "split", &client])
            .env("AGENMUX_DEBUG", &debug)
            .output()
            .unwrap(),
        "start all-pane reload daemon",
    );
    tmux.wait_for(Duration::from_secs(5), || {
        tmux.text(&["list-panes", "-a", "-F", "#{pane_title}\t#{pane_pid}"])
            .lines()
            .filter(|line| *line == "agenmux\t0")
            .count()
            == 1
            && std::fs::read_to_string(tmux.tmp.join("agenmux-scan-cache"))
                .unwrap_or_default()
                .contains(&agent)
    });
    let assert_agent_only_cache = || {
        let cache = std::fs::read_to_string(tmux.tmp.join("agenmux-scan-cache")).unwrap();
        let lines = cache.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1, "{cache}");
        assert_eq!(lines[0].split('\t').count(), 6, "{cache}");
        assert!(lines[0].starts_with(&format!("{agent}\t")), "{cache}");
        for pane in [&single, &ordinary, &ordinary_only] {
            assert!(!cache.contains(pane), "{cache}");
        }
    };
    assert_agent_only_cache();
    // Hidden windows receive their processless sidebar lazily on first visit.
    tmux.assert_tmux(&["switch-client", "-c", &client, "-t", &agent]);
    tmux.wait_for(Duration::from_secs(5), || {
        !tmux
            .text(&[
                "list-panes",
                "-t",
                "plugin:mixed",
                "-f",
                "#{==:#{pane_title},agenmux}",
                "-F",
                "#{pane_id}",
            ])
            .is_empty()
    });

    let sidebar = tmux.text(&[
        "list-panes",
        "-t",
        "plugin:mixed",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}",
    ]);
    tmux.assert_tmux(&["switch-client", "-c", &client, "-t", &sidebar]);
    tmux.assert_tmux(&["switch-client", "-c", &client, "-T", "agenmux"]);
    let capture = || tmux.text(&["capture-pane", "-p", "-t", &sidebar]);
    let selected = || {
        std::fs::read_to_string(tmux.tmp.join("agenmux-rows"))
            .unwrap_or_default()
            .lines()
            .find_map(|line| {
                let fields = line.split('\t').collect::<Vec<_>>();
                (fields.get(2) == Some(&"1")).then(|| fields[0].to_owned())
            })
            .unwrap_or_default()
    };
    let inventory_present = || {
        let rows = std::fs::read_to_string(tmux.tmp.join("agenmux-rows")).unwrap_or_default();
        [&single, &ordinary, &ordinary_only].iter().all(|pane| {
            rows.lines()
                .any(|line| line.split('\t').next() == Some(pane.as_str()))
        })
    };
    tmux.wait_for(Duration::from_secs(5), || selected() == agent);
    let false_frame = capture();
    assert!(!false_frame.contains("ordinary-only"), "{false_frame}");
    assert!(!false_frame.contains(&ordinary), "{false_frame}");

    let control = tmux.text(&["show-option", "-gqv", "@agenmux-control-client"]);
    let daemon = tmux.text(&[
        "list-clients",
        "-f",
        &format!("#{{==:#{{client_name}},{control}}}"),
        "-F",
        "#{client_pid}",
    ]);
    let sidebars = tmux.text(&[
        "list-panes",
        "-a",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}\t#{pane_pid}",
    ]);
    assert!(!control.is_empty() && !daemon.is_empty());
    assert_eq!(sidebars.lines().count(), 2, "{sidebars}");
    assert!(
        sidebars.lines().all(|line| line.ends_with("\t0")),
        "{sidebars}"
    );

    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false",
    );
    assert_success(tmux.bin(&["config", "reload"]), "enable all panes");
    tmux.wait_for(Duration::from_secs(8), &inventory_present);
    tmux.wait_for(Duration::from_secs(8), || capture().contains("mixed"));
    let row_map = std::fs::read_to_string(tmux.tmp.join("agenmux-rows")).unwrap();
    for pane in [&single, &ordinary, &ordinary_only] {
        assert_eq!(
            row_map
                .lines()
                .filter(|line| line.split('\t').next() == Some(pane.as_str()))
                .count(),
            1,
            "ordinary pane {pane} missing or duplicated: {row_map}"
        );
    }
    for sidebar in sidebars.lines().filter_map(|line| line.split('\t').next()) {
        assert!(
            !row_map.lines().any(|line| line.starts_with(sidebar)),
            "{row_map}"
        );
    }
    assert_eq!(selected(), agent);
    assert_eq!(
        tmux.text(&["show-option", "-gqv", "@agenmux-control-client"]),
        control
    );
    assert_eq!(
        tmux.text(&[
            "list-clients",
            "-f",
            &format!("#{{==:#{{client_name}},{control}}}"),
            "-F",
            "#{client_pid}",
        ]),
        daemon
    );
    assert_eq!(
        tmux.text(&[
            "list-panes",
            "-a",
            "-f",
            "#{==:#{pane_title},agenmux}",
            "-F",
            "#{pane_id}\t#{pane_pid}",
        ]),
        sidebars
    );

    assert_agent_only_cache();

    app_file(&tmux, "invalid");
    let invalid = tmux.bin(&["config", "reload"]);
    assert_eq!(invalid.status.code(), Some(2));
    thread::sleep(Duration::from_millis(2200));
    assert!(
        inventory_present(),
        "all-pane projection changed after invalid reload"
    );
    assert_eq!(selected(), agent);
    assert_agent_only_cache();

    app_file(
        &tmux,
        "[display]\nshow_all_panes=false\n[behavior]\nnotifications=false",
    );
    assert_success(tmux.bin(&["config", "reload"]), "disable all panes");
    tmux.wait_for(Duration::from_secs(3), || !inventory_present());
    tmux.wait_for(Duration::from_secs(3), || !capture().contains(&ordinary));
    assert_eq!(selected(), agent);
    assert_agent_only_cache();
    assert_eq!(
        tmux.text(&["show-option", "-gqv", "@agenmux-control-client"]),
        control
    );
    assert_eq!(
        tmux.text(&[
            "list-clients",
            "-f",
            &format!("#{{==:#{{client_name}},{control}}}"),
            "-F",
            "#{client_pid}",
        ]),
        daemon
    );
    assert_eq!(
        tmux.text(&[
            "list-panes",
            "-a",
            "-f",
            "#{==:#{pane_title},agenmux}",
            "-F",
            "#{pane_id}\t#{pane_pid}",
        ]),
        sidebars
    );
    let debug = std::fs::read_to_string(debug).unwrap();
    assert!(debug.contains("# scan "), "{debug}");
    for line in debug
        .lines()
        .filter(|line| line.contains("# notification "))
    {
        assert!(
            line.contains(&agent),
            "ordinary pane reached tracker events: {line}"
        );
    }

    tmux.assert_tmux(&["kill-pane", "-t", &agent]);
    tmux.wait_for(Duration::from_secs(5), || capture().contains("no agents"));
    assert!(std::fs::read_to_string(tmux.tmp.join("agenmux-scan-cache"))
        .unwrap_or_default()
        .is_empty());

    assert_success(tmux.bin(&["key", "close"]), "close all-pane reload daemon");
    let _ = viewer.kill();
    let _ = viewer.wait();
}

#[test]
fn sidebar_refresh_uses_one_content_enumeration() {
    let tmux = TestTmux::new("scan-query-count");
    let debug = tmux.tmp.join("sidebar-debug");
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);
    let started = tmux
        .bin_command(&["toggle", "split"])
        .env("AGENMUX_DEBUG", &debug)
        .output()
        .unwrap();
    assert_success(started, "start debug sidebar");
    tmux.wait_for(Duration::from_secs(8), || {
        std::fs::read_to_string(&debug)
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(" # scan "))
            .count()
            >= 2
    });
    assert_success(tmux.bin(&["key", "close"]), "close debug sidebar");
    tmux.wait_for(Duration::from_secs(4), || {
        tmux.text(&["show-option", "-gqv", "@agenmux-control-client"])
            .is_empty()
    });

    let debug = std::fs::read_to_string(debug).unwrap();
    let completed = debug
        .lines()
        .filter(|line| line.contains(" # scan "))
        .count();
    let content_queries = debug
        .lines()
        .filter(|line| {
            line.contains("ms list-panes -a -F ") && !line.contains("ms list-panes -a -f '")
        })
        .count();
    let mirror_queries = debug
        .lines()
        .filter(|line| line.contains("ms list-panes -a -f '"))
        .count();
    assert!(completed >= 2, "{debug}");
    assert!(mirror_queries > 0, "{debug}");
    assert_eq!(completed, content_queries, "{debug}");
}

#[test]
fn startup_ack_waits_for_focused_live_frame() {
    use std::os::unix::fs::PermissionsExt;

    let tmux = TestTmux::new("focused-startup");
    tmux.assert_tmux(&["rename-window", "-t", "plugin:0", "ordinary"]);
    let codex = tmux.tmp.join("codex");
    std::fs::write(&codex, "#!/bin/sh\nwhile :; do sleep 60; done\n").unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let agent = tmux.text(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "plugin:",
        "-n",
        "focused",
        codex.to_str().unwrap(),
    ]);
    tmux.assert_tmux(&["select-pane", "-t", &agent, "-T", "codex"]);
    let plugin_dir = tmux.tmp.join("plugin");
    std::fs::create_dir_all(plugin_dir.join("agents")).unwrap();
    std::fs::write(
        plugin_dir.join("agents/codex.conf"),
        "AGENT_BINS='codex'\nSUBJECT_CMD='sleep 3; printf slow'\n",
    )
    .unwrap();
    tmux.assert_tmux(&[
        "new-window",
        "-d",
        "-t",
        "plugin:",
        "-n",
        "tail",
        "exec sleep 60",
    ]);
    app_file(
        &tmux,
        "[display]\nshow_all_panes=true\n[behavior]\nnotifications=false",
    );
    tmux.assert_tmux(&[
        "set-option",
        "-g",
        "@agenmux-bin",
        env!("CARGO_BIN_EXE_agenmux"),
    ]);

    let mut viewer = tmux.attach();
    tmux.wait_for(Duration::from_secs(2), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    let client = tmux.text(&["list-clients", "-F", "#{client_name}"]);
    tmux.assert_tmux(&["switch-client", "-c", &client, "-t", &agent]);
    let focused_window = tmux.text(&["display-message", "-p", "-t", &agent, "#{window_id}"]);

    let output = tmux
        .bin_command(&["toggle", "split", &client])
        .env("AGENMUX_DIR", &plugin_dir)
        .output()
        .unwrap();
    assert_success(output, "start focused sidebar");

    let sidebars = tmux.text(&[
        "list-panes",
        "-a",
        "-f",
        "#{==:#{pane_title},agenmux}",
        "-F",
        "#{pane_id}\t#{window_id}\t#{pane_pid}",
    ]);
    let rows = sidebars.lines().collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "{sidebars}");
    let fields = rows[0].split('\t').collect::<Vec<_>>();
    assert_eq!(fields.get(1), Some(&focused_window.as_str()), "{sidebars}");
    assert_eq!(fields.get(2), Some(&"0"), "{sidebars}");
    let pane = fields[0];
    let frame = tmux.text(&["capture-pane", "-p", "-t", pane]);
    assert!(
        frame.contains("slow"),
        "focused sidebar lacked live row: {frame:?}"
    );
    let row_map = std::fs::read_to_string(tmux.tmp.join("agenmux-rows")).unwrap();
    assert!(row_map.contains(&agent), "{row_map}");
    assert_success(tmux.bin(&["key", "close"]), "close focused sidebar");
    let _ = viewer.kill();
    let _ = viewer.wait();
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
                quote(
                    &tmux
                        .tmp
                        .join("config/agenmux/config.toml")
                        .to_string_lossy()
                ),
                quote(env!("CARGO_BIN_EXE_agenmux"))
            ),
            _ => "exec sleep 60".to_owned(),
        };
        if mode != "missing" {
            std::fs::write(
                &bin,
                format!(
                    "#!/bin/sh\necho $$ > {}\n{body}\n",
                    quote(&tmux.tmp.join("child-pid").to_string_lossy())
                ),
            )
            .unwrap();
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
        if mode == "timeout" {
            assert!(error.contains("timed out"), "{error}");
        }
        assert_eq!(
            tmux.text(&["list-panes", "-F", "#{pane_id}"]),
            before,
            "{mode}"
        );
        assert_eq!(
            tmux.text(&["display-message", "-p", "#{window_layout}"]),
            layout,
            "{mode}"
        );
        for name in [
            "@agenmux-on",
            "@agenmux-control-client",
            "@agenmux-runtime-dir",
        ] {
            assert_eq!(
                tmux.text(&["show-options", "-gq", name]),
                "",
                "{mode}: {name}"
            );
        }
        assert!(!tmux.tmp.join("agenmux-keys").exists());
        if mode != "missing" {
            let pid: i32 = std::fs::read_to_string(tmux.tmp.join("child-pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "startup child survived: {mode}"
            );
        }
    }
}

#[test]
fn public_popup_launch_errors_remove_pin_and_jump() {
    let tmux = TestTmux::new("popup-failure");
    let mut client = tmux.attach();
    tmux.wait_for(Duration::from_secs(3), || {
        !tmux
            .text(&["list-clients", "-F", "#{client_name}"])
            .is_empty()
    });
    for owner in [Some("missing-client"), None] {
        tmux.assert_tmux(&[
            "set-option",
            "-g",
            "@agenmux-bin",
            "/synthetic/missing-executable",
        ]);
        std::fs::write(tmux.tmp.join("agenmux-pin.jump"), "synthetic").unwrap();
        let args = if let Some(owner) = owner {
            vec!["toggle", "popup", owner]
        } else {
            vec!["toggle", "popup"]
        };
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
    assert!(first.starts_with("16."), "{first}");
    assert!(tmux.binding("agenmux", "n").contains("agenmux.down"));
    assert!(tmux.binding("agenmux", "j").is_empty());

    // Same session, edited file: activation alone must replace the tables.
    app_file(&tmux, "[keys.normal]\ndown = ['x']\n");
    assert_success(
        tmux.bin(&["toggle", "split", &client]),
        "reopen with the second keymap",
    );
    let second = tmux.text(&["show-option", "-gqv", "@agenmux-nav-version"]);
    assert_ne!(first, second, "fingerprint did not follow the keymap");
    assert!(tmux.binding("agenmux", "x").contains("agenmux.down"));
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
