use std::process::{Command, Output};

fn run_without_server(command: &str) -> Output {
    let socket = std::env::temp_dir().join(format!(
        "agenmux-no-server-{}-{command}",
        std::process::id()
    ));
    Command::new(env!("CARGO_BIN_EXE_agenmux"))
        .arg(command)
        .env("TMUX", format!("{},0,0", socket.display()))
        .output()
        .unwrap()
}

#[test]
fn version_comes_from_cargo_manifest() {
    let output = Command::new(env!("CARGO_BIN_EXE_agenmux"))
        .arg("--version")
        .output()
        .unwrap();

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("agenmux {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn scan_is_an_exact_alias_for_list_without_a_server() {
    let scan = run_without_server("scan");
    let list = run_without_server("list");

    assert_ne!(scan.status.code(), Some(2), "scan fell through to usage");
    assert_eq!(scan.status.code(), list.status.code());
    assert_eq!(scan.stdout, list.stdout);
    assert_eq!(scan.stderr, list.stderr);
}

#[test]
fn config_check_missing_file_needs_no_server() {
    let root = ConfigTest::new();
    let output = root.check();
    assert_eq!(output.status.code(), Some(0), "{:?}", output);
}

#[test]
fn shipped_example_config_passes_check() {
    let root = ConfigTest::new();
    std::fs::copy(
        concat!(env!("CARGO_MANIFEST_DIR"), "/examples/config.toml"),
        root.path(),
    )
    .unwrap();
    let output = root.check();
    assert_eq!(output.status.code(), Some(0), "{:?}", output);
}

#[test]
fn config_help_lists_every_configurable_key_without_a_server() {
    let root = ConfigTest::new();
    // PATH is an isolated empty directory: help must not need tmux.
    for args in [&["-h"][..], &["--help"], &[]] {
        let output = root.config(args);
        assert_eq!(output.status.code(), Some(0), "{args:?}");
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        // The shipped example spells out every supported key, and a separate
        // test proves it resolves to the defaults; so it is the drift check.
        let example = include_str!("../examples/config.toml");
        for key in example
            .lines()
            .filter_map(|line| line.split_once(" = "))
            .map(|(key, _)| key.trim().trim_start_matches("# "))
            .filter(|key| *key != "version")
        {
            assert!(text.contains(key), "help omits {key}");
        }
        for section in ["[display]", "[behavior]", "[theme]", "[keys.normal]", "[keys.search]"] {
            assert!(text.contains(section), "help omits {section}");
        }
        assert!(text.contains("config reload"), "help omits the reload command");
    }
}

#[test]
fn config_reload_needs_a_server_and_refuses_an_invalid_file() {
    let root = ConfigTest::new();
    // Reaching tmux is the only way to publish the signal, so a missing
    // server is a read failure, exactly like `check --effective`.
    let output = root.config(&["reload"]);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read application options"));

    // An invalid file is rejected before tmux is consulted at all.
    std::fs::write(root.path(), "[keys.normal]\ndown = ['C-a']\n").unwrap();
    let output = root.config(&["reload"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid configuration schema"), "{stderr}");
    assert!(!stderr.contains("C-a"), "diagnostic echoed the value: {stderr}");
}

#[test]
fn effective_check_requires_tmux_but_validates_the_file_first() {
    let root = ConfigTest::new();
    assert_eq!(root.check().status.code(), Some(0));
    // PATH is an isolated empty directory; plain checking does not need tmux.
    let output = root.command().arg("--effective").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot read application options"));
    std::fs::write(root.path(), "version = 2").unwrap();
    assert_eq!(
        root.command()
            .arg("--effective")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}

struct ConfigTest(std::path::PathBuf);
impl ConfigTest {
    fn new() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static ID: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "agenmux-config-cli-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("xdg/agenmux")).unwrap();
        Self(root)
    }
    fn path(&self) -> std::path::PathBuf {
        self.0.join("xdg/agenmux/config.toml")
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agenmux"));
        command
            .args(["config", "check"])
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("xdg"))
            .env("PATH", self.0.join("bin"))
            .env("AGENMUX_DIR", self.0.join("plugin"))
            .current_dir(&self.0);
        command
    }
    /// `config <args>` with the same isolated environment, for the
    /// subcommands that are not `check`.
    fn config(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_agenmux"));
        command
            .arg("config")
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("xdg"))
            .env("PATH", self.0.join("bin"))
            .env("AGENMUX_DIR", self.0.join("plugin"))
            .env("TMUX", format!("{}/no-server,0,0", self.0.display()))
            .current_dir(&self.0);
        command.output().unwrap()
    }
    fn check(&self) -> Output {
        use std::process::Stdio;
        let mut child = self
            .command()
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while child.try_wait().unwrap().is_none() {
            if start.elapsed() > std::time::Duration::from_secs(5) {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("config check blocked");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
    }
}
impl Drop for ConfigTest {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn config_check_exit_codes_and_safe_file_kinds() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let root = ConfigTest::new();
    for valid in ["", "version = 1", "[behavior]\nhide_windows = ''"] {
        std::fs::write(root.path(), valid).unwrap();
        assert_eq!(root.check().status.code(), Some(0));
    }
    for invalid in [
        b"version = 2".as_slice(),
        b"[theme]\ncommand = 'no'",
        b"\xff",
    ] {
        std::fs::write(root.path(), invalid).unwrap();
        let output = root.check();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(String::from_utf8(output.stderr)
            .unwrap()
            .contains("config.toml:"));
    }
    std::fs::write(root.path(), vec![b' '; 65537]).unwrap();
    assert_eq!(root.check().status.code(), Some(2));
    std::fs::remove_file(root.path()).unwrap();
    std::fs::create_dir(root.path()).unwrap();
    assert_eq!(root.check().status.code(), Some(2));
    std::fs::remove_dir(root.path()).unwrap();
    let fifo = std::ffi::CString::new(root.path().as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert_eq!(root.check().status.code(), Some(2));
    std::fs::remove_file(root.path()).unwrap();
    symlink(root.0.join("missing-target"), root.path()).unwrap();
    assert_eq!(root.check().status.code(), Some(1));
    std::fs::write(root.0.join("missing-target"), "version = 1").unwrap();
    assert_eq!(root.check().status.code(), Some(0));
}

#[test]
fn config_check_wrong_type_values_are_private() {
    let root = ConfigTest::new();
    for source in [
        "version = 'private-rejected-value-sentinel'",
        "[display]\nsidebar_width = 'private-rejected-value-sentinel'",
        "[behavior]\nnotifications = 'private-rejected-value-sentinel'",
        "[keys.search]\ncancel = 'private-rejected-value-sentinel'",
        "[theme]\nbase = 'private-rejected-value-sentinel'",
        "[behavior]\nnotifications = '''\n[private-rejected-value-sentinel]\nprivate-rejected-value-sentinel = value\n'''",
    ] {
        std::fs::write(root.path(), source).unwrap();
        let output = root.check();
        assert_eq!(output.status.code(), Some(2));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!stderr.contains("private-rejected-value-sentinel"));
        assert!(stderr.contains("config.toml:"));
        assert!(stderr.contains("line ") && stderr.contains("column "));
        assert!(stderr.contains("invalid configuration schema"));
    }
}

#[test]
fn config_check_without_root_never_searches_current_directory() {
    let root = ConfigTest::new();
    std::fs::write(root.0.join("config.toml"), "version = 99").unwrap();
    let output = root
        .command()
        .env_remove("HOME")
        .env("XDG_CONFIG_HOME", "relative")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("no absolute XDG or HOME root"));
    std::fs::create_dir_all(root.0.join(".config/agenmux")).unwrap();
    std::fs::write(root.0.join(".config/agenmux/config.toml"), "version = 99").unwrap();
    assert_eq!(
        root.command()
            .env("XDG_CONFIG_HOME", "")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}

#[test]
fn config_check_never_executes_subject_hooks_tmux_or_network_helpers() {
    use std::os::unix::fs::PermissionsExt;
    let root = ConfigTest::new();
    std::fs::create_dir(root.0.join("bin")).unwrap();
    for name in ["tmux", "bash", "sh", "curl", "wget", "git"] {
        let path = root.0.join("bin").join(name);
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf invoked > helper-marker\nexit 91\n",
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    for dir in [
        "plugin/agents",
        "xdg/agenmux/agents",
        "xdg/tmux-agents-mon/agents",
    ] {
        let dir = root.0.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("sample.conf"),
            "AGENT_BINS='sample'\nSUBJECT_CMD='printf executed > subject-marker'\n",
        )
        .unwrap();
    }
    for content in [
        None,
        Some("version = 1"),
        Some("[behavior]\nhide_windows = '#(touch injection-marker);${HOME}'"),
        Some("[theme]\ncommand = 'touch injection-marker'"),
    ] {
        if let Some(content) = content {
            std::fs::write(root.path(), content).unwrap();
        }
        let output = root.check();
        let expected = if content.is_some_and(|s| s.contains("[theme]")) {
            2
        } else {
            0
        };
        assert_eq!(output.status.code(), Some(expected), "{output:?}");
        for marker in ["helper-marker", "subject-marker", "injection-marker"] {
            assert!(
                !root.0.join(marker).exists(),
                "config check created {marker}"
            );
        }
    }
}

#[test]
fn broken_config_does_not_gate_version_or_key_delivery() {
    let root = ConfigTest::new();
    std::fs::write(root.path(), "version = 99").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_agenmux"))
        .arg("--version")
        .env_clear()
        .env("XDG_CONFIG_HOME", root.0.join("xdg"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let output = Command::new(env!("CARGO_BIN_EXE_agenmux"))
        .args(["key", "close"])
        .env_clear()
        .env("XDG_CONFIG_HOME", root.0.join("xdg"))
        .env("TMPDIR", &root.0)
        .env("PATH", root.0.join("bin"))
        .output()
        .unwrap();
    assert_ne!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("config.toml"));
}
