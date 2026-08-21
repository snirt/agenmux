use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "agents-mon-release-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/usr/bin/env bash\n{body}\n")).unwrap();
    let mut mode = fs::metadata(path).unwrap().permissions();
    mode.set_mode(0o755);
    fs::set_permissions(path, mode).unwrap();
}

fn command(plugin: &Path, bin_dir: &Path, args: &[&str]) -> Command {
    let path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_agents-mon"));
    command
        .args(args)
        .env("AGENTS_MON_DIR", plugin)
        .env("AGENTS_MON_REPO", "https://example.invalid/repo")
        .env("PATH", path);
    command
}

fn run(plugin: &Path, bin_dir: &Path, args: &[&str]) -> Output {
    command(plugin, bin_dir, args).output().unwrap()
}

fn git(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap()
}

fn git_ok(repo: &Path, args: &[&str]) {
    let out = git(repo, args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn real_git() -> String {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn write_release_tree(repo: &Path, version: &str, include_toggle: bool) {
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        format!("[package]\nname = \"agents-mon\"\nversion = \"{version}\"\n"),
    )
    .unwrap();
    fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    script(
        &repo.join("agents-mon.tmux"),
        r#"printf 'entrypoint\n' >> "$RESTART_LOG""#,
    );
    let install = format!(
        r#"write_bin() {{
  cat > "$1" <<'BIN'
#!/usr/bin/env bash
if [ "${{1:-}}" = --version ]; then printf 'agents-mon {version}\n'; elif [ "${{1:-}}" = toggle ]; then printf 'native-toggle\n' >> "$RESTART_LOG"; fi
BIN
  chmod +x "$1"
}}
if [ "${{1:-}}" = fetch ]; then
  pkg="$3/tmux-agents-mon-test"
  mkdir -p "$pkg/target/release"
  write_bin "$pkg/target/release/agents-mon"
  printf '%s\n' "$pkg"
  exit 0
fi
mkdir -p "$DIR/../target/release"
write_bin "$DIR/../target/release/agents-mon"
printf 'v{version}\n%s\n' "$(git -C "$DIR/.." rev-parse HEAD 2>/dev/null || printf -)" > "$DIR/../target/release/.agents-mon-version""#
    );
    script(
        &repo.join("scripts/install-bin.sh"),
        &format!("DIR=\"$(cd \"$(dirname \"$0\")\" && pwd)\"\n{install}"),
    );
    if include_toggle {
        script(
            &repo.join("scripts/toggle.sh"),
            r#"printf 'legacy-toggle\n' >> "$RESTART_LOG""#,
        );
    } else {
        let _ = fs::remove_file(repo.join("scripts/toggle.sh"));
    }
}

fn make_git_releases(repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    git_ok(repo, &["init", "-q"]);
    git_ok(repo, &["config", "user.email", "test@example.com"]);
    git_ok(repo, &["config", "user.name", "Test"]);
    write_release_tree(repo, "0.1.0", true);
    git_ok(repo, &["add", "-A"]);
    git_ok(repo, &["commit", "-qm", "old"]);
    git_ok(repo, &["tag", "v0.1.0"]);
    write_release_tree(repo, "0.1.1", true);
    git_ok(repo, &["add", "-A"]);
    git_ok(repo, &["commit", "-qm", "new"]);
    git_ok(repo, &["tag", "v0.1.1"]);
}

fn make_wrong_engine_release(repo: &Path) {
    git_ok(repo, &["checkout", "-q", "v0.1.0"]);
    script(
        &repo.join("scripts/install-bin.sh"),
        r#"if [ "${1:-}" = fetch ]; then
  pkg="$3/tmux-agents-mon-wrong"
  mkdir -p "$pkg/target/release"
  cat > "$pkg/target/release/agents-mon" <<'BIN'
#!/usr/bin/env bash
[ "${1:-}" = --version ] && printf 'agents-mon 9.9.9\n'
BIN
  chmod +x "$pkg/target/release/agents-mon"
  printf '%s\n' "$pkg"
  exit 0
fi
exit 1"#,
    );
    git_ok(repo, &["add", "scripts/install-bin.sh"]);
    git_ok(repo, &["commit", "--amend", "-qm", "old wrong engine"]);
    git_ok(repo, &["tag", "-f", "v0.1.0"]);
}

fn no_server_tmux(bin_dir: &Path) {
    script(
        &bin_dir.join("tmux"),
        r#"[ "$1" = info ] && exit 1
exit 0"#,
    );
}

#[test]
fn refresh_records_latest_and_published_tags() {
    let tmp = TempDir::new("refresh");
    let plugin = tmp.path().join("plugin");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(plugin.join("target/release")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    script(
        &bin.join("curl"),
        r#"printf 'https://example.invalid/repo/releases/tag/v1.2.0'"#,
    );
    script(
        &bin.join("git"),
        r#"printf 'aaa\trefs/tags/v1.3.0\nbbb\trefs/tags/v1.2.0\nccc\trefs/tags/v1.1.9\n'"#,
    );

    let out = run(&plugin, &bin, &["releases", "refresh"]);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        fs::read_to_string(plugin.join("target/release/.agents-mon-latest")).unwrap(),
        "v1.2.0\n"
    );
    assert_eq!(
        fs::read_to_string(plugin.join("target/release/.agents-mon-tags")).unwrap(),
        "v1.2.0\nv1.1.9\n"
    );
}

#[test]
fn installer_refresh_delegates_to_native_release_command() {
    let tmp = TempDir::new("installer-refresh");
    let plugin = tmp.path().join("plugin");
    let scripts = plugin.join("scripts");
    let release = plugin.join("target/release");
    let log = tmp.path().join("args.log");
    fs::create_dir_all(&scripts).unwrap();
    fs::create_dir_all(&release).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    fs::copy(
        root.join("scripts/install-bin.sh"),
        scripts.join("install-bin.sh"),
    )
    .unwrap();
    script(
        &release.join("agents-mon"),
        r#"printf '%s\n' "$*" >> "$ARGS_LOG""#,
    );

    let refresh = Command::new("bash")
        .arg(scripts.join("install-bin.sh"))
        .arg("refresh")
        .env("ARGS_LOG", &log)
        .status()
        .unwrap();

    assert!(refresh.success());
    assert_eq!(fs::read_to_string(log).unwrap(), "releases refresh\n");
}

#[test]
fn git_update_switches_latest_and_refuses_dirty_or_unknown_targets() {
    let tmp = TempDir::new("git-switch");
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    make_git_releases(&repo);
    no_server_tmux(&bin);
    fs::create_dir_all(repo.join("target/release")).unwrap();
    fs::write(repo.join("target/release/.agents-mon-latest"), "v0.1.0\n").unwrap();

    let back = run(&repo, &bin, &["update", "latest"]);
    assert!(
        back.status.success(),
        "{}",
        String::from_utf8_lossy(&back.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&git(&repo, &["describe", "--tags", "--exact-match"]).stdout)
            .trim(),
        "v0.1.0"
    );
    assert_eq!(
        String::from_utf8_lossy(
            &Command::new(repo.join("target/release/agents-mon"))
                .arg("--version")
                .output()
                .unwrap()
                .stdout
        )
        .trim(),
        "agents-mon 0.1.0"
    );

    fs::write(repo.join("scratch"), "dirty\n").unwrap();
    let dirty = run(&repo, &bin, &["update", "v0.1.1"]);
    assert!(!dirty.status.success());
    assert_eq!(
        String::from_utf8_lossy(&git(&repo, &["describe", "--tags", "--exact-match"]).stdout)
            .trim(),
        "v0.1.0"
    );
    fs::remove_file(repo.join("scratch")).unwrap();

    let unknown = run(&repo, &bin, &["update", "v9.9.9"]);
    assert!(!unknown.status.success());
    let forward = run(&repo, &bin, &["update", "v0.1.1"]);
    assert!(forward.status.success());
    assert_eq!(
        String::from_utf8_lossy(&git(&repo, &["describe", "--tags", "--exact-match"]).stdout)
            .trim(),
        "v0.1.1"
    );
}

#[test]
fn git_status_errors_refuse_to_touch_the_checkout() {
    let tmp = TempDir::new("git-status-error");
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    make_git_releases(&repo);
    no_server_tmux(&bin);
    script(
        &bin.join("git"),
        r#"case "$*" in
  *" status --porcelain") exit 1 ;;
esac
exec "$REAL_GIT" "$@""#,
    );

    let out = command(&repo, &bin, &["update", "v0.1.0"])
        .env("REAL_GIT", real_git())
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&git(&repo, &["describe", "--tags", "--exact-match"]).stdout)
            .trim(),
        "v0.1.1"
    );
    assert_eq!(
        fs::read_to_string(repo.join("Cargo.toml")).unwrap(),
        "[package]\nname = \"agents-mon\"\nversion = \"0.1.1\"\n"
    );
}

#[test]
fn wrong_target_engine_restores_the_previous_git_source() {
    let tmp = TempDir::new("wrong-engine");
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    make_git_releases(&repo);
    no_server_tmux(&bin);

    make_wrong_engine_release(&repo);
    git_ok(&repo, &["checkout", "-q", "v0.1.1"]);

    let out = run(&repo, &bin, &["update", "v0.1.0"]);

    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&git(&repo, &["describe", "--tags", "--exact-match"]).stdout)
            .trim(),
        "v0.1.1"
    );
    assert_eq!(
        fs::read_to_string(repo.join("Cargo.toml")).unwrap(),
        "[package]\nname = \"agents-mon\"\nversion = \"0.1.1\"\n"
    );
}

#[test]
fn failed_git_update_restores_the_previous_branch() {
    let tmp = TempDir::new("restore-branch");
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    make_git_releases(&repo);
    no_server_tmux(&bin);
    let branch = String::from_utf8_lossy(
        &git(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).stdout,
    )
    .trim()
    .to_string();
    let revision = String::from_utf8_lossy(&git(&repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();
    make_wrong_engine_release(&repo);
    git_ok(&repo, &["checkout", "-q", &branch]);

    let out = run(&repo, &bin, &["update", "v0.1.0"]);

    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8_lossy(
            &git(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).stdout
        )
        .trim(),
        branch
    );
    assert_eq!(
        String::from_utf8_lossy(&git(&repo, &["rev-parse", "HEAD"]).stdout).trim(),
        revision
    );
}

#[test]
fn update_waits_for_old_daemon_then_reenters_the_target_release() {
    let tmp = TempDir::new("restart");
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    let log = tmp.path().join("restart.log");
    let polls = tmp.path().join("polls");
    fs::create_dir_all(&bin).unwrap();
    make_git_releases(&repo);
    script(
        &bin.join("tmux"),
        r#"printf 'tmux %s\n' "$*" >> "$RESTART_LOG"
case "$*" in
  info) exit 0 ;;
  "show-option -gqv @agents-mon-on") printf '1\n' ;;
  "show-option -gqv @agents-mon-control-client") printf 'old-control\n' ;;
  "list-clients -F #{client_name}")
    n="$(cat "$CLIENT_POLLS" 2>/dev/null || printf 0)"
    n=$((n + 1)); printf '%s\n' "$n" > "$CLIENT_POLLS"
    [ "$n" -lt 3 ] && printf 'old-control\n'
    ;;
esac
exit 0"#,
    );

    let out = command(&repo, &bin, &["update", "v0.1.0"])
        .env("RESTART_LOG", &log)
        .env("CLIENT_POLLS", &polls)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = fs::read_to_string(&log).unwrap();
    let last_poll = events.rfind("tmux list-clients -F #{client_name}").unwrap();
    let entry = events.find("entrypoint").unwrap();
    assert!(last_poll < entry, "{events}");
    assert!(events.contains("legacy-toggle"), "{events}");
    assert_eq!(fs::read_to_string(&polls).unwrap().trim(), "3");
}

#[test]
fn open_view_uses_native_toggle_when_target_has_no_legacy_script() {
    let tmp = TempDir::new("native-reopen");
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    let log = tmp.path().join("restart.log");
    fs::create_dir_all(&bin).unwrap();
    make_git_releases(&repo);
    write_release_tree(&repo, "0.2.0", false);
    git_ok(&repo, &["add", "-A"]);
    git_ok(&repo, &["commit", "-qm", "native"]);
    git_ok(&repo, &["tag", "v0.2.0"]);
    git_ok(&repo, &["checkout", "-q", "v0.1.1"]);
    script(
        &bin.join("tmux"),
        r#"case "$*" in
  info) exit 0 ;;
  "show-option -gqv @agents-mon-on") printf '1\n' ;;
esac
exit 0"#,
    );

    let out = command(&repo, &bin, &["update", "v0.2.0"])
        .env("RESTART_LOG", &log)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = fs::read_to_string(&log).unwrap();
    assert!(events.contains("entrypoint"));
    assert!(events.contains("native-toggle"));
    assert!(!events.contains("legacy-toggle"));
}

#[test]
fn closed_view_restarts_entrypoint_without_reopening() {
    let tmp = TempDir::new("closed");
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    let log = tmp.path().join("restart.log");
    fs::create_dir_all(&bin).unwrap();
    make_git_releases(&repo);
    script(
        &bin.join("tmux"),
        r#"case "$*" in
  info) exit 0 ;;
  "show-option -gqv @agents-mon-on"|"show-option -gqv @agents-mon-sidebar"|"show-option -gqv @agents-mon-control-client") ;;
esac
exit 0"#,
    );

    let out = command(&repo, &bin, &["update", "v0.1.0"])
        .env("RESTART_LOG", &log)
        .output()
        .unwrap();
    assert!(out.status.success());
    let events = fs::read_to_string(&log).unwrap();
    assert!(events.contains("entrypoint"));
    assert!(!events.contains("legacy-toggle"));
    assert!(!events.contains("native-toggle"));
}

#[test]
fn tarball_update_removes_stale_source_and_reopens_with_target_native_toggle() {
    let tmp = TempDir::new("tarball");
    let plugin = tmp.path().join("plugin");
    let bin = tmp.path().join("bin");
    let log = tmp.path().join("restart.log");
    fs::create_dir_all(plugin.join("scripts")).unwrap();
    fs::create_dir_all(plugin.join("target/release")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::write(
        plugin.join("Cargo.toml"),
        "[package]\nname = \"agents-mon\"\nversion = \"0.1.1\"\n",
    )
    .unwrap();
    fs::write(plugin.join("target/release/preserved"), "keep\n").unwrap();
    fs::write(
        plugin.join("target/release/.agents-mon-version"),
        "v0.1.1\nold\n",
    )
    .unwrap();
    script(
        &plugin.join("scripts/toggle.sh"),
        r#"printf 'stale-toggle\n' >> "$RESTART_LOG""#,
    );
    script(
        &plugin.join("scripts/install-bin.sh"),
        r#"DIR="$(cd "$(dirname "$0")/.." && pwd)"
if [ "${1:-}" = fetch ]; then
  pkg="$3/tmux-agents-mon-test"
  mkdir -p "$pkg/scripts" "$pkg/target/release"
  cat > "$pkg/Cargo.toml" <<'TOML'
[package]
name = "agents-mon"
version = "0.1.0"
TOML
  printf 'verified source\n' > "$pkg/source-marker"
  cp "$0" "$pkg/scripts/install-bin.sh"
  cat > "$pkg/agents-mon.tmux" <<'ENTRY'
#!/usr/bin/env bash
printf 'entrypoint\n' >> "$RESTART_LOG"
ENTRY
  cat > "$pkg/target/release/agents-mon" <<'BIN'
#!/usr/bin/env bash
if [ "${1:-}" = --version ]; then printf 'agents-mon 0.1.0\n'; elif [ "${1:-}" = toggle ]; then printf 'native-toggle\n' >> "$RESTART_LOG"; fi
BIN
  chmod +x "$pkg/target/release/agents-mon"
  printf '%s\n' "$pkg"
  exit 0
fi
exit 1"#,
    );
    script(
        &bin.join("tmux"),
        r#"case "$*" in
  info) exit 0 ;;
  "show-option -gqv @agents-mon-on") printf '1\n' ;;
esac
exit 0"#,
    );

    let out = command(&plugin, &bin, &["update", "v0.1.0"])
        .env("RESTART_LOG", &log)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!plugin.join("scripts/toggle.sh").exists());
    assert_eq!(
        fs::read_to_string(plugin.join("source-marker")).unwrap(),
        "verified source\n"
    );
    assert!(!plugin.join("target/release/preserved").exists());
    assert_eq!(
        fs::read_to_string(plugin.join("target/release/.agents-mon-version")).unwrap(),
        "v0.1.0\n-\n"
    );
    assert_eq!(
        Command::new(plugin.join("target/release/agents-mon"))
            .arg("--version")
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap(),
        "agents-mon 0.1.0"
    );
    let events = fs::read_to_string(log).unwrap();
    assert!(events.contains("entrypoint"));
    assert!(events.contains("native-toggle"));
    assert!(!events.contains("stale-toggle"));
}

#[test]
fn failed_source_copy_leaves_tarball_tree_untouched() {
    let tmp = TempDir::new("tarball-copy-failure");
    let plugin = tmp.path().join("plugin");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(plugin.join("scripts")).unwrap();
    fs::create_dir_all(plugin.join("target/release")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let manifest = "[package]\nname = \"agents-mon\"\nversion = \"0.1.1\"\n";
    fs::write(plugin.join("Cargo.toml"), manifest).unwrap();
    fs::write(plugin.join("stale-source"), "untouched\n").unwrap();
    fs::write(plugin.join("target/release/preserved"), "keep\n").unwrap();
    script(
        &plugin.join("scripts/toggle.sh"),
        r#"printf 'still here\n' >/dev/null"#,
    );
    script(
        &plugin.join("scripts/install-bin.sh"),
        r#"if [ "${1:-}" = fetch ]; then
  pkg="$3/tmux-agents-mon-test"
  mkdir -p "$pkg/scripts"
  printf '[package]\nname = "agents-mon"\nversion = "0.1.0"\n' > "$pkg/Cargo.toml"
  printf '#!/usr/bin/env bash\nexit 0\n' > "$pkg/scripts/install-bin.sh"
  printf '%s\n' "$pkg"
  exit 0
fi
exit 0"#,
    );
    no_server_tmux(&bin);
    script(&bin.join("cp"), "exit 1");

    let out = run(&plugin, &bin, &["update", "v0.1.0"]);

    assert!(!out.status.success());
    assert_eq!(
        fs::read_to_string(plugin.join("Cargo.toml")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(plugin.join("stale-source")).unwrap(),
        "untouched\n"
    );
    assert!(plugin.join("scripts/toggle.sh").is_file());
    assert_eq!(
        fs::read_to_string(plugin.join("target/release/preserved")).unwrap(),
        "keep\n"
    );
}

#[test]
fn failed_verified_fetch_leaves_tarball_tree_untouched() {
    let tmp = TempDir::new("tarball-fetch-failure");
    let plugin = tmp.path().join("plugin");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(plugin.join("scripts")).unwrap();
    fs::create_dir_all(plugin.join("target/release")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let manifest = "[package]\nname = \"agents-mon\"\nversion = \"0.1.1\"\n";
    fs::write(plugin.join("Cargo.toml"), manifest).unwrap();
    fs::write(plugin.join("stale-source"), "untouched\n").unwrap();
    fs::write(plugin.join("target/release/preserved"), "keep\n").unwrap();
    script(
        &plugin.join("scripts/install-bin.sh"),
        r#"[ "${1:-}" != fetch ]
exit 1"#,
    );
    script(
        &plugin.join("scripts/toggle.sh"),
        r#"printf 'still here\n' >/dev/null"#,
    );
    no_server_tmux(&bin);

    let out = run(&plugin, &bin, &["update", "v0.1.0"]);

    assert!(!out.status.success());
    assert_eq!(
        fs::read_to_string(plugin.join("Cargo.toml")).unwrap(),
        manifest
    );
    assert_eq!(
        fs::read_to_string(plugin.join("stale-source")).unwrap(),
        "untouched\n"
    );
    assert!(plugin.join("scripts/toggle.sh").is_file());
    assert_eq!(
        fs::read_to_string(plugin.join("target/release/preserved")).unwrap(),
        "keep\n"
    );
}
