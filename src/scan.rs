// Pane enumeration + state detection over the control-mode pipe.
// Output contract: byte-identical to `scan.sh list` / `scan.sh status`.
use crate::conf::AgentConf;
use crate::procs::{self, IdentCache, Snapshot};
use crate::tmux::{Tmux, TmuxError};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// pane -> (cwd, SUBJECT_CMD output). The sidebar loop must stay fork-free:
/// entries live while the pane sits idle and drop on any state change (a new
/// prompt flips the pane to working), so the fork re-runs only when the
/// subject could actually have changed.
/// ponytail: assumes new sessions always bounce through a non-idle state
pub type SubjectCache = HashMap<String, (String, String)>;

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
        capture: impl FnOnce() -> Result<String, TmuxError>,
    ) -> Result<(String, bool), TmuxError> {
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
}

pub struct ScanPolicy<'a> {
    pub dirty: &'a std::collections::HashSet<String>,
    pub full: bool,
    pub periodic: bool,
    pub covered_session: Option<&'a str>,
    pub now: Instant,
}

#[derive(Default)]
pub struct ScanStats {
    pub captured: usize,
    pub reused: usize,
}

fn force_capture(policy: &ScanPolicy<'_>, pane: &str, session: &str) -> bool {
    let covered = policy.covered_session == Some(session);
    policy.full
        || policy.dirty.contains(pane)
        || policy.covered_session.is_none()
        || (policy.periodic && !covered)
}

pub struct PaneRow {
    pub pane: String,
    pub loc: String,
    pub agent: String,
    pub state: String,
    pub cwd: String,
    pub title: String,
}

const LIST_FMT: &str = "list-panes -a -F '#{pane_id}\t#{pane_pid}\t#{pane_current_command}\t#{pane_current_path}\t#{session_name}:#{window_index}.#{pane_index}\t#{session_id}\t#{pane_width}\t#{pane_height}\t#{pane_title}'";

type PaneFields<'a> = (
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
);

fn pane_fields(line: &str) -> Option<PaneFields<'_>> {
    let mut fields = line.splitn(9, '\t');
    Some((
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    ))
}

pub fn scan(
    tmux: &mut Tmux,
    confs: &[AgentConf],
    cache: &mut IdentCache,
    subj: &mut SubjectCache,
    self_pane: Option<&str>,
) -> Result<Vec<PaneRow>, TmuxError> {
    let mut screens = ScreenCache::default();
    let dirty = std::collections::HashSet::new();
    scan_cached(
        tmux,
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
        },
    )
    .map(|(rows, _)| rows)
}

pub fn scan_cached(
    tmux: &mut Tmux,
    confs: &[AgentConf],
    cache: &mut IdentCache,
    subj: &mut SubjectCache,
    self_pane: Option<&str>,
    screens: &mut ScreenCache,
    policy: ScanPolicy<'_>,
) -> Result<(Vec<PaneRow>, ScanStats), TmuxError> {
    tmux.sync()?;
    let panes = tmux.run(LIST_FMT)?;
    let mut snap: Option<Snapshot> = None;
    let mut rows = Vec::new();
    let mut seen = IdentCache::new();
    let buf = format!("agenmux-{}", std::process::id());
    let cap = std::env::temp_dir().join(&buf);
    let mut used_buffer = false;
    let mut stats = ScanStats::default();
    let result: Result<Vec<PaneRow>, TmuxError> = (|| {
        for line in panes.lines() {
            let Some((pane, pid, cmd, path, loc, session, width, height, title)) =
                pane_fields(line)
            else {
                continue;
            };
            if self_pane == Some(pane) {
                continue; // sidebar skips itself
            }
            let pid: u32 = pid.parse().unwrap_or(0);
            let key = (pane.to_string(), pid, cmd.to_string());
            let name = cache.get(&key).cloned().or_else(|| {
                procs::identify(confs, &mut snap, pid, cmd).map(|i| confs[i].name.clone())
            });
            let Some(name) = name else { continue };
            seen.insert(key, name.clone());
            let Some(idx) = confs.iter().position(|c| c.name == name) else {
                continue; // conf removed since cached
            };
            let key = ScreenKey {
                pane: pane.to_string(),
                pid,
                command: cmd.to_string(),
                agent: name.clone(),
                width: width.parse().unwrap_or(0),
                height: height.parse().unwrap_or(0),
                title: title.to_string(),
                path: path.to_string(),
            };
            let force = force_capture(&policy, pane, session);
            // pane content must never travel over the control pipe: a pane
            // displaying literal "%end <t> <num>" text (logs, this plugin's own
            // docs...) would terminate the response block early and desync every
            // later command. Route it through a buffer + file instead.
            let (screen, reused) = screens.get_or_capture(key, force, policy.now, || {
                tmux.run(&format!("capture-pane -b '{buf}' -t '{pane}'"))?;
                used_buffer = true;
                tmux.run(&format!("save-buffer -b '{buf}' '{}'", cap.display()))?;
                std::fs::read_to_string(&cap).map_err(TmuxError::Io)
            })?;
            if reused {
                stats.reused += 1;
            } else {
                stats.captured += 1;
            }
            let state = crate::detect::detect_state(&confs[idx], title, &screen);
            let mut subject = crate::detect::subject(&confs[idx], title, &screen, path);
            if state != "idle" {
                subj.remove(pane); // pane got a new prompt — cached subject is stale
            } else if subject.is_empty() && confs[idx].subject_cmd.is_some() {
                match subj.get(pane).filter(|(cwd, _)| cwd == path) {
                    Some((_, s)) => subject = s.clone(),
                    None => {
                        let t0 = std::time::Instant::now();
                        let started = procs::agent_start(&confs[idx], &mut snap, pid);
                        subject = crate::detect::subject_cmd(&confs[idx], pane, path, started)
                            .unwrap_or_default();
                        crate::tmux::debug_note(&format!(
                            "subject_cmd {pane} {}ms",
                            t0.elapsed().as_millis()
                        ));
                        subj.insert(pane.to_string(), (path.to_string(), subject.clone()));
                    }
                }
            }
            rows.push(PaneRow {
                pane: pane.to_string(),
                loc: loc.to_string(),
                agent: name,
                state: state.to_string(),
                cwd: path.rsplit('/').next().unwrap_or(path).to_string(),
                title: subject,
            });
        }
        Ok(rows)
    })();
    if used_buffer {
        let _ = tmux.run(&format!("delete-buffer -b '{buf}'"));
        let _ = std::fs::remove_file(&cap);
    }
    let rows = result?;
    subj.retain(|pane, _| seen.keys().any(|k| &k.0 == pane)); // dead panes pruned
    screens
        .panes
        .retain(|pane, _| seen.keys().any(|key| &key.0 == pane));
    *cache = seen;
    Ok((rows, stats))
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
/// zero counts omitted, no trailing space. Empty when no agents.
pub fn status_segment(rows: &[PaneRow]) -> String {
    let n = |s: &str| rows.iter().filter(|r| r.state == s).count();
    let (b, w, i) = (n("blocked"), n("working"), n("idle"));
    let mut out = String::new();
    if b > 0 {
        out.push_str(&format!("#[fg=red]⣿#[default]{b} "));
    }
    if w > 0 {
        out.push_str(&format!("#[fg=yellow]⣾#[default]{w} "));
    }
    if i > 0 {
        out.push_str(&format!("#[fg=green]⣿#[default]{i} "));
    }
    out.trim_end().to_string()
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
        assert_eq!(status_segment(&[]), "");
        assert_eq!(
            status_segment(&[row("working"), row("idle"), row("idle")]),
            "#[fg=yellow]⣾#[default]1 #[fg=green]⣿#[default]2"
        );
        assert_eq!(status_segment(&[row("blocked")]), "#[fg=red]⣿#[default]1");
    }

    #[test]
    fn tsv_roundtrip() {
        let rows = vec![row("idle")];
        let parsed = from_tsv(&to_tsv(&rows));
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].state, "idle");
    }

    #[test]
    fn pane_title_keeps_embedded_tabs_as_the_final_field() {
        let fields =
            pane_fields("%1\t42\tagent\t/work\ts:0.0\t$1\t80\t24\ttitle\twith\ttabs").unwrap();
        assert_eq!(fields.8, "title\twith\ttabs");
    }

    #[test]
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
    fn failed_capture_is_returned_and_never_cached() {
        let mut cache = ScreenCache::default();
        let result = cache.get_or_capture(screen_key(), false, Instant::now(), || {
            Err(TmuxError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "synthetic missing capture",
            )))
        });
        assert!(matches!(result, Err(TmuxError::Io(_))));
        assert!(cache.panes.is_empty());
    }
}
