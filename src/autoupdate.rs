//! Default-on auto-update: prepare a newer stable release in the background,
//! switch to it on the next fresh start. See docs/plans/2026-10-07-default-on-auto-update.md.
//!
//! Lock order: server lifecycle lock → install.lock → prepare.lock → state.lock.
//! Preparation never takes install.lock, so it never waits on a runtime.

use crate::release;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAY: u64 = 24 * 60 * 60;

/// Within the daily throttle. A stamp from the future (the clock moved back)
/// does not count, or checks would stop until the clock caught up.
fn checked_recently(last: u64) -> bool {
    let now = now();
    last <= now && now - last < DAY
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `[behavior] auto_update`, file layer only. An unreadable or invalid file
/// disables automatic mutation; startup validation reports it separately.
pub(crate) fn enabled() -> bool {
    crate::app_config::load()
        .and_then(|file| crate::app_config::resolve(&file, &BTreeMap::new()))
        .is_ok_and(|config| config.auto_update)
}

/// `<plugin-parent>/.agenmux-state/<plugin-dir-name>`: outside the replaceable
/// tree, on its filesystem, and stable across source replacement.
pub(crate) fn state_dir(plugin_dir: &Path) -> Option<PathBuf> {
    let plugin = fs::canonicalize(plugin_dir).ok()?;
    Some(
        plugin
            .parent()?
            .join(".agenmux-state")
            .join(plugin.file_name()?),
    )
}

fn private_dir(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other("unsafe auto-update state directory"));
    }
    Ok(())
}

fn ensure_state_dir(plugin_dir: &Path) -> io::Result<PathBuf> {
    let dir = state_dir(plugin_dir).ok_or_else(|| io::Error::other("no state directory"))?;
    private_dir(dir.parent().unwrap())?;
    private_dir(&dir)?;
    Ok(dir)
}

/// An flock on a file in the state directory. The kernel releases it when the
/// last descriptor closes, so a killed holder never leaves it stale. The file
/// is never unlinked: a waiter may already have opened the inode.
pub(crate) struct Lock {
    file: File,
}

impl Lock {
    fn open(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    }

    /// `operation` is LOCK_SH or LOCK_EX; `wait` zero means a single attempt.
    pub(crate) fn acquire(path: &Path, operation: i32, wait: Duration) -> io::Result<Self> {
        let file = Self::open(path)?;
        let deadline = std::time::Instant::now() + wait;
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } == 0 {
                return Ok(Self { file });
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Err(error);
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "lock busy"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Adopt a lock descriptor handed down through exec (see `hand_down`),
    /// close-on-exec again so this process's own children never inherit it.
    pub(crate) fn inherited(var: &str) -> Option<Self> {
        let fd: std::os::fd::RawFd = std::env::var(var).ok()?.parse().ok()?;
        std::env::remove_var(var);
        if fd <= 2 || unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
            return None;
        }
        Some(Self {
            file: unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) },
        })
    }

    /// Downgrade to shared in place (for every holder of this descriptor).
    pub(crate) fn share(&self) {
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_SH) };
    }

    /// Let `command`'s child keep this lock across exec, announced in `var`.
    pub(crate) fn hand_down(&self, command: &mut Command, var: &str) {
        use std::os::unix::process::CommandExt;
        let fd = self.file.as_raw_fd();
        command.env(var, fd.to_string());
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

/// Whether this process is the plugin's own installed engine. Development and
/// custom builds neither coordinate through nor mutate the installation.
/// Decided once, at the first call (process start): after a switch replaces
/// the engine file, a still-running old engine must keep its identity.
fn coordinated(plugin_dir: &Path) -> bool {
    static COORDINATED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *COORDINATED.get_or_init(|| {
        let running = std::env::current_exe().and_then(fs::canonicalize).ok();
        running.is_some() && running == fs::canonicalize(release::engine_path(plugin_dir)).ok()
    })
}

fn install_lock(plugin_dir: &Path, operation: i32, wait: Duration) -> Result<Option<Lock>, ()> {
    if !coordinated(plugin_dir) {
        return Ok(None);
    }
    // No private state directory (read-only parent): nothing can activate an
    // update here either, so there is nothing to coordinate with.
    let Ok(dir) = ensure_state_dir(plugin_dir) else {
        return Ok(None);
    };
    Lock::acquire(&dir.join("install.lock"), operation, wait)
        .map(Some)
        .map_err(|_| ())
}

pub(crate) enum LeaseError {
    /// A version switch held the installation for too long.
    Busy,
    /// A version switch finished while this launch waited: this process is
    /// the previous release and must hand over to the installed one.
    Updated,
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Busy => "agenmux is switching versions; try again",
            Self::Updated => "agenmux was updated while starting; open it again",
        })
    }
}

/// A runtime's shared hold on the installation: toggle for its whole run
/// (popups included), the daemon for its lifetime, a direct sidebar.
pub(crate) fn lease(plugin_dir: &Path) -> Result<Option<Lock>, LeaseError> {
    if let Ok(lease) = install_lock(plugin_dir, libc::LOCK_SH, Duration::ZERO) {
        return Ok(lease);
    }
    let lease = install_lock(plugin_dir, libc::LOCK_SH, Duration::from_secs(30))
        .map_err(|()| LeaseError::Busy)?;
    if !release::engine_matches(&release::engine_path(plugin_dir), &running_release()) {
        return Err(LeaseError::Updated);
    }
    Ok(lease)
}

/// Exclusive hold for a manual version switch, once this server's own view
/// has closed. Busy means another server or popup still runs this install.
pub(crate) fn exclusive(plugin_dir: &Path, wait: Duration) -> Result<Option<Lock>, ()> {
    install_lock(plugin_dir, libc::LOCK_EX, wait)
}

fn try_lock(dir: &Path, name: &str) -> Option<Lock> {
    Lock::acquire(&dir.join(name), libc::LOCK_EX, Duration::ZERO).ok()
}

fn state_lock(dir: &Path) -> io::Result<Lock> {
    Lock::acquire(
        &dir.join("state.lock"),
        libc::LOCK_EX,
        Duration::from_secs(10),
    )
}

// ---------------------------------------------------------------------------
// Integrity

/// SHA-256 (FIPS 180-4). Release verification needs it without adding a
/// dependency or parsing another tool's output per file.
pub(crate) fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut tail = data[data.len() - data.len() % 64..].to_vec();
    tail.push(0x80);
    while tail.len() % 64 != 56 {
        tail.push(0);
    }
    tail.extend((data.len() as u64 * 8).to_be_bytes());
    for chunk in data[..data.len() - data.len() % 64]
        .chunks(64)
        .chain(tail.chunks(64))
    {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[4 * i..4 * i + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let [a, b, c, d, e, f, g, hh] = v;
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let t2 = s0.wrapping_add((a & b) ^ (a & c) ^ (b & c));
            v = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for (word, add) in h.iter_mut().zip(v) {
            *word = word.wrapping_add(add);
        }
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

/// `<sha256>  <relative path>` per regular file, `link:<target>` for symlinks,
/// sorted. Refuses symlinks leaving `root`, hard links and special files.
/// `skip(relative)` prunes entries (and whole directories).
pub(crate) fn tree_manifest(root: &Path, skip: &dyn Fn(&str) -> bool) -> io::Result<String> {
    fn walk(
        root: &Path,
        relative: &Path,
        skip: &dyn Fn(&str) -> bool,
        out: &mut Vec<String>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(root.join(relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            let name = path
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF-8 path"))?
                .to_string();
            if name.contains('\n') {
                return Err(io::Error::other("newline in path"));
            }
            if skip(&name) {
                continue;
            }
            let metadata = fs::symlink_metadata(root.join(&path))?;
            if metadata.is_dir() {
                walk(root, &path, skip, out)?;
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(root.join(&path))?;
                let mut depth = path.components().count() as i64 - 1;
                for part in target.components() {
                    match part {
                        std::path::Component::Normal(_) => depth += 1,
                        std::path::Component::ParentDir => depth -= 1,
                        std::path::Component::CurDir => {}
                        _ => depth = -1,
                    }
                    if depth < 0 {
                        return Err(io::Error::other(format!("link escapes tree: {name}")));
                    }
                }
                out.push(format!("link:{}  {name}", target.display()));
            } else if metadata.is_file() && metadata.nlink() == 1 {
                out.push(format!("{}  {name}", sha256(&fs::read(root.join(&path))?)));
            } else {
                return Err(io::Error::other(format!("unsupported entry: {name}")));
            }
        }
        Ok(())
    }
    let mut lines = Vec::new();
    walk(root, Path::new(""), skip, &mut lines)?;
    lines.sort_by(|a, b| {
        a.split_once("  ")
            .map(|x| x.1)
            .cmp(&b.split_once("  ").map(|x| x.1))
    });
    Ok(lines.iter().map(|line| format!("{line}\n")).collect())
}

/// Source files a tarball update would replace; `target/` holds the engine
/// and generated state, which the engine/version checks cover separately.
fn source_entries(manifest: &str) -> Vec<&str> {
    manifest
        .lines()
        .filter(|line| {
            !line
                .split_once("  ")
                .is_some_and(|(_, p)| p.starts_with("target/") || p == "target")
        })
        .collect()
}

// ---------------------------------------------------------------------------
// State files

fn read_kv(path: &Path) -> Option<BTreeMap<String, String>> {
    let text = fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    )
}

fn write_kv(path: &Path, values: &[(&str, &str)]) -> io::Result<()> {
    let text: String = values.iter().map(|(k, v)| format!("{k}={v}\n")).collect();
    release::atomic_write(path, &text)
}

fn remove(path: &Path) {
    let _ = if path.is_dir() && !path.is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
}

/// A verified package waiting for the next fresh start.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Pending {
    pub target: String,
    pub base: String,
    pub kind: String,
    pub base_rev: String,
    pub target_rev: String,
    pub package: String,
    pub manifest: String,
    pub prepared: u64,
}

impl Pending {
    pub(crate) fn read(dir: &Path) -> Option<Self> {
        let kv = read_kv(&dir.join("pending"))?;
        let get = |key: &str| kv.get(key).cloned().unwrap_or_default();
        let pending = Self {
            target: get("target"),
            base: get("base"),
            kind: get("kind"),
            base_rev: get("base_rev"),
            target_rev: get("target_rev"),
            package: get("package"),
            manifest: get("manifest"),
            prepared: get("prepared").parse().ok()?,
        };
        // The package name is joined to the state dir: keep it one component.
        (release::valid_tag(&pending.target)
            && pending.package == format!("pkg-{}", pending.target))
        .then_some(pending)
    }

    fn write(&self, dir: &Path) -> io::Result<()> {
        write_kv(
            &dir.join("pending"),
            &[
                ("target", &self.target),
                ("base", &self.base),
                ("kind", &self.kind),
                ("base_rev", &self.base_rev),
                ("target_rev", &self.target_rev),
                ("package", &self.package),
                ("manifest", &self.manifest),
                ("prepared", &self.prepared.to_string()),
            ],
        )
    }

    /// Recompute the package manifest and compare it with the recorded hash.
    pub(crate) fn intact(&self, dir: &Path) -> bool {
        tree_manifest(&dir.join(&self.package), &|_| false)
            .is_ok_and(|manifest| sha256(manifest.as_bytes()) == self.manifest)
    }
}

/// Drop the ready package and its marker (marker first: a crash in between
/// leaves an orphan package, never a marker pointing at nothing).
pub(crate) fn discard_pending(dir: &Path) {
    if let Some(pending) = Pending::read(dir) {
        remove(&dir.join("pending"));
        remove(&dir.join(&pending.package));
    } else {
        remove(&dir.join("pending"));
    }
}

fn set_status(dir: &Path, message: &str) {
    let _ = release::atomic_write(&dir.join("status"), &format!("{message}\n"));
}

// ---------------------------------------------------------------------------
// Eligibility

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Install {
    Git { head: String },
    Tarball,
}

fn git_line(plugin_dir: &Path, args: &[&str]) -> Option<String> {
    release::git_output(plugin_dir, args)
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|line| !line.is_empty())
}

fn stable(tag: &str) -> bool {
    release::valid_tag(tag) && !tag.contains('-')
}

/// Local checks only: what this install is, or why it is skipped. The same
/// rules guard preparation and activation.
pub(crate) fn eligibility(plugin_dir: &Path) -> Result<(String, Install), String> {
    let engine = release::engine_path(plugin_dir);
    if !coordinated(plugin_dir) {
        return Err("custom or development engine".into());
    }
    let current = release::manifest_tag(plugin_dir).ok_or("unreadable package version")?;
    if !stable(&current) {
        return Err(format!("{current} is not a stable release"));
    }
    let state = fs::read_to_string(release::state_path(plugin_dir)).unwrap_or_default();
    let mut state = state.lines();
    let (installed, installed_rev) = (state.next().unwrap_or(""), state.next().unwrap_or(""));
    let install = if plugin_dir.join(".git").exists() {
        // A branch checkout belongs to `git pull`/TPM; switching it to a
        // detached tag would break their updates.
        if git_line(plugin_dir, &["symbolic-ref", "-q", "HEAD"]).is_some() {
            return Err("branch checkout (updated by git pull or TPM)".into());
        }
        match release::git_previous(plugin_dir) {
            Err("dirty") => return Err("uncommitted changes".into()),
            Err(_) => return Err("cannot inspect the git checkout".into()),
            Ok(_) => {}
        }
        if git_line(plugin_dir, &["describe", "--tags", "--exact-match", "HEAD"]).as_deref()
            != Some(current.as_str())
        {
            return Err("development checkout (not on a release tag)".into());
        }
        let head = git_line(plugin_dir, &["rev-parse", "HEAD"]).ok_or("cannot read HEAD")?;
        if installed_rev != head {
            return Err("engine does not match the checkout".into());
        }
        Install::Git { head }
    } else {
        if installed_rev != "-" {
            return Err("engine does not match the source".into());
        }
        Install::Tarball
    };
    if installed != current || !release::engine_matches(&engine, &current) {
        return Err("engine does not match the source".into());
    }
    Ok((current, install))
}

/// Compare a tarball tree with the recorded baseline for `current`.
fn baseline_matches(dir: &Path, plugin_dir: &Path, current: &str) -> Option<bool> {
    let text = fs::read_to_string(dir.join("baseline")).ok()?;
    let (tag, manifest) = text.split_once('\n')?;
    if tag != current {
        return None;
    }
    let tree = tree_manifest(plugin_dir, &|path| path == "target" || path == ".git").ok()?;
    Some(source_entries(manifest) == source_entries(&tree))
}

fn record_baseline(dir: &Path, tag: &str, manifest: &str) -> io::Result<()> {
    release::atomic_write(&dir.join("baseline"), &format!("{tag}\n{manifest}"))
}

// ---------------------------------------------------------------------------
// Preparation

/// Scratch space inside the state dir (same filesystem as the final package).
struct Work(PathBuf);

impl Drop for Work {
    fn drop(&mut self) {
        remove(&self.0);
    }
}

fn validate_package(package: &Path, target: &str) -> Result<String, String> {
    if release::manifest_tag(package).as_deref() != Some(target) {
        return Err("package version does not match the release".into());
    }
    for required in [
        "agenmux.tmux",
        "scripts/install-bin.sh",
        "scripts/version.sh",
        "agents",
    ] {
        if !package.join(required).exists() {
            return Err(format!("package lacks {required}"));
        }
    }
    let manifest =
        tree_manifest(package, &|_| false).map_err(|e| format!("unsafe package: {e}"))?;
    // fetch_package already checked the version after checksum verification;
    // check again on the extracted file the manifest now pins.
    if !release::engine_matches(&release::package_engine(package), target) {
        return Err("package engine does not match the release".into());
    }
    Ok(manifest)
}

/// Throttled automatic check. Returns a one-line outcome for logs and tests.
pub(crate) fn prepare(plugin_dir: &Path) -> String {
    if !enabled() {
        return "auto-update is off".into();
    }
    let (current, install) = match eligibility(plugin_dir) {
        Ok(found) => found,
        Err(reason) => return format!("skipped: {reason}"),
    };
    let Ok(dir) = ensure_state_dir(plugin_dir) else {
        return "skipped: no private state directory".into();
    };
    let Some(_worker) = try_lock(&dir, "prepare.lock") else {
        return "another preparation is running".into();
    };
    let last = fs::read_to_string(dir.join("last-attempt"))
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok());
    if last.is_some_and(checked_recently) {
        return "checked within the last day".into();
    }
    // Recorded before any network work, so failures throttle too.
    if release::atomic_write(&dir.join("last-attempt"), &format!("{}\n", now())).is_err() {
        return "skipped: cannot record the attempt".into();
    }
    for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with("work-") {
            remove(&entry.path());
        }
    }
    match prepare_locked(plugin_dir, &dir, &current, &install) {
        Ok(message) => {
            remove(&dir.join("status"));
            message
        }
        Err(reason) => {
            set_status(&dir, &format!("auto-update: {reason}"));
            reason
        }
    }
}

fn prepare_locked(
    plugin_dir: &Path,
    dir: &Path,
    current: &str,
    install: &Install,
) -> Result<String, String> {
    let latest = release::latest_remote_tag(&release::repo()).ok_or("release check failed")?;
    if !stable(&latest) || release::compare_tags(&latest, current) != std::cmp::Ordering::Greater {
        return Ok(format!("{current} is current"));
    }
    if fs::read_to_string(dir.join("failed")).is_ok_and(|failed| failed.trim() == latest) {
        return Err(format!(
            "{latest} failed to start; waiting for a newer release"
        ));
    }
    if Pending::read(dir).is_some_and(|pending| {
        pending.target == latest && pending.base == current && pending.intact(dir)
    }) {
        return Ok(format!("{latest} ready"));
    }
    let work = Work(dir.join(format!(
        "work-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )));
    fs::create_dir(&work.0).map_err(|_| "cannot create staging")?;

    if *install == Install::Tarball && baseline_matches(dir, plugin_dir, current).is_none() {
        // First check of an existing tarball install: compare the tree with
        // its own verified release package once, then keep that baseline.
        let base = release::fetch_package(plugin_dir, current, &work.0.join("base"))
            .map_err(|_| format!("could not download {current} to verify local files"))?;
        let manifest = validate_package(&base, current)?;
        record_baseline(dir, current, &manifest).map_err(|_| "cannot record baseline")?;
        remove(&work.0.join("base"));
    }
    if *install == Install::Tarball && baseline_matches(dir, plugin_dir, current) != Some(true) {
        return Err("local changes in the plugin directory".into());
    }

    let package = release::fetch_package(plugin_dir, &latest, &work.0.join("target"))
        .map_err(|_| format!("could not download {latest}"))?;
    let manifest = validate_package(&package, &latest)?;
    let (kind, base_rev, target_rev) = match install {
        Install::Git { head } => {
            let refspec = format!("refs/tags/{latest}:refs/tags/{latest}");
            // A failed fetch is fine when the tag is already local.
            let _ = Command::new("git")
                .arg("-C")
                .arg(plugin_dir)
                .args(["fetch", "--quiet", "--no-tags", "origin", &refspec])
                .env("GIT_HTTP_LOW_SPEED_LIMIT", "1000")
                .env("GIT_HTTP_LOW_SPEED_TIME", "20")
                .env("GIT_TERMINAL_PROMPT", "0")
                .env(
                    "GIT_SSH_COMMAND",
                    std::env::var("GIT_SSH_COMMAND").unwrap_or_else(|_| {
                        "ssh -oBatchMode=yes -oConnectTimeout=20 -oServerAliveInterval=20".into()
                    }),
                )
                .stdin(Stdio::null())
                .output();
            let commit = release::git_tag_commit(plugin_dir, &latest)
                .ok_or_else(|| format!("could not fetch {latest} into the checkout"))?;
            ("git", head.clone(), commit)
        }
        Install::Tarball => ("tarball", "-".to_string(), "-".to_string()),
    };

    // Publish: only if policy and base are unchanged since the work began,
    // so a manual switch or opt-out that raced this worker always wins.
    let _state = state_lock(dir).map_err(|_| "state lock unavailable")?;
    if !enabled() {
        return Ok("auto-update turned off during preparation".into());
    }
    if eligibility(plugin_dir).ok() != Some((current.to_string(), install.clone())) {
        return Ok("installation changed during preparation".into());
    }
    discard_pending(dir);
    let name = format!("pkg-{latest}");
    remove(&dir.join(&name));
    fs::rename(&package, dir.join(&name)).map_err(|_| "cannot stage package")?;
    let pending = Pending {
        target: latest.clone(),
        base: current.to_string(),
        kind: kind.into(),
        base_rev,
        target_rev,
        package: name,
        manifest: sha256(manifest.as_bytes()),
        prepared: now(),
    };
    if !pending.intact(dir) || pending.write(dir).is_err() {
        discard_pending(dir);
        remove(&dir.join(&pending.package));
        return Err("cannot publish the prepared update".into());
    }
    Ok(format!("{latest} ready"))
}

// ---------------------------------------------------------------------------
// Activation on the next fresh start

/// How a launch re-enters after switching (and after rolling back).
#[derive(Clone)]
pub(crate) enum Entry {
    /// `agenmux.tmux activate <mode> <client>`: the tmux bootstrap.
    Toggle { mode: String, client: String },
    /// This engine again with the same arguments: direct sidebar or daemon.
    Direct,
}

/// What a launch holds after the gate.
pub(crate) enum Gate {
    /// No runtime used the install; this shared lease keeps it that way.
    Lease(Lock),
    /// Another runtime holds a lease (or nothing to coordinate): take one.
    Open,
    /// This process is the target release of an activation it must confirm.
    Txn(Txn),
    /// The launch was handed to the installed release; exit with this code.
    Exit(i32),
}

/// An activation the target release confirms after readiness, or undoes.
pub(crate) struct Txn {
    lock: Lock,
    dir: PathBuf,
    plugin_dir: PathBuf,
    entry: Entry,
}

/// Test hook: fail the named activation step. Only ever makes activation fail.
fn fault(step: &str) -> io::Result<()> {
    if std::env::var("AGENMUX_ACTIVATION_FAULT").as_deref() == Ok(step) {
        return Err(io::Error::other(format!("injected fault: {step}")));
    }
    Ok(())
}

fn notifier_path(plugin_dir: &Path) -> PathBuf {
    release::release_dir(plugin_dir).join(format!("{}-notifier", release::runtime_name(plugin_dir)))
}

fn restore_file(saved: &Path, path: &Path) -> io::Result<()> {
    if !saved.exists() {
        return match fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let staged = path.with_extension(format!("restore-{}", std::process::id()));
    fs::copy(saved, &staged)?;
    fs::rename(staged, path)
}

/// Atomically swap two directories, so the plugin path never goes missing.
/// Falls back to two renames where the filesystem cannot exchange.
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let (from, to) = (
        std::ffi::CString::new(a.as_os_str().as_bytes())?,
        std::ffi::CString::new(b.as_os_str().as_bytes())?,
    );
    #[cfg(target_os = "macos")]
    let swapped = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } == 0;
    // The raw syscall: musl release builds have no renameat2 wrapper.
    #[cfg(target_os = "linux")]
    let swapped = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } == 0;
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let swapped = {
        let _ = (&from, &to);
        false
    };
    if swapped {
        return Ok(());
    }
    // ponytail: non-atomic fallback; a crash between the renames leaves `b`
    // missing until the next start's recovery renames it back.
    let parked = b.with_extension(format!("swap-{}", std::process::id()));
    fs::rename(b, &parked)?;
    if let Err(error) = fs::rename(a, b) {
        let _ = fs::rename(&parked, b);
        return Err(error);
    }
    fs::rename(parked, a)
}

/// Put back the installation a transaction replaced and drop the transaction.
/// `failed` records the target so it is never retried automatically: set for
/// a target that did not become ready, not for a local error while switching.
/// Returns whether the previous files are in place.
fn recover(plugin_dir: &Path, dir: &Path, failed: bool) -> bool {
    let Some(txn) = read_kv(&dir.join("transaction")) else {
        return true;
    };
    let get = |key: &str| txn.get(key).cloned().unwrap_or_default();
    let (base, target) = (get("base"), get("target"));
    let backup = dir.join("backup");
    let restored = match get("kind").as_str() {
        "git" => {
            let base_ref = get("base_ref");
            let checkout = !base_ref.is_empty()
                && release::git_output(plugin_dir, &["checkout", "--quiet", &base_ref])
                    .is_some_and(|output| output.status.success());
            let files = [
                ("engine", release::engine_path(plugin_dir)),
                ("notifier", notifier_path(plugin_dir)),
                ("state", release::state_path(plugin_dir)),
            ]
            .iter()
            .all(|(name, path)| restore_file(&backup.join(name), path).is_ok());
            checkout && files
        }
        "tarball" => {
            // The old tree is in the backup, or still where the package was
            // if the switch stopped between its two steps.
            let is_base =
                |path: &Path| release::manifest_tag(path).as_deref() == Some(base.as_str());
            let package = dir.join(format!("pkg-{target}"));
            let old = [backup.join("tree"), package.clone()]
                .into_iter()
                .find(|path| is_base(path));
            let back = match &old {
                _ if is_base(plugin_dir) => true,
                Some(old) if plugin_dir.exists() => exchange(old, plugin_dir).is_ok(),
                Some(old) => fs::rename(old, plugin_dir).is_ok(),
                None => false,
            };
            // The new tree is where the old one was; keep it as the package
            // so a retry needs no download.
            if back && old.is_some_and(|old| old != package) {
                let _ = fs::rename(backup.join("tree"), &package);
            }
            // The switch may already have written the version state into it.
            let _ = fs::remove_file(release::state_path(&package));
            back
        }
        _ => true,
    };
    if failed && release::valid_tag(&target) {
        let _ = release::atomic_write(&dir.join("failed"), &format!("{target}\n"));
    }
    if restored {
        // Marker first: a backup without a marker is ignored; a marker with
        // a half-deleted backup would restore a broken tree.
        remove(&dir.join("transaction"));
        remove(&backup);
        if failed {
            discard_pending(dir);
            set_status(
                dir,
                &format!("auto-update: {target} did not start; kept {base}"),
            );
        }
    } else {
        // Keep the backup and marker: the next start tries the restore again.
        set_status(dir, &format!("auto-update: could not restore {base}"));
    }
    restored
}

enum Check {
    Apply(Pending, Install),
    Keep,
    Discard(String),
}

// "Prepared before this invocation" holds by construction: the gate runs
// before this process can start a worker. No timestamp compare, so clock
// changes cannot strand a prepared release.
fn check(plugin_dir: &Path, dir: &Path) -> Check {
    let Some(pending) = Pending::read(dir) else {
        return Check::Keep;
    };
    if !enabled() {
        return Check::Keep;
    }
    let Ok((current, install)) = eligibility(plugin_dir) else {
        return Check::Keep;
    };
    if pending.base != current
        || release::compare_tags(&pending.target, &current) != std::cmp::Ordering::Greater
    {
        return Check::Discard("installed version changed since preparation".into());
    }
    if fs::read_to_string(dir.join("failed")).is_ok_and(|failed| failed.trim() == pending.target) {
        return Check::Discard(format!("{} failed to start before", pending.target));
    }
    match &install {
        Install::Git { head } => {
            if pending.kind != "git"
                || *head != pending.base_rev
                || release::git_tag_commit(plugin_dir, &pending.target).as_deref()
                    != Some(pending.target_rev.as_str())
            {
                return Check::Discard("checkout changed since preparation".into());
            }
        }
        Install::Tarball => {
            if pending.kind != "tarball" {
                return Check::Discard("installation changed since preparation".into());
            }
            if baseline_matches(dir, plugin_dir, &current) != Some(true) {
                return Check::Keep;
            }
        }
    }
    if !pending.intact(dir) {
        return Check::Discard("prepared package changed on disk".into());
    }
    Check::Apply(pending, install)
}

/// Swap source, engine, notifier and version state, offline. The transaction
/// marker is written only once the backup is complete, so a crash before it
/// changed nothing and a crash after it is undone by `recover`.
fn apply(plugin_dir: &Path, dir: &Path, pending: &Pending, install: &Install) -> io::Result<()> {
    let backup = dir.join("backup");
    remove(&backup);
    fs::create_dir(&backup)?;
    let package = dir.join(&pending.package);
    let target = pending.target.as_str();
    match install {
        Install::Git { .. } => {
            let base_ref = release::git_previous(plugin_dir).map_err(io::Error::other)?;
            for (name, path) in [
                ("engine", release::engine_path(plugin_dir)),
                ("notifier", notifier_path(plugin_dir)),
                ("state", release::state_path(plugin_dir)),
            ] {
                if path.exists() {
                    fs::copy(&path, backup.join(name))?;
                }
            }
            write_kv(
                &dir.join("transaction"),
                &[
                    ("kind", "git"),
                    ("base", &pending.base),
                    ("base_ref", &base_ref),
                    ("target", target),
                ],
            )?;
            fault("source")?;
            release::git_checkout_tag(plugin_dir, target).map_err(io::Error::other)?;
            fault("engine")?;
            let swap = backup.join("swap");
            fs::create_dir(&swap)?;
            release::install_engine_from(plugin_dir, &package, target, &swap)
                .map_err(io::Error::other)?;
        }
        Install::Tarball => {
            write_kv(
                &dir.join("transaction"),
                &[
                    ("kind", "tarball"),
                    ("base", &pending.base),
                    ("target", target),
                ],
            )?;
            fault("source")?;
            exchange(&package, plugin_dir)?;
            fault("engine")?;
            fs::rename(&package, backup.join("tree"))?;
            release::write_engine_state(plugin_dir, target)?;
        }
    }
    fault("state")?;
    if !release::engine_matches(&release::engine_path(plugin_dir), target) {
        return Err(io::Error::other("installed engine does not match"));
    }
    Ok(())
}

/// Load the installed release's tmux integration (launchers and setup), the
/// way sourcing the plugin does. Outside tmux there is nothing to set up.
/// Bounded: a hung setup must not hold the installation lock forever.
fn load_plugin(plugin_dir: &Path) -> bool {
    if std::env::var_os("TMUX").is_none_or(|tmux| tmux.is_empty()) {
        return true;
    }
    if fault("setup").is_err() {
        return false;
    }
    let Ok(mut child) = Command::new("bash")
        .arg(plugin_dir.join(format!("{}.tmux", release::runtime_name(plugin_dir))))
        .env("AGENMUX_INSTALL_REFRESH", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// macOS: refresh the notification helper app from the new engine, as
/// install-bin.sh does after an install. Best effort.
fn sync_notifier(plugin_dir: &Path) {
    if !cfg!(target_os = "macos") || !notifier_path(plugin_dir).is_file() {
        return;
    }
    let quiet = |command: &mut Command| {
        command
            .env("AGENMUX_DIR", plugin_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    if quiet(
        Command::new(release::engine_path(plugin_dir)).args(["internal", "notification-eligible"]),
    ) {
        quiet(
            Command::new("bash")
                .arg(plugin_dir.join("scripts/install-app.sh"))
                .arg("--quiet"),
        );
    }
}

/// Replace this process with the installed release's public entry point.
fn reenter(plugin_dir: &Path, entry: &Entry, lock: Option<&Lock>) -> io::Error {
    use std::os::unix::process::CommandExt;
    let mut command = match entry {
        Entry::Toggle { mode, client } => {
            let mut command = Command::new("bash");
            command
                .arg(plugin_dir.join(format!("{}.tmux", release::runtime_name(plugin_dir))))
                .args(["activate", mode, client]);
            command
        }
        Entry::Direct => {
            let mut command = Command::new(release::engine_path(plugin_dir));
            command.args(std::env::args_os().skip(1));
            command
        }
    };
    command.env("AGENMUX_DIR", plugin_dir);
    if let Some(lock) = lock {
        lock.hand_down(&mut command, "AGENMUX_UPDATE_TXN_FD");
    }
    command.exec()
}

/// This process's engine is no longer the installed one: hand the launch to
/// the installed release. Returns only if that is impossible.
pub(crate) fn restart_installed(plugin_dir: &Path, entry: Entry) -> i32 {
    eprintln!("agenmux: {}", reenter(plugin_dir, &entry, None));
    1
}

fn running_release() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

/// The common launch gate. A fresh start (no runtime holds a lease) recovers
/// an interrupted activation, then activates a package prepared earlier and
/// re-enters the target; anything else leaves the update pending.
pub(crate) fn gate(plugin_dir: &Path, entry: Entry) -> Gate {
    if let Some(lock) = Lock::inherited("AGENMUX_UPDATE_TXN_FD") {
        if let Some(dir) = state_dir(plugin_dir) {
            if dir.join("transaction").is_file() {
                return Gate::Txn(Txn {
                    lock,
                    dir,
                    plugin_dir: plugin_dir.to_path_buf(),
                    entry,
                });
            }
        }
        return Gate::Open;
    }
    let Some(dir) = state_dir(plugin_dir).filter(|dir| dir.is_dir()) else {
        return Gate::Open;
    };
    let Ok(lock) = Lock::acquire(&dir.join("install.lock"), libc::LOCK_EX, Duration::ZERO) else {
        return Gate::Open;
    };
    let Ok(plugin_dir) = fs::canonicalize(plugin_dir) else {
        return Gate::Open;
    };
    let plugin_dir = plugin_dir.as_path();
    // A target that never confirmed (crashed, killed, hung): restore the
    // previous release and, if this process is that target, start the
    // previous release's own entry point instead of continuing as the target.
    if dir.join("transaction").exists()
        && recover(plugin_dir, &dir, true)
        && release::manifest_tag(plugin_dir).is_some_and(|tag| tag != running_release())
    {
        drop(lock);
        return Gate::Exit(restart_installed(plugin_dir, entry));
    }
    let into_lease = |lock: Lock| {
        // flock conversion is not atomic; never block in it. Losing the
        // instant to another launch falls back to an ordinary lease wait.
        if unsafe { libc::flock(lock.file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
            Gate::Lease(lock)
        } else {
            Gate::Open
        }
    };
    if !coordinated(plugin_dir) {
        return into_lease(lock);
    }
    let Ok(state) = state_lock(&dir) else {
        return into_lease(lock);
    };
    let (pending, install) = match check(plugin_dir, &dir) {
        Check::Apply(pending, install) => (pending, install),
        Check::Keep => return into_lease(lock),
        Check::Discard(reason) => {
            discard_pending(&dir);
            set_status(&dir, &format!("auto-update: {reason}"));
            return into_lease(lock);
        }
    };
    if let Err(error) = apply(plugin_dir, &dir, &pending, &install) {
        // A local failure (busy git index, full disk): retried next start.
        recover(plugin_dir, &dir, false);
        set_status(
            &dir,
            &format!(
                "auto-update: could not switch to {}: {error}",
                pending.target
            ),
        );
        return into_lease(lock);
    }
    drop(state);
    if !load_plugin(plugin_dir) {
        recover(plugin_dir, &dir, true);
        load_plugin(plugin_dir);
        return into_lease(lock);
    }
    sync_notifier(plugin_dir);
    let _ = reenter(plugin_dir, &entry, Some(&lock));
    // exec failed: the old engine keeps running on the old files.
    recover(plugin_dir, &dir, false);
    load_plugin(plugin_dir);
    into_lease(lock)
}

impl Txn {
    pub(crate) fn lock(&self) -> &Lock {
        &self.lock
    }

    /// The target is up: keep it, and turn the exclusive hold into the
    /// launch's shared lease (shared with a daemon that inherited it).
    pub(crate) fn commit(self) -> Lock {
        let _state = state_lock(&self.dir);
        let txn = read_kv(&self.dir.join("transaction")).unwrap_or_default();
        if txn.get("kind").map(String::as_str) == Some("tarball") {
            if let (Some(target), Ok(manifest)) = (
                txn.get("target"),
                tree_manifest(&self.plugin_dir, &|path| path == "target" || path == ".git"),
            ) {
                let _ = record_baseline(&self.dir, target, &manifest);
            }
        }
        // Marker first, as in `recover`.
        remove(&self.dir.join("transaction"));
        remove(&self.dir.join("backup"));
        discard_pending(&self.dir);
        remove(&self.dir.join("status"));
        self.lock.share();
        self.lock
    }

    /// The target failed to become ready: restore the previous release and
    /// start it once through its own entry point. Returns only if that fails.
    pub(crate) fn rollback(self) -> i32 {
        recover(&self.plugin_dir, &self.dir, true);
        load_plugin(&self.plugin_dir);
        let Txn {
            lock,
            plugin_dir,
            entry,
            ..
        } = self;
        drop(lock);
        restart_installed(&plugin_dir, entry)
    }
}

/// Gate for a sidebar or daemon started directly, not by a launch that
/// already passed it: readiness is a valid configuration in the target.
pub(crate) fn gate_direct(plugin_dir: &Path) -> Option<Lock> {
    match gate(plugin_dir, Entry::Direct) {
        Gate::Lease(lock) => Some(lock),
        Gate::Open => None,
        Gate::Exit(code) => std::process::exit(code),
        Gate::Txn(txn) => {
            if crate::app_config::current_process().is_ok() {
                Some(txn.commit())
            } else {
                std::process::exit(txn.rollback())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Manual version switches

/// `auto_update = false`, written before a manual switch so a worker that
/// finishes late sees it, and undone if the switch fails.
pub(crate) struct Pause {
    path: PathBuf,
    previous: Option<String>,
    written: String,
}

/// Persist the pause, or explain why it cannot be. Only the plugin's own
/// engine is ever auto-updated, so only it records a pause.
pub(crate) fn pause(plugin_dir: &Path) -> Result<Option<Pause>, String> {
    if !coordinated(plugin_dir) {
        return Ok(None);
    }
    let unusable = |error: crate::app_config::ConfigError| {
        format!("cannot pause auto-update ({error}); fix config.toml and retry")
    };
    let (path, existed, source) = crate::app_config::document().map_err(unusable)?;
    let file = crate::app_config::parse(&source).map_err(unusable)?;
    if file.behavior.and_then(|behavior| behavior.auto_update) == Some(false) {
        return Ok(None);
    }
    let written = crate::app_config::edit_document(&source, "behavior.auto_update", Some("false"))
        .map_err(unusable)?;
    crate::app_config::save_document(&path, &written).map_err(unusable)?;
    Ok(Some(Pause {
        path,
        previous: existed.then_some(source),
        written,
    }))
}

impl Pause {
    pub(crate) fn undo(self) {
        match self.previous {
            Some(source) => {
                let _ = crate::app_config::save_document(&self.path, &source);
            }
            // Remove the file this switch created, unless edited since.
            None => {
                if fs::read_to_string(&self.path).is_ok_and(|now| now == self.written) {
                    let _ = fs::remove_file(&self.path);
                }
            }
        }
    }
}

/// After a successful manual switch nothing prepared for, or failed on, the
/// previous version survives.
pub(crate) fn switched(plugin_dir: &Path) {
    let Some(dir) = state_dir(plugin_dir).filter(|dir| dir.is_dir()) else {
        return;
    };
    if let Ok(_state) = state_lock(&dir) {
        // An unconfirmed activation this switch replaced must not be "recovered"
        // over it at the next start.
        remove(&dir.join("transaction"));
        remove(&dir.join("backup"));
        discard_pending(&dir);
        remove(&dir.join("failed"));
        remove(&dir.join("status"));
    }
}

/// Settings turned auto-update back on: allow retrying a failed target and
/// check at the next scheduler tick instead of waiting out the day.
pub(crate) fn resumed(plugin_dir: &Path) {
    let Some(dir) = state_dir(plugin_dir).filter(|dir| dir.is_dir()) else {
        return;
    };
    if let Ok(_state) = state_lock(&dir) {
        remove(&dir.join("failed"));
        remove(&dir.join("last-attempt"));
        remove(&dir.join("status"));
    }
}

/// Start a detached worker if the daily check is due. Cheap enough to call
/// on every start and hourly from long-running views.
pub(crate) fn kick(plugin_dir: &Path) {
    if !enabled() || eligibility(plugin_dir).is_err() {
        return;
    }
    let due = state_dir(plugin_dir)
        .and_then(|dir| fs::read_to_string(dir.join("last-attempt")).ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .is_none_or(|last| !checked_recently(last));
    if !due {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(exe);
    // Own session: closing the view's pane must not kill a download, and no
    // controlling terminal means nothing (ssh, git) can prompt.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if let Ok(mut child) = command
        .args(["internal", "auto-update"])
        .env("AGENMUX_DIR", plugin_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Hourly due-check for daemons and popups that stay open for days.
pub(crate) fn spawn_scheduler(plugin_dir: PathBuf) {
    std::thread::spawn(move || loop {
        kick(&plugin_dir);
        std::thread::sleep(Duration::from_secs(60 * 60));
    });
}

/// One line for the version picker: what auto-update will do or why not.
pub(crate) fn picker_note(plugin_dir: &Path) -> Option<String> {
    if !enabled() {
        return Some("auto-update off · turn on in settings".into());
    }
    let dir = state_dir(plugin_dir);
    if let Some(pending) = dir.as_deref().and_then(Pending::read) {
        if release::compare_tags(&pending.target, &release::manifest_tag(plugin_dir)?)
            == std::cmp::Ordering::Greater
        {
            return Some(format!("{} ready · next start", pending.target));
        }
    }
    if let Err(reason) = eligibility(plugin_dir) {
        return Some(format!("auto-update skipped: {reason}"));
    }
    dir.and_then(|dir| fs::read_to_string(dir.join("status")).ok())
        .map(|status| status.trim().to_string())
        .filter(|status| !status.is_empty())
}

/// The prepared target, for the header and picker row marks.
pub(crate) fn ready_target(plugin_dir: &Path) -> Option<String> {
    let pending = Pending::read(&state_dir(plugin_dir)?)?;
    (release::manifest_tag(plugin_dir).as_deref() == Some(pending.base.as_str()))
        .then_some(pending.target)
}

/// `agenmux internal auto-update`: the detached worker kick() starts..
pub fn run(plugin_dir: &Path) -> i32 {
    println!("agenmux: {}", prepare(plugin_dir));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256(&[b'a'; 1000]),
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "agenmux-autoupdate-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn manifests_pin_contents_and_refuse_escaping_links() {
        let root = scratch("manifest");
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/file"), "x").unwrap();
        std::os::unix::fs::symlink("b/file", root.join("a/inside")).unwrap();
        let manifest = tree_manifest(&root, &|_| false).unwrap();
        assert!(manifest.contains("link:b/file  a/inside\n"));
        assert!(manifest.contains(&format!("{}  a/b/file\n", sha256(b"x"))));
        fs::write(root.join("a/b/file"), "y").unwrap();
        assert_ne!(tree_manifest(&root, &|_| false).unwrap(), manifest);
        for escaping in ["../../outside", "/etc/passwd", "b/../../.."] {
            let link = root.join("a/escape");
            let _ = fs::remove_file(&link);
            std::os::unix::fs::symlink(escaping, &link).unwrap();
            assert!(tree_manifest(&root, &|_| false).is_err(), "{escaping}");
        }
        fs::remove_file(root.join("a/escape")).unwrap();
        fs::hard_link(root.join("a/b/file"), root.join("hard")).unwrap();
        assert!(tree_manifest(&root, &|_| false).is_err());
        assert!(tree_manifest(&root, &|path| path == "hard" || path == "a").is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pending_markers_round_trip_and_reject_foreign_paths() {
        let dir = scratch("pending");
        let pending = Pending {
            target: "v1.2.0".into(),
            base: "v1.1.0".into(),
            kind: "git".into(),
            base_rev: "abc".into(),
            target_rev: "def".into(),
            package: "pkg-v1.2.0".into(),
            manifest: "00".into(),
            prepared: 7,
        };
        pending.write(&dir).unwrap();
        assert_eq!(Pending::read(&dir), Some(pending.clone()));
        let text = fs::read_to_string(dir.join("pending")).unwrap();
        fs::write(dir.join("pending"), text.replace("pkg-v1.2.0", "../../x")).unwrap();
        assert_eq!(Pending::read(&dir), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn handed_down_leases_outlive_the_parent_descriptor() {
        let dir = scratch("lease");
        let path = dir.join("install.lock");
        let lease = Lock::acquire(&path, libc::LOCK_SH, Duration::ZERO).unwrap();
        let mut child = Command::new("sh");
        child.args(["-c", "read _"]).stdin(Stdio::piped());
        lease.hand_down(&mut child, "AGENMUX_LEASE_FD");
        let mut child = child.spawn().unwrap();
        drop(lease);
        // The child still shares the open file description, so the lock holds.
        assert!(Lock::acquire(&path, libc::LOCK_EX, Duration::ZERO).is_err());
        drop(child.stdin.take());
        child.wait().unwrap();
        // Released, give or take a process another test is starting: it holds
        // a copy of every descriptor between its fork and exec.
        let released = Duration::from_secs(5);
        assert!(Lock::acquire(&path, libc::LOCK_EX, released).is_ok());
        // An ordinary child never inherits it.
        let lease = Lock::acquire(&path, libc::LOCK_SH, Duration::ZERO).unwrap();
        let mut plain = Command::new("sh")
            .args(["-c", "read _"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        drop(lease);
        assert!(Lock::acquire(&path, libc::LOCK_EX, released).is_ok());
        drop(plain.stdin.take());
        plain.wait().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn throttle_ignores_stamps_from_the_future() {
        assert!(checked_recently(now() - 60));
        assert!(!checked_recently(now() - DAY - 1));
        assert!(!checked_recently(now() + DAY * 30));
    }

    #[test]
    fn source_entries_ignore_generated_engine_state() {
        let manifest = "aa  agenmux.tmux\nbb  target/release/agenmux\ncc  targets.md\n";
        assert_eq!(
            source_entries(manifest),
            ["aa  agenmux.tmux", "cc  targets.md"]
        );
    }
}
