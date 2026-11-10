// The tmux backend. Its command helpers double as the escape hatch for
// tmux-only features.
mod control;

pub use control::*;

use super::{
    Changes, Error, Identity, Ids, Kind, Multiplexer, New, Owner, SidebarPane, WindowMeta,
};
use crate::app_config::{AppConfig, Color, ConfigError};
use crate::focus::{Client, ClientFocus};
use crate::scan::{PaneMeta, ParsedPane};
use crate::snapshot::{Closed, Live, Pane, Resume, Snapshot};
use std::collections::{BTreeMap, HashSet};
use std::os::fd::RawFd;
use std::path::Path;
use std::time::{Duration, Instant};

/// `window_name` goes last: it may contain the delimiter. The fourth field is
/// 1 when the window's sidebar is on the right.
const WINDOWS_FMT: &str = "list-windows -a -F '#{window_id}|#{window_panes}|#{automatic-rename}|#{P:#{?#{==:#{pane_title},agenmux},#{pane_at_right},}}|#{window_layout}|#{window_name}'";

// tmux 3.7 can render tabs and non-ASCII format separators as underscores; keep
// machine-readable tmux formats on a printable ASCII delimiter.
const LIST_FMT: &str = "list-panes -a -F '#{session_id}|#{session_name}|#{window_id}|#{window_index}|#{window_name}|#{pane_id}|#{pane_index}|#{pane_pid}|#{pane_current_command}|#{pane_current_path}|#{pane_width}|#{pane_height}|#{pane_title}|#{@agenmux}'";

impl Multiplexer for Tmux {
    fn panes(&mut self, skip: Option<&str>) -> Result<Vec<ParsedPane>, Error> {
        self.sync()?;
        Ok(parse_panes(&self.run(LIST_FMT)?, skip))
    }

    fn capture(&mut self, pane: &str) -> Result<String, Error> {
        // pane content must never travel over the control pipe: a pane
        // displaying literal "%end <t> <num>" text (logs, this plugin's own
        // docs...) would terminate the response block early and desync every
        // later command. Route it through a buffer + file.
        let buf = format!("agenmux-{}", std::process::id());
        let cap = std::env::temp_dir().join(&buf);
        self.run(&format!("capture-pane -b '{buf}' -t '{pane}'"))?;
        let saved = self.run(&format!("save-buffer -b '{buf}' '{}'", cap.display()));
        let _ = self.run(&format!("delete-buffer -b '{buf}'"));
        saved?;
        let read = std::fs::read(&cap);
        let _ = std::fs::remove_file(&cap);
        // only pipe failures are Io: the daemon exits on Io as a desync
        read.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|e| {
                let message = format!("capture file {}: {e}", cap.display());
                trace!("{message}");
                Error::Failed(message)
            })
    }

    fn focus(&mut self) -> Option<ClientFocus> {
        // A scan syncs only at its start; a hook's run-shell block landing
        // during the capture loop leaves every later command paired with the
        // wrong response. Re-barrier before reading anything we act on.
        self.sync().ok()?;
        let focus_events = self
            .run("show-option -gqv focus-events")
            .ok()
            .is_some_and(|value| value.trim() == "on");
        let out = self
            .run("list-clients -F '#{client_activity}|#{client_name}|#{session_id}|#{pane_id}|#{pane_title}|#{client_flags}'")
            .ok()?;
        Some(ClientFocus::from_clients(&parse_clients(
            &out,
            focus_events,
        )))
    }

    fn events_fd(&self) -> Option<RawFd> {
        Some(self.fd())
    }
    fn events_buffered(&self) -> bool {
        self.buffered()
    }
    fn drain_events(&mut self) -> Result<bool, Error> {
        self.drain_notifications()
    }
    fn changes(&self) -> &Changes {
        self.pending_changes()
    }
    fn take_changes(&mut self) -> Changes {
        self.take_pending_changes()
    }
    fn defer_output(&mut self, panes: HashSet<String>) {
        Tmux::defer_output(self, panes)
    }
    fn covered_session(&self) -> Option<&str> {
        self.attached_session()
    }
    /// Notifications cover the attached session only.
    fn follow_session(&mut self, session: &str) -> bool {
        self.run(&format!("switch-client -t '{session}'")).is_ok()
    }

    // Management forks plain tmux: the plugin's hooks answer these commands
    // with run-shell output that would desync the control pipe.
    fn is_live(&mut self, kind: Option<Kind>, ids: Ids) -> bool {
        let (target, format, expected) = match kind {
            None => (
                ids.pane,
                "#{pane_id}|#{window_id}|#{session_id}",
                format!("{}|{}|{}", ids.pane, ids.window, ids.session),
            ),
            Some(Kind::Pane) => (ids.pane, "#{pane_id}", ids.pane.to_string()),
            Some(Kind::Window) => (ids.window, "#{window_id}", ids.window.to_string()),
            Some(Kind::Session) => (ids.session, "#{session_id}", ids.session.to_string()),
        };
        command(&["display-message", "-p", "-t", target, format])
            .is_ok_and(|actual| actual.trim() == expected)
    }

    fn name(&mut self, kind: Kind, ids: Ids) -> Result<String, Error> {
        let (id, format) = match kind {
            Kind::Pane => (ids.pane, "#{pane_title}"),
            Kind::Window => (ids.window, "#{window_name}"),
            Kind::Session => (ids.session, "#{session_name}"),
        };
        command(&["display-message", "-p", "-t", id, format])
            .map(|name| name.trim_end_matches(['\r', '\n']).to_string())
    }

    fn rename(&mut self, kind: Kind, ids: Ids, name: &str) -> Result<(), Error> {
        match kind {
            Kind::Pane => command_status(&["select-pane", "-t", ids.pane, "-T", name]),
            Kind::Window => {
                let _ = command_status(&[
                    "set-window-option",
                    "-t",
                    ids.window,
                    "automatic-rename",
                    "off",
                ]);
                command_status(&["rename-window", "-t", ids.window, name])
            }
            Kind::Session => command_status(&["rename-session", "-t", ids.session, name]),
        }
    }

    fn delete(&mut self, kind: Kind, ids: Ids) -> Result<(), Error> {
        match kind {
            Kind::Pane => {
                let not_sidebar = format!("#{{?{},0,1}}", crate::panes::IS_SIDEBAR);
                let panes = command(&[
                    "list-panes",
                    "-s",
                    "-t",
                    ids.session,
                    "-f",
                    &not_sidebar,
                    "-F",
                    "#{pane_id}",
                ])?;
                if panes.lines().count() <= 1 {
                    return Err(Error::Failed(
                        "cannot delete the last pane in a tmux session; delete the session explicitly"
                            .into(),
                    ));
                }
                command_status(&["kill-pane", "-t", ids.pane])
            }
            Kind::Window => {
                let windows = command(&["list-windows", "-t", ids.session, "-F", "#{window_id}"])?;
                if windows.lines().count() <= 1 {
                    return Err(Error::Failed(
                        "cannot delete the last window in a tmux session; delete the session explicitly"
                            .into(),
                    ));
                }
                command_status(&["kill-window", "-t", ids.window])
            }
            Kind::Session => {
                let sessions = command(&["list-sessions", "-F", "#{session_id}"])?;
                let fallback = sessions
                    .lines()
                    .find(|session| *session != ids.session)
                    .ok_or_else(|| {
                        Error::Failed(
                            "cannot delete the last tmux session; nothing was modified".into(),
                        )
                    })?;
                let clients = command(&["list-clients", "-F", "#{client_name}|#{session_id}"])?;
                for line in clients.lines() {
                    let Some((client, session)) = line.split_once('|') else {
                        continue;
                    };
                    if session == ids.session {
                        command_status(&["switch-client", "-c", client, "-t", fallback])?;
                    }
                }
                command_status(&["kill-session", "-t", ids.session])
            }
        }
    }

    fn pane_cwd(&mut self, ids: Ids) -> Result<String, Error> {
        let response = command(&[
            "display-message",
            "-p",
            "-t",
            ids.pane,
            "#{pane_id}|#{window_id}|#{session_id}|#{pane_current_path}",
        ])?;
        parse_pane_cwd(ids, &response)
    }

    fn session_cwd(&mut self, session: &str) -> Result<String, Error> {
        let cwd = command(&["display-message", "-p", "-t", session, "#{session_path}"])?;
        let cwd = cwd.trim_end_matches(['\n', '\r']);
        if cwd.is_empty() {
            return Err(Error::Failed(
                "target session has no working directory".into(),
            ));
        }
        Ok(cwd.to_string())
    }

    fn create(&mut self, new: New) -> Result<String, Error> {
        let mut args = Vec::new();
        let session;
        match new {
            New::Window {
                session: s,
                name,
                cwd,
            } => {
                session = format!("{s}:");
                args.extend(["new-window", "-d", "-P", "-F", "#{pane_id}"]);
                if !name.is_empty() {
                    args.extend(["-n", name]);
                }
                args.extend(["-t", session.as_str(), "-c", cwd]);
            }
            New::Session { name, cwd } => {
                args.extend(["new-session", "-d", "-P", "-F", "#{pane_id}"]);
                if !name.is_empty() {
                    args.extend(["-s", name]);
                }
                args.extend(["-c", cwd]);
            }
            New::Launch {
                session: s,
                cwd,
                argv,
            } => {
                session = format!("{s}:");
                args.extend(["new-window", "-d", "-P", "-F", "#{pane_id}", "-t"]);
                args.extend([session.as_str(), "-c", cwd]);
                args.extend(argv);
            }
        }
        command(&args).map(|pane| pane.trim().to_string())
    }

    fn jump(&mut self, pane: &str) -> Result<(), Error> {
        // the most recently active client: with several terminals attached,
        // the first listed one may not be the one the user is looking at (and
        // the 'focused' flag sticks on all of them)
        let client = self
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
        // switch/select MUST NOT go over the control pipe: they fire the
        // plugin's select-window/session hooks, and tmux delivers each hook's
        // run-shell result to the triggering client as an extra %begin/%end
        // block, desyncing every later response. Fork plain tmux instead.
        let mut args = Vec::new();
        if let Some(client) = &client {
            args.extend([
                "switch-client",
                "-c",
                client,
                "-t",
                pane,
                ";",
                "switch-client",
                "-c",
                client,
                "-T",
                "root",
                ";",
            ]);
        }
        args.extend(["select-window", "-t", pane, ";", "select-pane", "-t", pane]);
        command_status(&args)
    }

    fn show(&mut self, client: &str, pane: &str, stay: bool) -> Result<String, Error> {
        let location = command(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{session_id}|#{window_id}|#{pane_id}",
        ])?;
        let mut fields = location.trim_end_matches(['\n', '\r']).split('|');
        let (Some(session), Some(window), Some(actual_pane)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return Err(Error::Failed("created pane has no tmux location".into()));
        };
        if actual_pane != pane {
            return Err(Error::Failed(
                "created pane changed before client switch".into(),
            ));
        }
        command_status(&["switch-client", "-c", client, "-t", session])?;
        command_status(&["select-window", "-t", window])?;
        if !stay {
            command_status(&["select-pane", "-t", pane])?;
            command_status(&["switch-client", "-c", client, "-T", "root"])?;
        }
        Ok(window.to_string())
    }

    /// A window just created gets its sidebar from the background pane-add
    /// hook, so wait briefly for it to appear.
    fn focus_sidebar(&mut self, client: &str, expected_window: Option<&str>) {
        let deadline = Instant::now() + Duration::from_millis(1500);
        let location = format!("#{{window_id}}|#{{?{},1,0}}", crate::panes::IS_SIDEBAR);
        let client_filter = format!("#{{==:#{{client_name}},{client}}}");
        let mut reached_expected = expected_window.is_none();
        loop {
            let Ok(location) = command(&["list-clients", "-f", &client_filter, "-F", &location])
            else {
                break;
            };
            let (window, on_sidebar) = location.trim().split_once('|').unwrap_or(("", "0"));
            // Creation names the exact destination. Wait for tmux to publish the
            // completed switch; only treat a later move away as user intent.
            if let Some(expected) = expected_window {
                if window == expected {
                    reached_expected = true;
                } else if reached_expected {
                    break;
                } else if Instant::now() >= deadline {
                    break;
                } else {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            }
            if expected_window.is_none() && on_sidebar == "1" {
                break; // already there: never fight a newer focus change
            }
            let sidebar = command(&[
                "list-panes",
                "-t",
                window,
                "-f",
                crate::panes::IS_SIDEBAR,
                "-F",
                "#{pane_id}",
            ])
            .ok()
            .and_then(|panes| panes.lines().next().map(str::to_string));
            if let Some(pane) = sidebar {
                let _ = command_status(&["switch-client", "-c", client, "-t", &pane]);
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// display-message expands formats and strftime, which would eat `%12`.
    fn message(&mut self, client: &str, text: &str) -> bool {
        let message = format!("agenmux: {text}")
            .replace('%', "%%")
            .replace('#', "##");
        command_status(&["display-message", "-c", client, "-d", "3000", &message]).is_ok()
    }

    /// `-w` also sends it to the client's clipboard (OSC 52).
    fn copy(&mut self, client: Option<&str>, text: &str) -> Result<Option<String>, Error> {
        let mut args = vec!["set-buffer", "-w"];
        if let Some(client) = client {
            args.extend(["-t", client]);
        }
        args.extend(["--", text]);
        command_status(&args).map(|()| None)
    }

    fn reference(&self, kind: Kind, id: &str) -> String {
        reference(kind, id)
    }

    /// `#{pid}|#{start_time}`, and the socket's file name.
    fn identity(&mut self) -> Option<Identity> {
        // A fresh control client answers empty until its attach completes.
        let mut out = String::new();
        for _ in 0..100 {
            out = self
                .run("display-message -p '#{pid}|#{start_time}|#{socket_path}'")
                .ok()?;
            if !out.trim().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut fields = out.trim().splitn(3, '|');
        let server = format!("{}|{}", fields.next()?, fields.next()?);
        let socket = std::path::Path::new(fields.next()?).file_name()?.to_str()?;
        Some(Identity {
            server,
            name: socket.to_string(),
        })
    }

    fn windows(&mut self) -> Result<Vec<WindowMeta>, Error> {
        Ok(parse_windows(&self.run(WINDOWS_FMT)?))
    }

    fn live(&mut self) -> Live {
        let mut live = Live::default();
        let panes = self
            .run("list-panes -a -F '#{window_index}|#{window_id}|#{pane_id}|#{session_name}'")
            .unwrap_or_default();
        for line in panes.lines() {
            let fields: Vec<&str> = line.splitn(4, '|').collect();
            let [index, window, pane, session] = fields[..] else {
                continue;
            };
            if let Ok(index) = index.parse() {
                live.windows.insert((session.to_string(), index));
            }
            live.ids.insert(window.to_string());
            live.ids.insert(pane.to_string());
        }
        live
    }

    fn restore(&mut self, snap: &Snapshot, live: &Live, resume: &Resume) {
        replay(
            snap,
            &live.windows,
            |agent| resume.get(agent).cloned(),
            &mut run_tmux,
        );
    }

    fn restore_pane(&mut self, entry: &Closed, live: &Live, resume: &Resume) {
        replay_pane(
            entry,
            live,
            |agent| resume.get(agent).cloned(),
            &mut run_tmux,
        );
    }

    fn options(&mut self) -> Result<BTreeMap<String, String>, ConfigError> {
        read_options(|cmd| self.run(cmd))
    }

    fn reload_token(&mut self) -> Option<String> {
        self.run(&format!("show-option -gqv {RELOAD_OPTION}"))
            .ok()
            .map(|value| value.trim_end_matches('\n').to_owned())
            .filter(|value| !value.is_empty())
    }

    fn publish_reload(&mut self, token: &str) -> Result<(), Error> {
        self.run(&format!("set-option -g {RELOAD_OPTION} {token}"))
            .map(drop)
    }

    fn sidebar_panes(&mut self) -> Result<Vec<SidebarPane>, Error> {
        let out = self.run("list-panes -a -f '#{==:#{pane_title},agenmux}' -F '#{pane_id}|#{window_id}|#{pane_width}|#{pane_height}|#{window_width} #{window_height}|#{window_panes}|#{window_active}|#{session_id}'")?;
        Ok(out.lines().filter_map(parse_measurement).collect())
    }

    fn visible_windows(&mut self) -> HashSet<String> {
        self.run("list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{window_id}'")
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// Forked, as one command: resize-pane fires hooks whose run-shell echoes
    /// would desync the control pipe.
    fn resize_panes(&mut self, cols: &[(String, usize)]) -> bool {
        let widths: Vec<String> = cols.iter().map(|(_, w)| w.to_string()).collect();
        batch(
            cols.iter().zip(&widths).map(|((pane, _), w)| {
                ["resize-pane", "-t", pane.as_str(), "-x", w.as_str()].to_vec()
            }),
        )
    }

    fn close_panes(&mut self, panes: &[String]) {
        batch(
            panes
                .iter()
                .map(|pane| vec!["kill-pane", "-t", pane.as_str()]),
        );
    }

    fn close_sidebars(&mut self) {
        let _ = crate::panes::teardown_panes();
    }

    fn remember_width(&mut self, cols: usize) -> bool {
        self.run(&format!("set-option -g @agenmux-width {cols}"))
            .is_ok()
    }

    /// Asked on this connection, so it names the exact client even when tmux
    /// forks and reports a different OS PID.
    fn connection(&mut self) -> Option<String> {
        self.run("display-message -p '#{client_name}'")
            .ok()
            .map(|client| client.trim().to_string())
            .filter(|client| !client.is_empty())
    }

    /// A stale hook response must not look like a newer daemon's claim.
    fn owner(&mut self) -> Option<Owner> {
        self.sync().ok()?;
        parse_owner(&self.run(OWNER_QUERY).ok()?)
    }

    /// Unescaped: show-option hands the value back unescaped too.
    fn claim(&mut self, client: &str, runtime: &Path) -> Result<(), Error> {
        let client = client.replace('\'', "\\'");
        self.run(&format!("set-option -g @agenmux-control-client '{client}'"))?;
        let runtime = runtime.to_string_lossy().replace('\'', "\\'");
        self.run(&format!("set-option -g @agenmux-runtime-dir '{runtime}'"))
            .map(drop)
    }

    fn release(&mut self, generation: Option<&str>) {
        let _ = crate::panes::clear_ownership(generation);
    }

    fn as_tmux(&mut self) -> Option<&mut Tmux> {
        Some(self)
    }
}

impl Tmux {
    /// Route keys typed in sidebar panes through `table`.
    pub fn sidebar_key_table(&mut self, table: &str) {
        let clients = self
            .run("list-clients -F '#{client_name}|#{pane_title}'")
            .unwrap_or_default();
        for line in clients.lines() {
            let Some((client, title)) = line.split_once('|') else {
                continue;
            };
            if title == "agenmux" {
                let _ = command_spawn(&["switch-client", "-c", client, "-T", table]);
            }
        }
    }

    /// Show `text` on this connection's client. Formats and strftime expand.
    pub fn display(&mut self, text: &str) {
        let _ = self.run(&format!("display-message {}", quote(text)));
    }

    /// Header colors from the tmux theme, where the settings leave them
    /// unset. Returns the active border's foreground.
    pub fn inherit_header(&self, settings: &mut AppConfig) -> Option<Color> {
        let option = |name: &str| command(&["show-option", "-gv", name]).unwrap_or_default();
        let active = std::env::var("AGENMUX_TMUX_ACTIVE_BORDER_STYLE")
            .unwrap_or_else(|_| option("pane-active-border-style"));
        let pane =
            std::env::var("AGENMUX_TMUX_WINDOW_STYLE").unwrap_or_else(|_| option("window-style"));
        let active_border_fg = style_color(&active, "fg");
        apply_header(settings, &active, &pane);
        active_border_fg
    }

    /// The settings moved the sidebars to the other side. The default jump
    /// and fold keys follow the side: reload and the settings overlay
    /// reinstall them already, a bare tmux option does not.
    pub fn dock(&mut self, plugin_dir: &Path, settings: &crate::app_config::AppConfig) {
        crate::panes::dock(settings.sidebar_position, settings.sidebar_width);
        if crate::setup::bindings_stale(settings)
            && crate::setup::run_config(plugin_dir, settings) == 0
        {
            crate::setup::reclaim_client_tables();
        }
    }
}

/// One forked tmux running `commands` in order; true when there were none.
fn batch<'a>(commands: impl Iterator<Item = Vec<&'a str>>) -> bool {
    let mut args = Vec::new();
    for command in commands {
        if !args.is_empty() {
            args.push(";");
        }
        args.extend(command);
    }
    args.is_empty() || command_status(&args).is_ok()
}

/// Both owner options arrive in one tagged reply: a stray hook block read in
/// its place lacks the tag, and cannot shift one value into the other.
const OWNER_QUERY: &str =
    "display-message -p 'am-owner|#{@agenmux-generation}|#{@agenmux-control-client}'";

fn parse_owner(reply: &str) -> Option<Owner> {
    let (generation, client) = reply
        .trim_end()
        .strip_prefix("am-owner|")?
        .split_once('|')?;
    Some(Owner {
        generation: generation.to_string(),
        client: client.to_string(),
    })
}

/// `TMUX_PANE`, when it is a pane id.
pub fn own_pane() -> Option<String> {
    std::env::var("TMUX_PANE")
        .ok()
        .filter(|pane| valid_tmux_id(pane, '%'))
}

/// The shell of `TMUX_PANE`, for telling nested multiplexers apart.
pub fn pane_pid() -> Option<u32> {
    let pane = own_pane()?;
    quiet(&["display-message", "-p", "-t", &pane, "#{pane_pid}"])?
        .trim()
        .parse()
        .ok()
}

/// The server `TMUX` names: `socket,pid,session`.
pub fn server_pid() -> Option<u32> {
    std::env::var("TMUX").ok()?.split(',').nth(1)?.parse().ok()
}

/// A tmux server answers.
pub fn running() -> bool {
    quiet(&["list-sessions"]).is_some()
}

fn quiet(args: &[&str]) -> Option<String> {
    let out = process()
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The owner record, without a connection.
pub fn owner() -> Option<Owner> {
    let query = OWNER_QUERY
        .strip_prefix("display-message -p ")?
        .trim_matches('\'');
    parse_owner(&command(&["display-message", "-p", query]).ok()?)
}

fn parse_measurement(line: &str) -> Option<SidebarPane> {
    let fields: Vec<&str> = line.split('|').collect();
    let [pane, win, pw, ph, ws, wp, active, sess] = fields.as_slice() else {
        return None;
    };
    let (Ok(w), Ok(h), Ok(panes)) = (
        pw.parse::<usize>(),
        ph.parse::<usize>(),
        wp.parse::<usize>(),
    ) else {
        return None;
    };
    let (window_width, window_height) = ws
        .split_once(' ')
        .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))?;
    Some(SidebarPane {
        pane: (*pane).into(),
        win: (*win).into(),
        sess: (*sess).into(),
        w,
        h,
        win_size: (window_width, window_height),
        panes,
        active: *active == "1",
        self_sized: false,
    })
}

/// The option `config reload` bumps. Long-lived views watch it rather than the
/// file: one explicit signal beats polling the filesystem, and it reaches the
/// daemon and any popup at once.
const RELOAD_OPTION: &str = "@agenmux-reload";

pub fn options() -> Result<BTreeMap<String, String>, ConfigError> {
    read_options(|cmd| command(&cmd.split_whitespace().collect::<Vec<_>>()))
}

/// Presence is read separately from raw values: tmux's quoted list output is
/// not a serialization format for arbitrary option values.
fn read_options(
    mut run: impl FnMut(&str) -> Result<String, Error>,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let io = |_| ConfigError::unreadable_options("tmux");
    let listed = run("show-options -g").map_err(io)?;
    let present: HashSet<_> = listed
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    let mut options = BTreeMap::new();
    for prefix in ["@agenmux-", "@agents-mon-"] {
        for suffix in crate::app_config::OPTIONS {
            let name = format!("{prefix}{suffix}");
            if present.contains(name.as_str()) {
                let value = run(&format!("show-option -gqv {name}")).map_err(io)?;
                options.insert(name, value.strip_suffix('\n').unwrap_or(&value).to_owned());
            }
        }
    }
    Ok(options)
}

pub fn publish_reload(token: &str) -> Result<(), Error> {
    command_status(&["set-option", "-g", RELOAD_OPTION, token])
}

/// Restore's runner. Plain tmux forks, like jump: their hooks' output must
/// not reach the control pipe.
fn run_tmux(args: &[String]) -> Option<String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    command(&args).ok().map(|out| out.trim().to_string())
}

/// Rows formatted as activity, client, session, pane, title, flags. With
/// focus events off, tmux cannot tell which client has focus: all of them may.
fn parse_clients(rows: &str, focus_events: bool) -> Vec<Client> {
    let mut clients = Vec::new();
    for line in rows.lines() {
        let separator = if line.contains('|') { '|' } else { '\t' };
        let mut fields = line.splitn(6, separator);
        let (Some(activity), Some(client), Some(session), Some(pane), Some(title), Some(flags)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let Ok(activity) = activity.parse::<u64>() else {
            continue;
        };
        let has_flag = |wanted: &str| flags.split(',').any(|flag| flag == wanted);
        if has_flag("control-mode") {
            continue;
        }
        clients.push(Client {
            activity,
            name: client.to_string(),
            session: session.to_string(),
            pane: pane.to_string(),
            on_sidebar: title == "agenmux",
            focused: !focus_events || has_flag("focused"),
        });
    }
    clients
}

fn style_color(style: &str, role: &str) -> Option<Color> {
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

fn apply_header(settings: &mut AppConfig, active: &str, pane: &str) {
    if settings
        .theme
        .colors
        .as_ref()
        .and_then(|colors| colors.header_bg.as_ref())
        .is_none()
    {
        if let Some(color) = style_color(active, "fg") {
            settings
                .theme
                .colors
                .get_or_insert_with(Default::default)
                .header_bg = Some(color);
            settings.sources.insert(
                "theme.colors.header_bg".into(),
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
        let (color, source) = style_color(active, "bg")
            .map(|color| (color, "tmux pane-active-border-style bg"))
            .or_else(|| style_color(pane, "bg").map(|color| (color, "tmux window-style bg")))
            .unwrap_or((Color::Default, "terminal background"));
        settings
            .theme
            .colors
            .get_or_insert_with(Default::default)
            .header_fg = Some(color);
        settings
            .sources
            .insert("theme.colors.header_fg".into(), source.into());
    }
}

fn parse_windows(rows: &str) -> Vec<WindowMeta> {
    rows.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.splitn(6, '|').collect();
            let [id, panes, automatic, right, layout, _name] = fields[..] else {
                return None;
            };
            Some(WindowMeta {
                id: id.to_string(),
                panes: panes.parse().unwrap_or(0),
                automatic_rename: automatic != "0",
                sidebar_right: right == "1",
                layout: layout.to_string(),
            })
        })
        .collect()
}

impl Tmux {
    /// Route `client`'s keys through `table`.
    pub fn key_table(&mut self, client: &str, table: &str) {
        let _ = command_status(&["switch-client", "-c", client, "-T", table]);
    }
}

/// Split a closed pane back into its live window: after the nearest saved
/// neighbour still open (before the next one when it was first), then the
/// window's saved layout. tmux rejects that layout unless the window has its
/// saved pane count again, which leaves tmux's own split in place.
fn replay_pane(
    entry: &Closed,
    live: &Live,
    resume: impl Fn(&str) -> Option<String>,
    run: &mut impl FnMut(&[String]) -> Option<String>,
) {
    let (Some(window), Some(pane)) = (entry.session.windows.first(), entry.pane()) else {
        return;
    };
    let at = window
        .panes
        .iter()
        .position(|p| p.id == pane.id)
        .unwrap_or(0);
    let open = |p: &&Pane| live.ids.contains(&p.id);
    let (before, target) = match window.panes[..at].iter().rev().find(open) {
        Some(prev) => (false, prev.id.as_str()),
        None => match window.panes[at + 1..].iter().find(open) {
            Some(next) => (true, next.id.as_str()),
            None => (false, window.id.as_str()),
        },
    };
    let mut split = vec!["split-window", "-d"];
    if before {
        split.push("-b");
    }
    split.extend(["-t", target, "-c", &pane.path, "-P", "-F", "#{pane_id}"]);
    let Some(id) = run(&split.iter().map(|s| s.to_string()).collect::<Vec<_>>()) else {
        return;
    };
    if !window.layout.is_empty() {
        run(&["select-layout", "-t", &window.id, &window.layout].map(String::from));
    }
    if let Some(command) = pane.agent.as_deref().and_then(resume) {
        run(&["send-keys", "-t", &id, "-l", &command].map(String::from));
        run(&["send-keys", "-t", &id, "Enter"].map(String::from));
    }
}

/// Recreate `prev`'s missing sessions, and its missing windows inside existing
/// sessions; never touch a live window. `live` is every live
/// `(session name, window index)`. `run` executes one tmux command and returns
/// its trimmed output, or `None` on failure. `resume` gives the command typed
/// into an agent's restored shell, if any.
fn replay(
    prev: &Snapshot,
    live: &HashSet<(String, u32)>,
    resume: impl Fn(&str) -> Option<String>,
    run: &mut impl FnMut(&[String]) -> Option<String>,
) {
    let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    for session in &prev.sessions {
        let target = format!("={}", session.name);
        let mut exists = live.iter().any(|(name, _)| *name == session.name);
        for window in &session.windows {
            if window.panes.is_empty() || live.contains(&(session.name.clone(), window.index)) {
                continue;
            }
            let first = &window.panes[0].path;
            let mut create = if exists {
                args(&[
                    "new-window",
                    "-d",
                    "-t",
                    &format!("{target}:{}", window.index),
                ])
            } else {
                args(&["new-session", "-d", "-s", &session.name])
            };
            create.extend(args(&[
                "-c",
                first,
                "-P",
                "-F",
                "#{window_index} #{pane_id}",
            ]));
            if !window.automatic_rename {
                create.extend(args(&["-n", &window.name]));
            }
            let Some(created) = run(&create) else {
                continue;
            };
            let Some((index, p0)) = created.split_once(' ') else {
                continue;
            };
            let p0 = p0.to_string();
            if !exists && index != window.index.to_string() {
                run(&args(&[
                    "move-window",
                    "-d",
                    "-s",
                    &p0,
                    "-t",
                    &format!("{target}:{}", window.index),
                ]));
            }
            exists = true;
            // The agenmux pane was the first pane (the last when on the right);
            // a placeholder holds its cell until select-layout has placed the rest.
            let left = window.sidebar && !window.sidebar_right;
            let (split, mut resumes) = if left {
                (&window.panes[..], Vec::new())
            } else {
                (&window.panes[1..], vec![(p0.clone(), &window.panes[0])])
            };
            let mut last = p0.clone();
            for pane in split {
                let Some(id) = run(&args(&[
                    "split-window",
                    "-t",
                    &last,
                    "-c",
                    &pane.path,
                    "-P",
                    "-F",
                    "#{pane_id}",
                ])) else {
                    break;
                };
                resumes.push((id.clone(), pane));
                last = id;
            }
            let placeholder = if left {
                Some(p0.clone())
            } else if window.sidebar {
                run(&args(&[
                    "split-window",
                    "-t",
                    &last,
                    "-P",
                    "-F",
                    "#{pane_id}",
                ]))
            } else {
                None
            };
            if !window.layout.is_empty() {
                run(&args(&["select-layout", "-t", &p0, &window.layout]));
            }
            if let Some(placeholder) = placeholder {
                run(&args(&["kill-pane", "-t", &placeholder]));
            }
            // After the layout, so agents start at their final size.
            for (id, pane) in resumes {
                if let Some(command) = pane.agent.as_deref().and_then(&resume) {
                    run(&args(&["send-keys", "-t", &id, "-l", &command]));
                    run(&args(&["send-keys", "-t", &id, "Enter"]));
                }
            }
        }
    }
}

fn reference(kind: Kind, id: &str) -> String {
    let kind = match kind {
        Kind::Session => "session",
        Kind::Window => "window",
        Kind::Pane => "pane",
    };
    format!("tmux {kind} {id}")
}

fn parse_pane_cwd(ids: Ids, response: &str) -> Result<String, Error> {
    let response = response.strip_suffix('\n').unwrap_or(response);
    let mut fields = response.splitn(4, '|');
    let (Some(pane), Some(window), Some(session), Some(cwd)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(Error::Failed(
            "selected pane changed before launcher start".into(),
        ));
    };
    if pane != ids.pane || window != ids.window || session != ids.session {
        return Err(Error::Failed(
            "selected pane changed before launcher start".into(),
        ));
    }
    if cwd.is_empty() {
        return Err(Error::Failed(
            "selected pane has no working directory".into(),
        ));
    }
    Ok(cwd.to_string())
}

fn valid_tmux_id(value: &str, prefix: char) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn parse_panes(rows: &str, self_pane: Option<&str>) -> Vec<ParsedPane> {
    rows.lines()
        .filter_map(|line| {
            let separator = if line.contains('|') { '|' } else { '\t' };
            let fields = line.splitn(13, separator).collect::<Vec<_>>();
            let [session_id, session_name, window_id, window_index, window_name, pane, pane_index, pid, command, path, width, height, title_and_marked] =
                fields.as_slice()
            else {
                return None;
            };
            let (pane_title, marked) = title_and_marked.rsplit_once(separator)?;
            if !valid_tmux_id(session_id, '$')
                || !valid_tmux_id(window_id, '@')
                || !valid_tmux_id(pane, '%')
            {
                return None;
            }
            let window_index = window_index.parse().ok()?;
            let pane_index = pane_index.parse().ok()?;
            let pid = pid.parse().ok()?;
            let width = width.parse().ok()?;
            let height = height.parse().ok()?;
            if self_pane == Some(*pane)
                || marked == "1"
                || (pid == 0 && pane_title == "agenmux")
            {
                return None;
            }
            Some(ParsedPane {
                meta: PaneMeta {
                    pane: (*pane).to_string(),
                    pane_index,
                    pane_title: pane_title.to_string(),
                    command: (*command).to_string(),
                    path: (*path).to_string(),
                    window_id: (*window_id).to_string(),
                    window_index,
                    window_name: (*window_name).to_string(),
                    session_id: (*session_id).to_string(),
                    session_name: (*session_name).to_string(),
                    agent_index: None,
                },
                pid,
                width,
                height,
                reported: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_style_colors_drive_default_header_contrast() {
        assert_eq!(
            style_color("fg=#f5a97f,bg=default", "fg"),
            Some(Color::Rgb(245, 169, 127))
        );
        assert_eq!(
            style_color("fg=colour42,bg=color7", "bg"),
            Some(Color::Indexed(7))
        );
        assert_eq!(
            style_color("fg=#f5a97f,bg=default", "bg"),
            Some(Color::Default)
        );
        assert_eq!(style_color("fg=red", "fg"), Some(Color::Indexed(1)));
        assert_eq!(
            style_color("pane-active-border-style fg=colour202,bg=default", "fg",),
            Some(Color::Indexed(202))
        );
        let mut settings =
            crate::app_config::resolve(&Default::default(), &Default::default()).unwrap();
        apply_header(&mut settings, "fg=#f5a97f,bg=colour236", "bg=colour235");
        let palette = crate::app_config::Palette::resolve(&settings.theme);
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
    fn focus_events_require_the_focused_flag_and_ignore_control_clients() {
        let rows = concat!(
            "20\tclient-1\t$1\t%2\twork\tattached,focused,utf8\n",
            "30\tclient-2\t$2\t%3\twork\tattached,utf8\n",
            "40\tcontrol\t$3\t%4\tagenmux\tattached,focused,control-mode,utf8\n",
        );
        let focus = ClientFocus::from_clients(&parse_clients(rows, true));

        assert_eq!(focus.active_pane, "%3");
        assert_eq!(focus.active_session, "$2");
        assert!(!focus.plugin_selected);
        assert_eq!(focus.focused_panes, HashSet::from(["%2".to_string()]));
    }

    #[test]
    fn focus_events_off_treat_every_selected_real_client_pane_as_focused() {
        let rows = concat!(
            "20\tclient-1\t$1\t%2\twork\tattached,utf8\n",
            "30\tclient-2\t$2\t%3\tagenmux\tattached,utf8\n",
        );
        let focus = ClientFocus::from_clients(&parse_clients(rows, false));

        assert_eq!(focus.active_pane, "%3");
        assert!(focus.plugin_selected);
        assert_eq!(
            focus.focused_panes,
            HashSet::from(["%2".to_string(), "%3".to_string()])
        );
    }

    #[test]
    fn only_a_tagged_owner_reply_counts_as_a_record() {
        let owner = |generation: &str, client: &str| Owner {
            generation: generation.into(),
            client: client.into(),
        };
        assert_eq!(
            parse_owner("am-owner|g1|client-7\n"),
            Some(owner("g1", "client-7"))
        );
        assert_eq!(parse_owner("am-owner||\n"), Some(owner("", "")));
        // A stray block read in the reply's place: empty, or another value.
        assert_eq!(parse_owner(""), None);
        assert_eq!(parse_owner("g2\n"), None);
    }
    use crate::snapshot::tests::sample;
    use crate::snapshot::{Session, Window};

    #[test]
    fn replay_creates_only_missing_branches_in_order() {
        let mut prev = sample("1|1");
        prev.sessions[0].windows.push(Window {
            index: 4,
            name: "shell".into(),
            automatic_rename: true,
            layout: "efgh,80x24,0,0,9".into(),
            sidebar: false,
            panes: vec![
                Pane {
                    id: String::new(),
                    path: "/a".into(),
                    agent: Some("codex".into()),
                },
                Pane {
                    id: String::new(),
                    path: "/b".into(),
                    agent: None,
                },
            ],
            ..Window::default()
        });
        prev.sessions.push(Session {
            name: "keep".into(),
            windows: vec![Window {
                index: 0,
                panes: vec![Pane::default()],
                ..Window::default()
            }],
            ..Session::default()
        });
        let live = HashSet::from([("keep".to_string(), 0)]);
        let mut log = Vec::new();
        let mut next = 10;
        replay(
            &prev,
            &live,
            |agent| (agent == "claude").then(|| "claude --continue".into()),
            &mut |cmd: &[String]| {
                log.push(cmd.join(" "));
                next += 1;
                Some(match cmd[0].as_str() {
                    "new-session" => format!("0 %{next}"),
                    "new-window" => format!("4 %{next}"),
                    _ => format!("%{next}"),
                })
            },
        );
        assert_eq!(
            log,
            [
                "new-session -d -s work -c /repo -P -F #{window_index} #{pane_id} -n editor",
                "move-window -d -s %11 -t =work:2",
                "split-window -t %11 -c /repo -P -F #{pane_id}",
                "split-window -t %13 -c /tmp -P -F #{pane_id}",
                "select-layout -t %11 abcd,80x24,0,0,1",
                "kill-pane -t %11",
                "send-keys -t %13 -l claude --continue",
                "send-keys -t %13 Enter",
                "new-window -d -t =work:4 -c /a -P -F #{window_index} #{pane_id}",
                "split-window -t %19 -c /b -P -F #{pane_id}",
                "select-layout -t %19 efgh,80x24,0,0,9",
            ]
        );
    }

    #[test]
    fn pane_restore_splits_next_to_its_nearest_open_neighbour() {
        let window = Window {
            id: "@4".into(),
            layout: "L".into(),
            panes: ["%1", "%2", "%3"]
                .map(|id| Pane {
                    id: id.into(),
                    path: format!("/p{id}"),
                    agent: (id == "%2").then(|| "claude".into()),
                })
                .into(),
            ..Window::default()
        };
        let restore = |id: &str, open: &[&str]| {
            let entry = Closed {
                id: id.into(),
                session: Session {
                    windows: vec![window.clone()],
                    ..Session::default()
                },
                ..Closed::default()
            };
            let live = Live {
                ids: open.iter().map(|id| id.to_string()).collect(),
                ..Live::default()
            };
            let mut log = Vec::new();
            replay_pane(&entry, &live, |_| Some("claude -c".into()), &mut |cmd| {
                log.push(cmd.join(" "));
                Some("%9".into())
            });
            log
        };
        assert_eq!(
            restore("%2", &["@4", "%1", "%3"]),
            [
                "split-window -d -t %1 -c /p%2 -P -F #{pane_id}",
                "select-layout -t @4 L",
                "send-keys -t %9 -l claude -c",
                "send-keys -t %9 Enter",
            ]
        );
        assert_eq!(
            restore("%1", &["@4", "%3"])[0],
            "split-window -d -b -t %3 -c /p%1 -P -F #{pane_id}",
            "first pane goes before the next open one"
        );
        assert_eq!(
            restore("%3", &["@4"])[0],
            "split-window -d -t @4 -c /p%3 -P -F #{pane_id}",
            "no saved neighbour left: split the window"
        );
    }

    #[test]
    fn restore_replays_a_session_or_a_window_into_its_live_session() {
        let replayed = |session: &Session, live: &HashSet<(String, u32)>| {
            let mut log = Vec::new();
            let snap = Snapshot {
                sessions: vec![session.clone()],
                ..Snapshot::default()
            };
            replay(&snap, live, |_| None, &mut |cmd: &[String]| {
                log.push(cmd.join(" "));
                Some(
                    if cmd[0] == "split-window" {
                        "%9"
                    } else {
                        "3 %8"
                    }
                    .into(),
                )
            });
            log
        };
        let mut session = sample("1|1").sessions.remove(0);
        session.windows[0].index = 3;
        session.windows[0].sidebar = false;
        assert_eq!(
            replayed(&session, &HashSet::from([("work".to_string(), 0)])),
            [
                "new-window -d -t =work:3 -c /repo -P -F #{window_index} #{pane_id} -n editor",
                "split-window -t %8 -c /tmp -P -F #{pane_id}",
                "select-layout -t %8 abcd,80x24,0,0,1",
            ]
        );
        // A right-hand sidebar's cell is last in layout order, so its
        // placeholder is split off after the saved panes.
        session.windows[0].sidebar = true;
        session.windows[0].sidebar_right = true;
        assert_eq!(
            replayed(&session, &HashSet::from([("work".to_string(), 0)])),
            [
                "new-window -d -t =work:3 -c /repo -P -F #{window_index} #{pane_id} -n editor",
                "split-window -t %8 -c /tmp -P -F #{pane_id}",
                "split-window -t %9 -P -F #{pane_id}",
                "select-layout -t %8 abcd,80x24,0,0,1",
                "kill-pane -t %9",
            ]
        );
        assert_eq!(
            replayed(&session, &HashSet::new())[0],
            "new-session -d -s work -c /repo -P -F #{window_index} #{pane_id} -n editor"
        );
    }
    #[test]
    fn window_rows_parse_with_the_name_last() {
        let windows = parse_windows("@4|3|0|1|L4|my|name\n@6|1|1||L6|zsh\nbad\n");
        let [w4, w6] = &windows[..] else {
            panic!("{}", windows.len())
        };
        assert_eq!(
            (w4.id.as_str(), w4.panes, w4.layout.as_str()),
            ("@4", 3, "L4")
        );
        assert!(!w4.automatic_rename && w4.sidebar_right);
        assert!(w6.automatic_rename && !w6.sidebar_right);
    }

    #[test]
    fn references_name_the_record_by_tmux_id() {
        let tmux = |kind, id| format!("tmux {kind} {id}");
        for (kind, name, id) in [
            (Kind::Session, "session", "$3"),
            (Kind::Window, "window", "@7"),
            (Kind::Pane, "pane", "%12"),
        ] {
            assert_eq!(reference(kind, id), tmux(name, id));
        }
    }

    #[test]
    fn launcher_revalidation_rejects_stale_identity_without_using_another_pane_cwd() {
        let ids = Ids {
            session: "$1",
            window: "@2",
            pane: "%4",
        };
        assert_eq!(
            parse_pane_cwd(ids, "%4|@2|$1|/path/with|pipe\n")
                .ok()
                .as_deref(),
            Some("/path/with|pipe")
        );
        for stale in [
            "%9|@2|$1|/different/pane\n",
            "%4|@8|$1|/different/window\n",
            "%4|@2|$9|/different/session\n",
            "%4|@2|$1|\n",
        ] {
            let error = parse_pane_cwd(ids, stale).unwrap_err();
            assert!(
                error.to_string().contains("selected pane")
                    || error.to_string().contains("working directory"),
                "unexpected stale-target error: {error}"
            );
        }
    }

    #[test]
    fn pane_inventory_parser_skips_malformed_and_sidebar_rows() {
        let rows = [
            "$1\twork\t@1\t0\teditor\t%1\t0\t101\tcodex\t/repo\t80\t24\trepo\t",
            "$1\twork\t@1\t0\teditor\t%2\t1\t0\t\t/repo\t80\t24\tordinary\t",
            "missing\tfields",
            "$1\twork\t@1\tx\teditor\t%3\t2\t102\tsleep\t/repo\t80\t24\tbad window\t",
            "$1\twork\t@1\t0\teditor\t%4\tx\t103\tsleep\t/repo\t80\t24\tbad pane\t",
            "$1\twork\t@1\t0\teditor\t%5\t3\tnot-a-pid\tsleep\t/repo\t80\t24\tbad pid\t",
            "1\twork\t@1\t0\teditor\t%13\t3\t109\tsleep\t/repo\t80\t24\tbad session prefix\t",
            "$x\twork\t@1\t0\teditor\t%14\t3\t110\tsleep\t/repo\t80\t24\tbad session digits\t",
            "$1\twork\t1\t0\teditor\t%15\t3\t111\tsleep\t/repo\t80\t24\tbad window prefix\t",
            "$1\twork\t@x\t0\teditor\t%16\t3\t112\tsleep\t/repo\t80\t24\tbad window digits\t",
            "$1\twork\t@1\t0\teditor\t17\t3\t113\tsleep\t/repo\t80\t24\tbad pane prefix\t",
            "$1\twork\t@1\t0\teditor\t%x\t3\t114\tsleep\t/repo\t80\t24\tbad pane digits\t",
            "$\twork\t@1\t0\teditor\t%18\t3\t115\tsleep\t/repo\t80\t24\tempty session id\t",
            "$1\twork\t@\t0\teditor\t%19\t3\t116\tsleep\t/repo\t80\t24\tempty window id\t",
            "$1\twork\t@1\t0\teditor\t%\t3\t117\tsleep\t/repo\t80\t24\tempty pane id\t",
            "$1\twork\t@1\t0\teditor\t%6\t4\t104\tsleep\t/repo\t80\t24\ttab\tfragment\t",
            "$1\twork\t@1\t0\teditor\t%7\t5\t105\tbroken",
            "fragment\t/repo\tnewline\t",
            "$1\twork\t@1\t0\teditor\t%8\t6\t106\tsleep\t/repo\t80\t24\tself\t",
            "$1\twork\t@1\t0\teditor\t%9\t7\t0\t\t/repo\t80\t24\tmarked\t1",
            "$1\twork\t@1\t0\teditor\t%10\t8\t0\t\t/repo\t80\t24\tagenmux\t",
            "$1\twork\t@1\t0\teditor\t%11\t9\t107\tsleep\t/repo\t80\t24\tagenmux\t",
            "$2\tother\t@2\t3\tserver\t%12\t4\t108\tsleep\t/tmp\t90\t25\tlater valid\t",
        ]
        .join("\n");

        let parsed = parse_panes(&rows, Some("%8"));

        assert_eq!(
            parsed
                .iter()
                .map(|pane| {
                    (
                        pane.meta.pane.as_str(),
                        pane.pid,
                        pane.meta.command.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                ("%1", 101, "codex"),
                ("%2", 0, ""),
                ("%6", 104, "sleep"),
                ("%11", 107, "sleep"),
                ("%12", 108, "sleep"),
            ]
        );
        assert_eq!(parsed[0].meta.session_id, "$1");
        assert_eq!(parsed[0].meta.session_name, "work");
        assert_eq!(parsed[0].meta.window_id, "@1");
        assert_eq!(parsed[0].meta.window_index, 0);
        assert_eq!(parsed[0].meta.window_name, "editor");
        assert_eq!(parsed[0].meta.pane_index, 0);
        assert_eq!(parsed[0].meta.path, "/repo");
        assert_eq!(parsed[0].meta.pane_title, "repo");
        assert_eq!(parsed[0].meta.agent_index, None);
    }

    #[test]
    fn pane_title_keeps_embedded_tabs_as_the_final_field() {
        let parsed = parse_panes(
            "$1\twork\t@2\t3\twindow\t%1\t0\t42\tagent\t/work\t80\t24\ttitle\twith\ttabs\t",
            None,
        );
        assert_eq!(parsed[0].meta.pane_title, "title\twith\ttabs");
    }
}
