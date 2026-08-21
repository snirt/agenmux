use crate::{panes, tmux};
use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_REPO: &str = "https://github.com/snirt/tmux-agents-mon";

fn repo() -> String {
    std::env::var("AGENTS_MON_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string())
}

fn release_dir(plugin_dir: &Path) -> PathBuf {
    plugin_dir.join("target/release")
}

fn latest_file(plugin_dir: &Path) -> PathBuf {
    release_dir(plugin_dir).join(".agents-mon-latest")
}

fn tags_file(plugin_dir: &Path) -> PathBuf {
    release_dir(plugin_dir).join(".agents-mon-tags")
}

fn run(program: impl AsRef<OsStr>, args: &[&str]) -> std::io::Result<Output> {
    Command::new(program).args(args).output()
}

fn success(program: impl AsRef<OsStr>, args: &[&str]) -> bool {
    run(program, args).is_ok_and(|output| output.status.success())
}

fn atomic_write(path: &Path, value: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staged = path.with_extension(format!("tmp-{}-{stamp}", std::process::id()));
    fs::write(&staged, value)?;
    fs::rename(staged, path)
}

fn latest_remote_tag(repo: &str) -> Option<String> {
    let output = run(
        "curl",
        &[
            "-fsSL",
            "-o",
            "/dev/null",
            "-w",
            "%{url_effective}",
            &format!("{repo}/releases/latest"),
        ],
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8(output.stdout).ok()?;
    let tag = url.trim_end_matches('/').rsplit('/').next()?;
    valid_tag(tag).then(|| tag.to_string())
}

fn numeric_parts(tag: &str) -> Vec<u64> {
    tag.trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

fn natural_cmp(left: &str, right: &str) -> Ordering {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    let (mut l, mut r) = (0, 0);
    while l < left.len() && r < right.len() {
        if left[l].is_ascii_digit() && right[r].is_ascii_digit() {
            let (l0, r0) = (l, r);
            while l < left.len() && left[l].is_ascii_digit() {
                l += 1;
            }
            while r < right.len() && right[r].is_ascii_digit() {
                r += 1;
            }
            let ld = &left[l0..l];
            let rd = &right[r0..r];
            let ld = &ld[ld.iter().position(|b| *b != b'0').unwrap_or(ld.len())..];
            let rd = &rd[rd.iter().position(|b| *b != b'0').unwrap_or(rd.len())..];
            match ld.len().cmp(&rd.len()).then_with(|| ld.cmp(rd)) {
                Ordering::Equal => {}
                order => return order,
            }
        } else {
            match left[l].cmp(&right[r]) {
                Ordering::Equal => {
                    l += 1;
                    r += 1;
                }
                order => return order,
            }
        }
    }
    (left.len() - l).cmp(&(right.len() - r))
}

fn compare_tags(left: &str, right: &str) -> Ordering {
    let (left_version, left_pre) = left
        .split_once('-')
        .map_or((left, None), |(v, p)| (v, Some(p)));
    let (right_version, right_pre) = right
        .split_once('-')
        .map_or((right, None), |(v, p)| (v, Some(p)));
    let (left_parts, right_parts) = (numeric_parts(left_version), numeric_parts(right_version));
    for i in 0..left_parts.len().max(right_parts.len()) {
        match left_parts
            .get(i)
            .copied()
            .unwrap_or(0)
            .cmp(&right_parts.get(i).copied().unwrap_or(0))
        {
            Ordering::Equal => {}
            order => return order,
        }
    }
    match (left_pre, right_pre) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => natural_cmp(left, right),
    }
}

fn remote_tags(repo: &str, latest: &str) -> Vec<String> {
    let Ok(output) = run("git", &["ls-remote", "--tags", "--refs", repo]) else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut tags: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(|reference| reference.strip_prefix("refs/tags/"))
        .filter(|tag| valid_tag(tag))
        .map(str::to_string)
        .collect();
    tags.sort_by(|left, right| compare_tags(right, left).then_with(|| right.cmp(left)));
    tags.iter()
        .position(|tag| tag == latest)
        .map_or_else(Vec::new, |start| tags.split_off(start))
}

/// Refresh the release notice and picker files. Network failures are best effort
/// and never interrupt the sidebar.
pub fn refresh(plugin_dir: &Path) -> i32 {
    let repository = repo();
    let Some(latest) = latest_remote_tag(&repository) else {
        return 0;
    };
    if atomic_write(&latest_file(plugin_dir), &format!("{latest}\n")).is_err() {
        return 0;
    }
    let tags = remote_tags(&repository, &latest);
    if !tags.is_empty() {
        let _ = atomic_write(&tags_file(plugin_dir), &format!("{}\n", tags.join("\n")));
    }
    0
}

fn valid_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    rest.as_bytes().first().is_some_and(u8::is_ascii_digit)
        && rest
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

fn manifest_tag(plugin_dir: &Path) -> Option<String> {
    fs::read_to_string(plugin_dir.join("Cargo.toml"))
        .ok()?
        .lines()
        .find_map(|line| {
            let line = line.trim();
            let version = line
                .strip_prefix("version")?
                .trim_start()
                .strip_prefix('=')?
                .trim();
            let version = version.strip_prefix('"')?.strip_suffix('"')?;
            Some(format!("v{version}"))
        })
}

fn note(message: &str) {
    if !success(
        "tmux",
        &["display-message", &format!("agents-mon: {message}")],
    ) {
        println!("agents-mon: {message}");
    }
}

fn fail(message: &str) -> i32 {
    note(message);
    1
}

fn first_line(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()?
        .lines()
        .next()
        .map(str::to_string)
}

fn resolve_target(plugin_dir: &Path, requested: &str) -> Option<String> {
    if requested != "latest" {
        return valid_tag(requested).then(|| requested.to_string());
    }
    first_line(&latest_file(plugin_dir))
        .filter(|tag| valid_tag(tag))
        .or_else(|| latest_remote_tag(&repo()))
}

fn git_output(plugin_dir: &Path, args: &[&str]) -> Option<Output> {
    Command::new("git")
        .arg("-C")
        .arg(plugin_dir)
        .args(args)
        .output()
        .ok()
}

fn git_success(plugin_dir: &Path, args: &[&str]) -> bool {
    git_output(plugin_dir, args).is_some_and(|output| output.status.success())
}

fn git_install(plugin_dir: &Path, target: &str) -> Result<(), &'static str> {
    let status = git_output(plugin_dir, &["status", "--porcelain"]).ok_or("status")?;
    if !status.status.success() {
        return Err("status");
    }
    if !status.stdout.is_empty() {
        return Err("dirty");
    }
    let _ = git_success(plugin_dir, &["fetch", "--tags", "--quiet", "origin"]);
    let revision = format!("refs/tags/{target}^{{commit}}");
    if !git_success(plugin_dir, &["rev-parse", "-q", "--verify", &revision]) {
        return Err("unknown");
    }
    git_success(plugin_dir, &["checkout", "--quiet", target])
        .then_some(())
        .ok_or("checkout")
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(prefix: &str) -> std::io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}.{}.{stamp}", std::process::id()));
        fs::create_dir_all(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn update_sibling(plugin_dir: &Path, label: &str) -> std::io::Result<PathBuf> {
    let parent = plugin_dir
        .parent()
        .ok_or_else(|| std::io::Error::other("plugin directory has no parent"))?;
    let mut name = plugin_dir
        .file_name()
        .unwrap_or_else(|| OsStr::new("plugin"))
        .to_os_string();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    name.push(format!(".{label}-{}-{stamp}", std::process::id()));
    Ok(parent.join(name))
}

fn copy_tree(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = format!("{}/.", source.display());
    success("cp", &["-R", &source, &destination.display().to_string()])
        .then_some(())
        .ok_or_else(|| std::io::Error::other("could not copy tree"))
}

fn synchronize_tarball_source(plugin_dir: &Path, package: &Path) -> std::io::Result<()> {
    let replacement = update_sibling(plugin_dir, "replacement")?;
    let backup = update_sibling(plugin_dir, "backup")?;
    fs::create_dir(&replacement)?;

    let staged = (|| {
        copy_tree(package, &replacement)?;
        let installed_release = plugin_dir.join("target/release");
        if installed_release.is_dir() {
            let replacement_target = replacement.join("target");
            fs::create_dir_all(&replacement_target)?;
            let installed = installed_release.display().to_string();
            if !success(
                "cp",
                &["-R", &installed, &replacement_target.display().to_string()],
            ) {
                return Err(std::io::Error::other("could not preserve installed engine"));
            }
        }
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&replacement);
        return Err(error);
    }

    if let Err(error) = fs::rename(plugin_dir, &backup) {
        let _ = fs::remove_dir_all(&replacement);
        return Err(error);
    }
    if let Err(error) = fs::rename(&replacement, plugin_dir) {
        let _ = fs::rename(&backup, plugin_dir);
        let _ = fs::remove_dir_all(&replacement);
        return Err(error);
    }
    let _ = fs::remove_dir_all(backup);
    Ok(())
}

fn tarball_install(plugin_dir: &Path, target: &str) -> Result<(), &'static str> {
    let scratch = Scratch::new("agents-mon-up").map_err(|_| "scratch")?;
    let installer = plugin_dir.join("scripts/install-bin.sh");
    let output = Command::new("bash")
        .arg(installer)
        .arg("fetch")
        .arg(target)
        .arg(&scratch.0)
        .output()
        .map_err(|_| "fetch")?;
    if !output.status.success() {
        return Err("fetch");
    }
    let package = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    if !package.is_dir() {
        return Err("fetch");
    }
    let _ = fs::remove_dir_all(package.join("target"));
    synchronize_tarball_source(plugin_dir, &package).map_err(|_| "copy")
}

fn install_engine(plugin_dir: &Path) -> bool {
    let _ = fs::remove_file(release_dir(plugin_dir).join(".agents-mon-version"));
    Command::new("bash")
        .arg(plugin_dir.join("scripts/install-bin.sh"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn tmux_value(args: &[&str]) -> String {
    tmux::command(args)
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

fn tmux_running() -> bool {
    success("tmux", &["info"])
}

fn wait_for_client(name: &str) {
    for _ in 0..80 {
        let clients = tmux_value(&["list-clients", "-F", "#{client_name}"]);
        if !clients.lines().any(|client| client == name) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn restart(plugin_dir: &Path, was_open: bool, old_control: &str) {
    if !tmux_running() {
        return;
    }
    let _ = panes::teardown();
    if !old_control.is_empty() {
        wait_for_client(old_control);
    }
    let _ = Command::new("bash")
        .arg(plugin_dir.join("agents-mon.tmux"))
        .status();
    if !was_open {
        return;
    }
    let toggle = plugin_dir.join("scripts/toggle.sh");
    if toggle.is_file() {
        let _ = Command::new("bash").arg(toggle).status();
    } else {
        let _ = Command::new(release_dir(plugin_dir).join("agents-mon"))
            .arg("toggle")
            .status();
    }
}

/// Switch source and engine to a release, then re-enter through that release's
/// own public entrypoint so rollbacks do not assume new commands exist.
pub fn update(plugin_dir: &Path, requested: &str) -> i32 {
    let Some(target) = resolve_target(plugin_dir, requested) else {
        return fail("no release to switch to");
    };
    let Some(current) = manifest_tag(plugin_dir) else {
        return fail("could not read package version");
    };
    if target == current {
        note(&format!("already on {target}"));
        return 0;
    }

    let was_open = tmux_value(&["show-option", "-gqv", "@agents-mon-on"]) == "1"
        || !tmux_value(&["show-option", "-gqv", "@agents-mon-sidebar"]).is_empty();
    let old_control = tmux_value(&["show-option", "-gqv", "@agents-mon-control-client"]);
    note(&format!("switching to {target}…"));

    let result = if plugin_dir.join(".git").exists() {
        git_install(plugin_dir, &target)
    } else {
        tarball_install(plugin_dir, &target)
    };
    if let Err(reason) = result {
        return match reason {
            "dirty" => fail(&format!(
                "uncommitted changes in {} — commit or stash first",
                plugin_dir.display()
            )),
            "status" => fail(&format!(
                "could not inspect working tree in {}",
                plugin_dir.display()
            )),
            "unknown" => fail(&format!("unknown release {target}")),
            "checkout" => fail(&format!("could not check out {target}")),
            "fetch" => fail(&format!("could not download {target}")),
            _ => fail(&format!("could not write to {}", plugin_dir.display())),
        };
    }
    if !install_engine(plugin_dir) {
        return fail(&format!("could not install engine for {target}"));
    }
    restart(plugin_dir, was_open, &old_control);
    note(&format!("now on {target}"));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_sort_numerically() {
        assert_eq!(compare_tags("v1.10.0", "v1.9.9"), Ordering::Greater);
        assert_eq!(compare_tags("v1.0", "v1.0.0"), Ordering::Equal);
        assert_eq!(compare_tags("v1.2.3-rc1", "v1.2.2"), Ordering::Greater);
        assert_eq!(compare_tags("v1.2.3", "v1.2.3-rc1"), Ordering::Greater);
        assert_eq!(compare_tags("v1.2.3-rc10", "v1.2.3-rc2"), Ordering::Greater);
    }

    #[test]
    fn release_targets_are_boring() {
        assert!(valid_tag("v1.2.3"));
        assert!(valid_tag("v1.2.3-rc1"));
        assert!(!valid_tag("latest"));
        assert!(!valid_tag("v;rm"));
        assert!(!valid_tag("v1;rm"));
    }
}
