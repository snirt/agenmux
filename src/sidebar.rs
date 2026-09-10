// Sidebar TUI — runs inside the sidebar pane/popup. Port of sidebar.sh:
// same keys, same frame bytes, same rows/cache/pin file protocol, but one
// process, one tmux pipe, zero forks per tick.
//
// Two entry points share the engine:
//  - run():        tty mode — popup pane, draws to stdout, keys from stdin.
//  - run_daemon(): headless preserved-pane mode — frames go directly to the
//    processless panes visible in attached clients; keys arrive over a FIFO.
//    The panes never move between windows, so switching causes no join-pane
//    reflow (the "bump").
use crate::app_config::{KeyMode, Keymap, Palette};
use crate::attention::Tracker;
use crate::conf::AgentConf;
use crate::procs::IdentCache;
use crate::scan::{self, PaneMeta, PaneRow};
use crate::tmux::{command_status, Tmux, TmuxError};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::input::{
    poll_inputs, protocol_keys, read_key, read_search_key, Key, KeySequence, RawMode,
    SequenceResult,
};
#[allow(unused_imports)]
pub use crate::input::{select, send_key};

mod daemon;
pub use daemon::run_daemon;
use daemon::Daemon;
mod filter;
use filter::StateFilter;
mod overlay;
mod render;
use overlay::{update_available, Overlay};

static WINCH: AtomicBool = AtomicBool::new(false);
static QUIT: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, Ordering::Relaxed);
}
extern "C" fn on_term(_: libc::c_int) {
    QUIT.store(true, Ordering::Relaxed);
}

pub(crate) const E: &str = "\x1b";
#[derive(Debug, PartialEq, Eq)]
enum DispatchMode {
    Overlay,
    Search,
    Normal,
}

#[derive(Debug, PartialEq, Eq)]
enum DispatchResult {
    Continue,
    Break,
    QuietExit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PaneOccurrence {
    session_id: String,
    window_id: String,
    pane: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisiblePane {
    Agent(usize),
    Inventory(usize),
}

fn dispatch_mode(overlay: Option<&Overlay>, search_focused: bool) -> DispatchMode {
    if overlay.is_some() {
        DispatchMode::Overlay
    } else if search_focused {
        DispatchMode::Search
    } else {
        DispatchMode::Normal
    }
}

pub struct Sidebar {
    tmux: Tmux,
    settings: crate::app_config::LiveConfig,
    adopted_show_all_panes: bool,
    palette: Palette,    // immutable startup snapshot shared by popup and split
    normal_keys: Keymap, // startup snapshot: keys never change while running
    search_keys: Keymap,
    confs: Vec<AgentConf>,
    ident: IdentCache,
    subj: scan::SubjectCache,
    tracker: Tracker,
    rows: Vec<PaneRow>, // complete debounced view-model; never filter cache/status
    panes: Vec<PaneMeta>, // latest complete sidebar-excluded inventory
    visible: Vec<VisiblePane>, // selectable panes; headers never enter this projection
    query: String,
    state_filter: Option<StateFilter>,
    search_focused: bool,
    key_sequence: KeySequence,
    sel: usize,    // 1-based index into visible, like the bash script
    scroll: usize, // first visible list line
    follow_selection: bool,
    sel_pane: String,
    sel_occurrence: Option<PaneOccurrence>,
    last_active: String,
    active: String,
    active_session: String,
    plugin_selected: bool,
    tick: u32,
    self_pane: String,
    pin: Option<String>,
    popup_client: String,
    plugin_dir: PathBuf,
    rows_file: PathBuf,
    cache_file: PathBuf,
    last_frame: String,
    update: Option<String>, // newer release to advertise in the header
    daemon: Option<Daemon>,
    overlay: Option<Overlay>,
}

/// `self_pane` is the pane the sidebar itself occupies, skipped by every scan.
/// The daemon is headless and owns no pane, so it MUST pass "" — inheriting
/// TMUX_PANE from whoever pressed the toggle key hid that pane's agent.
fn new_sidebar(
    tmux: Tmux,
    plugin_dir: PathBuf,
    cache_file: PathBuf,
    rows_file: PathBuf,
    self_pane: String,
    settings: crate::app_config::AppConfig,
) -> Sidebar {
    let confs = crate::conf::load_all(&plugin_dir);
    // read once: the check behind it runs at most daily, and switching version
    // restarts the engine anyway
    let update = update_available(&plugin_dir);
    let adopted_show_all_panes = settings.show_all_panes;
    let mut sb = Sidebar {
        tmux,
        palette: Palette::resolve(&settings.theme),
        normal_keys: settings.normal.clone(),
        search_keys: settings.search.clone(),
        settings: crate::app_config::LiveConfig::new(settings),
        adopted_show_all_panes,
        confs,
        ident: IdentCache::new(),
        subj: scan::SubjectCache::new(),
        tracker: Tracker::default(),
        rows: Vec::new(),
        panes: Vec::new(),
        visible: Vec::new(),
        query: String::new(),
        state_filter: None,
        search_focused: false,
        key_sequence: KeySequence::default(),
        sel: 1,
        scroll: 0,
        sel_pane: String::new(),
        sel_occurrence: None,
        follow_selection: true,
        last_active: String::new(),
        active: String::new(),
        active_session: String::new(),
        plugin_selected: false,
        tick: 0,
        self_pane,
        pin: None,
        popup_client: crate::compat_env("AGENMUX_POPUP_CLIENT", "AGENTS_MON_POPUP_CLIENT")
            .unwrap_or_default(),
        plugin_dir,
        rows_file,
        cache_file,
        last_frame: String::new(),
        update,
        daemon: None,
        overlay: None,
    };
    // seed from the previous instance's scan for an instant first frame
    if !sb.settings.settings.show_all_panes {
        if let Ok(tsv) = std::fs::read_to_string(&sb.cache_file) {
            sb.rows = scan::from_tsv(&tsv);
            sb.rows.retain(|r| r.pane != sb.self_pane);
        }
    }
    sb.rebuild_visible(false);
    sb
}

pub fn run(plugin_dir: PathBuf, cache_file: PathBuf) -> i32 {
    // Catch termination during startup validation too; no terminal or tmux
    // mutation happens until configuration has passed validation.
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
    }
    let settings = match crate::app_config::current(None) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    let self_pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let pin = crate::compat_env("AGENMUX_PIN", "AGENTS_MON_PIN").filter(|p| !p.is_empty());
    let rows_file = std::env::temp_dir().join(format!(
        "agenmux-rows-{}",
        self_pane.trim_start_matches('%')
    ));

    if QUIT.load(Ordering::Relaxed) {
        cleanup(&rows_file, &pin);
        return 0;
    }
    let _raw = RawMode::enable();
    print!("{E}[?25l{E}[2J");
    let _ = std::io::stdout().flush();

    let tmux = match Tmux::connect() {
        Ok(t) => t,
        Err(_) => {
            cleanup(&rows_file, &pin);
            return 0;
        }
    };
    let mut sb = new_sidebar(tmux, plugin_dir, cache_file, rows_file, self_pane, settings);
    sb.pin = pin;
    // tty mode is the popup: while it is visible it owns input.
    sb.plugin_selected = true;
    sb.render(true);
    event_loop(&mut sb);
    cleanup(&sb.rows_file, &sb.pin);
    0
}

/// Returns true when the loop ended because another daemon took ownership.
fn event_loop(sb: &mut Sidebar) -> bool {
    let key_fd = sb.daemon.as_ref().map_or(0, |d| d.keys_fd);
    let mut next_scan = Instant::now(); // scan immediately
    let mut next_tick = Instant::now();
    loop {
        if QUIT.load(Ordering::Relaxed) {
            break;
        }
        let mut now = Instant::now();
        if now >= next_scan {
            match sb.scan_tick() {
                Ok(()) => {}
                // a pipe I/O error can leave a response block half-read —
                // the pipe is desynced, restarting is the only safe move
                Err(TmuxError::Exited) | Err(TmuxError::Io(_)) => break,
                Err(TmuxError::Error(_)) => {} // e.g. pane died mid-scan
            }
            if sb.daemon.is_some() && sb.superseded() {
                return true; // a newer daemon owns the panes now
            }
            if sb.daemon.is_some() && !sb.mirror_tick() {
                break; // all preserved panes gone — nothing left to display
            }
            if sb.daemon.as_ref().is_some_and(|d| !d.keys_path.exists()) {
                break; // runtime dir vanished: deaf to keys, better gone than a zombie
            }
            sb.render(false);
            // a scan takes tens of ms — with the pre-scan `now`, a tick due
            // mid-scan is missed and the poll sleeps its full stale remainder
            now = Instant::now();
            next_scan = now + Duration::from_secs(2);
        }
        let animating = sb
            .visible
            .iter()
            .any(|&pane| matches!(sb.visible_state(pane), "working" | "blocked" | "done"));
        // deadline-based tick: held keys keep poll_inputs returning early, so
        // advancing on poll timeout would freeze the spinner during key repeat
        if animating && now >= next_tick {
            sb.tick = (sb.tick + 1) % 40; // divisible by 8 (spin) and 4 (blink)
            next_tick = now + Duration::from_millis(250);
            sb.render(false);
        }
        // animated states need ticks; all-idle sleeps until the next scan
        let wake = if animating {
            next_tick.saturating_duration_since(now)
        } else {
            next_scan.saturating_duration_since(now)
        };
        let (key_ready, pipe_ready) = poll_inputs(key_fd, sb.tmux.fd(), sb.tmux.buffered(), wake);
        if pipe_ready {
            // focus notification (%window-pane-changed etc.) — rescan now so
            // the cursor snaps to the newly focused pane without the 2s wait
            match sb.tmux.drain_notifications() {
                Ok(true) => next_scan = Instant::now(),
                Ok(false) => {}
                Err(_) => break,
            }
        }
        if key_ready {
            let mode = if sb.search_focused {
                KeyMode::Search
            } else {
                KeyMode::Normal
            };
            let keys = if sb.daemon.is_some() {
                protocol_keys(mode)
            } else if sb.search_focused {
                &sb.search_keys
            } else {
                &sb.normal_keys
            };
            let key = if sb.search_focused && sb.daemon.is_none() {
                read_search_key(key_fd, keys)
            } else {
                read_key(key_fd, keys)
            };
            match sb.dispatch_key(key) {
                DispatchResult::Continue => {}
                DispatchResult::Break => break,
                DispatchResult::QuietExit => return true,
            }
            sb.render(false);
        }
        if sb.daemon.is_none() && WINCH.swap(false, Ordering::Relaxed) {
            print!("{E}[2J");
            sb.render(true);
        }
    }
    false // every other exit is a real close: the caller tears down
}

fn cleanup(rows_file: &PathBuf, pin: &Option<String>) {
    print!("{E}[?25h");
    let _ = std::io::stdout().flush();
    let _ = std::fs::remove_file(rows_file);
    if let Some(p) = pin {
        // keep the pin when a jump is pending — native toggle reopens the popup
        if !std::path::Path::new(&format!("{p}.jump")).exists() {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Sidebar {
    /// Route every logical key through the active UI mode. Mouse selection is
    /// handled before mode dispatch; overlays clear their row map, so they cannot
    /// receive a stale click on the hidden list.
    fn dispatch_key(&mut self, key: Key) -> DispatchResult {
        if let Key::Sequence(key) = key {
            return match self
                .key_sequence
                .push(key, Instant::now(), &[("gg", Key::First)])
            {
                SequenceResult::Match(action) => self.dispatch_key(action),
                SequenceResult::Pending | SequenceResult::Miss => DispatchResult::Continue,
            };
        }
        self.key_sequence.clear();
        if let Key::Select(index) = &key {
            self.select_index(*index);
            return DispatchResult::Continue;
        }
        match dispatch_mode(self.overlay.as_ref(), self.search_focused) {
            DispatchMode::Overlay => {
                self.overlay_key(key);
                return DispatchResult::Continue;
            }
            DispatchMode::Search => {
                self.search_key(key);
                return DispatchResult::Continue;
            }
            DispatchMode::Normal => {}
        }
        match key {
            Key::First => self.select_index(1),
            Key::Last => self.select_index(self.visible.len()),
            Key::Down => self.move_sel(1),
            Key::Up => self.move_sel(-1),
            Key::WheelUp => self.scroll_viewport(-1),
            Key::WheelDown => self.scroll_viewport(1),
            Key::Jump => {
                if self.jump() {
                    return DispatchResult::Break;
                }
            }
            Key::Help => self.help(),
            Key::Versions => self.versions(),
            Key::Search => self.focus_search(),
            Key::CycleState => self.cycle_state_filter(),
            Key::AllStates => self.clear_filter(),
            Key::Quit => {
                if self.daemon.is_none() {
                    // Popup/tty mode owns stdin, so q/Ctrl-C/Ctrl-D closes it.
                    if let Some(p) = &self.pin {
                        let _ = std::fs::remove_file(p);
                    }
                    return DispatchResult::Break;
                }
                // In preserved-pane mode close arrives as Key::Close from the
                // key table; Quit also covers FIFO EOF, which must not kill it.
            }
            Key::Close => {
                if self.daemon.is_some() {
                    // Finish teardown before a fast reopen can observe the
                    // dying control client and attach panes to it.
                    self.teardown();
                    self.daemon = None;
                    return DispatchResult::QuietExit;
                }
                return DispatchResult::Break;
            }
            Key::Sequence(_)
            | Key::Backspace
            | Key::ClearSearch
            | Key::Text(_)
            | Key::Select(_)
            | Key::Other => {}
        }
        DispatchResult::Continue
    }

    /// Adopt whatever a `config reload` just published. Keys the tmux tables
    /// own were reinstalled by the reload itself; these are the parts this
    /// process holds: how it paints, and which chords it names in its hints.
    fn adopt_reload(&mut self, refreshed: crate::app_config::Refreshed) {
        if !refreshed.reloaded {
            return;
        }
        self.palette = Palette::resolve(&self.settings.settings.theme);
        self.normal_keys = self.settings.settings.normal.clone();
        self.search_keys = self.settings.settings.search.clone();
        let show_all_panes = self.settings.settings.show_all_panes;
        if show_all_panes != self.adopted_show_all_panes {
            self.adopted_show_all_panes = show_all_panes;
            self.rebuild_visible(false);
            if let Some(index) = self.active_visible_index() {
                self.select_index(index + 1);
            }
        }
        self.last_frame.clear(); // colors or projection changed: redraw every pane
    }

    fn scan_tick(&mut self) -> Result<(), TmuxError> {
        if self.daemon.is_none() {
            let refreshed = self.settings.refresh(&mut self.tmux);
            self.adopt_reload(refreshed);
        }
        let t0 = Instant::now();
        let scan::ScanSnapshot {
            agents: scanned,
            panes,
        } = scan::scan(
            &mut self.tmux,
            &self.confs,
            &mut self.ident,
            &mut self.subj,
            Some(&self.self_pane),
        )?;
        crate::tmux::debug_note(&format!("scan {}ms", t0.elapsed().as_millis()));
        let _ = std::fs::write(&self.cache_file, scan::to_tsv(&scanned));
        let mut focus = self.client_focus().unwrap_or_default();
        // A popup owns the terminal's input even though tmux still reports the
        // pane underneath it as selected. That underlying agent is not being
        // viewed while the popup is open.
        if self.daemon.is_none() {
            focus.discount_client(&self.popup_client);
            focus.plugin_selected = true;
        }
        self.active = focus.active_pane;
        self.active_session = focus.active_session;
        self.plugin_selected = focus.plugin_selected;
        // notifications only cover the attached session — follow the user so
        // drags and focus changes where they're looking react instantly
        // (background sessions wait for the 2s scan, which nobody can see)
        if self.daemon.is_some()
            && !self.active_session.is_empty()
            && self.daemon.as_ref().unwrap().attached != self.active_session
        {
            let sid = self.active_session.clone();
            if self.tmux.run(&format!("switch-client -t '{sid}'")).is_ok() {
                // insurance: keep pane output off the control pipe
                let _ = self.tmux.run("refresh-client -f no-output");
                self.daemon.as_mut().unwrap().attached = sid;
            }
        }

        let update = self.tracker.update(scanned, &focus.focused_panes);
        self.rows = update.rows;
        self.panes = panes;
        for event in &update.events {
            let _ = crate::notifications::deliver(self.settings.settings.notifications, event);
        }
        self.rebuild_visible(false);
        // single cursor: focus landing on a visible agent pane snaps selection
        // to it; active filters never select a row they intentionally hid
        if !self.active.is_empty() && self.active != self.last_active {
            if let Some(i) = self.active_visible_index() {
                self.select_index(i + 1);
            }
            self.last_active = self.active.clone();
        }
        Ok(())
    }

    fn client_focus(&mut self) -> Option<crate::focus::ClientFocus> {
        // scan() only syncs at its START; a hook's run-shell block landing
        // during the capture loop leaves every later command paired with the
        // wrong response. Re-barrier before reading anything we act on.
        self.tmux.sync().ok()?;
        let focus_events = self
            .tmux
            .run("show-option -gqv focus-events")
            .ok()
            .is_some_and(|value| value.trim() == "on");
        let out = self
            .tmux
            .run("list-clients -F '#{client_activity}\t#{client_name}\t#{session_id}\t#{pane_id}\t#{pane_title}\t#{client_flags}'")
            .ok()?;
        Some(crate::focus::parse_clients(&out, focus_events))
    }

    /// true = exit the loop (popup jump hands off to native toggle)
    fn jump(&mut self) -> bool {
        let Some(target) = self
            .visible
            .get(self.sel.wrapping_sub(1))
            .map(|&pane| self.visible_pane_id(pane).to_string())
        else {
            return false;
        };
        if !target.starts_with('%') {
            return false;
        }
        // This sidebar stays mounted after a jump, so clear its transient
        // navigator state before leaving.
        self.clear_filter();
        if let Some(pin) = &self.pin {
            // popup holds the client — hand the target to native toggle, which
            // jumps after the popup closes
            let _ = std::fs::write(format!("{pin}.jump"), &target);
            return true;
        }
        // switch/select MUST NOT go over the control pipe: they fire the
        // plugin's select-window/session hooks, and tmux delivers each hook's
        // run-shell result to the triggering client as an extra %begin/%end
        // block — desyncing every later response. Fork plain tmux instead
        // (jump is rare and user-initiated).
        // pick the most recently active client — with several terminals
        // attached, the first listed one may not be the one the user is
        // looking at (and the 'focused' flag sticks on all of them)
        let client = self
            .tmux
            .run("list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{client_activity} #{client_name}'")
            .ok()
            .and_then(|c| {
                c.lines()
                    .filter_map(|l| {
                        let (act, name) = l.split_once(' ')?;
                        Some((act.parse::<u64>().ok()?, name.to_string()))
                    })
                    .max_by_key(|(act, _)| *act)
                    .map(|(_, name)| name)
            });
        let mut args = Vec::new();
        if let Some(client) = &client {
            args.extend([
                "switch-client",
                "-c",
                client,
                "-t",
                target.as_str(),
                ";",
                "switch-client",
                "-c",
                client,
                "-T",
                "root",
                ";",
            ]);
        }
        args.extend([
            "select-window",
            "-t",
            target.as_str(),
            ";",
            "select-pane",
            "-t",
            target.as_str(),
        ]);
        let _ = command_status(&args);
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_modes_route_search_help_and_versions() {
        assert_eq!(dispatch_mode(None, true), DispatchMode::Search);
        assert_eq!(
            dispatch_mode(Some(&Overlay::Help), false),
            DispatchMode::Overlay
        );
        assert_eq!(
            dispatch_mode(
                Some(&Overlay::Versions {
                    sel: 0,
                    chosen: None,
                }),
                false,
            ),
            DispatchMode::Overlay
        );
        assert_eq!(dispatch_mode(None, false), DispatchMode::Normal);
    }
}
