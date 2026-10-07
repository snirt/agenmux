//! Non-executable, partial application settings. Runtime application is deliberately separate.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

pub const MAX_BYTES: usize = 65_536;
/// Closed sessions, windows and panes the undo list may keep; each holds a
/// layout copy in the snapshot file.
pub const MAX_UNDO_HISTORY: u16 = 100;

#[derive(Debug, Clone)]
pub struct ConfigError {
    location: String,
    field: String,
    reason: String,
    read_error: bool,
}
impl ConfigError {
    fn invalid(field: &str, reason: impl Into<String>) -> Self {
        Self {
            location: "<config>".into(),
            field: field.into(),
            reason: reason.into(),
            read_error: false,
        }
    }
    fn io(path: &Path, error: io::Error) -> Self {
        Self {
            location: path.to_string_lossy().into_owned(),
            field: "file".into(),
            reason: error.to_string(),
            read_error: true,
        }
    }
    pub fn exit_code(&self) -> i32 {
        if self.read_error {
            1
        } else {
            2
        }
    }
}
fn escaped(value: &str) -> String {
    value
        .chars()
        .take(300)
        .flat_map(char::escape_debug)
        .collect()
}
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {}: {}",
            escaped(&self.location),
            escaped(&self.field),
            escaped(&self.reason)
        )
    }
}
impl std::error::Error for ConfigError {}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub version: Option<u32>,
    pub display: Option<DisplayConfig>,
    pub behavior: Option<BehaviorConfig>,
    pub theme: Option<ThemeConfig>,
    pub tmux_management: Option<TmuxManagementConfig>,
    pub quick_launchers: Option<BTreeMap<String, QuickLauncherConfig>>,
    pub keys: Option<KeyConfig>,
}
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    pub mode: Option<DisplayMode>,
    pub show_all_panes: Option<bool>,
    pub show_frame: Option<bool>,
    pub agent_label: Option<AgentLabel>,
    pub sidebar_width: Option<u16>,
    pub popup_width: Option<u16>,
    pub popup_height: Option<PopupHeight>,
}
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(try_from = "String")]
pub enum DisplayMode {
    Split,
    Popup,
}
/// How agent rows name their agent: `AGENT_ICON` and name, icon, or name.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(try_from = "String")]
pub enum AgentLabel {
    IconText,
    Icon,
    Text,
}
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum PopupHeight {
    Cells(u16),
    Auto(AutoHeight),
}
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(try_from = "String")]
pub enum AutoHeight {
    Auto,
}
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BehaviorConfig {
    pub notifications: Option<bool>,
    pub auto_update: Option<bool>,
    pub hide_windows: Option<String>,
}
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TmuxManagementConfig {
    pub enabled: Option<bool>,
    pub confirm_delete: Option<bool>,
    pub resume_agents: Option<bool>,
    pub undo_history: Option<u16>,
}

// Deserialize through String so TOML's externally tagged enum tables cannot
// masquerade as string-only schema values (e.g. mode = { split = {} }).
macro_rules! string_values {
    ($ty:ty, $reason:literal, $( $text:literal => $variant:ident ),+ $(,)?) => {
        impl TryFrom<String> for $ty {
            type Error = &'static str;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                match value.as_str() {
                    $( $text => Ok(Self::$variant), )+
                    _ => Err($reason),
                }
            }
        }
    };
}
string_values!(DisplayMode, "expected split or popup", "split" => Split, "popup" => Popup);
string_values!(AgentLabel, "expected icon-text, icon, or text", "icon-text" => IconText, "icon" => Icon, "text" => Text);
string_values!(AutoHeight, "expected auto", "auto" => Auto);

mod file;
mod keys;
mod launchers;
mod report;
mod resolve;
mod theme;

pub use file::*;
pub use keys::*;
pub use launchers::*;
pub use report::*;
pub use resolve::*;
pub use theme::*;

pub fn parse(source: &str) -> Result<FileConfig, ConfigError> {
    if source.len() > MAX_BYTES {
        return Err(ConfigError::invalid("file", "exceeds 65536 bytes"));
    }
    // Neither Error::message nor source-derived key/section guesses are safe:
    // both can contain rejected values (including multiline string contents).
    // Render only fixed categories and numeric locations for parser errors.
    let diagnostic = |e: toml::de::Error, category: &str| {
        let location = e
            .span()
            .and_then(|span| source.get(..span.start))
            .map(|before| {
                let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
                let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
                format!("document (line {line}, column {column})")
            })
            .unwrap_or_else(|| "document".into());
        ConfigError::invalid(&location, category)
    };
    let document: toml::Value =
        toml::from_str(source).map_err(|e| diagnostic(e, "invalid TOML syntax"))?;
    let config: FileConfig = toml::from_str(source)
        .map_err(|e| diagnostic(e, "invalid configuration schema (field, type, or value)"))?;
    // Serde's derived structs also accept positional sequences. TOML sections
    // must be tables, never arrays that happen to have the right field order.
    for field in [
        "display",
        "tmux_management",
        "behavior",
        "theme",
        "theme.colors",
        "quick_launchers",
        "keys",
        "keys.normal",
        "keys.search",
    ] {
        let value = field
            .split('.')
            .try_fold(&document, |value, key| value.get(key));
        if value.is_some_and(|value| !value.is_table()) {
            return Err(ConfigError::invalid(field, "must be a TOML table"));
        }
    }
    if let Some(launchers) = document
        .get("quick_launchers")
        .and_then(toml::Value::as_table)
    {
        for (id, value) in launchers {
            if !value.is_table() {
                return Err(ConfigError::invalid(
                    &format!("quick_launchers.{id}"),
                    "must be a TOML table",
                ));
            }
        }
    }
    validate(&config)?;
    Ok(config)
}

fn validate(config: &FileConfig) -> Result<(), ConfigError> {
    if config.version.is_some_and(|v| v != 1) {
        return Err(ConfigError::invalid(
            "version",
            "only version 1 is supported",
        ));
    }
    if let Some(d) = &config.display {
        for (field, value) in [
            ("display.sidebar_width", d.sidebar_width),
            ("display.popup_width", d.popup_width),
            (
                "display.popup_height",
                match d.popup_height {
                    Some(PopupHeight::Cells(n)) => Some(n),
                    _ => None,
                },
            ),
        ] {
            if value.is_some_and(|v| !(1..=10000).contains(&v)) {
                return Err(ConfigError::invalid(field, "must be 1..=10000 cells"));
            }
        }
    }
    if let Some(b) = &config.behavior {
        if b.hide_windows
            .as_ref()
            .is_some_and(|s| s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(ConfigError::invalid(
                "behavior.hide_windows",
                "must be a literal glob of at most 256 bytes without control characters",
            ));
        }
    }
    if config
        .tmux_management
        .as_ref()
        .and_then(|management| management.undo_history)
        .is_some_and(|entries| entries > MAX_UNDO_HISTORY)
    {
        return Err(ConfigError::invalid(
            "tmux_management.undo_history",
            "must be 0..=100",
        ));
    }
    let k = config.keys.as_ref();
    if config
        .keys
        .as_ref()
        .and_then(|keys| keys.sequence_timeout_ms)
        == Some(0)
    {
        return Err(ConfigError::invalid(
            "keys.sequence_timeout_ms",
            "must be a positive integer",
        ));
    }
    resolved_keys(KeyMode::Normal, k.and_then(|k| k.normal.as_ref()))?;
    resolved_keys(KeyMode::Search, k.and_then(|k| k.search.as_ref()))?;
    let management_enabled = config
        .tmux_management
        .as_ref()
        .and_then(|management| management.enabled)
        .unwrap_or(true);
    let (launchers, _) = resolve_quick_launchers(config.quick_launchers.as_ref())?;
    let normal = resolved_keys(KeyMode::Normal, k.and_then(|k| k.normal.as_ref()))?;
    validate_quick_launcher_conflicts(&launchers, management_enabled, &normal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_partial_preserve_absence() {
        assert_eq!(parse("").unwrap(), FileConfig::default());
        assert_eq!(parse("# comment\n").unwrap(), FileConfig::default());
        let c = parse("[behavior]\nhide_windows = ''\nnotifications = false\n").unwrap();
        let b = c.behavior.unwrap();
        assert_eq!(b.hide_windows.as_deref(), Some(""));
        assert_eq!(b.notifications, Some(false));
        assert_eq!(c.display, None);
    }

    #[test]
    fn complete_example_is_typed() {
        let c = parse(
            r##"
version = 1
[display]
mode = "split"
sidebar_width = 30
popup_width = 40
popup_height = "auto"
[behavior]
notifications = true
hide_windows = "agents*"
[tmux_management]
enabled = false
confirm_delete = true
[theme]
base = "light"
[theme.colors]
header_bg = "#eeeeee"
header_fg = "#202020"
working_bg = "#fff0cc"
working_bg_unfocused = "#f7f2e5"
working_fg = "#775500"
[keys]
sequence_timeout_ms = 1000
[keys.normal]
down = ["j", "Down"]
up = ["k", "Up"]
jump = ["Enter", "l"]
search = ["/"]
filter = ["f"]
reset = ["Escape"]
help = ["?"]
versions = ["U"]
close = ["q", "Q"]
[keys.search]
up = ["Up", "C-p"]
down = ["Down", "C-n"]
accept = ["Enter"]
cancel = ["Escape", "C-c"]
backspace = ["BSpace"]
clear = ["C-u"]
"##,
        )
        .unwrap();
        assert_eq!(c.version, Some(1));
        assert_eq!(
            c.display.unwrap().popup_height,
            Some(PopupHeight::Auto(AutoHeight::Auto))
        );
        assert_eq!(
            c.theme.unwrap().colors.unwrap().header_bg,
            Some(Color::Rgb(238, 238, 238))
        );
    }

    #[test]
    fn rejects_unknown_duplicate_and_wrong_types_at_every_level() {
        for source in [
            "command = 'no'",
            "display = ['split', 30, 40, 'auto']",
            "behavior = [true, 300, 'agents*']",
            "theme = ['dark', {}]",
            "tmux_management = [false, true]",
            "[tmux_management]\nenabled = 'yes'",
            "[tmux_management]\nconfirm_delete = 1",
            "[tmux_management]\ncommand = true",
            "keys = [{}, {}, {}]",
            "version = 2",
            "version = -1",
            "version = '1'",
            "version = 1\nversion = 1",
            "[display]\nmode = 'float'",
            "[display]\nmode = { split = {} }",
            "[display]\npopup_height = { auto = {} }",
            "[theme]\nbase = { dark = {} }",
            "[display]\ncommand = 'no'",
            "[behavior]\ncommand = 'no'",
            "[theme]\ncommand = 'no'",
            "[theme.colors]\ncommand = 'no'",
            "[keys]\ncommand = {}",
            "[keys.normal]\ncommand = []",
            "[keys.prefix]",
            "[keys.prefix]\ntoggle = ['A']\npopup = []",
            "[keys.normal]\ntoggle = []",
            "[keys.search]\npopup = []",
            "[keys.search]\njump = []",
            "[keys.normal]\ndown = ['j']\ndown = []",
            "[display]\nmode = 'split'\n[display]\nmode = 'popup'",
            "[behavior]\nnotifications = 'on'",
            "[theme]\nbase = 'remote'",
            "[keys.normal]\ndown = 'j'",
            "[keys.normal]\ndown = [1]",
        ] {
            assert!(parse(source).is_err(), "accepted {source}");
        }
    }

    #[test]
    fn geometry_and_delays_are_bounded() {
        for field in ["sidebar_width", "popup_width", "popup_height"] {
            for value in ["0", "10001", "-1", "1.5", "true", "'30'", "65536"] {
                assert!(parse(&format!("[display]\n{field} = {value}")).is_err());
            }
            for value in [1, 10000] {
                assert!(parse(&format!("[display]\n{field} = {value}")).is_ok());
            }
        }
        for value in ["0", "-1", "1.5", "true", "'1000'"] {
            assert!(
                parse(&format!("[keys]\nsequence_timeout_ms = {value}")).is_err(),
                "accepted sequence timeout {value}"
            );
        }
        for value in [1, 1000, 60_000] {
            assert!(parse(&format!("[keys]\nsequence_timeout_ms = {value}")).is_ok());
        }
        assert!(parse("[display]\nmode = 'popup'\npopup_height = 'off'").is_err());
    }

    #[test]
    fn colors_and_globs_are_data_not_code() {
        for value in ["'default'", "0", "255", "'#aBcD09'"] {
            assert!(parse(&format!("[theme.colors]\nheader_bg = {value}")).is_ok());
        }
        for value in [
            "-1",
            "256",
            "1.5",
            "true",
            "'#abc'",
            "'#GG0000'",
            "'red'",
            "'#(touch marker)'",
            "'\u{1b}[31m'",
        ] {
            assert!(parse(&format!("[theme.colors]\nheader_bg = {value}")).is_err());
        }
        assert!(parse("[theme]\ncommand = 'touch marker'").is_err());
        for base in ["dark", "light", "terminal"] {
            assert!(parse(&format!("[theme]\nbase = '{base}'")).is_ok());
        }
        for (glob, valid) in [
            ("x".repeat(256), true),
            ("é".repeat(129), false),
            ("x".repeat(257), false),
            ("a\u{7f}b".into(), false),
        ] {
            assert_eq!(
                parse(&format!("[behavior]\nhide_windows = '{glob}'")).is_ok(),
                valid
            );
        }
        assert!(parse("[behavior]\nhide_windows = \"a\\nb\"").is_err());
        assert!(parse("[behavior]\nhide_windows = '#(touch marker);${HOME}'").is_ok());
    }

    /// The shipped example spells out every default, so it must resolve to them.
    #[test]
    fn shipped_example_spells_out_the_defaults() {
        let example = parse(include_str!("../../examples/config.toml")).unwrap();
        let mut resolved = resolve(&example, &Default::default()).unwrap();
        let defaults = resolve(&Default::default(), &Default::default()).unwrap();
        assert_eq!(
            Palette::resolve(&resolved.theme),
            Palette::resolve(&defaults.theme)
        );
        resolved.theme = defaults.theme.clone();
        resolved.sources = defaults.sources.clone();
        assert_eq!(resolved, defaults);
    }
}
