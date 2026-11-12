// Pane enumeration + state detection over the control-mode pipe.
// Output contract: byte-identical to `scan.sh list` / `scan.sh status`.
use crate::app_config::Glyphs;
use crate::conf::AgentConf;
use crate::mux::{self, Multiplexer};
use crate::procs::{self, IdentCache, Snapshot};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// pane -> (cwd, SUBJECT_CMD output). Startup seeds may hold cwd basename;
/// entries live while the pane sits idle and drop on any state change (a new
/// prompt flips the pane to working), so the fork re-runs only when the
/// subject could actually have changed.
/// Assumes new sessions always bounce through a non-idle state
pub type SubjectCache = HashMap<String, (String, String)>;

/// Subjects from published rows (the scan cache TSV), so a starting daemon
/// or a one-shot `list` does not fork SUBJECT_CMD for unchanged panes.
/// The TSV stores the cwd basename; live scans upgrade matching entries to
/// full paths. Basename can collide; persist full cwd if cache format changes.
pub fn seed_subjects(rows: &[PaneRow], self_pane: &str) -> SubjectCache {
    rows.iter()
        .filter(|row| row.pane != self_pane && !row.title.is_empty())
        .map(|row| (row.pane.clone(), (row.cwd.clone(), row.title.clone())))
        .collect()
}

/// SUBJECT_CMD runs synchronously inside a scan: one call is killed after
/// SUBJECT_CMD_TIMEOUT, and all calls of one scan share SUBJECT_SCAN_BUDGET,
/// so subjects never stall a scan for longer than the budget (#256).
const SUBJECT_CMD_TIMEOUT: Duration = Duration::from_secs(1);
const SUBJECT_SCAN_BUDGET: Duration = Duration::from_secs(2);
const SCREEN_MAX_AGE: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq)]
struct ScreenKey {
    pane: String,
    pid: u32,
    command: String,
    agent: String,
    width: usize,
    height: usize,
    title: String,
    path: String,
}

struct CachedScreen {
    key: ScreenKey,
    screen: String,
    captured_at: Instant,
}

#[derive(Default)]
pub struct ScreenCache {
    panes: HashMap<String, CachedScreen>,
}

impl ScreenCache {
    pub(crate) fn next_expiry(&self) -> Option<Instant> {
        self.panes
            .values()
            .map(|cached| cached.captured_at + SCREEN_MAX_AGE)
            .min()
    }

    fn get_or_capture(
        &mut self,
        key: ScreenKey,
        force: bool,
        now: Instant,
        capture: impl FnOnce() -> Result<String, mux::Error>,
    ) -> Result<(String, bool), mux::Error> {
        let reusable = !force
            && self.panes.get(&key.pane).is_some_and(|cached| {
                cached.key == key
                    && now.saturating_duration_since(cached.captured_at) < SCREEN_MAX_AGE
            });
        if reusable {
            return Ok((self.panes[&key.pane].screen.clone(), true));
        }
        let screen = capture()?;
        self.panes.insert(
            key.pane.clone(),
            CachedScreen {
                key,
                screen: screen.clone(),
                captured_at: now,
            },
        );
        Ok((screen, false))
    }

    /// Last screen of `pane` regardless of key or age: what a yielded scan
    /// shows until the deferred capture lands.
    fn stale(&self, pane: &str) -> Option<String> {
        self.panes.get(pane).map(|cached| cached.screen.clone())
    }
}

pub struct ScanPolicy<'a> {
    pub dirty: &'a std::collections::HashSet<String>,
    pub full: bool,
    pub periodic: bool,
    pub covered_session: Option<&'a str>,
    pub now: Instant,
    /// Key fd to watch between captures: a waiting key stops the capture
    /// loop, and every remaining pane keeps its last screen (`deferred`).
    /// None = capture everything (CLI snapshots, the startup scan).
    pub key_fd: Option<libc::c_int>,
}

#[derive(Default)]
pub struct ScanStats {
    pub captured: usize,
    pub reused: usize,
    /// Panes whose capture yielded to a key; hand them back as dirty so the
    /// next output scan refreshes them.
    pub deferred: std::collections::HashSet<String>,
}

/// Keep the last screen instead of capturing: a key is waiting and the pane
/// has one. A pane with no screen yet is captured anyway — leaving it out
/// would drop its row for a tick.
/// A held key defers every capture; states go stale until release,
/// same as the pre-scan key gate already does.
fn defer_capture(key_waiting: bool, has_screen: bool) -> bool {
    key_waiting && has_screen
}

fn force_capture(policy: &ScanPolicy<'_>, pane: &str, session: &str) -> bool {
    let covered = policy.covered_session == Some(session);
    policy.full
        || policy.dirty.contains(pane)
        || policy.covered_session.is_none()
        || (policy.periodic && !covered)
}

#[derive(Clone)]
pub struct PaneRow {
    pub pane: String,
    pub loc: String,
    pub agent: String,
    pub state: String,
    pub cwd: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneMeta {
    pub pane: String,
    pub pane_index: u32,
    pub pane_title: String,
    pub command: String,
    pub path: String,
    pub window_id: String,
    pub window_index: u32,
    pub window_name: String,
    pub session_id: String,
    pub session_name: String,
    pub agent_index: Option<usize>,
}

pub struct ScanSnapshot {
    pub panes: Vec<PaneMeta>,
    pub agents: Vec<PaneRow>,
}

/// One pane as a backend lists it.
pub struct ParsedPane {
    pub meta: PaneMeta,
    pub pid: u32,
    pub width: usize,
    pub height: usize,
    /// The state the agent reports of itself, where the backend carries it.
    pub reported: Option<&'static str>,
}

pub fn scan(
    mux: &mut dyn Multiplexer,
    confs: &[AgentConf],
    cache: &mut IdentCache,
    subj: &mut SubjectCache,
    self_pane: Option<&str>,
) -> Result<ScanSnapshot, mux::Error> {
    let mut screens = ScreenCache::default();
    let dirty = std::collections::HashSet::new();
    scan_cached(
        mux,
        confs,
        cache,
        subj,
        self_pane,
        &mut screens,
        ScanPolicy {
            dirty: &dirty,
            full: true,
            periodic: true,
            covered_session: None,
            now: Instant::now(),
            key_fd: None,
        },
    )
    .map(|(snapshot, _)| snapshot)
}

pub fn scan_cached(
    mux: &mut dyn Multiplexer,
    confs: &[AgentConf],
    cache: &mut IdentCache,
    subj: &mut SubjectCache,
    self_pane: Option<&str>,
    screens: &mut ScreenCache,
    policy: ScanPolicy<'_>,
) -> Result<(ScanSnapshot, ScanStats), mux::Error> {
    crate::diag::begin_scan();
    let listed = mux.panes(self_pane)?;
    let mut snap: Option<Snapshot> = None;
    let mut panes = Vec::new();
    let mut agents = Vec::new();
    let mut seen = IdentCache::new();
    let mut stats = ScanStats::default();
    let mut subject_spent = Duration::ZERO;
    let result: Result<ScanSnapshot, mux::Error> = (|| {
        for parsed in listed {
            let ParsedPane {
                mut meta,
                pid,
                width,
                height,
                reported,
            } = parsed;
            let pane = meta.pane.as_str();
            let cmd = meta.command.as_str();
            let path = meta.path.as_str();
            let title = meta.pane_title.as_str();
            let key = (pane.to_string(), pid, cmd.to_string());
            let name = cache.get(&key).cloned().or_else(|| {
                (pid != 0)
                    .then(|| procs::identify(confs, &mut snap, pid, cmd))
                    .flatten()
                    .map(|i| confs[i].name.clone())
            });
            let Some(name) = name else {
                panes.push(meta);
                continue;
            };
            seen.insert(key, name.clone());
            let Some(idx) = confs.iter().position(|c| c.name == name) else {
                panes.push(meta);
                continue; // conf removed since cached
            };
            // A reported idle or blocked needs no screen. A reported working
            // still reads it: Pi reports working while its question waits.
            let (state, screen) = if let Some(state) = reported.filter(|&s| s != "working") {
                (state, String::new())
            } else {
                let key = ScreenKey {
                    pane: pane.to_string(),
                    pid,
                    command: cmd.to_string(),
                    agent: name.clone(),
                    width,
                    height,
                    title: title.to_string(),
                    path: path.to_string(),
                };
                let force = force_capture(&policy, pane, &meta.session_id);
                let key_waiting = policy.key_fd.is_some_and(crate::input::key_pending);
                let stale = screens.stale(pane);
                let (screen, reused) = if defer_capture(key_waiting, stale.is_some()) {
                    stats.deferred.insert(pane.to_string());
                    (stale.unwrap_or_default(), true)
                } else {
                    screens.get_or_capture(key, force, policy.now, || mux.capture(pane))?
                };
                if reused {
                    stats.reused += 1;
                } else {
                    stats.captured += 1;
                }
                let state = match crate::detect::detect_state(&confs[idx], title, &screen) {
                    "blocked" => "blocked",
                    detected => reported.unwrap_or(detected),
                };
                // Pane content never leaves a release binary: title and screen
                // tail are dev-build evidence for tuning agents/*.conf rules.
                if cfg!(debug_assertions) && !reused {
                    let tail = screen
                        .lines()
                        .rev()
                        .filter(|line| !line.trim().is_empty())
                        .take(2)
                        .map(|line| line.chars().take(80).collect::<String>())
                        .collect::<Vec<_>>();
                    trace!("detect {pane} {name} state={state} title={title:?} tail={tail:?}");
                }
                (state, screen)
            };
            let mut subject = crate::detect::subject(&confs[idx], title, &screen, path);
            if state != "idle" {
                subj.remove(pane); // pane got a new prompt — cached subject is stale
            } else if subject.is_empty() && confs[idx].subject_cmd.is_some() {
                let cached = subj
                    .get(pane)
                    .filter(|(cwd, _)| {
                        cwd == path || cwd == path.rsplit('/').next().unwrap_or(path)
                    })
                    .map(|(_, subject)| subject.clone());
                match cached {
                    Some(cached) => {
                        subject = cached;
                        subj.insert(pane.to_string(), (path.to_string(), subject.clone()));
                    }
                    None if subject_spent >= SUBJECT_SCAN_BUDGET => {
                        // left uncached: a later scan fills it
                        trace!("subject_cmd {pane} deferred: scan budget spent");
                    }
                    None => {
                        let t0 = std::time::Instant::now();
                        let limit = SUBJECT_CMD_TIMEOUT.min(SUBJECT_SCAN_BUDGET - subject_spent);
                        let started = procs::agent_start(&confs[idx], &mut snap, pid);
                        let out =
                            crate::detect::subject_cmd(&confs[idx], pane, path, started, limit);
                        subject_spent += t0.elapsed();
                        trace!(
                            "subject_cmd {pane} {}ms{}",
                            t0.elapsed().as_millis(),
                            if out.is_none() { " failed" } else { "" }
                        );
                        subject = out.clone().unwrap_or_default();
                        // A command cut short by the scan's budget gets another
                        // try next scan. One that failed or used its whole
                        // timeout stays empty until the pane's next prompt,
                        // rather than costing every scan.
                        if out.is_some() || limit == SUBJECT_CMD_TIMEOUT {
                            subj.insert(pane.to_string(), (path.to_string(), subject.clone()));
                        }
                    }
                }
            }
            meta.agent_index = Some(agents.len());
            agents.push(PaneRow {
                pane: pane.to_string(),
                loc: format!(
                    "{}:{}.{}",
                    meta.session_name, meta.window_index, meta.pane_index
                ),
                agent: name,
                state: state.to_string(),
                cwd: path.rsplit('/').next().unwrap_or(path).to_string(),
                title: subject,
            });
            panes.push(meta);
        }
        trace!(
            "snapshot panes={} agents={} deferred={}",
            panes.len(),
            agents.len(),
            stats.deferred.len()
        );
        Ok(ScanSnapshot { panes, agents })
    })();
    let snapshot = result?;
    subj.retain(|pane, _| seen.keys().any(|k| &k.0 == pane)); // dead panes pruned
    screens
        .panes
        .retain(|pane, _| seen.keys().any(|key| &key.0 == pane));
    *cache = seen;
    Ok((snapshot, stats))
}

pub fn to_tsv(rows: &[PaneRow]) -> String {
    rows.iter()
        .map(|r| {
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                r.pane, r.loc, r.agent, r.state, r.cwd, r.title
            )
        })
        .collect()
}

/// `#[fg=red]⣿#[default]N #[fg=yellow]⣾#[default]N #[fg=green]⣿#[default]N`,
/// zero counts omitted, no trailing space. Empty when no agents. `markup`:
/// tmux status-line colors; without them, plain counts for any status bar.
pub fn status_segment(rows: &[PaneRow], glyphs: Glyphs, markup: bool) -> String {
    let n = |s: &str| rows.iter().filter(|r| r.state == s).count();
    [("blocked", "red"), ("working", "yellow"), ("idle", "green")]
        .into_iter()
        .filter_map(|(state, color)| {
            let count = n(state);
            let glyph = glyphs.state(state);
            (count > 0).then(|| match markup {
                true => format!("#[fg={color}]{glyph}#[default]{count}"),
                false => format!("{glyph}{count}"),
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse cached TSV back into rows (only state is needed downstream, but
/// keep the full row for the sidebar's instant first frame).
pub fn from_tsv(tsv: &str) -> Vec<PaneRow> {
    tsv.lines()
        .filter_map(|line| {
            let mut f = line.splitn(6, '\t');
            Some(PaneRow {
                pane: f.next()?.to_string(),
                loc: f.next()?.to_string(),
                agent: f.next()?.to_string(),
                state: f.next()?.to_string(),
                cwd: f.next()?.to_string(),
                title: f.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn screen_key() -> ScreenKey {
        ScreenKey {
            pane: "%1".into(),
            pid: 42,
            command: "synthetic-agent".into(),
            agent: "synthetic".into(),
            width: 80,
            height: 24,
            title: "task".into(),
            path: "/workspace".into(),
        }
    }

    fn row(state: &str) -> PaneRow {
        PaneRow {
            pane: "%1".into(),
            loc: "s:1.1".into(),
            agent: "claude".into(),
            state: state.into(),
            cwd: "x".into(),
            title: String::new(),
        }
    }

    #[test]
    fn segment_counts_and_omits_zeros() {
        assert_eq!(status_segment(&[], Glyphs::Rich, true), "");
        assert_eq!(
            status_segment(
                &[row("working"), row("idle"), row("idle")],
                Glyphs::Rich,
                true
            ),
            "#[fg=yellow]⣾#[default]1 #[fg=green]⣿#[default]2"
        );
        assert_eq!(
            status_segment(&[row("blocked")], Glyphs::Rich, true),
            "#[fg=red]⣿#[default]1"
        );
        let all = [row("blocked"), row("working"), row("idle")];
        assert_eq!(
            status_segment(&all, Glyphs::Plain, true),
            "#[fg=red]!#[default]1 #[fg=yellow]>#[default]1 #[fg=green].#[default]1"
        );
        assert_eq!(status_segment(&all, Glyphs::Plain, false), "!1 >1 .1");
    }

    #[test]
    fn tsv_roundtrip() {
        let rows = vec![row("idle")];
        let parsed = from_tsv(&to_tsv(&rows));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].state, "idle");
    }

    #[test]
    #[allow(clippy::needless_borrows_for_generic_args)]
    fn successful_unchanged_screen_is_reused_until_dirty_or_expired() {
        let now = Instant::now();
        let captures = Cell::new(0);
        let mut cache = ScreenCache::default();
        let mut capture = || {
            captures.set(captures.get() + 1);
            Ok(format!("screen-{}", captures.get()))
        };

        let (first, reused) = cache
            .get_or_capture(screen_key(), false, now, &mut capture)
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(first, "screen-1");
        assert!(!reused);
        assert_eq!(cache.next_expiry(), Some(now + SCREEN_MAX_AGE));
        let (same, reused) = cache
            .get_or_capture(
                screen_key(),
                false,
                now + Duration::from_secs(9),
                &mut capture,
            )
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(same, "screen-1");
        assert!(reused);
        let (dirty, reused) = cache
            .get_or_capture(
                screen_key(),
                true,
                now + Duration::from_secs(9),
                &mut capture,
            )
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(dirty, "screen-2");
        assert!(!reused);
        assert_eq!(cache.next_expiry(), Some(now + Duration::from_secs(19)));
        let (_, reused) = cache
            .get_or_capture(
                screen_key(),
                false,
                now + Duration::from_secs(19),
                &mut capture,
            )
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(!reused);
        assert_eq!(captures.get(), 3);
    }

    #[test]
    fn identity_metadata_and_size_changes_refresh_the_screen() {
        let now = Instant::now();
        for mutate in [
            |key: &mut ScreenKey| key.pid += 1,
            |key: &mut ScreenKey| key.command.push_str("-new"),
            |key: &mut ScreenKey| key.agent.push_str("-new"),
            |key: &mut ScreenKey| key.width += 1,
            |key: &mut ScreenKey| key.height += 1,
            |key: &mut ScreenKey| key.title.push_str(" changed"),
            |key: &mut ScreenKey| key.path.push_str("/changed"),
        ] {
            let mut cache = ScreenCache::default();
            cache
                .get_or_capture(screen_key(), false, now, || Ok("old".into()))
                .unwrap_or_else(|error| panic!("{error}"));
            let mut changed = screen_key();
            mutate(&mut changed);
            let (_, reused) = cache
                .get_or_capture(changed, false, now, || Ok("new".into()))
                .unwrap_or_else(|error| panic!("{error}"));
            assert!(!reused);
        }
    }

    #[test]
    fn coverage_policy_refreshes_dirty_background_full_and_unknown_panes() {
        let dirty = std::collections::HashSet::from(["%2".to_string()]);
        let policy = |full, periodic, covered_session| ScanPolicy {
            dirty: &dirty,
            full,
            periodic,
            covered_session,
            now: Instant::now(),
            key_fd: None,
        };
        assert!(!force_capture(&policy(false, true, Some("$1")), "%1", "$1"));
        assert!(force_capture(&policy(false, false, Some("$1")), "%2", "$1"));
        assert!(force_capture(&policy(false, true, Some("$1")), "%3", "$2"));
        assert!(!force_capture(
            &policy(false, false, Some("$1")),
            "%3",
            "$2"
        ));
        assert!(force_capture(&policy(true, false, Some("$1")), "%1", "$1"));
        assert!(force_capture(&policy(false, false, None), "%1", "$1"));
    }

    #[test]
    fn a_waiting_key_defers_only_panes_that_have_a_screen() {
        assert!(!defer_capture(false, true));
        assert!(!defer_capture(true, false)); // never drop a row for a tick
        assert!(defer_capture(true, true));
        let mut cache = ScreenCache::default();
        assert_eq!(cache.stale("%1"), None);
        cache
            .get_or_capture(screen_key(), false, Instant::now(), || Ok("old".into()))
            .unwrap_or_else(|error| panic!("{error}"));
        // expired and re-keyed: still the screen a yielded scan shows
        assert_eq!(cache.stale("%1").as_deref(), Some("old"));
    }

    #[test]
    fn failed_capture_is_returned_and_never_cached() {
        let mut cache = ScreenCache::default();
        let result = cache.get_or_capture(screen_key(), false, Instant::now(), || {
            Err(mux::Error::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "synthetic missing capture",
            )))
        });
        assert!(matches!(result, Err(mux::Error::Io(_))));
        assert!(cache.panes.is_empty());
    }

    #[test]
    fn a_scan_detects_agents_from_any_backend_and_skips_its_own_pane() {
        use crate::mux::fake::{pane, FakeMux};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let confs = crate::conf::load_all(root);
        let fixture = |ext| {
            std::fs::read_to_string(
                root.join("tests/fixtures/claude-working")
                    .with_extension(ext),
            )
            .unwrap()
        };
        let agent = PaneMeta {
            pane_title: fixture("title").trim_end().to_string(),
            ..pane("$1", "@1", "%1", "claude")
        };
        let mut mux = FakeMux {
            panes: vec![
                (agent, 101),
                (pane("$1", "@1", "%2", "agenmux"), 102),
                (pane("$1", "@2", "%3", "zsh"), 0),
            ],
            screens: [("%1".to_string(), fixture("txt"))].into(),
            ..FakeMux::default()
        };
        let mut idents = IdentCache::new();
        idents.insert(("%1".into(), 101, "claude".into()), "claude".into());
        let snapshot = scan(
            &mut mux,
            &confs,
            &mut idents,
            &mut SubjectCache::new(),
            Some("%2"),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let panes: Vec<&str> = snapshot.panes.iter().map(|p| p.pane.as_str()).collect();
        assert_eq!(panes, ["%1", "%3"]);
        let [row] = &snapshot.agents[..] else {
            panic!("{:?}", snapshot.agents.len())
        };
        assert_eq!((row.pane.as_str(), row.state.as_str()), ("%1", "working"));
    }

    #[test]
    fn a_reported_state_skips_the_screen_unless_working() {
        use crate::mux::fake::{pane, FakeMux};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let confs = crate::conf::load_all(root);
        let screen = |name: &str| {
            std::fs::read_to_string(root.join(format!("tests/fixtures/{name}.txt"))).unwrap()
        };
        // no fixture: a capture would fail the scan
        let state = |fixture: Option<&str>, reported: Option<&'static str>| {
            let mut mux = FakeMux {
                panes: vec![(pane("$1", "@1", "%1", "pi"), 101)],
                screens: fixture
                    .map(|f| ("%1".to_string(), screen(f)))
                    .into_iter()
                    .collect(),
                reported: reported
                    .map(|r| ("%1".to_string(), r))
                    .into_iter()
                    .collect(),
                ..FakeMux::default()
            };
            let mut idents = IdentCache::new();
            idents.insert(("%1".into(), 101, "pi".into()), "pi".into());
            let snapshot = scan(
                &mut mux,
                &confs,
                &mut idents,
                &mut SubjectCache::new(),
                None,
            )
            .unwrap_or_else(|error| panic!("{error}"));
            snapshot.agents[0].state.clone()
        };
        assert_eq!(state(Some("pi-working"), None), "working");
        assert_eq!(state(Some("pi-blocked"), None), "blocked");
        assert_eq!(state(None, Some("idle")), "idle");
        assert_eq!(state(None, Some("blocked")), "blocked");
        // a reported working still reads the screen
        assert_eq!(state(Some("pi-idle"), Some("working")), "working");
        assert_eq!(state(Some("pi-blocked"), Some("working")), "blocked");
    }

    /// Idle panes %1..=%n of a `slow` agent whose SUBJECT_CMD is `cmd`.
    fn slow_agents(n: u32, cmd: &str) -> (crate::mux::fake::FakeMux, Vec<AgentConf>, IdentCache) {
        use crate::mux::fake::{pane, FakeMux};
        let dir = std::env::temp_dir().join(format!(
            "agenmux-slow-subject-{}-{}",
            std::process::id(),
            cmd.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("slow.conf");
        std::fs::write(&file, format!("AGENT_BINS='slow'\nSUBJECT_CMD='{cmd}'\n")).unwrap();
        let conf = crate::conf::load_conf(&file).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        let mut idents = IdentCache::new();
        let mut mux = FakeMux::default();
        for i in 1..=n {
            let id = format!("%{i}");
            mux.panes.push((pane("$1", "@1", &id, "slow"), 100 + i));
            mux.reported.insert(id.clone(), "idle");
            idents.insert((id, 100 + i, "slow".into()), "slow".into());
        }
        (mux, vec![conf], idents)
    }

    fn subjects(snapshot: &ScanSnapshot) -> Vec<&str> {
        snapshot.agents.iter().map(|a| a.title.as_str()).collect()
    }

    // #256: every idle pane's SUBJECT_CMD ran back to back inside one scan,
    // so `list` took ~36s. A scan now stops starting calls once its budget
    // is spent; the rest stay uncached and later scans fill them.
    #[test]
    fn subject_commands_share_a_scan_budget() {
        let (mut mux, confs, mut idents) = slow_agents(4, "sleep 0.7; printf %s \"s$pane\"");
        let mut subj = SubjectCache::new();
        let t0 = Instant::now();
        let first = scan(&mut mux, &confs, &mut idents, &mut subj, None)
            .unwrap_or_else(|error| panic!("{error}"));
        let took = t0.elapsed();
        let filled = subjects(&first).iter().filter(|s| !s.is_empty()).count();
        assert!((1..4).contains(&filled), "{:?}", subjects(&first));
        assert!(
            took < SUBJECT_SCAN_BUDGET + Duration::from_millis(500),
            "{took:?}"
        );
        let mut last = first;
        for _ in 0..3 {
            last = scan(&mut mux, &confs, &mut idents, &mut subj, None)
                .unwrap_or_else(|error| panic!("{error}"));
        }
        assert_eq!(subjects(&last), ["s%1", "s%2", "s%3", "s%4"]);
    }

    #[test]
    fn a_timed_out_subject_is_empty_and_not_rerun_while_idle() {
        let (mut mux, confs, mut idents) = slow_agents(1, "sleep 5; printf late");
        let mut subj = SubjectCache::new();
        let t0 = Instant::now();
        let first = scan(&mut mux, &confs, &mut idents, &mut subj, None)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(t0.elapsed() < SUBJECT_CMD_TIMEOUT * 4, "{:?}", t0.elapsed());
        assert_eq!(subjects(&first), [""]);
        let t1 = Instant::now();
        let again = scan(&mut mux, &confs, &mut idents, &mut subj, None)
            .unwrap_or_else(|error| panic!("{error}"));
        assert!(t1.elapsed() < SUBJECT_CMD_TIMEOUT, "{:?}", t1.elapsed());
        assert_eq!(subjects(&again), [""]);
    }
}
