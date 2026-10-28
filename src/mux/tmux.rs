use super::{Ids, Kind, Multiplexer, Owner, SidebarPane};
use crate::app_config::AppConfig;
use crate::scan::{parse_panes, ParsedPane, LIST_FMT};
use crate::snapshot::{Closed, Live, Snapshot};
use crate::tmux::{command, command_status, PendingChanges, Tmux, TmuxError};
use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

impl Multiplexer for Tmux {
    fn panes(&mut self, self_pane: Option<&str>) -> Result<Vec<ParsedPane>, TmuxError> {
        self.sync()?;
        Ok(parse_panes(&self.run(LIST_FMT)?, self_pane))
    }

    fn capture(&mut self, pane: &str) -> Result<String, TmuxError> {
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
                TmuxError::Error(message)
            })
    }

    fn client_focus(&mut self) -> Option<crate::focus::ClientFocus> {
        // scan() only syncs at its START; a hook's run-shell block landing
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
        Some(crate::focus::parse_clients(&out, focus_events))
    }

    fn notify_fd(&self) -> libc::c_int {
        self.fd()
    }
    fn notify_buffered(&self) -> bool {
        self.buffered()
    }
    fn drain_notifications(&mut self) -> Result<bool, TmuxError> {
        Tmux::drain_notifications(self)
    }
    fn pending_changes(&self) -> Option<&PendingChanges> {
        Some(Tmux::pending_changes(self))
    }
    fn take_pending_changes(&mut self) -> PendingChanges {
        Tmux::take_pending_changes(self)
    }
    fn defer_output(&mut self, panes: std::collections::HashSet<String>) {
        Tmux::defer_output(self, panes)
    }
    fn attached_session(&self) -> Option<&str> {
        Tmux::attached_session(self)
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

    fn create_window(&mut self, session: &str, name: &str, cwd: &str) -> Result<String, TmuxError> {
        let session = format!("{session}:");
        let mut args = vec!["new-window", "-d", "-P", "-F", "#{pane_id}"];
        if !name.is_empty() {
            args.extend(["-n", name]);
        }
        args.extend(["-t", session.as_str(), "-c", cwd]);
        command(&args).map(|pane| pane.trim().to_string())
    }

    fn create_session(&mut self, name: &str, cwd: &str) -> Result<String, TmuxError> {
        let mut args = vec!["new-session", "-d", "-P", "-F", "#{pane_id}"];
        if !name.is_empty() {
            args.extend(["-s", name]);
        }
        args.extend(["-c", cwd]);
        command(&args).map(|pane| pane.trim().to_string())
    }

    fn launch(&mut self, session: &str, cwd: &str, argv: &[&str]) -> Result<String, TmuxError> {
        let session = format!("{session}:");
        let mut args = vec!["new-window", "-d", "-P", "-F", "#{pane_id}", "-t", &session];
        args.extend(["-c", cwd]);
        args.extend(argv);
        command(&args).map(|pane| pane.trim().to_string())
    }

    fn name(&mut self, kind: Kind, ids: Ids) -> Result<String, TmuxError> {
        let (id, format) = match kind {
            Kind::Pane => (ids.pane, "#{pane_title}"),
            Kind::Window => (ids.window, "#{window_name}"),
            Kind::Session => (ids.session, "#{session_name}"),
        };
        command(&["display-message", "-p", "-t", id, format])
    }

    fn rename(&mut self, kind: Kind, ids: Ids, name: &str) -> Result<(), TmuxError> {
        match kind {
            Kind::Pane => command(&["select-pane", "-t", ids.pane, "-T", name]),
            Kind::Window => {
                let _ = command_status(&[
                    "set-window-option",
                    "-t",
                    ids.window,
                    "automatic-rename",
                    "off",
                ]);
                command(&["rename-window", "-t", ids.window, name])
            }
            Kind::Session => command(&["rename-session", "-t", ids.session, name]),
        }
        .map(drop)
    }

    fn delete(&mut self, kind: Kind, ids: Ids) -> Result<(), TmuxError> {
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
                    return Err(TmuxError::Error(
                        "cannot delete the last pane in a tmux session; delete the session explicitly"
                            .into(),
                    ));
                }
                command(&["kill-pane", "-t", ids.pane])
            }
            Kind::Window => {
                let windows = command(&["list-windows", "-t", ids.session, "-F", "#{window_id}"])?;
                if windows.lines().count() <= 1 {
                    return Err(TmuxError::Error(
                        "cannot delete the last window in a tmux session; delete the session explicitly"
                            .into(),
                    ));
                }
                command(&["kill-window", "-t", ids.window])
            }
            Kind::Session => {
                let sessions = command(&["list-sessions", "-F", "#{session_id}"])?;
                let fallback = sessions
                    .lines()
                    .find(|session| *session != ids.session)
                    .ok_or_else(|| {
                        TmuxError::Error(
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
                command(&["kill-session", "-t", ids.session])
            }
        }
        .map(drop)
    }

    fn pane_cwd(&mut self, ids: Ids) -> Result<String, TmuxError> {
        let response = command(&[
            "display-message",
            "-p",
            "-t",
            ids.pane,
            "#{pane_id}|#{window_id}|#{session_id}|#{pane_current_path}",
        ])?;
        parse_pane_cwd(ids, &response)
    }

    fn session_cwd(&mut self, session: &str) -> Result<String, TmuxError> {
        let cwd = command(&["display-message", "-p", "-t", session, "#{session_path}"])?;
        let cwd = cwd.trim_end_matches(['\n', '\r']);
        if cwd.is_empty() {
            return Err(TmuxError::Error(
                "target session has no working directory".into(),
            ));
        }
        Ok(cwd.to_string())
    }

    fn show(&mut self, client: &str, pane: &str, stay: bool) -> Result<(), TmuxError> {
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
            return Err(TmuxError::Error(
                "created target has no tmux location".into(),
            ));
        };
        if actual_pane != pane {
            return Err(TmuxError::Error(
                "created target changed before client switch".into(),
            ));
        }
        command_status(&["switch-client", "-c", client, "-t", session])?;
        command_status(&["select-window", "-t", window])?;
        if stay {
            self.focus_sidebar(client, Some(window));
            return Ok(());
        }
        command_status(&["select-pane", "-t", pane])?;
        command_status(&["switch-client", "-c", client, "-T", "root"])
    }

    /// A window this sidebar just created gets its pane from the background
    /// pane-add hook, so wait briefly for it to appear.
    fn focus_sidebar(&mut self, client: &str, expected_window: Option<&str>) {
        let deadline = Instant::now() + Duration::from_millis(1500);
        let location = format!("#{{window_id}}|#{{?{},1,0}}", crate::panes::IS_SIDEBAR);
        let client_filter = format!("#{{==:#{{client_name}},{client}}}");
        let mut reached_expected = expected_window.is_none();
        while let Ok(location) = command(&["list-clients", "-f", &client_filter, "-F", &location]) {
            let (window, on_sidebar) = location.trim().split_once('|').unwrap_or(("", "0"));
            // Creation names the exact destination. Wait for tmux to publish the
            // completed switch; only treat a later move away as user intent.
            if let Some(expected) = expected_window {
                if window == expected {
                    reached_expected = true;
                } else if reached_expected || Instant::now() >= deadline {
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

    fn message(&mut self, client: &str, text: &str) -> bool {
        // display-message expands formats and strftime, which would eat `%12`
        let text = format!("agenmux: {text}")
            .replace('%', "%%")
            .replace('#', "##");
        let _ = command_status(&["display-message", "-c", client, "-d", "3000", &text]);
        true
    }

    fn copy(&mut self, client: Option<&str>, text: &str) -> Result<Option<String>, TmuxError> {
        let mut args = vec!["set-buffer", "-w"];
        if let Some(client) = client {
            args.extend(["-t", client]);
        }
        args.extend(["--", text]);
        command_status(&args).map(|()| None)
    }

    fn reference(&self, id: &str) -> String {
        reference(id)
    }

    fn snapshot_home(&mut self) -> Option<(String, std::path::PathBuf)> {
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
        Some((server, crate::snapshot::path(fields.next()?)?))
    }

    fn windows(&mut self) -> Result<String, TmuxError> {
        self.run(crate::snapshot::WINDOWS_FMT)
    }

    fn open_panes(&mut self) -> String {
        self.run("list-panes -a -F '#{pane_id}'")
            .unwrap_or_default()
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

    // Plain tmux forks, like jump: their hooks' output must not reach the
    // control pipe.
    fn replay(&mut self, snap: &Snapshot, live: &Live, resume: &dyn Fn(&str) -> Option<String>) {
        crate::snapshot::replay(snap, &live.windows, resume, &mut run_forked);
    }

    fn replay_pane(
        &mut self,
        entry: &Closed,
        live: &Live,
        resume: &dyn Fn(&str) -> Option<String>,
    ) {
        crate::snapshot::replay_pane(entry, live, resume, &mut run_forked);
    }

    fn reload_token(&mut self) -> Option<String> {
        self.run(&format!(
            "show-option -gqv {}",
            crate::app_config::RELOAD_OPTION
        ))
        .ok()
        .map(|value| value.trim_end_matches('\n').to_owned())
        .filter(|value| !value.is_empty())
    }

    fn publish_reload(&mut self, token: &str) -> Result<(), TmuxError> {
        self.run(&format!(
            "set-option -g {} {token}",
            crate::app_config::RELOAD_OPTION
        ))
        .map(drop)
    }

    fn options(&mut self) -> Result<BTreeMap<String, String>, crate::app_config::ConfigError> {
        crate::app_config::read_options(|cmd| self.run(cmd))
    }

    fn sidebar_panes(&mut self) -> Vec<SidebarPane> {
        self.run("list-panes -a -f '#{==:#{pane_title},agenmux}' -F '#{pane_id}|#{window_id}|#{pane_width}|#{pane_height}|#{window_width} #{window_height}|#{window_panes}|#{window_active}|#{session_id}'")
            .unwrap_or_default()
            .lines()
            .filter_map(parse_sidebar_pane)
            .collect()
    }

    fn visible_windows(&mut self) -> HashSet<String> {
        self.run("list-clients -f '#{?#{m:*control-mode*,#{client_flags}},0,1}' -F '#{window_id}'")
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    // Forked tmux: resizes and kills fire hooks whose run-shell output
    // would desync the control pipe.
    fn resize_panes(&mut self, widths: &[(String, usize)]) -> bool {
        forked_batch(widths.iter().map(|(pane, width)| {
            ["resize-pane", "-t", pane, "-x", &width.to_string()]
                .map(String::from)
                .to_vec()
        }))
    }

    fn close_panes(&mut self, panes: &[String]) {
        forked_batch(
            panes
                .iter()
                .map(|pane| ["kill-pane", "-t", pane].map(String::from).to_vec()),
        );
    }

    fn dock_sidebars(&mut self, plugin_dir: &std::path::Path, settings: &AppConfig) {
        crate::panes::dock(settings.sidebar_position, settings.sidebar_width);
        // The default jump and fold keys follow the side. Reload and the
        // settings overlay reinstall already; a bare tmux option does not.
        let installed =
            command(&["show-option", "-gqv", "@agenmux-nav-version"]).unwrap_or_default();
        if installed.trim_end() != crate::setup::nav_version(settings)
            && crate::setup::run_config(plugin_dir, settings) == 0
        {
            crate::setup::reclaim_client_tables();
        }
    }

    fn close_sidebars(&mut self) {
        let _ = crate::panes::teardown_panes();
    }

    fn save_width(&mut self, width: usize) -> bool {
        self.run(&format!("set-option -g @agenmux-width {width}"))
            .is_ok()
    }

    fn connection(&mut self) -> Option<String> {
        // The command runs on this control connection, so it identifies the
        // exact client even when tmux forks and reports a different OS PID.
        self.run("display-message -p '#{client_name}'")
            .ok()
            .map(|client| client.trim().to_string())
            .filter(|client| !client.is_empty())
    }

    fn owner(&mut self) -> Option<Owner> {
        parse_owner(&self.run(OWNER_QUERY).ok()?)
    }

    fn publish_owner(&mut self, client: &str, runtime: &std::path::Path) -> Result<(), TmuxError> {
        // Unescaped: show-option hands the value back unescaped too.
        let client = client.replace('\'', "\\'");
        self.run(&format!("set-option -g @agenmux-control-client '{client}'"))?;
        let runtime = runtime.to_string_lossy().replace('\'', "\\'");
        self.run(&format!("set-option -g @agenmux-runtime-dir '{runtime}'"))
            .map(drop)
    }

    fn clear_owner(&mut self, generation: Option<&str>) {
        let _ = crate::panes::clear_ownership(generation);
    }

    fn set_key_sink(&mut self, fd: libc::c_int) {
        Tmux::set_key_sink(self, fd);
    }

    fn follow_session(&mut self, session: &str) -> bool {
        self.run(&format!("switch-client -t '{session}'")).is_ok()
    }

    fn text_input(&mut self, client: &str, on: bool) {
        let table = if on {
            crate::setup::SEARCH_TABLE
        } else {
            crate::setup::NORMAL_TABLE
        };
        let _ = command_status(&["switch-client", "-c", client, "-T", table]);
    }

    fn run(&mut self, cmd: &str) -> Result<String, TmuxError> {
        Tmux::run(self, cmd)
    }
    fn sync(&mut self) -> Result<(), TmuxError> {
        Tmux::sync(self)
    }

    fn as_tmux(&mut self) -> Option<&mut Tmux> {
        Some(self)
    }
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

fn parse_sidebar_pane(line: &str) -> Option<SidebarPane> {
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
        settled: true,
        width_drag: None, // tmux reports exact window cell sizes
    })
}

/// One forked tmux running `commands` in order; true when there were none.
fn forked_batch(commands: impl Iterator<Item = Vec<String>>) -> bool {
    let mut argv: Vec<String> = Vec::new();
    for cmd in commands {
        if !argv.is_empty() {
            argv.push(";".into());
        }
        argv.extend(cmd);
    }
    if argv.is_empty() {
        return true;
    }
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    command_status(&args).is_ok()
}

fn run_forked(args: &[String]) -> Option<String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    command(&args).ok().map(|out| out.trim().to_string())
}

/// `$n`/`@n`/`%n` as `tmux session $3` and so on.
fn reference(id: &str) -> String {
    let kind = match id.as_bytes().first() {
        Some(b'$') => "session",
        Some(b'@') => "window",
        _ => "pane",
    };
    format!("tmux {kind} {id}")
}

fn parse_pane_cwd(ids: Ids, response: &str) -> Result<String, TmuxError> {
    let response = response.strip_suffix('\n').unwrap_or(response);
    let mut fields = response.splitn(4, '|');
    let (Some(pane), Some(window), Some(session), Some(cwd)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(TmuxError::Error(
            "selected pane changed before launcher start".into(),
        ));
    };
    if pane != ids.pane || window != ids.window || session != ids.session {
        return Err(TmuxError::Error(
            "selected pane changed before launcher start".into(),
        ));
    }
    if cwd.is_empty() {
        return Err(TmuxError::Error(
            "selected pane has no working directory".into(),
        ));
    }
    Ok(cwd.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_tagged_owner_reply_is_read() {
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

    #[test]
    fn reference_names_the_selected_record_by_tmux_id() {
        assert_eq!(reference("$3"), "tmux session $3");
        assert_eq!(reference("@7"), "tmux window @7");
        assert_eq!(reference("%12"), "tmux pane %12");
    }

    #[test]
    fn launcher_revalidation_rejects_stale_identity_without_using_another_pane_cwd() {
        let ids = Ids {
            pane: "%4",
            window: "@2",
            session: "$1",
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
}
