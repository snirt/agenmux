//! Layered resolution of file, tmux options, and CLI, plus live reload.

use super::*;
use std::collections::HashSet;

/// Fully resolved behavior. Theme/input application belongs to later tasks.
#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub sequence_timeout_ms: u64,
    pub tmux_management_enabled: bool,
    pub tmux_management_confirm_delete: bool,
    pub tmux_management_resume_agents: bool,
    pub tmux_management_undo_history: usize,
    pub mode: DisplayMode,
    pub show_all_panes: bool,
    pub show_frame: bool,
    pub agent_label: AgentLabel,
    pub sidebar_width: u16,
    pub popup_width: u16,
    pub popup_height: PopupHeight,
    pub notifications: bool,
    pub hide_windows: Option<String>,
    pub theme: ThemeConfig,
    pub normal: BTreeMap<Action, Vec<KeyChord>>,
    pub search: BTreeMap<Action, Vec<KeyChord>>,
    pub quick_launchers: Vec<QuickLauncher>,
    /// Field name to the layer that decided it, named precisely enough to
    /// act on: a winning tmux option prints as `tmux @agenmux-width`.
    pub sources: BTreeMap<String, String>,
}

const OPTIONS: &[&str] = &[
    "display",
    "width",
    "height",
    "notifications",
    "hide-windows",
];

fn compatibility(suffix: &str, value: &str) -> Result<FileConfig, ConfigError> {
    let mut file = FileConfig::default();
    let bad = || {
        ConfigError::invalid(
            suffix,
            "invalid tmux option value (see application configuration documentation)",
        )
    };
    let cells = || {
        value
            .parse::<u16>()
            .ok()
            .filter(|n| (1..=10000).contains(n))
            .ok_or_else(bad)
    };
    match suffix {
        "display" => {
            file.display.get_or_insert_default().mode = Some(match value {
                "" | "split" => DisplayMode::Split,
                "popup" | "float" => DisplayMode::Popup,
                _ => return Err(bad()),
            })
        }
        "width" => {
            let d = file.display.get_or_insert_default();
            // Empty resets the two separate built-ins, not the file or legacy.
            d.sidebar_width = Some(if value.is_empty() { 30 } else { cells()? });
            d.popup_width = Some(if value.is_empty() { 40 } else { cells()? });
        }
        "height" => {
            file.display.get_or_insert_default().popup_height = Some(match value {
                "" | "auto" => PopupHeight::Auto(AutoHeight::Auto),
                _ => PopupHeight::Cells(cells()?),
            })
        }
        "notifications" => {
            file.behavior.get_or_insert_default().notifications =
                Some(match value.trim().to_ascii_lowercase().as_str() {
                    "" | "on" | "true" | "1" | "yes" => true,
                    "off" | "false" | "0" | "no" => false,
                    _ => return Err(bad()),
                })
        }
        "hide-windows" => {
            file.behavior.get_or_insert_default().hide_windows = Some(value.to_owned())
        }
        _ => unreachable!(),
    }
    // Validate supplied compatibility values even when shadowed.
    if let Some(b) = &file.behavior {
        if b.hide_windows
            .as_ref()
            .is_some_and(|s| s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(bad());
        }
    }
    Ok(file)
}

pub fn resolve(
    file: &FileConfig,
    options: &BTreeMap<String, String>,
) -> Result<AppConfig, ConfigError> {
    resolve_cli(file, options, None)
}

pub fn resolve_cli(
    file: &FileConfig,
    options: &BTreeMap<String, String>,
    cli: Option<&str>,
) -> Result<AppConfig, ConfigError> {
    validate(file)?;
    let (quick_launchers, mut sources) = resolve_quick_launchers(file.quick_launchers.as_ref())?;
    sources.extend(
        [
            ("display.mode", "default"),
            ("display.show_all_panes", "default"),
            ("display.show_frame", "default"),
            ("display.agent_label", "default"),
            ("display.sidebar_width", "default"),
            ("display.popup_width", "default"),
            ("display.popup_height", "default"),
            ("behavior.notifications", "default"),
            ("behavior.hide_windows", "default"),
            ("tmux_management.enabled", "default"),
            ("tmux_management.confirm_delete", "default"),
            ("tmux_management.resume_agents", "default"),
            ("tmux_management.undo_history", "default"),
            ("keys.sequence_timeout_ms", "default"),
        ]
        .into_iter()
        .map(|(name, source)| (name.to_string(), source.to_string())),
    );
    let mut result = AppConfig {
        sequence_timeout_ms: 1000,
        tmux_management_enabled: true,
        tmux_management_confirm_delete: true,
        tmux_management_resume_agents: false,
        tmux_management_undo_history: 20,
        mode: DisplayMode::Split,
        show_all_panes: true,
        show_frame: true,
        agent_label: AgentLabel::IconText,
        sidebar_width: 30,
        popup_width: 40,
        popup_height: PopupHeight::Auto(AutoHeight::Auto),
        notifications: true,
        hide_windows: None,
        theme: file.theme.clone().unwrap_or_default(),
        normal: resolved_keys(
            KeyMode::Normal,
            file.keys.as_ref().and_then(|k| k.normal.as_ref()),
        )?,
        search: resolved_keys(
            KeyMode::Search,
            file.keys.as_ref().and_then(|k| k.search.as_ref()),
        )?,
        quick_launchers,
        sources,
    };
    fn apply(r: &mut AppConfig, f: &FileConfig, source: &str) {
        macro_rules! set {
            ($field:ident, $value:expr, $name:literal) => {
                if let Some(v) = $value {
                    r.$field = v;
                    r.sources.insert($name.into(), source.to_owned());
                }
            };
        }
        if let Some(d) = &f.display {
            set!(mode, d.mode, "display.mode");
            set!(show_all_panes, d.show_all_panes, "display.show_all_panes");
            set!(show_frame, d.show_frame, "display.show_frame");
            set!(agent_label, d.agent_label, "display.agent_label");
            set!(sidebar_width, d.sidebar_width, "display.sidebar_width");
            set!(popup_width, d.popup_width, "display.popup_width");
            set!(popup_height, d.popup_height, "display.popup_height");
        }
        if let Some(b) = &f.behavior {
            set!(notifications, b.notifications, "behavior.notifications");
            if let Some(glob) = &b.hide_windows {
                r.hide_windows = Some(glob.clone());
                r.sources
                    .insert("behavior.hide_windows".into(), source.to_owned());
            }
        }
        if let Some(management) = &f.tmux_management {
            set!(
                tmux_management_enabled,
                management.enabled,
                "tmux_management.enabled"
            );
            set!(
                tmux_management_confirm_delete,
                management.confirm_delete,
                "tmux_management.confirm_delete"
            );
            set!(
                tmux_management_resume_agents,
                management.resume_agents,
                "tmux_management.resume_agents"
            );
            set!(
                tmux_management_undo_history,
                management.undo_history.map(usize::from),
                "tmux_management.undo_history"
            );
        }
        if let Some(k) = &f.keys {
            set!(
                sequence_timeout_ms,
                k.sequence_timeout_ms,
                "keys.sequence_timeout_ms"
            );
        }
    }
    apply(&mut result, file, "file");
    result.sources.insert(
        "theme.base".into(),
        if file.theme.as_ref().and_then(|t| t.base).is_some() {
            "file"
        } else {
            "default"
        }
        .into(),
    );
    macro_rules! color_sources {
        ($($field:ident),*) => { $(result.sources.insert(concat!("theme.colors.", stringify!($field)).into(),
            if file.theme.as_ref().and_then(|t| t.colors.as_ref()).and_then(|c| c.$field.as_ref()).is_some() { "file" } else { "theme base" }.into());)* };
    }
    color_sources!(
        header_fg,
        header_bg,
        pane_bg,
        selected_bg,
        text_fg,
        muted_fg,
        accent_fg,
        error_fg,
        blocked_fg,
        blocked_bg,
        blocked_bg_unfocused,
        working_fg,
        working_bg,
        working_bg_unfocused,
        idle_fg,
        idle_bg,
        idle_bg_unfocused,
        done_fg,
        done_bg,
        done_bg_unfocused
    );
    for (mode, action, field) in [
        (KeyMode::Normal, Action::Down, "keys.normal.down"),
        (KeyMode::Normal, Action::Up, "keys.normal.up"),
        (KeyMode::Normal, Action::Jump, "keys.normal.jump"),
        (KeyMode::Normal, Action::Search, "keys.normal.search"),
        (KeyMode::Normal, Action::Filter, "keys.normal.filter"),
        (KeyMode::Normal, Action::Reset, "keys.normal.reset"),
        (KeyMode::Normal, Action::Help, "keys.normal.help"),
        (KeyMode::Normal, Action::Versions, "keys.normal.versions"),
        (KeyMode::Normal, Action::Settings, "keys.normal.settings"),
        (KeyMode::Normal, Action::Close, "keys.normal.close"),
        (KeyMode::Search, Action::Up, "keys.search.up"),
        (KeyMode::Search, Action::Down, "keys.search.down"),
        (KeyMode::Search, Action::Accept, "keys.search.accept"),
        (KeyMode::Search, Action::Cancel, "keys.search.cancel"),
        (KeyMode::Search, Action::Backspace, "keys.search.backspace"),
        (KeyMode::Search, Action::Clear, "keys.search.clear"),
    ] {
        let supplied = file
            .keys
            .as_ref()
            .and_then(|k| match mode {
                KeyMode::Normal => k.normal.as_ref(),
                _ => k.search.as_ref(),
            })
            .is_some_and(|keys| keys.contains_key(&action));
        result.sources.insert(
            field.into(),
            if supplied { "file" } else { "default" }.into(),
        );
    }
    // Parse every supplied layer, including shadowed legacy values. Presence,
    // never nonemptiness, selects canonical over legacy.
    for prefix in ["@agents-mon-", "@agenmux-"] {
        for suffix in OPTIONS {
            if let Some(value) = options.get(&format!("{prefix}{suffix}")) {
                let source = format!("tmux {prefix}{suffix}");
                let layer = compatibility(suffix, value).map_err(|mut error| {
                    error.location = "<tmux>".into();
                    error.field = format!("{prefix}{suffix}");
                    error
                })?;
                apply(&mut result, &layer, &source);
            }
        }
    }
    if let Some(mode) = cli {
        // Bootstrap's explicit empty argument is validated as no selection.
        if mode.is_empty() {
            return Ok(result);
        }
        result.mode = match mode {
            "split" => DisplayMode::Split,
            "popup" => DisplayMode::Popup,
            _ => {
                return Err(ConfigError::invalid(
                    "CLI display",
                    "expected split or popup; empty selects resolved mode",
                ))
            }
        };
        result.sources.insert("display.mode".into(), "CLI".into());
    }
    Ok(result)
}

/// One immutable file snapshot, including failures, per process. Startup
/// validation and every one-shot command read through this, so a file edited
/// mid-command cannot make one process act on two different configurations.
pub fn snapshot() -> Result<&'static FileConfig, ConfigError> {
    static FILE: std::sync::OnceLock<Result<FileConfig, ConfigError>> = std::sync::OnceLock::new();
    FILE.get_or_init(load).as_ref().map_err(Clone::clone)
}

/// The option `config reload` bumps. Long-lived views watch it rather than the
/// file: one explicit signal beats polling the filesystem, and it reaches the
/// daemon and any popup at once.
pub const RELOAD_OPTION: &str = "@agenmux-reload";

/// Presence is read separately from raw values: tmux's quoted list output is
/// not a serialization format for arbitrary option values.
pub fn read_options(
    mut run: impl FnMut(&str) -> Result<String, crate::tmux::TmuxError>,
) -> Result<BTreeMap<String, String>, ConfigError> {
    let io = |_| ConfigError {
        location: "<tmux>".into(),
        field: "options".into(),
        reason: "cannot read application options".into(),
        read_error: true,
    };
    let listed = run("show-options -g").map_err(io)?;
    let present: HashSet<_> = listed
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    let mut options = BTreeMap::new();
    for prefix in ["@agenmux-", "@agents-mon-"] {
        for suffix in OPTIONS {
            let name = format!("{prefix}{suffix}");
            if present.contains(name.as_str()) {
                let value = run(&format!("show-option -gqv {name}")).map_err(io)?;
                options.insert(name, value.strip_suffix('\n').unwrap_or(&value).to_owned());
            }
        }
    }
    Ok(options)
}
pub fn current(cli: Option<&str>) -> Result<AppConfig, ConfigError> {
    let file = snapshot()?;
    let options =
        read_options(|cmd| crate::tmux::command(&cmd.split_whitespace().collect::<Vec<_>>()))?;
    resolve_cli(file, &options, cli)
}

pub fn current_process() -> Result<AppConfig, ConfigError> {
    let mode = std::env::var("AGENMUX_DISPLAY_OVERRIDE").ok();
    current(mode.as_deref())
}

/// What a refresh changed. Width drives relayout; a reload additionally
/// replaces the palette and the keys the view names in its hints.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Refreshed {
    pub width_changed: bool,
    pub reloaded: bool,
}

pub struct LiveConfig {
    pub settings: AppConfig,
    /// Owned rather than borrowed from the process snapshot: a reload replaces
    /// it, and a rejected reload must leave the previous one standing.
    file: FileConfig,
    reload_token: Option<String>,
    cli_mode: Option<String>,
    last_read: std::time::Instant,
    reported: HashSet<String>,
}
impl LiveConfig {
    pub fn new(settings: AppConfig) -> Self {
        let cli_mode = (settings.sources.get("display.mode").map(String::as_str) == Some("CLI"))
            .then(|| match settings.mode {
                DisplayMode::Split => "split".into(),
                DisplayMode::Popup => "popup".into(),
            });
        Self {
            settings,
            file: snapshot().cloned().unwrap_or_default(),
            reload_token: None,
            cli_mode,
            last_read: std::time::Instant::now() - std::time::Duration::from_secs(1),
            reported: HashSet::new(),
        }
    }
    /// Invalid overrides retain the entire last valid snapshot. At most 16
    /// distinct diagnostics per process.
    pub fn refresh(&mut self, tmux: &mut crate::tmux::Tmux) -> Refreshed {
        if self.last_read.elapsed() < std::time::Duration::from_millis(200) {
            return Refreshed::default();
        }
        self.last_read = std::time::Instant::now();
        // A bumped token means someone ran `config reload`: re-read the file
        // once for that token. A rejected file keeps the last valid one, and
        // the token still advances so one bad edit cannot re-report forever.
        let token = tmux
            .run(&format!("show-option -gqv {RELOAD_OPTION}"))
            .ok()
            .map(|value| value.trim_end_matches('\n').to_owned())
            .filter(|value| !value.is_empty());
        let mut reloaded = false;
        if token != self.reload_token {
            self.reload_token = token;
            match load() {
                Ok(file) => {
                    reloaded = self.file != file;
                    self.file = file;
                }
                Err(e) => self.report(&e),
            }
        }
        let next = read_options(|cmd| tmux.run(cmd))
            .and_then(|options| resolve_cli(&self.file, &options, self.cli_mode.as_deref()));
        let reported = self.reported.len();
        let width_changed = self.accept(next);
        if self.reported.len() != reported {
            let _ = tmux.run("display-message 'agenmux: invalid live application configuration; keeping last valid settings. Run agenmux config check --effective for details.'");
        }
        Refreshed {
            width_changed,
            reloaded,
        }
    }
    pub(crate) fn accept(&mut self, next: Result<AppConfig, ConfigError>) -> bool {
        match next {
            Ok(next) => {
                let changed = next.sidebar_width != self.settings.sidebar_width;
                self.settings = next;
                changed
            }
            Err(e) => {
                self.report(&e);
                false
            }
        }
    }
    pub(crate) fn resolve(
        &self,
        file: &FileConfig,
        options: &BTreeMap<String, String>,
    ) -> Result<AppConfig, ConfigError> {
        resolve_cli(file, options, self.cli_mode.as_deref())
    }

    pub(crate) fn replace(&mut self, file: FileConfig, settings: AppConfig) {
        self.file = file;
        self.settings = settings;
    }
    fn report(&mut self, error: &ConfigError) {
        let diagnostic = error.to_string();
        if self.reported.len() < 16 && self.reported.insert(diagnostic.clone()) {
            eprintln!("agenmux: {diagnostic}; retaining last valid application settings");
        }
    }
}

/// Validate the file, reinstall the key tables when the keymap moved, then
/// bump the token every live view watches. Nothing is signalled unless the file
/// is valid, so a typo cannot take a running sidebar down with it.
pub fn reload(plugin_dir: &Path) -> i32 {
    // Deliberately not the process snapshot: reload exists to see a newer file.
    let file = match load() {
        Ok(file) => file,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    let options =
        match read_options(|cmd| crate::tmux::command(&cmd.split_whitespace().collect::<Vec<_>>()))
        {
            Ok(options) => options,
            Err(e) => {
                eprintln!("agenmux: {e}");
                return e.exit_code();
            }
        };
    let config = match resolve(&file, &options) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    let installed =
        crate::tmux::command(&["show-option", "-gqv", "@agenmux-nav-version"]).unwrap_or_default();
    let reinstalled = installed.trim_end() != crate::setup::nav_version(&config);
    if reinstalled {
        if crate::setup::run_config(plugin_dir, &config) != 0 {
            return 1;
        }
        crate::setup::reclaim_client_tables();
    }
    // Any changing value works; the clock keeps it readable in show-options.
    let token = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_else(|_| "reload".into());
    if crate::tmux::command_status(&["set-option", "-g", RELOAD_OPTION, &token]).is_err() {
        eprintln!("agenmux: cannot publish the reload signal");
        return 1;
    }
    let path = config_path()
        .map(|path| escaped(&path.to_string_lossy()))
        .unwrap_or_else(|| "defaults".into());
    if reinstalled {
        println!("reloaded {path}");
        println!(
            "{} settings differ from the defaults; key tables reinstalled",
            customized(&rows(&config))
        );
    } else {
        println!("reloaded {path}");
        println!(
            "{} settings differ from the defaults",
            customized(&rows(&config))
        );
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_empty_layer_semantics() {
        let empty = parse("").unwrap();
        let default = resolve(&empty, &BTreeMap::new()).unwrap();
        assert!(default.show_all_panes);
        assert!(default.show_frame);
        assert_eq!(default.sequence_timeout_ms, 1000);
        assert!(default.tmux_management_enabled);
        assert!(default.tmux_management_confirm_delete);
        assert!(!default.tmux_management_resume_agents);
        assert_eq!(default.tmux_management_undo_history, 20);
        let management = parse(
            "[tmux_management]\nenabled = true\nconfirm_delete = false\nresume_agents = true\nundo_history = 0",
        )
        .unwrap();
        let management = resolve(&management, &BTreeMap::new()).unwrap();
        assert_eq!(management.tmux_management_undo_history, 0);
        for value in ["101", "-1", "'5'"] {
            assert!(
                parse(&format!("[tmux_management]\nundo_history = {value}")).is_err(),
                "accepted undo_history {value}"
            );
        }
        assert!(management.tmux_management_enabled);
        assert!(!management.tmux_management_confirm_delete);
        assert!(management.tmux_management_resume_agents);
        // Each opt-out is independent, and disabling management keeps the
        // delete confirmation default.
        let read_only = parse("[tmux_management]\nenabled = false").unwrap();
        let read_only = resolve(&read_only, &BTreeMap::new()).unwrap();
        assert!(!read_only.tmux_management_enabled);
        assert!(read_only.show_all_panes);
        assert!(read_only.tmux_management_confirm_delete);
        let agents_only = parse("[display]\nshow_all_panes = false").unwrap();
        let agents_only = resolve(&agents_only, &BTreeMap::new()).unwrap();
        assert!(!agents_only.show_all_panes);
        assert!(agents_only.tmux_management_enabled);
        assert!(agents_only.tmux_management_confirm_delete);
        let effective = rows(&default);
        for (name, value) in [
            ("display.show_all_panes", "true"),
            ("tmux_management.enabled", "true"),
            ("tmux_management.confirm_delete", "true"),
            ("tmux_management.resume_agents", "false"),
            ("tmux_management.undo_history", "20"),
            ("keys.sequence_timeout_ms", "1000"),
        ] {
            let row = effective.iter().find(|row| row.name == name).unwrap();
            assert_eq!(row.value, value);
            assert_eq!(row.source, "default");
        }
        let timeout = parse("[keys]\nsequence_timeout_ms = 250").unwrap();
        let timeout = resolve(&timeout, &BTreeMap::new()).unwrap();
        assert_eq!(timeout.sequence_timeout_ms, 250);
        assert_eq!(timeout.sources["keys.sequence_timeout_ms"], "file");
        assert_eq!(agents_only.sources["display.show_all_panes"], "file");
        let framed = parse("[display]\nshow_frame = false").unwrap();
        let framed = resolve(&framed, &BTreeMap::new()).unwrap();
        assert!(!framed.show_frame);
        assert_eq!(framed.sources["display.show_frame"], "file");
        assert!(parse("[display]\nshow_all_panes = 'true'").is_err());
        assert!(parse("[display]\nshow_frame = 'false'").is_err());
        assert_eq!((default.sidebar_width, default.popup_width), (30, 40));
        assert_eq!(default.hide_windows, None);
        let file = parse("[display]\nmode='popup'\nsidebar_width=22\npopup_width=24\npopup_height=18\n[behavior]\nnotifications=false\nhide_windows='hidden*'").unwrap();
        let mut options = BTreeMap::new();
        for suffix in OPTIONS {
            options.insert(format!("@agenmux-{suffix}"), String::new());
        }
        options.insert("@agents-mon-width".into(), "77".into());
        options.insert("@agents-mon-popup-key".into(), "E".into());
        let config = resolve(&file, &options).unwrap();
        assert_eq!(config.mode, DisplayMode::Split);
        assert_eq!((config.sidebar_width, config.popup_width), (30, 40));
        assert_eq!(config.popup_height, PopupHeight::Auto(AutoHeight::Auto));
        assert!(config.notifications);
        assert_eq!(config.hide_windows.as_deref(), Some(""));
        assert!(config
            .sources
            .iter()
            .filter(|(field, _)| {
                **field != "display.show_all_panes"
                    && **field != "display.show_frame"
                    && **field != "display.agent_label"
            })
            .filter(|(field, _)| field.starts_with("display.") || field.starts_with("behavior."))
            .all(|(_, source)| source.starts_with("tmux @agenmux-")));
    }

    #[test]
    fn precedence_cli_canonical_legacy_file_defaults_and_unset() {
        let file =
            parse("[display]\nmode='popup'\nsidebar_width=22\n[behavior]\nnotifications=false")
                .unwrap();
        let mut options = BTreeMap::new();
        assert_eq!(resolve(&file, &options).unwrap().sidebar_width, 22);
        options.insert("@agents-mon-width".into(), "44".into());
        assert_eq!(resolve(&file, &options).unwrap().sidebar_width, 44);
        options.insert("@agenmux-width".into(), "55".into());
        let config = resolve_cli(&file, &options, Some("split")).unwrap();
        assert_eq!(config.sidebar_width, 55);
        assert_eq!(config.mode, DisplayMode::Split);
        assert_eq!(config.sources["display.mode"], "CLI");
        assert_eq!(
            config.sources["display.sidebar_width"],
            "tmux @agenmux-width"
        );
        assert_eq!(
            resolve_cli(&file, &options, Some("")).unwrap().mode,
            DisplayMode::Popup
        );
        assert!(resolve_cli(&file, &options, Some("float")).is_err());
        options.remove("@agenmux-width");
        assert_eq!(resolve(&file, &options).unwrap().sidebar_width, 44);
        options.clear();
        let config = resolve(&file, &options).unwrap();
        assert_eq!(config.sidebar_width, 22);
        assert!(!config.notifications);
    }

    #[test]
    fn all_shadowed_values_are_validated_and_live_failures_are_bounded() {
        let file = parse("").unwrap();
        for (suffix, invalid) in [
            ("width", "0"),
            ("height", "10001"),
            ("display", "junk"),
            ("notifications", "anything-else"),
            ("hide-windows", "bad\nvalue"),
        ] {
            let options = BTreeMap::from([
                (format!("@agents-mon-{suffix}"), invalid.into()),
                (format!("@agenmux-{suffix}"), String::new()),
            ]);
            assert!(
                resolve_cli(&file, &options, Some("popup")).is_err(),
                "{suffix}"
            );
        }
        let initial = resolve(&file, &BTreeMap::new()).unwrap();
        let mut live = LiveConfig::new(initial.clone());
        for i in 0..100 {
            assert!(!live.accept(Err(ConfigError::invalid(&format!("field-{i}"), "invalid"))));
        }
        assert_eq!(live.reported.len(), 16);
        assert_eq!(live.settings, initial);
        let options = BTreeMap::from([
            ("@agenmux-width".into(), "42".into()),
            ("@agenmux-notifications".into(), " FALSE ".into()),
        ]);
        assert!(live.accept(resolve(&file, &options)));
        assert_eq!(live.settings.sidebar_width, 42);
        assert!(!live.settings.notifications);
        assert!(live.accept(resolve(&file, &BTreeMap::new())));
        assert_eq!(live.settings, initial);
    }

    #[test]
    fn live_candidate_resolution_keeps_the_launch_override() {
        let initial = resolve_cli(&Default::default(), &Default::default(), Some("popup")).unwrap();
        let live = LiveConfig::new(initial);
        let file = parse("[display]\nmode = 'split'\n").unwrap();
        let candidate = live.resolve(&file, &Default::default()).unwrap();
        assert_eq!(candidate.mode, DisplayMode::Popup);
        assert_eq!(candidate.sources["display.mode"], "CLI");
    }
}
