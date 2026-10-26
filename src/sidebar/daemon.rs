use crate::mux::{Owner, SidebarPane as M};
use crate::pane_writers::PaneWriters;
use crate::tmux::PendingChanges;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{event_loop, new_sidebar, on_term, Sidebar};

/// Width is an intent, not just a materialized size. A peer whose cached PTY
/// already matches may still have an old resize in flight that must be cancelled.
fn peer_widths(ms: &[M], source: &str, width: usize) -> Vec<(String, usize)> {
    ms.iter()
        .filter(|m| m.pane != source)
        .map(|m| (m.pane.clone(), width))
        .collect()
}

/// The watched sidebar sets the shared row layout. Hidden windows may retain
/// arbitrary old widths; each pane projects this frame into its own viewport.
fn watched_width(ms: &[M], active_session: &str) -> usize {
    ms.iter()
        .find(|m| m.active && m.sess == active_session)
        .map(|m| m.w)
        .or_else(|| ms.iter().filter(|m| m.active).map(|m| m.w).min())
        .or_else(|| ms.iter().map(|m| m.w).min())
        .unwrap_or(30)
}

/// Height to render the shared frame at. NOT the minimum: every visible pane
/// shows the same frame, so folding to the shortest let one stale 23-row window
/// in a session nobody is looking at clip the list everywhere. Size to the
/// mirror the user is actually watching; pane readers project shorter PTY
/// viewports locally without scrolling.
fn watched_height(ms: &[M], active_session: &str) -> usize {
    ms.iter()
        .find(|m| m.active && m.sess == active_session)
        .map(|m| m.h)
        // several clients on several sessions, or no client measured yet
        .or_else(|| ms.iter().filter(|m| m.active).map(|m| m.h).max())
        .or_else(|| ms.iter().map(|m| m.h).max())
        .unwrap_or(24)
}

/// A window's sidebar as the last settled measure saw it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Baseline {
    size: (usize, usize),
    panes: usize,
    width: usize,
}

/// The width a sidebar should have in a window `window_width` wide.
fn bound(wopt: usize, window_width: usize) -> usize {
    wopt.min(window_width.saturating_sub(2).max(1))
}

/// The user dragged this sidebar's border: its window is the active one, its
/// size and pane count match the previous tick (nothing was split, killed or
/// resized around it), the server applied its size, and the width moved away
/// from the configured bound it had. A sidebar already off that width was
/// placed there (a server that places by ratio misses by a few cells), not
/// dragged. A sidebar as wide as its window has no horizontal neighbour: it is
/// alone, or a layout change stacked it above or below the other panes. No
/// border drag ends there, so that width is a layout accident, never a
/// preference.
fn dragged(m: &M, wopt: usize, baseline: Option<&Baseline>) -> bool {
    let bound = bound(wopt, m.win_size.0);
    m.active
        && m.settled
        && m.width_drag != Some(false)
        && m.w < m.win_size.0
        && m.w != bound
        && baseline.is_some_and(|b| {
            b.panes == m.panes
                && (m.width_drag == Some(true) || (b.width == bound && b.size == m.win_size))
        })
}

/// Should the daemon shut down after measuring no preserved panes? Only once
/// panes existed (or the startup grace ran out) AND the emptiness repeats:
/// a hook's run-shell block can desync the control pipe for exactly one
/// command, and a single garbage read must not tear down the whole mirror set.
fn suicide(seen_mirror: bool, since_start: Duration, empty_ticks: u32) -> bool {
    (seen_mirror || since_start >= Duration::from_secs(30)) && empty_ticks >= 2
}

/// Has `@agenmux-control-client` named someone else? Only a claim counts:
/// teardown.sh's unset can land after the next daemon claimed it, and treating
/// empty as "replaced" makes a reopened sidebar kill its own daemon.
fn superseded(mine: &str, current: &str) -> bool {
    let current = current.trim();
    !mine.is_empty() && !current.is_empty() && current != mine
}

fn claimed_elsewhere(generation: &str, client: &str, owner: Option<Owner>) -> bool {
    owner.is_some_and(|owner| {
        superseded(generation, &owner.generation) || superseded(client, &owner.client)
    })
}

/// Headless-mode state: frames → visible panes, keys ← FIFO, size ← panes.
pub(super) struct Daemon {
    pub(super) keys_path: PathBuf,
    pub(super) keys_fd: libc::c_int,
    /// Raw terminal input from sidebar panes the server sends no keys for.
    pub(super) input_path: PathBuf,
    pub(super) input_fd: libc::c_int,
    pub(super) writers: PaneWriters,
    pub(super) size: (usize, usize), // watched pane's measured viewport
    pub(super) seen_mirror: bool,    // suicide only arms after the first pane appears
    pub(super) empty_ticks: u32,     // consecutive measurements that found no pane
    pub(super) client: String,       // our control client, as published in the option
    pub(super) generation: String,   // activation that owns panes, FIFO and options
    pub(super) started: Instant,
    // window id -> (window size, pane count) at the last measure: a mirror
    // whose width changed while both stayed put is a user border-drag. The
    // pane count matters: a closing pane hands its columns to the mirror
    // (all of them, when the mirror is the last pane left) without changing
    // the window size — width-only would adopt that as the global width
    pub(super) win_sizes: HashMap<String, Baseline>,
    // session the control client is attached to: layout/focus notifications
    // are session-scoped, so the client follows the user's active session
    pub(super) attached: String,
}

/// A new FIFO at `path`, open for reading. O_RDWR: it never hits EOF as
/// senders come and go.
fn fresh_fifo(path: &std::path::Path) -> libc::c_int {
    let _ = std::fs::remove_file(path);
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    unsafe {
        libc::mkfifo(c.as_ptr(), 0o600);
        libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK)
    }
}

/// Headless engine for preserved-pane mode: renders through live pane writers,
/// reads keys from a FIFO, and sizes itself from the preserved panes. Exits
/// (with full teardown) when the last pane disappears.
pub fn run_daemon(plugin_dir: PathBuf, cache_file: PathBuf) -> i32 {
    // The launching toggle hands down its installation lease; a daemon started
    // directly takes its own. Either way it lives as long as this process.
    let lease = match crate::autoupdate::Lock::inherited("AGENMUX_LEASE_FD") {
        Some(lease) => Some(lease),
        None => match crate::autoupdate::lease(&plugin_dir) {
            Ok(lease) => lease,
            Err(error) => {
                eprintln!("agenmux: {error}");
                return 1;
            }
        },
    };
    let settings = match crate::app_config::current_process() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    let Ok(mut mux) = crate::mux::connect(true) else {
        return 1;
    };
    let generation = std::env::var("AGENMUX_GENERATION").unwrap_or_default();
    // before touching the FIFO: it belongs to the activation that replaced us
    if !generation.is_empty()
        && !(mux.sync().is_ok() && mux.owner().is_some_and(|o| o.generation == generation))
    {
        return 1;
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    let tmp = match crate::tmux::prepare_runtime_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("agenmux: cannot prepare runtime directory: {error}");
            return 1;
        }
    };
    let keys_path = tmp.join("agenmux-keys");
    let input_path = crate::pane_writers::input_path(&tmp);
    // A previous mirror-based daemon used this as its liveness heartbeat.
    let _ = std::fs::remove_file(tmp.join("agenmux-frame"));
    let (keys_fd, input_fd) = (fresh_fifo(&keys_path), fresh_fifo(&input_path));
    if keys_fd < 0 || input_fd < 0 {
        return 1;
    }
    mux.set_key_sink(keys_fd);
    // "" not TMUX_PANE: toggle.sh launches the daemon from the pane the user
    // pressed the key in, and adopting that pane would hide its agent
    let mut sb = new_sidebar(
        mux,
        plugin_dir,
        cache_file,
        tmp.join("agenmux-rows"),
        String::new(),
        settings,
    );
    sb.snapshot = crate::snapshot::Store::open(sb.mux.as_mut());
    // The connection's name can briefly be empty while a busy server
    // finishes attaching; retry.
    let mut control_client = String::new();
    for _ in 0..100 {
        if let Some(client) = sb.mux.connection() {
            if sb.mux.publish_owner(&client, &tmp).is_err() {
                return 1;
            }
            control_client = client;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if control_client.is_empty() {
        return 1;
    }
    sb.daemon = Some(Daemon {
        keys_path,
        keys_fd,
        input_path,
        input_fd,
        writers: PaneWriters::new(),
        size: (30, 24),
        seen_mirror: false,
        empty_ticks: 0,
        client: control_client,
        generation,
        started: Instant::now(),
        win_sizes: HashMap::new(),
        attached: String::new(),
    });
    // Publish nothing until the first preserved pane has been measured: its
    // size IS the frame size, and a default-sized first frame visibly resizes
    // once the real measurement lands. Deriving the size instead of measuring it
    // does not work — pane-border-status silently costs a row. toggle.sh creates
    // the panes right after us, so this wait is short.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(3) {
        if !sb.mirror_tick() || sb.daemon.as_ref().unwrap().seen_mirror {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let ready = sb.daemon.as_ref().is_some_and(|d| d.seen_mirror)
        && sb
            .scan_tick(
                false,
                &PendingChanges {
                    full: true,
                    ..PendingChanges::default()
                },
            )
            .is_ok();
    if !ready {
        sb.quiet_exit();
        return 1;
    }
    crate::autoupdate::spawn_scheduler(sb.plugin_dir.clone());
    sb.render(true);
    if std::env::var_os("AGENMUX_STARTUP_ACK").is_some() {
        use std::io::Write;
        if sb
            .daemon
            .as_ref()
            .is_none_or(|d| d.client.is_empty() || !d.seen_mirror)
            || std::io::stdout().write_all(b"R").is_err()
            || std::io::stdout().flush().is_err()
        {
            sb.quiet_exit();
            return 1;
        }
    }
    // An activating launch hands down its exclusive hold. Once this daemon is
    // up it only needs a shared one, so a launcher that dies before
    // confirming cannot leave the installation locked for the daemon's life.
    if let Some(lease) = &lease {
        lease.share();
    }
    if event_loop(&mut sb) {
        sb.quiet_exit();
    } else {
        sb.teardown();
    }
    0
}

impl Sidebar {
    /// Refresh preserved-pane inventory: min pane size drives the render, zero
    /// panes (after at least one existed, or a 30s startup grace) = false.
    /// Also detects a user dragging a sidebar border — width changed while
    /// the window size and pane count did not, in a window the user can
    /// actually see — and
    /// adopts it as the global width. Serialization matters: one daemon
    /// doing this (instead of racing hook scripts) means no stale
    /// resize-pane ever fights the drag, and the dragged pane itself is
    /// never touched — only the hidden sidebars in other windows move.
    pub(super) fn superseded(&mut self) -> bool {
        let Some((client, generation)) = self
            .daemon
            .as_ref()
            .map(|d| (d.client.clone(), d.generation.clone()))
        else {
            return false;
        };
        // Ownership decides whether we abandon every pane without teardown.
        // A stale hook response must not look like a newer daemon's claim.
        if self.mux.sync().is_err() {
            return false;
        }
        // a broken pipe is not a takeover; the loop exits elsewhere
        let owner = self.mux.owner();
        claimed_elsewhere(&generation, &client, owner)
    }

    /// Refresh only render dimensions after a layout notification. Drag
    /// adoption and pane reconciliation stay on the periodic mirror tick.
    pub(super) fn refresh_geometry(&mut self) -> bool {
        let _ = self.mux.sync();
        let ms = self.mux.sidebar_panes();
        if ms.is_empty() {
            return false;
        };
        let size = (
            watched_width(&ms, &self.active_session),
            watched_height(&ms, &self.active_session),
        );
        let Some(daemon) = self.daemon.as_mut() else {
            return false;
        };
        if daemon.size == size {
            return false;
        }
        daemon.size = size;
        true
    }

    pub(super) fn mirror_tick(&mut self) -> bool {
        // same barrier as active_pane: reading zero panes off a desynced
        // pipe used to tear the whole mirror set down
        let _ = self.mux.sync();
        let position = self.settings.settings.sidebar_position;
        let refreshed = self.settings.refresh(self.mux.as_mut());
        self.adopt_reload(refreshed);
        // Before measuring, so the moved panes are what the drag probe sees.
        if self.settings.settings.sidebar_position != position {
            self.mux
                .dock_sidebars(&self.plugin_dir, &self.settings.settings);
        }
        self.sync_panes_view();
        let width_changed = refreshed.width_changed;
        let mut ms = self.mux.sidebar_panes();
        if ms.is_empty() {
            let d = self.daemon.as_mut().unwrap();
            d.empty_ticks += 1;
            return !suicide(d.seen_mirror, d.started.elapsed(), d.empty_ticks);
        }
        self.daemon.as_mut().unwrap().empty_ticks = 0;
        let wopt = self.settings.settings.sidebar_width as usize;
        if width_changed {
            for m in &mut ms {
                let width = wopt.min(m.win_size.0.saturating_sub(2).max(1));
                if self.mux.resize_panes(&[(m.pane.clone(), width)]) {
                    m.settled = false;
                }
            }
        }
        // One sidebar per window. mirror-add.sh claims atomically now, but servers
        // that ran the old racy version still carry duplicates. Keep the first —
        // a -hbf split takes index 0, so that's the newest and the one actually at
        // the requested width (a right-hand -hf split is last, but the resize
        // below fixes whichever survives); the squeezed leftovers go. This runs before the
        // width fold and the drag probe below, and it puts the survivor back at
        // wopt: every extra split squeezed it, and that squeezed width would
        // otherwise look exactly like a border drag and get adopted globally.
        let mut seen: HashSet<String> = HashSet::new();
        let dup: Vec<usize> = (0..ms.len())
            .filter(|&i| !seen.insert(ms[i].win.clone()))
            .collect();
        if !dup.is_empty() {
            let hit: HashSet<String> = dup.iter().map(|&i| ms[i].win.clone()).collect();
            let killed: Vec<String> = dup.iter().map(|&i| ms[i].pane.clone()).collect();
            self.mux.close_panes(&killed);
            for &i in dup.iter().rev() {
                ms.remove(i);
            }
            // unconditional: the survivor absorbs the columns the killed panes
            // give back, so even one that measured wopt a moment ago ends up wide
            let mut widths = Vec::new();
            for m in ms.iter_mut().filter(|m| hit.contains(&m.win)) {
                widths.push((m.pane.clone(), wopt));
                m.settled = false;
            }
            self.mux.resize_panes(&widths);
        }
        let h = watched_height(&ms, &self.active_session);
        let drag: Option<(String, usize)> = {
            let d = self.daemon.as_ref().unwrap();
            ms.iter()
                .find(|m| !width_changed && dragged(m, wopt, d.win_sizes.get(&m.win)))
                .map(|m| (m.pane.clone(), m.w))
        };
        if let Some((src_pane, width)) = drag {
            trace!("sidebar {src_pane} dragged to {width}");
            if (1..=10000).contains(&width) && self.mux.save_width(width) {
                self.settings.settings.sidebar_width = width as u16;
                self.settings.settings.popup_width = width as u16;
            }
            // only the OTHER sidebars: the dragged pane stays untouched so
            // nothing ever fights the user's drag
            let others = peer_widths(&ms, &src_pane, width);
            self.mux.resize_panes(&others);
            for m in ms.iter_mut().filter(|m| m.pane != src_pane) {
                m.settled = false;
            }
        }
        // A killed neighbour hands its columns to the sidebar, a server that
        // splits by ratio grows it with its window, and one that places by
        // ratio misses by a few cells. The pane count or the window size
        // changed, or the sidebar never had the width: the drag probe above
        // stays quiet, so put the width back.
        let wopt = self.settings.settings.sidebar_width as usize;
        let mut widths = Vec::new();
        for m in &mut ms {
            let want = bound(wopt, m.win_size.0);
            let counted = self.daemon.as_ref().unwrap().win_sizes.get(&m.win);
            let reshaped = counted.is_none_or(|b| {
                b.panes != m.panes
                    || b.size != m.win_size
                    || b.width != want
                    || m.width_drag == Some(false)
            });
            if m.w == want || !m.settled || !reshaped {
                continue;
            }
            widths.push((m.pane.clone(), want));
            m.settled = false;
        }
        if !widths.is_empty() {
            trace!("sidebar widths put back: {widths:?}");
            self.mux.resize_panes(&widths);
        }
        let visible_windows = self.mux.visible_windows();
        let visible_panes = if visible_windows.is_empty() {
            // Detached startup and integration tests have no real client yet.
            // Keep one active pane warm until a real client appears.
            ms.iter()
                .filter(|m| m.active)
                .take(1)
                .map(|m| m.pane.clone())
                .collect::<Vec<_>>()
        } else {
            ms.iter()
                .filter(|m| visible_windows.contains(&m.win))
                .map(|m| m.pane.clone())
                .collect::<Vec<_>>()
        };
        let d = self.daemon.as_mut().unwrap();
        if d.writers.reconcile(visible_panes) {
            // A new pane reader has no copy of the last frame yet.
            self.last_frame.clear();
        }
        // a window mid-resize keeps its last settled baseline
        let before = std::mem::take(&mut d.win_sizes);
        d.win_sizes = ms
            .iter()
            .filter_map(|m| {
                let baseline = if m.settled {
                    Baseline {
                        size: m.win_size,
                        panes: m.panes,
                        width: m.w,
                    }
                } else {
                    *before.get(&m.win)?
                };
                Some((m.win.clone(), baseline))
            })
            .collect();
        d.seen_mirror = true;
        d.size = (watched_width(&ms, &self.active_session), h);
        true
    }

    /// Focus moved: write only to sidebar panes in windows real clients view.
    pub(super) fn refocus_writers(&mut self) {
        let windows = self.mux.visible_windows();
        if windows.is_empty() {
            return; // no real client: leave mirror_tick's warm-one fallback alone
        }
        let panes: Vec<String> = self
            .mux
            .sidebar_panes()
            .into_iter()
            .filter(|m| windows.contains(&m.win))
            .map(|m| m.pane)
            .collect();
        if self.daemon.as_mut().unwrap().writers.reconcile(panes) {
            self.last_frame.clear();
        }
    }

    /// Preserved-pane shutdown: close visible writers, kill sidebar panes and
    /// restore layouts through the native pane lifecycle, then drop the key
    /// FIFO and row map.
    /// Drop only what is ours. The panes, the FIFO path and the rows file
    /// belong to the daemon that replaced us — teardown() would delete them.
    fn quiet_exit(&mut self) {
        if let Some(d) = &mut self.daemon {
            d.writers.clear();
        }
        if let Some(d) = &self.daemon {
            unsafe {
                libc::close(d.keys_fd);
                libc::close(d.input_fd);
            }
        }
    }

    pub(super) fn teardown(&mut self) {
        let Some((generation, client)) = self
            .daemon
            .as_ref()
            .map(|d| (d.generation.clone(), d.client.clone()))
        else {
            return;
        };
        let owns = self.mux.owner().is_some_and(|owner| {
            if generation.is_empty() {
                owner.client == client
            } else {
                owner.generation == generation
            }
        });
        if !owns {
            self.quiet_exit();
            return;
        }
        if let Some(d) = &mut self.daemon {
            d.writers.clear();
        }
        self.mux.close_sidebars();
        if let Some(d) = &self.daemon {
            let _ = std::fs::remove_file(&d.keys_path);
            let _ = std::fs::remove_file(&d.input_path);
            unsafe {
                libc::close(d.keys_fd);
                libc::close(d.input_fd);
            }
        }
        let _ = std::fs::remove_file(&self.rows_file);
        self.mux
            .clear_owner((!generation.is_empty()).then_some(generation.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(sess: &str, h: usize, active: bool) -> M {
        M {
            pane: "%1".into(),
            win: "@1".into(),
            sess: sess.into(),
            w: 30,
            h,
            win_size: (200, h),
            panes: 2,
            active,
            settled: true,
            width_drag: None,
        }
    }

    #[test]
    fn new_drag_intent_reaches_peers_even_when_their_cached_width_matches() {
        let mut source = m("$0", 46, true);
        source.pane = "source".into();
        let mut peer = m("$1", 46, true);
        peer.pane = "peer".into();
        peer.w = 35;
        assert_eq!(
            peer_widths(&[source, peer], "source", 35),
            [("peer".into(), 35)]
        );
    }

    #[test]
    fn watched_width_ignores_stale_hidden_terminals() {
        let mut ms = [m("$0", 46, true), m("$1", 18, true), m("$0", 20, false)];
        ms[0].w = 35;
        ms[1].w = 24;
        ms[2].w = 10;
        assert_eq!(watched_width(&ms, "$0"), 35);
        assert_eq!(watched_width(&ms, "$1"), 24);
        assert_eq!(watched_width(&ms, "$missing"), 24);
    }

    #[test]
    fn a_full_width_sidebar_is_not_a_border_drag() {
        let baseline = Baseline {
            size: (200, 50),
            panes: 2,
            width: 30,
        };
        let at = |w: usize, panes: usize| M {
            w,
            panes,
            ..m("$0", 50, true)
        };
        // dragged from 30 to 45 with the window otherwise untouched
        assert!(dragged(&at(45, 2), 30, Some(&baseline)));
        // the regression: even-vertical stacked the sidebar above the agent
        // pane, so it spans the full width with the pane count unchanged
        assert!(!dragged(&at(200, 2), 30, Some(&baseline)));
        // alone after its neighbour was killed: the count changed, and a tick
        // later the count matches again but the width is still the window's
        assert!(!dragged(&at(200, 1), 30, Some(&baseline)));
        assert!(!dragged(
            &at(200, 1),
            30,
            Some(&Baseline {
                panes: 1,
                ..baseline
            })
        ));
        // placed off the configured width, never at it: not the user's
        let placed = Baseline {
            width: 37,
            ..baseline
        };
        assert!(!dragged(&at(37, 2), 30, Some(&placed)));
        // A known divider drag remains usable when a client minimum prevented
        // the old preference from ever being reached.
        assert!(dragged(
            &M {
                width_drag: Some(true),
                ..at(45, 2)
            },
            30,
            Some(&placed)
        ));
        // a split or kill beside the sidebar is not a drag
        assert!(!dragged(&at(45, 3), 30, Some(&baseline)));
        // A client resize without divider movement must not save a new width.
        assert!(!dragged(
            &M {
                width_drag: Some(false),
                ..at(31, 2)
            },
            30,
            Some(&baseline)
        ));
        // Rex padding estimates shift as divider ratios change. They cannot
        // veto authoritative divider+PTY evidence; tmux's exact size still can.
        assert!(dragged(
            &M {
                width_drag: Some(true),
                win_size: (205, 50),
                ..at(45, 2)
            },
            30,
            Some(&baseline)
        ));
        assert!(!dragged(
            &M {
                win_size: (205, 50),
                ..at(45, 2)
            },
            30,
            Some(&baseline)
        ));
        // first measure of a window: nothing to compare against
        assert!(!dragged(&at(45, 2), 30, None));
        // at the configured width there is nothing to adopt
        assert!(!dragged(&at(30, 2), 30, Some(&baseline)));
        // a sidebar the server has not sized to its layout yet is mid-placement
        assert!(!dragged(
            &M {
                settled: false,
                ..at(45, 2)
            },
            30,
            Some(&baseline)
        ));
        // only the active window's sidebar can be the one under the pointer
        assert!(!dragged(
            &M {
                active: false,
                ..at(45, 2)
            },
            30,
            Some(&baseline)
        ));
    }

    #[test]
    fn one_garbage_measurement_does_not_kill_the_daemon() {
        let s = Duration::from_secs;
        // the regression: a hook block desyncs the pipe for one command, the
        // mirror list reads back empty, and every mirror pane got torn down
        assert!(!suicide(true, s(60), 1));
        assert!(suicide(true, s(60), 2)); // user really did close them all
                                          // startup grace: no mirror has ever appeared yet
        assert!(!suicide(false, s(5), 9));
        assert!(suicide(false, s(30), 2)); // none ever came — give up
    }

    #[test]
    fn a_replaced_daemon_stops_owning_the_panes() {
        assert!(!superseded("client-7", "client-7"));
        assert!(!superseded("client-7", "client-7\n")); // show-option keeps the newline
        assert!(superseded("client-7", "client-9"));
        assert!(!superseded("client-7", "")); // unset is not a claim
        assert!(!superseded("", "client-9")); // we never published a name
        assert!(!superseded("", ""));
    }

    #[test]
    fn only_a_read_owner_record_counts_as_a_claim() {
        let owner = |generation: &str, client: &str| {
            Some(Owner {
                generation: generation.into(),
                client: client.into(),
            })
        };
        let mine = |owner| claimed_elsewhere("g1", "client-7", owner);
        assert!(!mine(owner("g1", "client-7")));
        assert!(mine(owner("g2", "client-7")));
        assert!(mine(owner("g1", "client-9")));
        assert!(!mine(owner("", ""))); // unset is not a claim
        assert!(!mine(None)); // the record could not be read
        assert!(!claimed_elsewhere("", "client-7", owner("g2", "client-7")));
    }

    #[test]
    fn watched_height_ignores_short_unwatched_windows() {
        // the regression: a 23-row window in a session nobody is looking at
        // used to clip the list in the 82-row window the user is watching
        let ms = [m("$0", 82, true), m("$1", 23, true), m("$0", 47, false)];
        assert_eq!(watched_height(&ms, "$0"), 82);
    }

    #[test]
    fn watched_height_falls_back_to_tallest_visible_then_tallest() {
        // no client measured yet: tallest among the active windows
        let ms = [m("$0", 40, true), m("$1", 23, true), m("$0", 99, false)];
        assert_eq!(watched_height(&ms, ""), 40);
        // nothing active at all: tallest overall
        let ms = [m("$0", 23, false), m("$1", 60, false)];
        assert_eq!(watched_height(&ms, "$0"), 60);
        assert_eq!(watched_height(&[], "$0"), 24);
    }
}
