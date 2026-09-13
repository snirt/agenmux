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
use crate::app_config::{Color, KeyMode, Keymap, Palette};
use crate::attention::Tracker;
use crate::conf::AgentConf;
use crate::procs::IdentCache;
use crate::scan::{self, PaneMeta, PaneRow};
use crate::tmux::{command, command_status, PendingChanges, Tmux, TmuxError};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const PERIODIC_SCAN: Duration = Duration::from_secs(2);
const OUTPUT_SCAN_MIN: Duration = Duration::from_millis(500);

struct ScanSchedule {
    next_periodic: Instant,
    next_output_eligible: Instant,
    output_due: Option<Instant>,
    immediate: bool,
}

impl ScanSchedule {
    fn new(now: Instant) -> Self {
        Self {
            next_periodic: now,
            next_output_eligible: now,
            output_due: None,
            immediate: false,
        }
    }

    fn observe_output(&mut self, now: Instant) {
        self.output_due
            .get_or_insert(now.max(self.next_output_eligible));
    }

    fn request_immediate(&mut self) {
        self.immediate = true;
    }

    fn due(&self, now: Instant, cache_expiry: Option<Instant>) -> Option<bool> {
        (self.immediate
            || now >= self.next_periodic
            || self.output_due.is_some_and(|d| now >= d)
            || cache_expiry.is_some_and(|d| now >= d))
        .then_some(now >= self.next_periodic)
    }

    fn complete(&mut self, now: Instant, periodic: bool) {
        self.immediate = false;
        if periodic {
            while self.next_periodic <= now {
                self.next_periodic += PERIODIC_SCAN;
            }
        }
        if self.output_due.is_some_and(|due| due <= now) {
            self.output_due = None;
        }
        self.next_output_eligible = now + OUTPUT_SCAN_MIN;
    }

    fn next_deadline(&self, cache_expiry: Option<Instant>) -> Instant {
        if self.immediate {
            return Instant::now();
        }
        [Some(self.next_periodic), self.output_due, cache_expiry]
            .into_iter()
            .flatten()
            .min()
            .unwrap()
    }
}

#[allow(unused_imports)]
pub use crate::input::send_key;
use crate::input::{
    key_pending, poll_inputs, protocol_keys, read_key, read_search_key, settings_keys, Key,
    KeySequence, RawMode, SequenceAction, SequenceResult,
};

mod daemon;
pub use daemon::run_daemon;
use daemon::Daemon;
mod filter;
use filter::StateFilter;
mod overlay;
mod render;
mod ui;
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

#[derive(Clone, Debug)]
struct MutationTarget {
    action: SequenceAction,
    pane_id: String,
    window_id: String,
    session_id: String,
    cwd: String,
    client: String,
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
    palette: Palette, // immutable startup snapshot shared by popup and split
    header_inherited: bool,
    normal_keys: Keymap, // startup snapshot: keys never change while running
    search_keys: Keymap,
    confs: Vec<AgentConf>,
    ident: IdentCache,
    subj: scan::SubjectCache,
    screens: scan::ScreenCache,
    tracker: Tracker,
    rows: Vec<PaneRow>, // complete debounced view-model; never filter cache/status
    panes: Vec<PaneMeta>, // latest complete sidebar-excluded inventory
    visible: Vec<VisiblePane>, // selectable panes; headers never enter this projection
    query: String,
    state_filter: Option<StateFilter>,
    search_focused: bool,
    key_sequence: KeySequence,
    refresh_requested: bool,
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

fn tmux_style_color(style: &str, role: &str) -> Option<Color> {
    let prefix = format!("{role}=");
    let value = &style[style.find(&prefix)? + prefix.len()..];
    if value.starts_with("default") {
        return Some(Color::Default);
    }
    if let Some(index) = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ]
    .iter()
    .position(|name| value.starts_with(name))
    {
        return Some(Color::Indexed(index as u8));
    }
    if let Some(rest) = value
        .strip_prefix("colour")
        .or_else(|| value.strip_prefix("color"))
    {
        let digits = rest
            .chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>();
        return digits.parse().ok().map(Color::Indexed);
    }
    if value.starts_with('#')
        && value.len() >= 7
        && value[1..7].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        let rgb = u32::from_str_radix(&value[1..7], 16).ok()?;
        return Some(Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));
    }
    None
}

fn apply_tmux_header(settings: &mut crate::app_config::AppConfig, active: &str, pane: &str) {
    if settings
        .theme
        .colors
        .as_ref()
        .and_then(|colors| colors.header_bg.as_ref())
        .is_none()
    {
        if let Some(color) = tmux_style_color(active, "fg") {
            settings
                .theme
                .colors
                .get_or_insert_with(Default::default)
                .header_bg = Some(color);
            settings.sources.insert(
                "theme.colors.header_bg",
                "tmux pane-active-border-style fg".into(),
            );
        }
    }
    if settings
        .theme
        .colors
        .as_ref()
        .and_then(|colors| colors.header_fg.as_ref())
        .is_none()
    {
        let (color, source) = tmux_style_color(active, "bg")
            .map(|color| (color, "tmux pane-active-border-style bg"))
            .or_else(|| tmux_style_color(pane, "bg").map(|color| (color, "tmux window-style bg")))
            .unwrap_or((Color::Default, "terminal background"));
        settings
            .theme
            .colors
            .get_or_insert_with(Default::default)
            .header_fg = Some(color);
        settings
            .sources
            .insert("theme.colors.header_fg", source.into());
    }
}

fn inherit_tmux_header(settings: &mut crate::app_config::AppConfig) {
    let active = std::env::var("AGENMUX_TMUX_ACTIVE_BORDER_STYLE").unwrap_or_else(|_| {
        command(&["show-option", "-gv", "pane-active-border-style"]).unwrap_or_default()
    });
    let pane = std::env::var("AGENMUX_TMUX_WINDOW_STYLE")
        .unwrap_or_else(|_| command(&["show-option", "-gv", "window-style"]).unwrap_or_default());
    apply_tmux_header(settings, &active, &pane);
}

fn uses_tmux_header_contrast(settings: &crate::app_config::AppConfig) -> bool {
    settings
        .sources
        .get("theme.colors.header_bg")
        .is_some_and(|source| source.starts_with("tmux "))
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
    mut settings: crate::app_config::AppConfig,
) -> Sidebar {
    let confs = crate::conf::load_all(&plugin_dir);
    // read once: the check behind it runs at most daily, and switching version
    // restarts the engine anyway
    let update = update_available(&plugin_dir);
    let adopted_show_all_panes = settings.show_all_panes;
    inherit_tmux_header(&mut settings);
    let header_inherited = uses_tmux_header_contrast(&settings);
    let palette = Palette::resolve(&settings.theme);
    let cached_rows = std::fs::read_to_string(&cache_file)
        .map(|tsv| scan::from_tsv(&tsv))
        .unwrap_or_default();
    // Cached TSV stores cwd basename, enough to avoid blocking startup on
    // unchanged panes. Live scans upgrade matching entries to full paths.
    // ponytail: basename can collide; persist full cwd if cache format changes.
    let seeded_subjects = cached_rows
        .iter()
        .filter(|row| row.pane != self_pane && !row.title.is_empty())
        .map(|row| (row.pane.clone(), (row.cwd.clone(), row.title.clone())))
        .collect();
    let mut sb = Sidebar {
        tmux,
        palette,
        header_inherited,
        normal_keys: settings.normal.clone(),
        search_keys: settings.search.clone(),
        settings: crate::app_config::LiveConfig::new(settings),
        adopted_show_all_panes,
        confs,
        ident: IdentCache::new(),
        subj: seeded_subjects,
        screens: scan::ScreenCache::default(),
        tracker: Tracker::default(),
        rows: Vec::new(),
        panes: Vec::new(),
        visible: Vec::new(),
        query: String::new(),
        state_filter: None,
        search_focused: false,
        key_sequence: KeySequence::default(),
        refresh_requested: false,
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
    // Agent-only mode can show the whole cached projection immediately.
    if !sb.settings.settings.show_all_panes {
        sb.rows = cached_rows;
        sb.rows.retain(|r| r.pane != sb.self_pane);
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
    let settings = match crate::app_config::current_process() {
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

    let tmux = match Tmux::connect_monitoring() {
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
    let mut scans = ScanSchedule::new(Instant::now());
    let mut next_tick = Instant::now();
    loop {
        if QUIT.load(Ordering::Relaxed) {
            break;
        }
        let mut now = Instant::now();
        // Keys outrank scans: a scan blocks the loop for 30-200ms, and under
        // key repeat that queued presses which then replayed after release.
        let key_waiting = key_pending(key_fd);
        let sequence_timeout = Duration::from_millis(sb.settings.settings.sequence_timeout_ms);
        if sb.key_sequence.expire(now, sequence_timeout) {
            sb.last_frame.clear();
            sb.render(false);
        }
        if let Some(periodic) = scans
            .due(now, sb.screens.next_expiry())
            .filter(|_| !key_waiting)
        {
            // Consume first: output observed by command-response reads during
            // this scan belongs to the next pass.
            let mut changes = sb.tmux.take_pending_changes();
            // Focus first, output after: captures cost 30-130ms and the
            // cursor must not wait behind them. Deferred panes stay pending
            // and reach the next output scan under its usual throttle.
            if !periodic && changes.focus && !changes.full {
                sb.tmux.defer_output(std::mem::take(&mut changes.panes));
            }
            match sb.scan_tick(periodic, &changes) {
                Ok(()) => {}
                // a pipe I/O error can leave a response block half-read —
                // the pipe is desynced, restarting is the only safe move
                Err(e @ TmuxError::Exited) | Err(e @ TmuxError::Io(_)) => {
                    trace!("scan ended the daemon: {e}");
                    break;
                }
                Err(TmuxError::Error(_)) => {} // e.g. pane died mid-scan
            }
            if sb.daemon.is_some() && sb.superseded() {
                return true; // a newer daemon owns the panes now
            }
            // Preserved-pane inventory, sizing and drag detection are periodic
            // reconciliation. Running them for output/focus scans can sample
            // transient tmux layouts twice and mistake them for a user drag.
            if sb.daemon.is_some() && periodic && !sb.mirror_tick() {
                break; // all preserved panes gone — nothing left to display
            }
            if sb.daemon.is_some() && periodic {
                if let Some(log) = crate::diag::cap_daemon_log(
                    &crate::diag::daemon_log_path(),
                    crate::diag::DAEMON_LOG_LIMIT,
                ) {
                    crate::diag::adopt_stderr(&log);
                }
            }
            // A focus change lands the user on a pane the writers may not be
            // feeding yet; retarget now instead of after the next periodic tick.
            if sb.daemon.is_some() && !periodic && changes.focus {
                sb.refocus_writers();
            }
            if sb.daemon.as_ref().is_some_and(|d| !d.keys_path.exists()) {
                break; // runtime dir vanished: deaf to keys, better gone than a zombie
            }
            sb.render(false);
            // a scan takes tens of ms — with the pre-scan `now`, a tick due
            // mid-scan is missed and the poll sleeps its full stale remainder
            now = Instant::now();
            scans.complete(now, periodic);
            if sb.has_immediate_change() {
                scans.request_immediate();
            }
            if sb.has_relevant_output() {
                scans.observe_output(now);
            }
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
        let mut wake = scans
            .next_deadline(sb.screens.next_expiry())
            .saturating_duration_since(now);
        if animating {
            wake = wake.min(next_tick.saturating_duration_since(now));
        }
        if let Some(deadline) = sb.key_sequence.deadline(sequence_timeout) {
            wake = wake.min(deadline.saturating_duration_since(now));
        }
        let (key_ready, pipe_ready) = poll_inputs(key_fd, sb.tmux.fd(), sb.tmux.buffered(), wake);
        if pipe_ready {
            // focus notification (%window-pane-changed etc.) — rescan now so
            // the cursor snaps to the newly focused pane without the 2s wait
            match sb.tmux.drain_notifications() {
                Ok(true) => scans.request_immediate(),
                Ok(false) => {}
                Err(_) => break,
            }
            if sb.has_relevant_output() {
                scans.observe_output(Instant::now());
            }
            if sb.has_immediate_change() {
                scans.request_immediate();
            }
        }
        if key_ready {
            // Drain every queued key before one render: each frame goes to
            // every sidebar pane of the session, too costly per repeat step.
            let mut drained = 0;
            loop {
                let editing_settings = sb.settings_editing();
                let text_input = sb.search_focused
                    || editing_settings
                    || matches!(
                        sb.overlay,
                        Some(
                            Overlay::Create { .. }
                                | Overlay::RenameScope(_)
                                | Overlay::Rename { .. }
                                | Overlay::Confirm(_)
                        )
                    );
                let mode = if text_input {
                    KeyMode::Search
                } else {
                    KeyMode::Normal
                };
                let keys = if editing_settings {
                    settings_keys()
                } else if sb.daemon.is_some() {
                    protocol_keys(mode)
                } else if text_input {
                    &sb.search_keys
                } else {
                    &sb.normal_keys
                };
                let key = if text_input && sb.daemon.is_none() {
                    read_search_key(key_fd, keys)
                } else {
                    read_key(key_fd, keys)
                };
                match sb.dispatch_key(key) {
                    DispatchResult::Continue => {}
                    DispatchResult::Break => return false,
                    DispatchResult::QuietExit => return true,
                }
                drained += 1;
                if drained >= 64 || !key_pending(key_fd) {
                    break;
                }
            }
            if std::mem::take(&mut sb.refresh_requested) {
                scans.request_immediate();
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
    /// Route every logical key through active UI mode. Overlay row maps may
    /// use mouse selection; normal list selection runs only after mode dispatch.
    fn dispatch_key(&mut self, key: Key) -> DispatchResult {
        if let Key::Sequence(key, client) = key {
            let timeout = Duration::from_millis(self.settings.settings.sequence_timeout_ms);
            let prefix = self.key_sequence.pending_prefix().unwrap_or(key);
            if crate::app_config::action_for(
                &self.normal_keys,
                crate::app_config::KeyChord::Printable(prefix as u8),
            )
            .is_some()
            {
                self.key_sequence.clear();
                return DispatchResult::Continue;
            }
            return match self.key_sequence.push(
                key,
                client,
                Instant::now(),
                timeout,
                self.settings.settings.tmux_management_enabled,
            ) {
                SequenceResult::Match(SequenceAction::First, _) => self.dispatch_key(Key::First),
                SequenceResult::Match(action, client) => self.begin_mutation(action, client),
                SequenceResult::Pending | SequenceResult::Miss => DispatchResult::Continue,
            };
        }
        self.key_sequence.clear();
        match dispatch_mode(self.overlay.as_ref(), self.search_focused) {
            DispatchMode::Overlay => return self.overlay_key(key),
            DispatchMode::Search => {
                self.search_key(key);
                return DispatchResult::Continue;
            }
            DispatchMode::Normal => {}
        }
        if let Key::Select(index) = &key {
            self.select_index(*index);
            return DispatchResult::Continue;
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
            Key::Settings => self.settings(),
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
            Key::Sequence(_, _)
            | Key::Backspace
            | Key::ClearSearch
            | Key::Text(_)
            | Key::Select(_)
            | Key::Other => {}
        }
        DispatchResult::Continue
    }

    fn begin_mutation(&mut self, action: SequenceAction, client: Option<String>) -> DispatchResult {
        if !self.settings.settings.tmux_management_enabled {
            return DispatchResult::Continue;
        }
        let client = client
            .filter(|value| !value.is_empty())
            .or_else(|| (!self.popup_client.is_empty()).then(|| self.popup_client.clone()));
        let Some(client) = client else {
            crate::diag::trace("mutation ignored: invoking tmux client is unknown");
            return DispatchResult::Continue;
        };
        let Some(pane) = self
            .panes
            .iter()
            .find(|pane| pane.pane == self.sel_pane)
            .cloned()
        else {
            self.mutation_error(&client, "selected pane no longer exists");
            return DispatchResult::Continue;
        };
        let target = MutationTarget {
            action,
            pane_id: pane.pane,
            window_id: pane.window_id,
            session_id: pane.session_id,
            cwd: pane.path,
            client,
        };
        match action {
            SequenceAction::CreateWindow | SequenceAction::CreateSession => {
                self.enter_mutation_input(&target.client);
                self.overlay = Some(Overlay::Create {
                    target,
                    name: String::new(),
                });
                DispatchResult::Continue
            }
            SequenceAction::Rename => {
                self.enter_mutation_input(&target.client);
                self.overlay = Some(Overlay::RenameScope(target));
                DispatchResult::Continue
            }
            SequenceAction::DeletePane
            | SequenceAction::DeleteWindow
            | SequenceAction::DeleteSession => {
                if self.settings.settings.tmux_management_confirm_delete {
                    self.enter_mutation_input(&target.client);
                    self.overlay = Some(Overlay::Confirm(target));
                    DispatchResult::Continue
                } else {
                    self.execute_mutation(&target, "")
                }
            }
            SequenceAction::First
            | SequenceAction::RenamePane
            | SequenceAction::RenameWindow
            | SequenceAction::RenameSession => DispatchResult::Continue,
        }
    }

    fn enter_mutation_input(&self, client: &str) {
        if self.daemon.is_some() {
            let _ = crate::tmux::command_status(&[
                "switch-client",
                "-c",
                client,
                "-T",
                crate::setup::SEARCH_TABLE,
            ]);
        }
    }

    fn restore_mutation_input(&self, client: &str) {
        if self.daemon.is_some() {
            let _ = crate::tmux::command_status(&[
                "switch-client",
                "-c",
                client,
                "-T",
                crate::setup::NORMAL_TABLE,
            ]);
        }
    }

    fn mutation_error(&mut self, client: &str, message: &str) {
        self.refresh_requested = true;
        let _ = crate::tmux::command_status(&[
            "display-message",
            "-c",
            client,
            "-d",
            "3000",
            &format!("agenmux: {message}"),
        ]);
    }

    fn target_is_live(target: &MutationTarget) -> bool {
        let (tmux_target, format, expected) = match target.action {
            SequenceAction::CreateWindow
            | SequenceAction::CreateSession
            | SequenceAction::Rename => (
                target.pane_id.as_str(),
                "#{pane_id}\t#{window_id}\t#{session_id}",
                format!(
                    "{}\t{}\t{}",
                    target.pane_id, target.window_id, target.session_id
                ),
            ),
            SequenceAction::DeletePane | SequenceAction::RenamePane => (
                target.pane_id.as_str(),
                "#{pane_id}",
                target.pane_id.clone(),
            ),
            SequenceAction::DeleteWindow | SequenceAction::RenameWindow => (
                target.window_id.as_str(),
                "#{window_id}",
                target.window_id.clone(),
            ),
            SequenceAction::DeleteSession | SequenceAction::RenameSession => (
                target.session_id.as_str(),
                "#{session_id}",
                target.session_id.clone(),
            ),
            SequenceAction::First => return false,
        };
        crate::tmux::command(&["display-message", "-p", "-t", tmux_target, format])
            .is_ok_and(|actual| actual.trim() == expected)
    }

    fn switch_client_to_pane(
        &self,
        client: &str,
        pane: &str,
    ) -> Result<(), crate::tmux::TmuxError> {
        let location = crate::tmux::command(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{session_id}\t#{window_id}\t#{pane_id}",
        ])?;
        let mut fields = location.trim().split('\t');
        let (Some(session), Some(window), Some(actual_pane)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(crate::tmux::TmuxError::Error(
                "created target has no tmux location".into(),
            ));
        };
        if actual_pane != pane {
            return Err(crate::tmux::TmuxError::Error(
                "created target changed before client switch".into(),
            ));
        }
        crate::tmux::command_status(&["switch-client", "-c", client, "-t", session])?;
        crate::tmux::command_status(&["select-window", "-t", window])?;
        crate::tmux::command_status(&["select-pane", "-t", pane])?;
        crate::tmux::command_status(&["switch-client", "-c", client, "-T", "root"])
    }

    fn execute_mutation(&mut self, target: &MutationTarget, name: &str) -> DispatchResult {
        self.restore_mutation_input(&target.client);
        if !self.settings.settings.tmux_management_enabled {
            self.refresh_requested = true;
            return DispatchResult::Continue;
        }
        if !Self::target_is_live(target) {
            self.mutation_error(&target.client, "target changed; nothing was modified");
            return DispatchResult::Continue;
        }
        let name = name.trim();
        let result = match target.action {
            SequenceAction::CreateWindow => {
                let session = format!("{}:", target.session_id);
                let mut args = vec!["new-window", "-d", "-P", "-F", "#{pane_id}"];
                if !name.is_empty() {
                    args.extend(["-n", name]);
                }
                args.extend(["-t", session.as_str(), "-c", target.cwd.as_str()]);
                crate::tmux::command(&args)
            }
            SequenceAction::CreateSession => {
                let mut args = vec!["new-session", "-d", "-P", "-F", "#{pane_id}"];
                if !name.is_empty() {
                    args.extend(["-s", name]);
                }
                args.extend(["-c", target.cwd.as_str()]);
                crate::tmux::command(&args)
            }
            SequenceAction::RenamePane => {
                if name.is_empty() {
                    return DispatchResult::Continue;
                }
                crate::tmux::command(&["select-pane", "-t", &target.pane_id, "-T", name])
            }
            SequenceAction::RenameWindow => {
                if name.is_empty() {
                    return DispatchResult::Continue;
                }
                crate::tmux::command(&["rename-window", "-t", &target.window_id, name])
            }
            SequenceAction::RenameSession => {
                if name.is_empty() {
                    return DispatchResult::Continue;
                }
                crate::tmux::command(&["rename-session", "-t", &target.session_id, name])
            }
            SequenceAction::DeletePane => {
                crate::tmux::command(&["kill-pane", "-t", &target.pane_id])
            }
            SequenceAction::DeleteWindow => {
                crate::tmux::command(&["kill-window", "-t", &target.window_id])
            }
            SequenceAction::DeleteSession => (|| -> Result<String, TmuxError> {
                let sessions = crate::tmux::command(&["list-sessions", "-F", "#{session_id}"])?;
                let fallback = sessions
                    .lines()
                    .find(|session| *session != target.session_id)
                    .ok_or_else(|| {
                        TmuxError::Error(
                            "cannot delete the last tmux session; nothing was modified".into(),
                        )
                    })?;
                let clients =
                    crate::tmux::command(&["list-clients", "-F", "#{client_name}\t#{session_id}"])?;
                for line in clients.lines() {
                    let Some((client, session)) = line.split_once('\t') else {
                        continue;
                    };
                    if session == target.session_id {
                        crate::tmux::command_status(&[
                            "switch-client",
                            "-c",
                            client,
                            "-t",
                            fallback,
                        ])?;
                    }
                }
                crate::tmux::command(&["kill-session", "-t", &target.session_id])
            })(),
            SequenceAction::First | SequenceAction::Rename => {
                return DispatchResult::Continue;
            }
        };
        let created_pane = match result {
            Ok(output) => output.trim().to_string(),
            Err(error) => {
                self.mutation_error(&target.client, &error.to_string());
                return DispatchResult::Continue;
            }
        };
        self.refresh_requested = true;
        if matches!(
            target.action,
            SequenceAction::CreateWindow | SequenceAction::CreateSession
        ) {
            if let Some(pin) = &self.pin {
                let _ = std::fs::write(format!("{pin}.jump"), &created_pane);
            } else if self
                .switch_client_to_pane(&target.client, &created_pane)
                .is_err()
            {
                self.mutation_error(&target.client, "created target, but client switch failed");
            }
        }
        if self.daemon.is_none() {
            DispatchResult::Break
        } else {
            DispatchResult::Continue
        }
    }

    /// Adopt whatever a `config reload` just published. Keys the tmux tables
    /// own were reinstalled by the reload itself; these are the parts this
    /// process holds: how it paints, and which chords it names in its hints.
    fn adopt_reload(&mut self, refreshed: crate::app_config::Refreshed) {
        if !refreshed.reloaded {
            return;
        }
        inherit_tmux_header(&mut self.settings.settings);
        self.header_inherited = uses_tmux_header_contrast(&self.settings.settings);
        self.palette = Palette::resolve(&self.settings.settings.theme);
        self.normal_keys = self.settings.settings.normal.clone();
        self.search_keys = self.settings.settings.search.clone();
        let management_enabled = self.settings.settings.tmux_management_enabled;
        if !management_enabled {
            let mutation_client = match self.overlay.as_ref() {
                Some(
                    Overlay::Create { target, .. }
                    | Overlay::RenameScope(target)
                    | Overlay::Rename { target, .. }
                    | Overlay::Confirm(target),
                ) => Some(target.client.clone()),
                _ => None,
            };
            if let Some(client) = mutation_client {
                self.restore_mutation_input(&client);
                self.overlay = None;
            }
        }
        if let Some(prefix) = self.key_sequence.pending_prefix() {
            let shadowed = crate::app_config::action_for(
                &self.normal_keys,
                crate::app_config::KeyChord::Printable(prefix as u8),
            )
            .is_some();
            let disabled_mutation = !management_enabled && matches!(prefix, 'c' | 'd' | 'r');
            if shadowed || disabled_mutation {
                self.key_sequence.clear();
            }
        }
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

    fn scan_tick(&mut self, periodic: bool, changes: &PendingChanges) -> Result<(), TmuxError> {
        if self.daemon.is_none() {
            let refreshed = self.settings.refresh(&mut self.tmux);
            self.adopt_reload(refreshed);
        }
        let t0 = Instant::now();
        let covered_session = self.tmux.attached_session().map(str::to_string);
        let (snapshot, stats) = scan::scan_cached(
            &mut self.tmux,
            &self.confs,
            &mut self.ident,
            &mut self.subj,
            Some(&self.self_pane),
            &mut self.screens,
            scan::ScanPolicy {
                dirty: &changes.panes,
                full: changes.full,
                periodic,
                covered_session: covered_session.as_deref(),
                now: Instant::now(),
            },
        )?;
        let scan::ScanSnapshot {
            agents: scanned,
            panes,
        } = snapshot;
        let reason = if periodic {
            "periodic"
        } else if !changes.panes.is_empty() {
            "output"
        } else if changes.full {
            "full"
        } else if changes.focus {
            "focus"
        } else {
            "scheduled"
        };
        trace!(
            "scan {}ms reason={reason} captured={} reused={}",
            t0.elapsed().as_millis(),
            stats.captured,
            stats.reused
        );
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
        if self
            .daemon
            .as_ref()
            .is_some_and(|daemon| daemon.attached != self.active_session)
            && !self.active_session.is_empty()
        {
            let sid = self.active_session.clone();
            if self.tmux.run(&format!("switch-client -t '{sid}'")).is_ok() {
                if let Some(daemon) = self.daemon.as_mut() {
                    daemon.attached = sid;
                }
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

    fn has_relevant_output(&self) -> bool {
        self.tmux
            .pending_changes()
            .panes
            .iter()
            .any(|pane| self.rows.iter().any(|row| &row.pane == pane))
    }

    fn has_immediate_change(&self) -> bool {
        let pending = self.tmux.pending_changes();
        pending.full || pending.focus
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
    fn tmux_style_colors_drive_default_header_contrast() {
        assert_eq!(
            tmux_style_color("fg=#f5a97f,bg=default", "fg"),
            Some(Color::Rgb(245, 169, 127))
        );
        assert_eq!(
            tmux_style_color("fg=colour42,bg=color7", "bg"),
            Some(Color::Indexed(7))
        );
        assert_eq!(
            tmux_style_color("fg=#f5a97f,bg=default", "bg"),
            Some(Color::Default)
        );
        assert_eq!(tmux_style_color("fg=red", "fg"), Some(Color::Indexed(1)));
        assert_eq!(
            tmux_style_color("pane-active-border-style fg=colour202,bg=default", "fg",),
            Some(Color::Indexed(202))
        );
        let mut settings =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        apply_tmux_header(&mut settings, "fg=#f5a97f,bg=colour236", "bg=colour235");
        let palette = Palette::resolve(&settings.theme);
        assert_eq!(
            palette.header_bg,
            crate::app_config::Ink::Typed(Color::Rgb(245, 169, 127))
        );
        assert_eq!(
            palette.header_fg,
            crate::app_config::Ink::Typed(Color::Indexed(236))
        );
        assert_eq!(
            settings.sources["theme.colors.header_bg"],
            "tmux pane-active-border-style fg"
        );
    }

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
    #[test]
    fn continuous_output_keeps_the_first_bounded_deadline() {
        let start = Instant::now();
        let mut schedule = ScanSchedule::new(start);
        schedule.complete(start, true);
        schedule.observe_output(start + Duration::from_millis(10));
        let due = schedule.output_due.unwrap();
        assert_eq!(due, start + OUTPUT_SCAN_MIN);
        for offset in [100, 200, 499] {
            schedule.observe_output(start + Duration::from_millis(offset));
            assert_eq!(schedule.output_due, Some(due));
        }
        assert!(schedule
            .due(start + Duration::from_millis(499), None)
            .is_none());
        assert_eq!(schedule.due(due, None), Some(false));
    }

    #[test]
    fn periodic_deadline_wins_even_when_output_and_animation_are_busy() {
        let start = Instant::now();
        let mut schedule = ScanSchedule::new(start);
        schedule.complete(start, true);
        schedule.next_output_eligible = start + Duration::from_secs(3);
        schedule.observe_output(start + Duration::from_millis(100));
        assert_eq!(schedule.next_deadline(None), start + PERIODIC_SCAN);
        assert_eq!(schedule.due(start + PERIODIC_SCAN, None), Some(true));

        let animation = start + Duration::from_millis(250);
        let wake = schedule
            .next_deadline(None)
            .min(animation)
            .saturating_duration_since(start);
        assert_eq!(wake, Duration::from_millis(250));
    }

    #[test]
    fn final_output_observed_during_a_scan_gets_another_capture_deadline() {
        let start = Instant::now();
        let mut schedule = ScanSchedule::new(start);
        schedule.complete(start, true);
        schedule.observe_output(start + Duration::from_millis(10));
        let first = start + OUTPUT_SCAN_MIN;
        assert_eq!(schedule.due(first, None), Some(false));
        let finished = first + Duration::from_millis(20);
        schedule.complete(finished, false);
        schedule.observe_output(finished);
        assert_eq!(schedule.output_due, Some(finished + OUTPUT_SCAN_MIN));
    }

    #[test]
    fn off_phase_cache_expiry_becomes_the_earliest_scan_deadline() {
        let start = Instant::now();
        let mut schedule = ScanSchedule::new(start);
        schedule.complete(start, true);

        // A covered pane is freshly captured by an output scan halfway
        // between periodic ticks. Periodic metadata scans reuse that screen.
        let captured = start + Duration::from_millis(500);
        let expiry = captured + Duration::from_secs(10);
        for seconds in [2, 4, 6, 8, 10] {
            let periodic = start + Duration::from_secs(seconds);
            assert_eq!(schedule.due(periodic, Some(expiry)), Some(true));
            schedule.complete(periodic, true);
        }

        assert_eq!(schedule.next_deadline(Some(expiry)), expiry);
        assert!(schedule
            .due(expiry - Duration::from_millis(1), Some(expiry))
            .is_none());
        assert_eq!(schedule.due(expiry, Some(expiry)), Some(false));
    }
}
