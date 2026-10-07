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
            "agenmux-release-{name}-{}-{}",
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_agenmux"));
    command
        .args(args)
        .env("AGENMUX_DIR", plugin)
        .env("AGENMUX_REPO", "https://example.invalid/repo")
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
        format!("[package]\nname = \"agenmux\"\nversion = \"{version}\"\n"),
    )
    .unwrap();
    fs::write(repo.join(".gitignore"), "target/\n").unwrap();
    script(
        &repo.join("agenmux.tmux"),
        r#"printf 'entrypoint\n' >> "$RESTART_LOG""#,
    );
    let install = format!(
        r#"write_bin() {{
  cat > "$1" <<'BIN'
#!/usr/bin/env bash
if [ "${{1:-}}" = --version ]; then printf 'agenmux {version}\n'; elif [ "${{1:-}}" = toggle ]; then printf 'native-toggle\n' >> "$RESTART_LOG"; fi
BIN
  chmod +x "$1"
}}
if [ "${{1:-}}" = fetch ]; then
  pkg="$3/agenmux-test"
  mkdir -p "$pkg/target/release"
  write_bin "$pkg/target/release/agenmux"
  sed -i.bak "s/agenmux {version}/agenmux ${{2#v}}/" "$pkg/target/release/agenmux"
  rm -f "$pkg/target/release/agenmux.bak"
  printf '%s\n' "$pkg"
  exit 0
fi
mkdir -p "$DIR/../target/release"
write_bin "$DIR/../target/release/agenmux"
printf 'v{version}\n%s\n' "$(git -C "$DIR/.." rev-parse HEAD 2>/dev/null || printf -)" > "$DIR/../target/release/.agenmux-version""#
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

/// The checked-out tree's installer fetches an engine reporting 9.9.9.
fn make_wrong_engine_fetch(repo: &Path) {
    script(
        &repo.join("scripts/install-bin.sh"),
        r#"if [ "${1:-}" = fetch ]; then
  pkg="$3/agenmux-wrong"
  mkdir -p "$pkg/target/release"
  cat > "$pkg/target/release/agenmux" <<'BIN'
#!/usr/bin/env bash
[ "${1:-}" = --version ] && printf 'agenmux 9.9.9\n'
BIN
  chmod +x "$pkg/target/release/agenmux"
  printf '%s\n' "$pkg"
  exit 0
fi
exit 1"#,
    );
    git_ok(repo, &["add", "scripts/install-bin.sh"]);
    git_ok(repo, &["commit", "-qm", "wrong engine fetch"]);
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
        fs::read_to_string(plugin.join("target/release/.agenmux-latest")).unwrap(),
        "v1.2.0\n"
    );
    assert_eq!(
        fs::read_to_string(plugin.join("target/release/.agenmux-tags")).unwrap(),
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
        &release.join("agenmux"),
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
    fs::write(repo.join("target/release/.agenmux-latest"), "v0.1.0\n").unwrap();

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
            &Command::new(repo.join("target/release/agenmux"))
                .arg("--version")
                .output()
                .unwrap()
                .stdout
        )
        .trim(),
        "agenmux 0.1.0"
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
        "[package]\nname = \"agenmux\"\nversion = \"0.1.1\"\n"
    );
}

#[test]
fn wrong_target_engine_leaves_the_git_source_untouched() {
    let tmp = TempDir::new("wrong-engine");
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    make_git_releases(&repo);
    no_server_tmux(&bin);

    make_wrong_engine_fetch(&repo);
    let head = git(&repo, &["rev-parse", "HEAD"]).stdout;

    let out = run(&repo, &bin, &["update", "v0.1.0"]);

    assert!(!out.status.success());
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).stdout, head);
    assert_eq!(
        fs::read_to_string(repo.join("Cargo.toml")).unwrap(),
        "[package]\nname = \"agenmux\"\nversion = \"0.1.1\"\n"
    );
}

#[test]
fn failed_git_update_keeps_the_previous_branch() {
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
    make_wrong_engine_fetch(&repo);
    let revision = String::from_utf8_lossy(&git(&repo, &["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

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
  "show-option -gqv @agenmux-on") printf '1\n' ;;
  "show-option -gqv @agenmux-control-client") printf 'old-control\n' ;;
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
  "show-option -gqv @agenmux-on") printf '1\n' ;;
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
  "show-option -gqv @agenmux-on"|"show-option -gqv @agenmux-sidebar"|"show-option -gqv @agenmux-control-client") ;;
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
        "[package]\nname = \"agenmux\"\nversion = \"0.1.1\"\n",
    )
    .unwrap();
    fs::write(plugin.join("target/release/preserved"), "keep\n").unwrap();
    fs::write(
        plugin.join("target/release/.agenmux-version"),
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
  pkg="$3/agenmux-test"
  mkdir -p "$pkg/scripts" "$pkg/target/release"
  cat > "$pkg/Cargo.toml" <<'TOML'
[package]
name = "agenmux"
version = "0.1.0"
TOML
  printf 'verified source\n' > "$pkg/source-marker"
  cp "$0" "$pkg/scripts/install-bin.sh"
  cat > "$pkg/agenmux.tmux" <<'ENTRY'
#!/usr/bin/env bash
printf 'entrypoint\n' >> "$RESTART_LOG"
ENTRY
  cat > "$pkg/target/release/agenmux" <<'BIN'
#!/usr/bin/env bash
if [ "${1:-}" = --version ]; then printf 'agenmux 0.1.0\n'; elif [ "${1:-}" = toggle ]; then printf 'native-toggle\n' >> "$RESTART_LOG"; fi
BIN
  chmod +x "$pkg/target/release/agenmux"
  printf '%s\n' "$pkg"
  exit 0
fi
exit 1"#,
    );
    script(
        &bin.join("tmux"),
        r#"case "$*" in
  info) exit 0 ;;
  "show-option -gqv @agenmux-on") printf '1\n' ;;
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
        fs::read_to_string(plugin.join("target/release/.agenmux-version")).unwrap(),
        "v0.1.0\n-\n"
    );
    assert_eq!(
        Command::new(plugin.join("target/release/agenmux"))
            .arg("--version")
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .unwrap(),
        "agenmux 0.1.0"
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
    let manifest = "[package]\nname = \"agenmux\"\nversion = \"0.1.1\"\n";
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
  pkg="$3/agenmux-test"
  mkdir -p "$pkg/scripts"
  printf '[package]\nname = "agenmux"\nversion = "0.1.0"\n' > "$pkg/Cargo.toml"
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
    let manifest = "[package]\nname = \"agenmux\"\nversion = \"0.1.1\"\n";
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

// ---------------------------------------------------------------------------
// Default-on auto-update. The installed engine is a copy of the real binary,
// so `current_exe` is the plugin's own engine, as in a real release install.

const VERSION: &str = env!("CARGO_PKG_VERSION");

struct Auto {
    tmp: TempDir,
    plugin: PathBuf,
    bin: PathBuf,
}

/// A release package tree as `install-bin.sh fetch` extracts it.
fn release_package(root: &Path, version: &str) -> PathBuf {
    let package = root.join(format!("v{version}/agenmux-test"));
    fs::create_dir_all(package.join("scripts")).unwrap();
    fs::create_dir_all(package.join("agents")).unwrap();
    fs::create_dir_all(package.join("target/release")).unwrap();
    fs::write(
        package.join("Cargo.toml"),
        format!("[package]\nname = \"agenmux\"\nversion = \"{version}\"\n"),
    )
    .unwrap();
    fs::write(package.join(".gitignore"), "target/\n").unwrap();
    fs::write(package.join("agents/test.conf"), "AGENT_NAME=test\n").unwrap();
    // Log, then (for `activate`, when a test asks) run the real engine the
    // way agenmux.tmux execs the installed one.
    script(
        &package.join("agenmux.tmux"),
        r#"printf 'entrypoint %s\n' "$*" >> "${RESTART_LOG:-/dev/null}"
if [ "${1:-}" = activate ] && [ -n "${AGENMUX_REAL:-}" ]; then
  exec env AGENMUX_DIR="$(cd "$(dirname "$0")" && pwd)" "$AGENMUX_REAL" toggle "$2" "$3"
fi"#,
    );
    script(&package.join("scripts/version.sh"), "exit 0");
    script(
        &package.join("scripts/install-bin.sh"),
        r#"[ "${1:-}" = fetch ] || exit 0
[ -d "$RELEASES/$2/agenmux-test" ] || exit 1
mkdir -p "$3" && cp -R "$RELEASES/$2/agenmux-test" "$3/" && printf '%s\n' "$3/agenmux-test""#,
    );
    script(
        &package.join("target/release/agenmux"),
        &format!(
            r#"[ "${{1:-}}" = --version ] && printf 'agenmux {version}\n' && exit 0
printf 'engine {version} %s\n' "$*" >> "${{RESTART_LOG:-/dev/null}}""#
        ),
    );
    package
}

/// Linux refuses to exec a file some process still has open for writing
/// (ETXTBSY). A process another parallel test forks while this copy is being
/// written keeps such a handle until it execs, so wait that out once here.
fn settle_executable(path: &Path) {
    for _ in 0..250 {
        match Command::new(path).arg("--version").output() {
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            _ => return,
        }
    }
    panic!("{} stayed busy", path.display());
}

fn auto_fixture(name: &str, git_install: bool) -> Auto {
    let tmp = TempDir::new(name);
    let releases = tmp.path().join("releases");
    let plugin = tmp.path().join("plugin");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let base = release_package(&releases, VERSION);
    release_package(&releases, "99.0.0");
    let copied = Command::new("cp")
        .arg("-R")
        .arg(&base)
        .arg(&plugin)
        .status()
        .unwrap();
    assert!(copied.success());
    fs::copy(
        env!("CARGO_BIN_EXE_agenmux"),
        plugin.join("target/release/agenmux"),
    )
    .unwrap();
    settle_executable(&plugin.join("target/release/agenmux"));
    let mut revision = "-".to_string();
    if git_install {
        git_ok(&plugin, &["init", "-q", "-b", "main"]);
        git_ok(&plugin, &["config", "user.email", "test@example.com"]);
        git_ok(&plugin, &["config", "user.name", "Test"]);
        git_ok(&plugin, &["add", "-A"]);
        git_ok(&plugin, &["commit", "-qm", "base"]);
        git_ok(&plugin, &["tag", &format!("v{VERSION}")]);
        fs::write(
            plugin.join("Cargo.toml"),
            "[package]\nname = \"agenmux\"\nversion = \"99.0.0\"\n",
        )
        .unwrap();
        git_ok(&plugin, &["commit", "-qam", "next"]);
        git_ok(&plugin, &["tag", "v99.0.0"]);
        git_ok(&plugin, &["reset", "-q", "--hard", &format!("v{VERSION}")]);
        git_ok(&plugin, &["checkout", "-q", "--detach"]);
        revision = String::from_utf8(git(&plugin, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
    }
    fs::write(
        plugin.join("target/release/.agenmux-version"),
        format!("v{VERSION}\n{revision}\n"),
    )
    .unwrap();
    script(
        &bin.join("curl"),
        r#"[ -n "${LATEST_TAG:-}" ] || exit 6
printf 'https://example.invalid/repo/releases/tag/%s' "$LATEST_TAG""#,
    );
    // No server; a popup's sidebar "draws" its first frame unless told not to.
    script(
        &bin.join("tmux"),
        r#"[ "$1" = info ] && exit 1
if [ "$1" = display-popup ] && [ -z "${POPUP_NEVER_READY:-}" ]; then
  for arg in "$@"; do
    case "$arg" in AGENMUX_READY=*) : > "${arg#AGENMUX_READY=}" ;; esac
  done
fi
exit 0"#,
    );
    Auto { tmp, plugin, bin }
}

impl Auto {
    fn state(&self) -> PathBuf {
        self.tmp.path().join(".agenmux-state/plugin")
    }

    fn config(&self, body: &str) {
        let dir = self.tmp.path().join("config/agenmux");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), body).unwrap();
    }

    fn command(&self, latest: &str, args: &[&str]) -> Command {
        let mut command = Command::new(self.plugin.join("target/release/agenmux"));
        command
            .args(args)
            .env_remove("TMUX")
            .env("AGENMUX_DIR", &self.plugin)
            .env("AGENMUX_REPO", "https://example.invalid/repo")
            .env("HOME", self.tmp.path())
            .env("XDG_CONFIG_HOME", self.tmp.path().join("config"))
            .env("RELEASES", self.tmp.path().join("releases"))
            .env("RESTART_LOG", self.tmp.path().join("log"))
            .env("LATEST_TAG", latest)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
        command
    }

    /// One worker run, with the daily throttle reset first.
    fn prepare(&self, latest: &str) -> String {
        let _ = fs::remove_file(self.state().join("last-attempt"));
        let out = self
            .command(latest, &["internal", "auto-update"])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn snapshot(&self) -> (Vec<u8>, String, String) {
        (
            fs::read(self.plugin.join("target/release/agenmux")).unwrap(),
            fs::read_to_string(self.plugin.join("target/release/.agenmux-version")).unwrap(),
            fs::read_to_string(self.plugin.join("Cargo.toml")).unwrap(),
        )
    }
}

#[test]
fn auto_update_prepares_newer_stable_release_without_touching_the_install() {
    for git_install in [true, false] {
        let auto = auto_fixture("auto-prepare", git_install);
        let before = auto.snapshot();
        let head = git(&auto.plugin, &["rev-parse", "HEAD"]).stdout;

        assert_eq!(auto.prepare("v99.0.0"), "agenmux: v99.0.0 ready");

        let pending = fs::read_to_string(auto.state().join("pending")).unwrap();
        assert!(pending.contains("target=v99.0.0\n"), "{pending}");
        assert!(pending.contains(&format!("base=v{VERSION}\n")), "{pending}");
        assert!(auto
            .state()
            .join("pkg-v99.0.0/target/release/agenmux")
            .is_file());
        assert_eq!(auto.snapshot(), before, "active install changed");
        assert_eq!(git(&auto.plugin, &["rev-parse", "HEAD"]).stdout, head);
        if !git_install {
            assert!(fs::read_to_string(auto.state().join("baseline"))
                .unwrap()
                .starts_with(&format!("v{VERSION}\n")));
        }
        let mode = fs::metadata(auto.state()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // The daily throttle holds even though nothing failed.
        let throttled = auto
            .command("v99.0.0", &["internal", "auto-update"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&throttled.stdout).trim(),
            "agenmux: checked within the last day"
        );
    }
}

#[test]
fn auto_update_ignores_prereleases_equal_and_older_releases() {
    let auto = auto_fixture("auto-not-newer", true);
    for latest in ["v99.0.0-rc1", &format!("v{VERSION}"), "v0.0.1"] {
        let outcome = auto.prepare(latest);
        assert_eq!(outcome, format!("agenmux: v{VERSION} is current"));
        assert!(!auto.state().join("pending").exists(), "{latest}");
    }
}

#[test]
fn auto_update_off_or_invalid_config_never_touches_state() {
    let auto = auto_fixture("auto-off", true);
    for config in [
        "[behavior]\nauto_update = false\n",
        "[behavior]\nauto_update = 'yes'\n",
    ] {
        auto.config(config);
        assert_eq!(auto.prepare("v99.0.0"), "agenmux: auto-update is off");
        assert!(!auto.state().exists());
    }
}

#[test]
fn auto_update_failures_keep_install_and_publish_nothing() {
    let auto = auto_fixture("auto-failures", true);
    let before = auto.snapshot();
    // Offline: the attempt is still recorded, so it throttles like success.
    assert_eq!(auto.prepare(""), "agenmux: release check failed");
    assert!(auto.state().join("last-attempt").is_file());
    assert_eq!(
        fs::read_to_string(auto.state().join("status")).unwrap(),
        "auto-update: release check failed\n"
    );
    // Missing asset, then an engine reporting the wrong version.
    fs::remove_dir_all(auto.tmp.path().join("releases/v99.0.0")).unwrap();
    assert_eq!(
        auto.prepare("v99.0.0"),
        "agenmux: could not download v99.0.0"
    );
    let package = release_package(&auto.tmp.path().join("releases"), "99.0.0");
    script(
        &package.join("target/release/agenmux"),
        "printf 'agenmux 9.9.9\\n'",
    );
    assert_eq!(
        auto.prepare("v99.0.0"),
        "agenmux: could not download v99.0.0"
    );
    // A link escaping the package is refused even after a good checksum.
    let package = release_package(&auto.tmp.path().join("releases"), "99.0.0");
    std::os::unix::fs::symlink("../../../../etc", package.join("agents/escape")).unwrap();
    assert!(auto.prepare("v99.0.0").contains("unsafe package"));
    assert!(!auto.state().join("pending").exists());
    assert_eq!(auto.snapshot(), before);
    let leftovers: Vec<_> = fs::read_dir(auto.state())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("work-") || name.starts_with("pkg-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn auto_update_skips_dirty_development_and_modified_installs() {
    let auto = auto_fixture("auto-dirty", true);
    fs::write(auto.plugin.join("scratch"), "local\n").unwrap();
    assert_eq!(
        auto.prepare("v99.0.0"),
        "agenmux: skipped: uncommitted changes"
    );
    fs::remove_file(auto.plugin.join("scratch")).unwrap();
    git_ok(
        &auto.plugin,
        &["commit", "-q", "--allow-empty", "-m", "dev"],
    );
    assert_eq!(
        auto.prepare("v99.0.0"),
        "agenmux: skipped: development checkout (not on a release tag)"
    );
    assert!(!auto.state().exists());

    // A branch at the release tag belongs to git pull/TPM.
    let branch = auto_fixture("auto-branch", true);
    git_ok(&branch.plugin, &["checkout", "-q", "-B", "main"]);
    assert_eq!(
        branch.prepare("v99.0.0"),
        "agenmux: skipped: branch checkout (updated by git pull or TPM)"
    );

    let tarball = auto_fixture("auto-modified-tarball", false);
    fs::write(tarball.plugin.join("agents/test.conf"), "AGENT_NAME=mine\n").unwrap();
    assert_eq!(
        tarball.prepare("v99.0.0"),
        "agenmux: local changes in the plugin directory"
    );
    assert!(!tarball.state().join("pending").exists());
}

/// Another tmux server's daemon: a shared lease on the installation lock.
fn hold_lease(auto: &Auto) -> fs::File {
    use std::os::fd::AsRawFd;
    fs::create_dir_all(auto.state()).unwrap();
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(auto.state().join("install.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) }, 0);
    file
}

#[test]
fn manual_update_refuses_while_another_runtime_uses_the_install() {
    let auto = auto_fixture("manual-busy", true);
    let before = auto.snapshot();
    let lease = hold_lease(&auto);
    let out = auto
        .command("v99.0.0", &["update", "v99.0.0"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(auto.snapshot(), before);
    drop(lease);
    let out = auto
        .command("v99.0.0", &["update", "v99.0.0"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        fs::read_to_string(auto.plugin.join("target/release/.agenmux-version"))
            .unwrap()
            .lines()
            .next(),
        Some("v99.0.0")
    );
}

impl Auto {
    /// A launch through the public toggle entry point, inside a (fake) tmux,
    /// offline. The target's bootstrap hands back to the real engine when
    /// `confirm` is set, so the target can confirm readiness.
    fn toggle(&self, mode: &str, confirm: bool) -> Output {
        let runtime = self.tmp.path().join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = self.command("", &["toggle", mode, "client-1"]);
        command
            .env("TMUX", format!("{}/socket,1,0", self.tmp.path().display()))
            .env("AGENMUX_RUNTIME_DIR", &runtime);
        if confirm {
            command.env("AGENMUX_REAL", env!("CARGO_BIN_EXE_agenmux"));
        }
        command.output().unwrap()
    }

    fn log(&self) -> String {
        fs::read_to_string(self.tmp.path().join("log")).unwrap_or_default()
    }

    fn head(&self) -> String {
        String::from_utf8(git(&self.plugin, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    fn installed(&self) -> String {
        fs::read_to_string(self.plugin.join("target/release/.agenmux-version"))
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string()
    }

    fn assert_clean_state(&self) {
        for leftover in ["transaction", "backup", "pending", "pkg-v99.0.0"] {
            assert!(!self.state().join(leftover).exists(), "{leftover} left");
        }
    }
}

fn activation_fixture(name: &str, git_install: bool) -> Auto {
    let auto = auto_fixture(name, git_install);
    assert_eq!(auto.prepare("v99.0.0"), "agenmux: v99.0.0 ready");
    let _ = fs::remove_file(auto.tmp.path().join("log"));
    auto
}

#[test]
fn fresh_popup_start_activates_the_prepared_release_offline() {
    for git_install in [true, false] {
        let auto = activation_fixture("activate-popup", git_install);
        let out = auto.toggle("popup", true);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(auto.installed(), "v99.0.0");
        assert!(fs::read_to_string(auto.plugin.join("Cargo.toml"))
            .unwrap()
            .contains("99.0.0"));
        if git_install {
            let tag = git(&auto.plugin, &["rev-parse", "v99.0.0^{commit}"]).stdout;
            assert_eq!(auto.head(), String::from_utf8(tag).unwrap().trim());
            assert!(auto.plugin.join(".git").exists());
        } else {
            assert!(fs::read_to_string(auto.state().join("baseline"))
                .unwrap()
                .starts_with("v99.0.0\n"));
        }
        // Target setup ran, then the target's own bootstrap with the original
        // mode and client.
        let log = auto.log();
        assert!(
            log.starts_with("entrypoint \nentrypoint activate popup client-1\n"),
            "{log}"
        );
        auto.assert_clean_state();
        assert!(!auto.state().join("failed").exists());
    }
}

#[test]
fn target_readiness_failure_restores_and_starts_the_old_release_once() {
    for git_install in [true, false] {
        let auto = activation_fixture("activate-readiness", git_install);
        let before = auto.snapshot();
        let head = auto.head();
        // The fake tmux cannot start a split daemon, so the target fails.
        let out = auto.toggle("split", true);
        assert!(!out.status.success());
        assert_eq!(auto.snapshot(), before);
        assert_eq!(auto.head(), head);
        assert_eq!(
            fs::read_to_string(auto.state().join("failed")).unwrap(),
            "v99.0.0\n"
        );
        // Target bootstrap, restored old setup, old bootstrap; no retry loop.
        let log = auto.log();
        assert_eq!(
            log.matches("entrypoint activate split client-1").count(),
            2,
            "{log}"
        );
        auto.assert_clean_state();
        // Preparation does not retry the failed target.
        assert!(auto.prepare("v99.0.0").contains("v99.0.0 failed to start"));
    }
}

#[test]
fn activation_faults_leave_a_complete_old_install() {
    for git_install in [true, false] {
        for step in ["source", "engine", "state", "setup"] {
            let auto = activation_fixture("activate-fault", git_install);
            let before = auto.snapshot();
            let head = auto.head();
            let out = auto
                .command("", &["toggle", "popup", "client-1"])
                .env("TMUX", format!("{}/socket,1,0", auto.tmp.path().display()))
                .env("AGENMUX_RUNTIME_DIR", auto.tmp.path().join("runtime"))
                .env("AGENMUX_REAL", env!("CARGO_BIN_EXE_agenmux"))
                .env("AGENMUX_ACTIVATION_FAULT", step)
                .output()
                .unwrap();
            // The old release still opens.
            assert!(out.status.success(), "{step}");
            assert_eq!(auto.snapshot(), before, "{step} git={git_install}");
            assert_eq!(auto.head(), head, "{step}");
            assert!(auto.plugin.join("agents/test.conf").is_file(), "{step}");
            assert!(!auto.log().contains("activate"), "{step}: {}", auto.log());
            assert!(!auto.state().join("transaction").exists(), "{step}");
            assert!(!auto.state().join("backup").exists(), "{step}");
            if step == "setup" {
                // The target's own setup failed: never retried automatically.
                assert_eq!(
                    fs::read_to_string(auto.state().join("failed")).unwrap(),
                    "v99.0.0\n"
                );
                auto.assert_clean_state();
            } else {
                // A local error while switching: the prepared release stays
                // and the next fresh start retries it.
                assert!(!auto.state().join("failed").exists(), "{step}");
                assert!(
                    fs::read_to_string(auto.state().join("status"))
                        .unwrap()
                        .contains("could not switch to v99.0.0"),
                    "{step}"
                );
                let out = auto.toggle("popup", true);
                assert!(out.status.success(), "{step} retry");
                assert_eq!(auto.installed(), "v99.0.0", "{step} retry");
            }
        }
    }
}

#[test]
fn interrupted_activation_is_recovered_by_the_next_start() {
    for git_install in [true, false] {
        let auto = activation_fixture("activate-interrupted", git_install);
        let before = auto.snapshot();
        // The target never confirms: it exits after its bootstrap ran.
        let _ = auto.toggle("popup", false);
        assert_eq!(auto.installed(), "v99.0.0");
        assert!(auto.state().join("transaction").is_file());
        // Any next start recovers before doing anything else.
        let out = Command::new(env!("CARGO_BIN_EXE_agenmux"))
            .args(["toggle", "popup", "client-1"])
            .env("AGENMUX_DIR", &auto.plugin)
            .env("HOME", auto.tmp.path())
            .env("XDG_CONFIG_HOME", auto.tmp.path().join("config"))
            .env("TMUX", format!("{}/socket,1,0", auto.tmp.path().display()))
            .env("AGENMUX_RUNTIME_DIR", auto.tmp.path().join("runtime"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    auto.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(auto.snapshot(), before);
        assert_eq!(
            fs::read_to_string(auto.state().join("failed")).unwrap(),
            "v99.0.0\n"
        );
        auto.assert_clean_state();
    }
}

#[test]
fn activation_waits_for_runtimes_policy_and_a_matching_base() {
    let auto = activation_fixture("activate-guarded", true);
    let before = auto.snapshot();
    // Status, scan and config commands never activate.
    for args in [&["status"][..], &["list"], &["config", "check"]] {
        let _ = auto.command("", args).output().unwrap();
    }
    // Another server's daemon holds a lease: launch the current version.
    let lease = hold_lease(&auto);
    assert!(auto.toggle("popup", true).status.success());
    assert_eq!(auto.snapshot(), before);
    assert!(auto.state().join("pending").is_file());
    drop(lease);
    // Turned off: the prepared release stays pending but is not applied.
    auto.config("[behavior]\nauto_update = false\n");
    assert!(auto.toggle("popup", true).status.success());
    assert_eq!(auto.snapshot(), before);
    assert!(auto.state().join("pending").is_file());
    auto.config("");
    // Prepared for another base (a manual or TPM switch since): discarded.
    let pending = fs::read_to_string(auto.state().join("pending")).unwrap();
    fs::write(
        auto.state().join("pending"),
        pending.replace(&format!("base=v{VERSION}"), "base=v0.0.1"),
    )
    .unwrap();
    assert!(auto.toggle("popup", true).status.success());
    assert_eq!(auto.snapshot(), before);
    assert!(!auto.state().join("pending").exists());
    assert!(!auto.state().join("pkg-v99.0.0").exists());
}

#[test]
fn manual_switch_pauses_auto_update_and_clears_prepared_state() {
    let auto = activation_fixture("manual-pause", true);
    fs::write(auto.state().join("failed"), "v98.0.0\n").unwrap();
    auto.config("# mine\nversion = 1\n[behavior]\nnotifications = false # keep\n");
    let out = auto.command("", &["update", "v99.0.0"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(auto.installed(), "v99.0.0");
    let config = fs::read_to_string(auto.tmp.path().join("config/agenmux/config.toml")).unwrap();
    assert_eq!(
        config,
        "# mine\nversion = 1\n[behavior]\nnotifications = false # keep\nauto_update = false\n"
    );
    for gone in ["pending", "pkg-v99.0.0", "failed", "status"] {
        assert!(!auto.state().join(gone).exists(), "{gone}");
    }
}

#[test]
fn failed_manual_switch_keeps_the_auto_update_preference() {
    // No config file before: a refused switch leaves none behind.
    let auto = auto_fixture("manual-pause-failed", true);
    let lease = hold_lease(&auto);
    let out = auto.command("", &["update", "v99.0.0"]).output().unwrap();
    assert!(!out.status.success());
    assert!(!auto.tmp.path().join("config/agenmux/config.toml").exists());
    drop(lease);
    // An existing file is restored byte for byte.
    let original = "version = 1\n# comment\n[behavior]\nauto_update = true\n";
    auto.config(original);
    fs::remove_dir_all(auto.tmp.path().join("releases/v99.0.0")).unwrap();
    let out = auto.command("", &["update", "v99.0.0"]).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(
        fs::read_to_string(auto.tmp.path().join("config/agenmux/config.toml")).unwrap(),
        original
    );
    assert_eq!(auto.installed(), format!("v{VERSION}"));
    // A configuration that cannot record the pause refuses the switch.
    auto.config("[behavior]\nauto_update = 'maybe'\n");
    let before = auto.snapshot();
    let out = auto.command("", &["update", "v99.0.0"]).output().unwrap();
    assert!(!out.status.success());
    assert_eq!(auto.snapshot(), before);
}

#[test]
fn popup_that_never_draws_keeps_the_old_release() {
    let auto = activation_fixture("activate-popup-not-ready", true);
    let before = auto.snapshot();
    let out = auto
        .command("", &["toggle", "popup", "client-1"])
        .env("TMUX", format!("{}/socket,1,0", auto.tmp.path().display()))
        .env("AGENMUX_RUNTIME_DIR", auto.tmp.path().join("runtime"))
        .env("AGENMUX_REAL", env!("CARGO_BIN_EXE_agenmux"))
        .env("POPUP_NEVER_READY", "1")
        .output()
        .unwrap();
    // The old release reopened in its place.
    assert!(out.status.success());
    assert_eq!(
        auto.log()
            .matches("entrypoint activate popup client-1")
            .count(),
        2
    );
    assert_eq!(auto.snapshot(), before);
    assert_eq!(
        fs::read_to_string(auto.state().join("failed")).unwrap(),
        "v99.0.0\n"
    );
    auto.assert_clean_state();
    assert!(!auto.tmp.path().join("runtime/agenmux-pin").exists());
}

#[test]
fn launch_that_waited_out_a_switch_hands_over_to_the_installed_release() {
    use std::os::fd::AsRawFd;
    let auto = activation_fixture("activate-waited", true);
    // Another launch is mid-switch: it holds the installation exclusively.
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(auto.state().join("install.lock"))
        .unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let runtime = auto.tmp.path().join("runtime");
    fs::create_dir_all(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let launch = auto
        .command("", &["toggle", "popup", "client-1"])
        .env("TMUX", format!("{}/socket,1,0", auto.tmp.path().display()))
        .env("AGENMUX_RUNTIME_DIR", &runtime)
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(500));
    // The switch replaces the engine (by rename, as installs do), then ends.
    let engine = auto.plugin.join("target/release/agenmux");
    let staged = auto.tmp.path().join("new-engine");
    fs::copy(
        auto.state().join("pkg-v99.0.0/target/release/agenmux"),
        &staged,
    )
    .unwrap();
    fs::rename(&staged, &engine).unwrap();
    drop(lock);
    let out = launch.wait_with_output().unwrap();
    // The waiting launch, still the old engine, re-entered the installed
    // bootstrap instead of starting a view of its own.
    assert!(
        auto.log().contains("entrypoint activate popup client-1"),
        "{} / {}",
        auto.log(),
        String::from_utf8_lossy(&out.stderr)
    );
}
