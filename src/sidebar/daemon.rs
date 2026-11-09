use crate::mux::{Changes, Owner, SidebarPane as M};
use crate::pane_writers::PaneWriters;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::{event_loop, new_sidebar, on_term, Sidebar};

/// Height to render the shared frame at. NOT the minimum: every visible pane
/// shows the same frame, so folding to the shortest let one stale 23-row window
/// in a session nobody is looking at clip the list everywhere. Size to the
/// mirror the user is actually watching — mirrors shorter than the frame clip
/// themselves in mirror::draw.
fn watched_height(ms: &[M], active_session: &str) -> usize {
    ms.iter()
        .find(|m| m.active && m.sess == active_session)
        .map(|m| m.h)
        // several clients on several sessions, or no client measured yet
        .or_else(|| ms.iter().filter(|m| m.active).map(|m| m.h).max())
        .or_else(|| ms.iter().map(|m| m.h).max())
        .unwrap_or(24)
}

/// Frame width: the narrowest sidebar, so no frame wraps. A self-sized
/// sidebar (Rex) is resized only while a client shows it, so hidden ones keep
/// stale widths; there the watched one decides.
fn watched_width(ms: &[M], active_session: &str) -> Option<usize> {
    ms.iter()
        .find(|m| m.self_sized && m.active && m.sess == active_session)
        .map(|m| m.w)
        .or_else(|| {
            // Rex resizes only shown sidebars: a hidden window's keeps the
            // width it last had there
            let shown = |m: &&M| m.active || !m.self_sized;
            ms.iter().filter(shown).map(|m| m.w).min()
        })
        .or_else(|| ms.iter().map(|m| m.w).min())
}

/// The user dragged this sidebar's border: its window is the active one, its
/// size and pane count match the previous tick (nothing was split, killed or
/// resized around it), and the width differs from the configured bound. A
/// sidebar as wide as its window has no horizontal neighbour: it is alone, or a
/// layout change stacked it above or below the other panes. No border drag
/// ends there, so that width is a layout accident, never a preference.
fn dragged(m: &M, wopt: usize, baseline: Option<&((usize, usize), usize)>) -> bool {
    let bound = wopt.min(m.win_size.0.saturating_sub(2).max(1));
    m.active && m.w < m.win_size.0 && m.w != bound && baseline == Some(&(m.win_size, m.panes))
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
    pub(super) writers: PaneWriters,
    pub(super) size: (usize, usize), // narrowest pane x watched pane's height
    pub(super) seen_mirror: bool,    // suicide only arms after the first pane appears
    pub(super) empty_ticks: u32,     // consecutive answered measurements that found no pane
    pub(super) client: String,       // our control client, as published in the option
    pub(super) generation: String,   // activation that owns panes, FIFO and options
    pub(super) started: Instant,
    // window id -> (window size, pane count) at the last measure: a mirror
    // whose width changed while both stayed put is a user border-drag. The
    // pane count matters: a closing pane hands its columns to the mirror
    // (all of them, when the mirror is the last pane left) without changing
    // the window size — width-only would adopt that as the global width
    pub(super) win_sizes: HashMap<String, ((usize, usize), usize)>,
    // session the control client is attached to: layout/focus notifications
    // are session-scoped, so the client follows the user's active session
    pub(super) attached: String,
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
    let generation = std::env::var("AGENMUX_GENERATION").unwrap_or_default();
    if !generation.is_empty()
        && !crate::mux::owner().is_some_and(|owner| owner.generation.trim() == generation)
    {
        return 1;
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    let tmp = match crate::mux::prepare_runtime_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("agenmux: cannot prepare runtime directory: {error}");
            return 1;
        }
    };
    let keys_path = tmp.join("agenmux-keys");
    // A previous mirror-based daemon used this as its liveness heartbeat.
    let _ = std::fs::remove_file(tmp.join("agenmux-frame"));
    let _ = std::fs::remove_file(&keys_path);
    let c = std::ffi::CString::new(keys_path.as_os_str().as_encoded_bytes()).unwrap();
    // O_RDWR: the FIFO never hits EOF as key senders come and go
    let keys_fd = unsafe {
        libc::mkfifo(c.as_ptr(), 0o600);
        libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK)
    };
    if keys_fd < 0 {
        return 1;
    }
    let mut mux = match crate::mux::connect(true) {
        Ok(mux) => mux,
        Err(_) => return 1,
    };
    if let Some(tmux) = mux.as_tmux() {
        tmux.set_key_sink(keys_fd);
    }
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
    // A busy server can name the connection only once it finishes attaching.
    let mut control_client = String::new();
    for _ in 0..100 {
        if let Some(client) = sb.mux.connection() {
            control_client = client;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if control_client.is_empty() || sb.mux.claim(&control_client, &tmp).is_err() {
        return 1;
    }
    sb.daemon = Some(Daemon {
        keys_path,
        keys_fd,
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
                &Changes {
                    full: true,
                    ..Changes::default()
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
    /// Reading zero panes off a desynced tmux control pipe used to tear the
    /// whole mirror set down: discard stale replies before measuring.
    fn barrier(&mut self) {
        if let Some(tmux) = self.mux.as_tmux() {
            let _ = tmux.sync();
        }
    }

    /// None when the query failed. A dead server is caught by `scan_tick`,
    /// which hits EOF on the next pass and ends the loop, so a failure here
    /// only skips the tick.
    fn measure_panes(&mut self) -> Option<Vec<M>> {
        self.mux.sidebar_panes().ok()
    }

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
        // Ownership decides whether we abandon every pane without teardown;
        // an unread record is not a takeover.
        claimed_elsewhere(&generation, &client, self.mux.owner())
    }

    /// Refresh only render dimensions after a layout notification. Drag
    /// adoption and pane reconciliation stay on the periodic mirror tick.
    pub(super) fn refresh_geometry(&mut self) -> bool {
        self.barrier();
        let Some(ms) = self.measure_panes() else {
            return false;
        };
        let Some(w) = watched_width(&ms, &self.active_session) else {
            return false;
        };
        let size = (w, watched_height(&ms, &self.active_session));
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
        self.barrier();
        let position = self.settings.settings.sidebar_position;
        let refreshed = self.settings.refresh(self.mux.as_mut());
        self.adopt_reload(refreshed);
        // Before measuring, so the moved panes are what the drag probe sees.
        if self.settings.settings.sidebar_position != position {
            if let Some(tmux) = self.mux.as_tmux() {
                tmux.dock(&self.plugin_dir, &self.settings.settings);
            }
        }
        self.sync_panes_view();
        let width_changed = refreshed.width_changed;
        let Some(mut ms) = self.measure_panes() else {
            return true;
        };
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
                    m.w = width;
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
            .filter(|&i| !seen.insert(ms[i].win.clone()) && !ms[i].self_sized)
            .collect();
        if !dup.is_empty() {
            let hit: HashSet<String> = dup.iter().map(|&i| ms[i].win.clone()).collect();
            let killed: Vec<String> = dup.iter().map(|&i| ms[i].pane.clone()).collect();
            for &i in dup.iter().rev() {
                ms.remove(i);
            }
            // unconditional: the survivor absorbs the columns the killed panes
            // give back, so even one that measured wopt a moment ago ends up wide
            let mut widths = Vec::new();
            for m in ms.iter_mut().filter(|m| hit.contains(&m.win)) {
                widths.push((m.pane.clone(), wopt));
                m.w = wopt;
            }
            self.mux.close_panes(&killed);
            self.mux.resize_panes(&widths);
        }
        // Width DOES fold to the minimum: mirror::draw clips rows but NOT
        // columns, so a frame wider than some pane would wrap and shift every
        // row below it (breaking the click -> rows-file mapping). tmux widths
        // are uniform by construction; Rex's follow watched_width.
        let mut w = watched_width(&ms, &self.active_session).unwrap_or(usize::MAX);
        let h = watched_height(&ms, &self.active_session);
        let drag: Option<(String, usize)> = {
            let d = self.daemon.as_ref().unwrap();
            ms.iter()
                .find(|m| {
                    !width_changed && !m.self_sized && dragged(m, wopt, d.win_sizes.get(&m.win))
                })
                .map(|m| (m.pane.clone(), m.w))
        };
        if let Some((src_pane, width)) = drag {
            if (1..=10000).contains(&width) && self.mux.remember_width(width) {
                self.settings.settings.sidebar_width = width as u16;
                self.settings.settings.popup_width = width as u16;
            }
            // the dragged pane stays untouched so nothing ever fights the drag
            let others: Vec<_> = ms
                .iter()
                .filter(|m| m.pane != src_pane && m.w != width)
                .map(|m| (m.pane.clone(), width))
                .collect();
            self.mux.resize_panes(&others);
            w = width; // render for the adopted width now, not the stale min
        }
        // A killed neighbour hands its columns to the sidebar. The pane count
        // changed, so the drag probe above stays quiet; put the width back.
        let mut put_back = Vec::new();
        for m in &mut ms {
            let want = wopt.min(m.win_size.0.saturating_sub(2).max(1));
            let counted = self.daemon.as_ref().unwrap().win_sizes.get(&m.win);
            if m.self_sized || m.w == want || counted.is_none_or(|&(_, n)| n == m.panes) {
                continue;
            }
            put_back.push((m.pane.clone(), want));
            m.w = want;
        }
        if !put_back.is_empty() {
            self.mux.resize_panes(&put_back);
            w = watched_width(&ms, &self.active_session).unwrap_or(w);
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
        d.win_sizes = ms
            .iter()
            .map(|m| (m.win.clone(), (m.win_size, m.panes)))
            .collect();
        d.seen_mirror = true;
        d.size = (w, h);
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
            .unwrap_or_default()
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
            unsafe { libc::close(d.keys_fd) };
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
                owner.client.trim() == client
            } else {
                owner.generation.trim() == generation
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
            unsafe { libc::close(d.keys_fd) };
        }
        let _ = std::fs::remove_file(&self.rows_file);
        self.mux
            .release((!generation.is_empty()).then_some(generation.as_str()));
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
            self_sized: false,
        }
    }

    #[test]
    fn a_full_width_sidebar_is_not_a_border_drag() {
        let baseline = ((200, 50), 2);
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
        assert!(!dragged(&at(200, 1), 30, Some(&((200, 50), 1))));
        // a split or kill beside the sidebar is not a drag
        assert!(!dragged(&at(45, 3), 30, Some(&baseline)));
        // first measure of a window: nothing to compare against
        assert!(!dragged(&at(45, 2), 30, None));
        // at the configured width there is nothing to adopt
        assert!(!dragged(&at(30, 2), 30, Some(&baseline)));
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
    fn only_a_claim_by_someone_else_supersedes() {
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
        assert!(!mine(None)); // nor is an unread record
        assert!(!claimed_elsewhere("", "client-7", owner("g2", "client-7")));
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
    fn self_sized_frames_follow_the_watched_sidebar_width() {
        let mut ms = [m("$0", 40, true), m("$1", 40, true)];
        (ms[0].w, ms[1].w) = (36, 29);
        assert_eq!(
            watched_width(&ms, "$0"),
            Some(29),
            "tmux folds to the narrowest"
        );
        for m in &mut ms {
            m.self_sized = true;
        }
        assert_eq!(
            watched_width(&ms, "$0"),
            Some(36),
            "a hidden Rex sidebar is stale"
        );
        assert_eq!(watched_width(&ms, ""), Some(29));
        ms[1].active = false;
        assert_eq!(
            watched_width(&ms, ""),
            Some(36),
            "unwatched: the shown sidebars, not a hidden one"
        );
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
