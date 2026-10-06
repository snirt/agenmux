//! Lua extension host (prototype, opt-in with AGENMUX_EXTENSIONS=1).
//!
//! Rust owns every mechanism: process spawning, budgets, layout, tmux. Plugins
//! only record state (handlers, keys, badges) or queue requests that the
//! sidebar validates and executes. No plugin call can block the render loop
//! longer than CALL_BUDGET or reach the filesystem except through `system`.
use mlua::{Function, HookTriggers, Lua, LuaOptions, StdLib, Table, Value, VmState};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

pub const ENV: &str = "AGENMUX_EXTENSIONS";
const CALL_BUDGET: Duration = Duration::from_millis(50);
const MEMORY_LIMIT: usize = 32 * 1024 * 1024;
const JOB_LIMIT: usize = 4;
const JOB_TIMEOUT: Duration = Duration::from_secs(2);
const JOB_TIMEOUT_MAX: Duration = Duration::from_secs(10);
const OUTPUT_CAP: usize = 64 * 1024;
const BADGE_CHARS: usize = 24;
const LIST_ITEMS: usize = 500;

pub fn enabled() -> bool {
    std::env::var(ENV).is_ok_and(|value| value == "1")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    PaneAdded,
    PaneRemoved,
    SelectionChanged,
    AgentStateChanged,
}

impl Event {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "PaneAdded" => Self::PaneAdded,
            "PaneRemoved" => Self::PaneRemoved,
            "SelectionChanged" => Self::SelectionChanged,
            "AgentStateChanged" => Self::AgentStateChanged,
            _ => return None,
        })
    }
}

/// Snapshot of one tmux pane as plugins see it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaneInfo {
    pub id: String,
    pub session: String,
    pub session_id: String,
    pub window: String,
    pub window_id: String,
    pub cwd: String,
    pub command: String,
    pub title: String,
    pub agent: Option<String>,
    pub state: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub text: String,
    pub hl: Highlight,
}

/// Semantic badge colors; the renderer maps them onto the theme palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Highlight {
    Muted,
    Accent,
    Ok,
    Warn,
    Error,
}

impl Highlight {
    fn parse(name: Option<&str>) -> Self {
        match name {
            Some("accent") => Self::Accent,
            Some("ok") => Self::Ok,
            Some("warn") => Self::Warn,
            Some("error") => Self::Error,
            _ => Self::Muted,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyDecl {
    pub sequence: String,
    pub label: String,
}

/// A picker requested by a plugin. The callback stays opaque to the sidebar.
pub struct ListView {
    pub title: String,
    pub items: Vec<String>,
    pub client: Option<String>,
    plugin: String,
    on_select: Option<Function>,
}

pub enum Request {
    OpenWindow {
        pane: String,
        command: String,
        args: Vec<String>,
        client: Option<String>,
    },
    SendKeys {
        pane: String,
        text: String,
    },
    ShowList(ListView),
    Notify {
        message: String,
        client: Option<String>,
    },
}

#[derive(Clone, Debug)]
struct JobResult {
    code: i32,
    stdout: String,
    stderr: String,
}

struct JobSpec {
    argv: Vec<String>,
    cwd: Option<String>,
    timeout: Duration,
}

struct Job {
    key: Option<String>,
    ttl: Option<Duration>,
    /// (callback, invoking client, plugin that asked)
    callbacks: Vec<(Function, Option<String>, String)>,
}

#[derive(Default)]
struct State {
    plugin: String,
    run_jobs: bool,
    client: Option<String>,
    handlers: Vec<(Event, Function, String)>,
    keys: Vec<(KeyDecl, Function, String)>,
    badges: BTreeMap<String, BTreeMap<String, Badge>>,
    requests: Vec<Request>,
    jobs: HashMap<u64, Job>,
    running: usize,
    queued: VecDeque<(u64, JobSpec)>,
    ready: VecDeque<(u64, JobResult)>,
    inflight: HashMap<String, u64>,
    cache: HashMap<String, (Instant, JobResult)>,
    next_job: u64,
    errors: Vec<String>,
    badges_changed: bool,
}

pub struct Extensions {
    lua: Lua,
    state: Rc<RefCell<State>>,
    deadline: Rc<Cell<Option<Instant>>>,
    tx: Sender<(u64, JobResult)>,
    rx: Receiver<(u64, JobResult)>,
    sources: Vec<(PathBuf, Option<SystemTime>)>,
    dirs: Vec<PathBuf>,
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| Path::new(&std::env::var_os("HOME").unwrap_or_default()).join(".config"))
}

fn lua_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// Bundled plugins, then the user's init.lua, then the user's plugin dir.
pub fn source_layout(plugin_dir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let user = config_home().join("agenmux");
    let dirs = vec![plugin_dir.join("runtime/plugin"), user.join("plugin")];
    let mut files = lua_files(&dirs[0]);
    files.push(user.join("init.lua"));
    files.extend(lua_files(&dirs[1]));
    (files, dirs)
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Key declarations for setup and sequence dispatch. Loaded once per process
/// in declare-only mode: no jobs start and no events fire.
pub fn declared_keys() -> &'static [KeyDecl] {
    static KEYS: std::sync::OnceLock<Vec<KeyDecl>> = std::sync::OnceLock::new();
    KEYS.get_or_init(|| {
        if !enabled() {
            return Vec::new();
        }
        let (files, dirs) = source_layout(&crate::plugin_dir());
        Extensions::load(&files, dirs, false).key_decls()
    })
}

impl Extensions {
    /// Build a fresh VM and run every source. A broken file is reported and
    /// skipped; the others still load.
    pub fn load(files: &[PathBuf], dirs: Vec<PathBuf>, run_jobs: bool) -> Self {
        let lua = Lua::new_with(
            StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8,
            LuaOptions::default(),
        )
        .expect("lua runtime");
        let _ = lua.set_memory_limit(MEMORY_LIMIT);
        let deadline = Rc::new(Cell::new(None::<Instant>));
        let hook_deadline = deadline.clone();
        let _ = lua.set_hook(
            HookTriggers {
                every_nth_instruction: Some(1000),
                ..HookTriggers::new()
            },
            move |_, _| match hook_deadline.get() {
                Some(deadline) if Instant::now() > deadline => {
                    Err(mlua::Error::runtime("time budget exceeded"))
                }
                _ => Ok(VmState::Continue),
            },
        );
        let (tx, rx) = channel();
        let state = Rc::new(RefCell::new(State {
            run_jobs,
            ..State::default()
        }));
        let mut ext = Self {
            lua,
            state,
            deadline,
            tx,
            rx,
            sources: Vec::new(),
            dirs,
        };
        if let Err(error) = ext.install_api() {
            ext.state.borrow_mut().errors.push(format!("api: {error}"));
        }
        for file in files {
            ext.sources.push((file.clone(), mtime(file)));
            let Ok(source) = std::fs::read_to_string(file) else {
                continue; // optional sources such as init.lua may be absent
            };
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            ext.state.borrow_mut().plugin = name.clone();
            ext.deadline.set(Some(Instant::now() + CALL_BUDGET));
            let result = ext.lua.load(source.as_str()).set_name(&name).exec();
            ext.deadline.set(None);
            if let Err(error) = result {
                ext.report(&name, &error);
            }
        }
        ext.state.borrow_mut().plugin.clear();
        ext
    }

    /// Any source added, removed, or edited since load.
    pub fn sources_changed(&self) -> bool {
        self.sources.iter().any(|(path, seen)| mtime(path) != *seen)
            || self.dirs.iter().any(|dir| {
                lua_files(dir)
                    .iter()
                    .any(|file| !self.sources.iter().any(|(path, _)| path == file))
            })
    }

    fn report(&self, plugin: &str, error: &mlua::Error) {
        let first = error.to_string();
        let first = first.lines().next().unwrap_or_default();
        self.state
            .borrow_mut()
            .errors
            .push(format!("{plugin}: {first}"));
    }

    pub fn take_errors(&self) -> Vec<String> {
        std::mem::take(&mut self.state.borrow_mut().errors)
    }

    pub fn key_decls(&self) -> Vec<KeyDecl> {
        self.state
            .borrow()
            .keys
            .iter()
            .map(|(decl, _, _)| decl.clone())
            .collect()
    }

    pub fn take_requests(&self) -> Vec<Request> {
        std::mem::take(&mut self.state.borrow_mut().requests)
    }

    pub fn badges(&self, pane: &str) -> Vec<Badge> {
        self.state
            .borrow()
            .badges
            .get(pane)
            .map(|badges| badges.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Drop badges of panes that no longer exist.
    pub fn retain_badges(&self, live: impl Fn(&str) -> bool) {
        let mut state = self.state.borrow_mut();
        let before = state.badges.len();
        state.badges.retain(|pane, _| live(pane));
        if state.badges.len() != before {
            state.badges_changed = true;
        }
    }

    pub fn take_badges_changed(&self) -> bool {
        std::mem::take(&mut self.state.borrow_mut().badges_changed)
    }

    pub fn jobs_pending(&self) -> bool {
        !self.state.borrow().jobs.is_empty()
    }

    /// Call `f` with the budget armed and the invoking client recorded, so
    /// requests and jobs created inside it target the same client.
    fn call(
        &self,
        plugin: &str,
        client: Option<&str>,
        f: &Function,
        args: impl mlua::IntoLuaMulti,
    ) {
        {
            let mut state = self.state.borrow_mut();
            state.client = client.map(str::to_string);
            state.plugin = plugin.to_string();
        }
        self.deadline.set(Some(Instant::now() + CALL_BUDGET));
        let result = f.call::<()>(args);
        self.deadline.set(None);
        {
            let mut state = self.state.borrow_mut();
            state.client = None;
            state.plugin.clear();
        }
        if let Err(error) = result {
            self.report(plugin, &error);
        }
    }

    pub fn fire(&self, event: Event, pane: &PaneInfo, old_state: Option<&str>) {
        let handlers: Vec<_> = self
            .state
            .borrow()
            .handlers
            .iter()
            .filter(|(e, _, _)| *e == event)
            .map(|(_, f, plugin)| (f.clone(), plugin.clone()))
            .collect();
        if handlers.is_empty() {
            return;
        }
        let Ok(ev) = self.event_table(pane, old_state) else {
            return;
        };
        for (f, plugin) in handlers {
            self.call(&plugin, None, &f, ev.clone());
        }
    }

    fn event_table(&self, pane: &PaneInfo, old_state: Option<&str>) -> mlua::Result<Table> {
        let ev = self.lua.create_table()?;
        ev.set("pane", self.pane_table(pane)?)?;
        if let Some(old) = old_state {
            ev.set("old", old)?;
        }
        if let Some(new) = &pane.state {
            ev.set("new", new.as_str())?;
        }
        Ok(ev)
    }

    fn pane_table(&self, pane: &PaneInfo) -> mlua::Result<Table> {
        let t = self.lua.create_table()?;
        t.set("id", pane.id.as_str())?;
        t.set("session", pane.session.as_str())?;
        t.set("session_id", pane.session_id.as_str())?;
        t.set("window", pane.window.as_str())?;
        t.set("window_id", pane.window_id.as_str())?;
        t.set("cwd", pane.cwd.as_str())?;
        t.set("command", pane.command.as_str())?;
        t.set("title", pane.title.as_str())?;
        t.set("agent", pane.agent.as_deref())?;
        t.set("state", pane.state.as_deref())?;
        Ok(t)
    }

    /// Run the handler bound to `sequence` for the selected pane.
    pub fn run_key(&self, sequence: &str, pane: Option<&PaneInfo>, client: Option<&str>) {
        let found = self
            .state
            .borrow()
            .keys
            .iter()
            .find(|(decl, _, _)| decl.sequence == sequence)
            .map(|(_, f, plugin)| (f.clone(), plugin.clone()));
        let Some((f, plugin)) = found else {
            return;
        };
        let arg = match pane.map(|pane| self.pane_table(pane)) {
            Some(Ok(table)) => Value::Table(table),
            _ => Value::Nil,
        };
        self.call(&plugin, client, &f, arg);
    }

    pub fn select(&self, view: ListView, index: usize) {
        let Some(item) = view.items.get(index) else {
            return;
        };
        if let Some(f) = &view.on_select {
            self.call(
                &view.plugin,
                view.client.as_deref(),
                f,
                (item.as_str(), index + 1),
            );
        }
    }

    /// Collect finished jobs, run their callbacks, and start queued jobs.
    /// Only results present on entry are delivered: a callback that asks for
    /// the same cached key again is answered on the next poll, not in a loop.
    pub fn poll(&self) {
        let cached: Vec<_> = self.state.borrow_mut().ready.drain(..).collect();
        let finished: Vec<_> = self.rx.try_iter().collect();
        let results = cached
            .into_iter()
            .map(|(id, result)| (id, result, false))
            .chain(finished.into_iter().map(|(id, result)| (id, result, true)));
        for (id, result, finished) in results {
            let job = {
                let mut state = self.state.borrow_mut();
                if finished {
                    state.running = state.running.saturating_sub(1);
                }
                let Some(job) = state.jobs.remove(&id) else {
                    continue;
                };
                if let Some(key) = &job.key {
                    state.inflight.remove(key);
                    if let Some(ttl) = job.ttl {
                        state
                            .cache
                            .insert(key.clone(), (Instant::now() + ttl, result.clone()));
                    }
                }
                job
            };
            for (f, client, plugin) in job.callbacks {
                self.deliver(&f, client.as_deref(), &plugin, &result);
            }
        }
        self.start_queued();
    }

    fn deliver(&self, f: &Function, client: Option<&str>, plugin: &str, result: &JobResult) {
        let Ok(t) = self.lua.create_table() else {
            return;
        };
        let _ = t.set("code", result.code);
        let _ = t.set("stdout", result.stdout.as_str());
        let _ = t.set("stderr", result.stderr.as_str());
        self.call(plugin, client, f, t);
    }

    fn start_queued(&self) {
        loop {
            let (id, spec) = {
                let mut state = self.state.borrow_mut();
                if state.running >= JOB_LIMIT {
                    return;
                }
                let Some(next) = state.queued.pop_front() else {
                    return;
                };
                state.running += 1;
                next
            };
            let tx = self.tx.clone();
            std::thread::spawn(move || {
                let _ = tx.send((id, run_job(&spec)));
            });
        }
    }

    fn install_api(&self) -> mlua::Result<()> {
        let lua = &self.lua;
        let api = lua.create_table()?;

        let state = self.state.clone();
        api.set(
            "on",
            lua.create_function(move |_, (name, f): (String, Function)| {
                let event = Event::parse(&name)
                    .ok_or_else(|| mlua::Error::runtime(format!("unknown event {name}")))?;
                let mut state = state.borrow_mut();
                let plugin = state.plugin.clone();
                state.handlers.push((event, f, plugin));
                Ok(())
            })?,
        )?;

        let keymap = lua.create_table()?;
        let state = self.state.clone();
        keymap.set(
            "set",
            lua.create_function(
                move |_, (sequence, f, opts): (String, Function, Option<Table>)| {
                    let valid = (1..=2).contains(&sequence.len())
                        && sequence.bytes().all(|b| b.is_ascii_graphic());
                    if !valid {
                        return Err(mlua::Error::runtime(format!(
                            "key sequence {sequence:?} must be 1-2 printable characters"
                        )));
                    }
                    let label = opts
                        .and_then(|opts| opts.get::<Option<String>>("desc").ok().flatten())
                        .unwrap_or_else(|| sequence.clone());
                    let mut state = state.borrow_mut();
                    let plugin = state.plugin.clone();
                    state.keys.retain(|(decl, _, _)| decl.sequence != sequence);
                    state.keys.push((KeyDecl { sequence, label }, f, plugin));
                    Ok(())
                },
            )?,
        )?;
        api.set("keymap", keymap)?;

        let state = self.state.clone();
        api.set(
            "system",
            lua.create_function(
                move |_, (argv, opts, f): (Vec<String>, Option<Table>, Function)| {
                    if argv.is_empty() {
                        return Err(mlua::Error::runtime("system: empty argv"));
                    }
                    let opt = |name: &str| -> Option<Value> {
                        opts.as_ref().and_then(|opts| opts.get::<Value>(name).ok())
                    };
                    let string = |name| match opt(name) {
                        Some(Value::String(s)) => Some(s.to_string_lossy()),
                        _ => None,
                    };
                    let millis = |name| match opt(name) {
                        Some(Value::Integer(n)) if n > 0 => Some(Duration::from_millis(n as u64)),
                        _ => None,
                    };
                    let key = string("key");
                    let ttl = millis("ttl_ms");
                    let spec = JobSpec {
                        argv,
                        cwd: string("cwd"),
                        timeout: millis("timeout_ms")
                            .unwrap_or(JOB_TIMEOUT)
                            .min(JOB_TIMEOUT_MAX),
                    };
                    let mut state = state.borrow_mut();
                    if !state.run_jobs {
                        return Ok(());
                    }
                    let client = state.client.clone();
                    let plugin = state.plugin.clone();
                    let id = state.next_job;
                    state.next_job += 1;
                    if let Some(key) = &key {
                        let now = Instant::now();
                        state.cache.retain(|_, (expires, _)| *expires > now);
                        if let Some((_, cached)) = state.cache.get(key).cloned() {
                            // Delivered on the next poll, never re-entrantly.
                            state.jobs.insert(
                                id,
                                Job {
                                    key: None,
                                    ttl: None,
                                    callbacks: vec![(f, client, plugin)],
                                },
                            );
                            state.ready.push_back((id, cached));
                            return Ok(());
                        }
                        if let Some(job) = state
                            .inflight
                            .get(key)
                            .copied()
                            .and_then(|running| state.jobs.get_mut(&running))
                        {
                            job.callbacks.push((f, client, plugin));
                            return Ok(());
                        }
                        state.inflight.insert(key.clone(), id);
                    }
                    state.jobs.insert(
                        id,
                        Job {
                            key,
                            ttl,
                            callbacks: vec![(f, client, plugin)],
                        },
                    );
                    state.queued.push_back((id, spec));
                    Ok(())
                },
            )?,
        )?;

        let ui = lua.create_table()?;
        let state = self.state.clone();
        ui.set(
            "badge",
            lua.create_function(
                move |_, (pane, ns, text, hl): (String, String, Option<String>, Option<String>)| {
                    let mut state = state.borrow_mut();
                    let badges = state.badges.entry(pane).or_default();
                    let next = text
                        .map(|text| {
                            text.chars()
                                .filter(|c| !c.is_control())
                                .take(BADGE_CHARS)
                                .collect::<String>()
                        })
                        .filter(|text| !text.is_empty())
                        .map(|text| Badge {
                            text,
                            hl: Highlight::parse(hl.as_deref()),
                        });
                    let changed = match next {
                        Some(badge) => badges.insert(ns, badge.clone()) != Some(badge),
                        None => badges.remove(&ns).is_some(),
                    };
                    if changed {
                        state.badges_changed = true;
                    }
                    Ok(())
                },
            )?,
        )?;
        let state = self.state.clone();
        ui.set(
            "list",
            lua.create_function(move |_, spec: Table| {
                let title: String = spec.get::<Option<String>>("title")?.unwrap_or_default();
                let items: Vec<String> = spec
                    .get::<Option<Vec<String>>>("items")?
                    .unwrap_or_default()
                    .into_iter()
                    .take(LIST_ITEMS)
                    .map(|item| item.chars().filter(|c| !c.is_control()).collect())
                    .collect();
                let on_select: Option<Function> = spec.get("on_select")?;
                let mut state = state.borrow_mut();
                let client = state.client.clone();
                let plugin = state.plugin.clone();
                state.requests.push(Request::ShowList(ListView {
                    title,
                    items,
                    client,
                    plugin,
                    on_select,
                }));
                Ok(())
            })?,
        )?;
        api.set("ui", ui)?;

        let calls = lua.create_table()?;
        let state = self.state.clone();
        calls.set(
            "open_window",
            lua.create_function(move |_, spec: Table| {
                let pane: String = spec.get("pane")?;
                let mut cmd: Vec<String> = spec.get("cmd")?;
                if cmd.is_empty() {
                    return Err(mlua::Error::runtime("open_window: empty cmd"));
                }
                let command = cmd.remove(0);
                let mut state = state.borrow_mut();
                let client = state.client.clone();
                state.requests.push(Request::OpenWindow {
                    pane,
                    command,
                    args: cmd,
                    client,
                });
                Ok(())
            })?,
        )?;
        let state = self.state.clone();
        calls.set(
            "send_keys",
            lua.create_function(move |_, (pane, text): (String, String)| {
                state
                    .borrow_mut()
                    .requests
                    .push(Request::SendKeys { pane, text });
                Ok(())
            })?,
        )?;
        api.set("api", calls)?;

        let state = self.state.clone();
        api.set(
            "notify",
            lua.create_function(move |_, message: String| {
                let mut state = state.borrow_mut();
                let client = state.client.clone();
                state.requests.push(Request::Notify { message, client });
                Ok(())
            })?,
        )?;
        let state = self.state.clone();
        api.set(
            "log",
            lua.create_function(move |_, message: String| {
                let plugin = state.borrow().plugin.clone();
                eprintln!("agenmux ext: {plugin}: {message}");
                Ok(())
            })?,
        )?;
        lua.globals().set("agenmux", api)?;
        Ok(())
    }
}

fn read_capped(mut reader: impl Read) -> String {
    let mut out = Vec::new();
    let _ = reader
        .by_ref()
        .take(OUTPUT_CAP as u64)
        .read_to_end(&mut out);
    let _ = std::io::copy(&mut reader, &mut std::io::sink());
    String::from_utf8_lossy(&out).into_owned()
}

fn run_job(spec: &JobSpec) -> JobResult {
    use std::process::{Command, Stdio};
    let mut command = Command::new(&spec.argv[0]);
    command
        .args(&spec.argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    let failed = |stderr: String| JobResult {
        code: -1,
        stdout: String::new(),
        stderr,
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failed(error.to_string()),
    };
    let stdout = child
        .stdout
        .take()
        .map(|out| std::thread::spawn(move || read_capped(out)));
    let stderr = child
        .stderr
        .take()
        .map(|err| std::thread::spawn(move || read_capped(err)));
    let deadline = Instant::now() + spec.timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break None,
        }
    };
    let collect = |handle: Option<std::thread::JoinHandle<String>>| {
        handle.and_then(|h| h.join().ok()).unwrap_or_default()
    };
    let stdout = collect(stdout);
    let stderr = collect(stderr);
    match status {
        Some(status) => JobResult {
            code: status.code().unwrap_or(-1),
            stdout,
            stderr,
        },
        None => failed("timed out".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(source: &str, run_jobs: bool) -> (Extensions, tempdir::Dir) {
        let dir = tempdir::Dir::new();
        let file = dir.path.join("test.lua");
        std::fs::write(&file, source).unwrap();
        (Extensions::load(&[file], vec![], run_jobs), dir)
    }

    mod tempdir {
        pub struct Dir {
            pub path: std::path::PathBuf,
        }
        impl Dir {
            pub fn new() -> Self {
                use std::sync::atomic::{AtomicUsize, Ordering};
                static N: AtomicUsize = AtomicUsize::new(0);
                let path = std::env::temp_dir().join(format!(
                    "agenmux-ext-test-{}-{}",
                    std::process::id(),
                    N.fetch_add(1, Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self { path }
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
    }

    fn pane(id: &str) -> PaneInfo {
        PaneInfo {
            id: id.into(),
            cwd: "/".into(),
            ..PaneInfo::default()
        }
    }

    fn wait_jobs(ext: &Extensions) {
        let until = Instant::now() + Duration::from_secs(5);
        while ext.jobs_pending() && Instant::now() < until {
            ext.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn broken_plugins_are_reported_without_stopping_others() {
        let dir = tempdir::Dir::new();
        let bad = dir.path.join("a.lua");
        let good = dir.path.join("b.lua");
        std::fs::write(&bad, "error('boom')").unwrap();
        std::fs::write(
            &good,
            "agenmux.keymap.set('gx', function() end, {desc='x'})",
        )
        .unwrap();
        let ext = Extensions::load(&[bad, good], vec![], false);
        let errors = ext.take_errors();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("a.lua:") && errors[0].contains("boom"));
        assert_eq!(
            ext.key_decls(),
            vec![KeyDecl {
                sequence: "gx".into(),
                label: "x".into()
            }]
        );
    }

    #[test]
    fn sandbox_hides_io_and_os() {
        let (ext, _dir) = load("assert(io == nil and os == nil and require == nil)", false);
        assert!(ext.take_errors().is_empty());
    }

    #[test]
    fn runaway_handlers_hit_the_budget() {
        let (ext, _dir) = load(
            "agenmux.on('PaneAdded', function() while true do end end)",
            false,
        );
        let started = Instant::now();
        ext.fire(Event::PaneAdded, &pane("%1"), None);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(ext.take_errors()[0].contains("time budget exceeded"));
    }

    #[test]
    fn invalid_keys_and_events_are_rejected() {
        let (ext, _dir) = load("agenmux.keymap.set('abc', function() end)", false);
        assert!(ext.take_errors()[0].contains("1-2 printable"));
        let (ext, _dir) = load("agenmux.on('Nope', function() end)", false);
        assert!(ext.take_errors()[0].contains("unknown event"));
    }

    #[test]
    fn events_set_badges_and_requests_carry_the_client() {
        let (ext, _dir) = load(
            r#"
            agenmux.on('AgentStateChanged', function(ev)
              agenmux.ui.badge(ev.pane.id, 'state', ev.old .. '>' .. ev.new, 'warn')
            end)
            agenmux.keymap.set('gz', function(pane)
              agenmux.notify('hi ' .. pane.id)
              agenmux.ui.list{ title = 't', items = {'a', 'b'}, on_select = function(item, i)
                agenmux.api.open_window{ pane = pane.id, cmd = {'echo', item, tostring(i)} }
              end }
            end)
            "#,
            false,
        );
        let mut p = pane("%3");
        p.state = Some("idle".into());
        ext.fire(Event::AgentStateChanged, &p, Some("working"));
        assert_eq!(
            ext.badges("%3"),
            vec![Badge {
                text: "working>idle".into(),
                hl: Highlight::Warn
            }]
        );
        assert!(ext.take_badges_changed());
        ext.run_key("gz", Some(&p), Some("client-1"));
        let mut requests = ext.take_requests();
        assert_eq!(requests.len(), 2);
        assert!(matches!(&requests[0], Request::Notify { message, client }
            if message == "hi %3" && client.as_deref() == Some("client-1")));
        let Request::ShowList(view) = requests.remove(1) else {
            panic!("expected list");
        };
        assert_eq!(view.items, vec!["a", "b"]);
        ext.select(view, 1);
        assert!(
            matches!(&ext.take_requests()[..], [Request::OpenWindow { pane, command, args, client }]
            if pane == "%3" && command == "echo" && args == &["b", "2"] && client.as_deref() == Some("client-1"))
        );
        assert!(ext.take_errors().is_empty());
    }

    #[test]
    fn jobs_share_a_key_and_cache_results() {
        let (ext, _dir) = load(
            r#"
            calls = 0
            local function run()
              agenmux.system({'sh', '-c', 'echo x >> runs; cat runs | wc -l'}, { cwd = CWD, key = 'k', ttl_ms = 60000 },
                function(r) calls = calls + 1; last = r.stdout end)
            end
            agenmux.keymap.set('gr', run)
            "#,
            true,
        );
        let dir = tempdir::Dir::new();
        ext.lua
            .globals()
            .set("CWD", dir.path.to_string_lossy().into_owned())
            .unwrap();
        ext.run_key("gr", None, None);
        ext.run_key("gr", None, None);
        wait_jobs(&ext);
        ext.run_key("gr", None, None);
        wait_jobs(&ext);
        let calls: i64 = ext.lua.globals().get("calls").unwrap();
        let last: String = ext.lua.globals().get("last").unwrap();
        assert_eq!(calls, 3);
        assert_eq!(last.trim(), "1", "one process served all three callbacks");
        assert!(ext.take_errors().is_empty());
    }

    #[test]
    fn a_callback_rerequesting_a_cached_key_waits_for_the_next_poll() {
        let (ext, _dir) = load(
            r#"
            calls = 0
            local function run()
              agenmux.system({'true'}, { key = 'k', ttl_ms = 60000 }, function()
                calls = calls + 1
                run()
              end)
            end
            agenmux.keymap.set('gr', run)
            "#,
            true,
        );
        ext.run_key("gr", None, None);
        let until = Instant::now() + Duration::from_secs(5);
        while ext.lua.globals().get::<i64>("calls").unwrap() == 0 && Instant::now() < until {
            ext.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        ext.poll();
        ext.poll();
        assert_eq!(ext.lua.globals().get::<i64>("calls").unwrap(), 3);
    }

    #[test]
    fn jobs_time_out() {
        let (ext, _dir) = load(
            r#"agenmux.keymap.set('gt', function()
              agenmux.system({'sleep', '5'}, { timeout_ms = 50 }, function(r) code = r.code; err = r.stderr end)
            end)"#,
            true,
        );
        ext.run_key("gt", None, None);
        wait_jobs(&ext);
        let code: i64 = ext.lua.globals().get("code").unwrap();
        let err: String = ext.lua.globals().get("err").unwrap();
        assert_eq!((code, err.as_str()), (-1, "timed out"));
    }

    #[test]
    fn declare_mode_never_runs_jobs() {
        let (ext, _dir) = load("agenmux.system({'true'}, nil, function() end)", false);
        assert!(!ext.jobs_pending());
        assert!(ext.take_errors().is_empty());
    }
}
