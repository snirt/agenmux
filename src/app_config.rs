//! Non-executable, partial application settings. Runtime application is deliberately separate.

use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const MAX_BYTES: usize = 65_536;

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
    pub keys: Option<KeyConfig>,
}
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    pub mode: Option<DisplayMode>,
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
    pub hide_windows: Option<String>,
}
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    pub base: Option<ThemeBase>,
    pub colors: Option<ThemeColors>,
}
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(try_from = "String")]
pub enum ThemeBase {
    Dark,
    Light,
    Terminal,
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
string_values!(AutoHeight, "expected auto", "auto" => Auto);
string_values!(ThemeBase, "expected dark, light, or terminal", "dark" => Dark, "light" => Light, "terminal" => Terminal);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}
impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Index(u8),
            Text(String),
        }
        match Value::deserialize(d)? {
            Value::Index(i) => Ok(Self::Indexed(i)),
            Value::Text(s) if s == "default" => Ok(Self::Default),
            Value::Text(s)
                if s.len() == 7
                    && s.starts_with('#')
                    && s[1..].bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                let n = u32::from_str_radix(&s[1..], 16).unwrap();
                Ok(Self::Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
            }
            _ => Err(serde::de::Error::custom(
                "color must be default, 0..=255, or #RRGGBB",
            )),
        }
    }
}
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ThemeColors {
    pub header_fg: Option<Color>,
    pub header_bg: Option<Color>,
    pub text_fg: Option<Color>,
    pub muted_fg: Option<Color>,
    pub accent_fg: Option<Color>,
    pub error_fg: Option<Color>,
    pub blocked_fg: Option<Color>,
    pub blocked_bg: Option<Color>,
    pub blocked_bg_unfocused: Option<Color>,
    pub working_fg: Option<Color>,
    pub working_bg: Option<Color>,
    pub working_bg_unfocused: Option<Color>,
    pub idle_fg: Option<Color>,
    pub idle_bg: Option<Color>,
    pub idle_bg_unfocused: Option<Color>,
    pub done_fg: Option<Color>,
    pub done_bg: Option<Color>,
    pub done_bg_unfocused: Option<Color>,
}

// Renderer-only representation. Basic ANSI and inherited foregrounds retain
// the historical dark bytes; file colors can only construct Typed values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ink {
    Inherited,
    Basic(u8),
    Typed(Color),
}
impl Ink {
    /// Spelled the way the file would write it, so a reported value can be
    /// pasted back into [theme.colors].
    pub fn describe(&self) -> String {
        match self {
            Self::Inherited => "terminal".into(),
            Self::Basic(n) => n.to_string(),
            Self::Typed(Color::Default) => "default".into(),
            Self::Typed(Color::Indexed(n)) => n.to_string(),
            Self::Typed(Color::Rgb(r, g, b)) => format!("#{r:02x}{g:02x}{b:02x}"),
        }
    }
    pub fn fg(&self, attributes: &str) -> String {
        let color = match self {
            Self::Inherited => String::new(),
            Self::Basic(n) => (30 + n).to_string(),
            Self::Typed(Color::Default) => "39".into(),
            Self::Typed(Color::Indexed(n)) => format!("38;5;{n}"),
            Self::Typed(Color::Rgb(r, g, b)) => format!("38;2;{r};{g};{b}"),
        };
        let params = [attributes, &color]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(";");
        if params.is_empty() {
            String::new()
        } else {
            format!("\x1b[{params}m")
        }
    }
    pub fn bg(&self) -> String {
        match self {
            Self::Inherited => String::new(),
            Self::Basic(n) => format!("\x1b[{}m", 40 + n),
            Self::Typed(Color::Default) => "\x1b[49m".into(),
            Self::Typed(Color::Indexed(n)) => format!("\x1b[48;5;{n}m"),
            Self::Typed(Color::Rgb(r, g, b)) => format!("\x1b[48;2;{r};{g};{b}m"),
        }
    }
}

/// Startup-resolved semantic roles. No source strings reach ANSI generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Palette {
    pub header_fg: Ink,
    pub header_bg: Ink,
    pub text_fg: Ink,
    pub muted_fg: Ink,
    pub accent_fg: Ink,
    pub error_fg: Ink,
    pub blocked_fg: Ink,
    pub blocked_bg: Ink,
    pub blocked_bg_unfocused: Ink,
    pub working_fg: Ink,
    pub working_bg: Ink,
    pub working_bg_unfocused: Ink,
    pub idle_fg: Ink,
    pub idle_bg: Ink,
    pub idle_bg_unfocused: Ink,
    pub done_fg: Ink,
    pub done_bg: Ink,
    pub done_bg_unfocused: Ink,
}
impl Palette {
    /// Every overridable role paired with its resolved colour, in the order
    /// `--help` and `check --effective` list them. One list, so a new role
    /// cannot appear in the palette without appearing in both.
    pub fn roles(&self) -> [(&'static str, Ink); 18] {
        [
            ("header_fg", self.header_fg),
            ("header_bg", self.header_bg),
            ("text_fg", self.text_fg),
            ("muted_fg", self.muted_fg),
            ("accent_fg", self.accent_fg),
            ("error_fg", self.error_fg),
            ("blocked_fg", self.blocked_fg),
            ("blocked_bg", self.blocked_bg),
            ("blocked_bg_unfocused", self.blocked_bg_unfocused),
            ("working_fg", self.working_fg),
            ("working_bg", self.working_bg),
            ("working_bg_unfocused", self.working_bg_unfocused),
            ("idle_fg", self.idle_fg),
            ("idle_bg", self.idle_bg),
            ("idle_bg_unfocused", self.idle_bg_unfocused),
            ("done_fg", self.done_fg),
            ("done_bg", self.done_bg),
            ("done_bg_unfocused", self.done_bg_unfocused),
        ]
    }
}
impl Default for Palette {
    fn default() -> Self {
        Self::resolve(&ThemeConfig::default())
    }
}
impl Palette {
    pub fn resolve(theme: &ThemeConfig) -> Self {
        use Ink::{Basic, Inherited, Typed};
        let rgb = |r, g, b| Typed(Color::Rgb(r, g, b));
        let mut p = Self {
            header_fg: Inherited,
            header_bg: Typed(Color::Indexed(236)),
            text_fg: Inherited,
            muted_fg: Inherited,
            accent_fg: Basic(4),
            error_fg: Inherited,
            blocked_fg: Basic(1),
            blocked_bg: rgb(42, 16, 16),
            blocked_bg_unfocused: rgb(32, 12, 12),
            working_fg: Basic(3),
            working_bg: rgb(38, 32, 16),
            working_bg_unfocused: rgb(29, 24, 12),
            idle_fg: Basic(2),
            idle_bg: rgb(15, 36, 16),
            idle_bg_unfocused: rgb(11, 27, 12),
            done_fg: Basic(2),
            done_bg: rgb(15, 36, 16),
            done_bg_unfocused: rgb(11, 27, 12),
        };
        match theme.base.unwrap_or(ThemeBase::Dark) {
            ThemeBase::Dark => {}
            ThemeBase::Light => {
                p.header_fg = rgb(32, 32, 32);
                p.header_bg = rgb(238, 238, 238);
                p.text_fg = rgb(32, 32, 32);
                p.muted_fg = rgb(80, 80, 80);
                p.accent_fg = rgb(32, 72, 144);
                p.error_fg = rgb(160, 32, 32);
                p.blocked_fg = rgb(160, 32, 32);
                p.blocked_bg = rgb(255, 224, 224);
                p.blocked_bg_unfocused = rgb(250, 238, 238);
                p.working_fg = rgb(119, 85, 0);
                p.working_bg = rgb(255, 240, 204);
                p.working_bg_unfocused = rgb(247, 242, 229);
                p.idle_fg = rgb(32, 104, 40);
                p.idle_bg = rgb(224, 244, 224);
                p.idle_bg_unfocused = rgb(238, 247, 238);
                p.done_fg = p.idle_fg.clone();
                p.done_bg = p.idle_bg.clone();
                p.done_bg_unfocused = p.idle_bg_unfocused.clone();
            }
            ThemeBase::Terminal => {
                p.error_fg = Basic(1);
                p.header_fg = Typed(Color::Default);
                p.text_fg = Typed(Color::Default);
                p.muted_fg = Typed(Color::Default);
                p.header_bg = Typed(Color::Default);
                p.blocked_bg = Typed(Color::Default);
                p.blocked_bg_unfocused = Typed(Color::Default);
                p.working_bg = Typed(Color::Default);
                p.working_bg_unfocused = Typed(Color::Default);
                p.idle_bg = Typed(Color::Default);
                p.idle_bg_unfocused = Typed(Color::Default);
                p.done_bg = Typed(Color::Default);
                p.done_bg_unfocused = Typed(Color::Default);
            }
        }
        if let Some(c) = &theme.colors {
            macro_rules! apply { ($($field:ident),*) => { $(if let Some(value) = &c.$field { p.$field = Typed(value.clone()); })* }; }
            apply!(
                header_fg,
                header_bg,
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
        }
        p
    }
    pub fn state_fg(&self, state: &str) -> &Ink {
        match state {
            "blocked" => &self.blocked_fg,
            "working" => &self.working_fg,
            "done" => &self.done_fg,
            _ => &self.idle_fg,
        }
    }
    pub fn state_bg(&self, state: &str, focused: bool) -> String {
        match (state, focused) {
            ("blocked", true) => &self.blocked_bg,
            ("blocked", false) => &self.blocked_bg_unfocused,
            ("working", true) => &self.working_bg,
            ("working", false) => &self.working_bg_unfocused,
            ("done", true) => &self.done_bg,
            ("done", false) => &self.done_bg_unfocused,
            (_, true) => &self.idle_bg,
            (_, false) => &self.idle_bg_unfocused,
        }
        .bg()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Down,
    Up,
    Jump,
    Search,
    Filter,
    Reset,
    Help,
    Versions,
    Close,
    Accept,
    Cancel,
    Backspace,
    Clear,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyChord {
    Printable(u8),
    Control(u8),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Enter,
    Escape,
    Tab,
    Backspace,
}
impl KeyChord {
    /// Conservative portable controls with the current ISIG/IXON/IEXTEN mode:
    /// C-c, C-d, C-g, C-k, C-n, C-p, C-u, C-w, C-x, C-], C-^, C-_, plus the
    /// C-i/C-m/C-[/C-? aliases of Tab/Enter/Escape/BSpace.
    /// Exclude conventional signal, flow-control, and extended editing controls
    /// (C-o/q/r/s/t/v/y/z and C-backslash), and the four bytes the sidebar's own
    /// input protocol claims before any keymap lookup.
    /// C-c is retained for the default search cancel; resolved_keys reserves
    /// C-c/C-d as emergency paths (except search cancel).
    /// Custom terminal control-character assignments are not guaranteed.
    pub fn parse(s: &str) -> Result<Self, &'static str> {
        Ok(match s {
            "Up" => Self::Up,
            "Down" => Self::Down,
            "Left" => Self::Left,
            "Right" => Self::Right,
            "Home" => Self::Home,
            "End" => Self::End,
            "PageUp" => Self::PageUp,
            "PageDown" => Self::PageDown,
            "Enter" => Self::Enter,
            "Escape" => Self::Escape,
            "Tab" => Self::Tab,
            "BSpace" => Self::Backspace,
            "Space" => Self::Printable(b' '),
            _ if s.len() == 1 && (b' '..=b'~').contains(&s.as_bytes()[0]) => {
                Self::Printable(s.as_bytes()[0])
            }
            _ if s.len() == 3 && s.starts_with("C-") => {
                let c = s.as_bytes()[2].to_ascii_uppercase();
                let code = match c {
                    b'@'..=b'_' => c & 31,
                    b'?' => 127,
                    _ => return Err("unsupported control chord"),
                };
                match code {
                    9 => Self::Tab,
                    13 => Self::Enter,
                    27 => Self::Escape,
                    127 => Self::Backspace,
                    // The terminal folds these into Enter/BSpace but tmux keeps
                    // them as distinct keys, so one spelling cannot mean the
                    // same key in the popup and in the split key tables.
                    8 | 10 => {
                        return Err("control chord is not distinguishable from Enter or BSpace")
                    }
                    // The sidebar's key transport spends these on its own
                    // packets (text prefix, wheel up/down, click target, key
                    // sequence, clear filter), so they never reach a keymap
                    // lookup in popup mode. Rejecting them keeps one config
                    // from meaning two things across modes.
                    0..=2 | 5..=6 | 12 => {
                        return Err("control chord is reserved by the input protocol")
                    }
                    3..=4 | 7 | 11 | 14 | 16 | 21 | 23..=24 | 29..=31 => Self::Control(code),
                    _ => return Err("control chord may be intercepted by the terminal"),
                }
            }
            _ => {
                return Err(
                    "expected printable ASCII, named key, or a single representable C- chord",
                )
            }
        })
    }
}
impl KeyChord {
    /// tmux key-string name. Every representable chord has one (verified
    /// against tmux 3.x bind-key); `;` needs the command-separator escape.
    pub fn tmux_name(self) -> String {
        match self {
            Self::Printable(b' ') => "Space".into(),
            Self::Printable(b';') => "\\;".into(),
            Self::Printable(b) => char::from(b).to_string(),
            Self::Control(c) => format!("C-{}", char::from(c + b'@').to_ascii_lowercase()),
            Self::Up => "Up".into(),
            Self::Down => "Down".into(),
            Self::Left => "Left".into(),
            Self::Right => "Right".into(),
            Self::Home => "Home".into(),
            Self::End => "End".into(),
            Self::PageUp => "PageUp".into(),
            Self::PageDown => "PageDown".into(),
            Self::Enter => "Enter".into(),
            Self::Escape => "Escape".into(),
            Self::Tab => "Tab".into(),
            Self::Backspace => "BSpace".into(),
        }
    }
    /// Help-overlay spelling ("Enter", "Esc", "C-u"); `short` is the footer
    /// spelling ("↵", "esc", "^u") that fits a narrow sidebar.
    pub fn label(self, short: bool) -> String {
        match self {
            Self::Printable(b' ') => "Space".into(),
            Self::Printable(b) => char::from(b).to_string(),
            Self::Control(c) if short => format!("^{}", char::from(c + b'@').to_ascii_lowercase()),
            Self::Control(c) => format!("C-{}", char::from(c + b'@').to_ascii_lowercase()),
            Self::Up => "↑".into(),
            Self::Down => "↓".into(),
            Self::Left => "←".into(),
            Self::Right => "→".into(),
            Self::Home => "Home".into(),
            Self::End => "End".into(),
            Self::PageUp => "PgUp".into(),
            Self::PageDown => "PgDn".into(),
            Self::Enter if short => "↵".into(),
            Self::Enter => "Enter".into(),
            Self::Escape if short => "esc".into(),
            Self::Escape => "Esc".into(),
            Self::Tab => "Tab".into(),
            Self::Backspace => "BSpace".into(),
        }
    }
}
/// Resolved per-mode bindings: every action of the mode, possibly with no chords.
pub type Keymap = BTreeMap<Action, Vec<KeyChord>>;
/// Reverse lookup; validation guarantees a chord maps to at most one action.
pub fn action_for(keys: &Keymap, chord: KeyChord) -> Option<Action> {
    keys.iter()
        .find(|(_, chords)| chords.contains(&chord))
        .map(|(action, _)| *action)
}
impl<'de> Deserialize<'de> for KeyChord {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KeyConfig {
    pub normal: Option<BTreeMap<Action, Vec<KeyChord>>>,
    pub search: Option<BTreeMap<Action, Vec<KeyChord>>>,
}
#[derive(Debug, Clone, Copy)]
pub enum KeyMode {
    Normal,
    Search,
}
impl KeyMode {
    fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Search => "search",
        }
    }
}
/// Replace only explicitly supplied action lists, then validate the complete mode.
pub fn resolved_keys(
    mode: KeyMode,
    overrides: Option<&BTreeMap<Action, Vec<KeyChord>>>,
) -> Result<BTreeMap<Action, Vec<KeyChord>>, ConfigError> {
    use Action::*;
    let defaults: &[(Action, &[&str])] = match mode {
        KeyMode::Normal => &[
            (Down, &["j", "Down"]),
            (Up, &["k", "Up"]),
            (Jump, &["Enter", "l"]),
            (Search, &["/"]),
            (Filter, &["f"]),
            (Reset, &["Escape"]),
            (Help, &["?"]),
            (Versions, &["u"]),
            (Close, &["q", "Q"]),
        ],
        KeyMode::Search => &[
            (Up, &["Up", "C-p"]),
            (Down, &["Down", "C-n"]),
            (Accept, &["Enter"]),
            (Cancel, &["Escape", "C-c"]),
            (Backspace, &["BSpace"]),
            (Clear, &["C-u"]),
        ],
    };
    let mut keys: BTreeMap<_, _> = defaults
        .iter()
        .map(|(a, k)| {
            (
                *a,
                k.iter()
                    .map(|s| KeyChord::parse(s).unwrap())
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    if let Some(overrides) = overrides {
        for (action, chords) in overrides {
            let field = format!("keys.{}.{action:?}", mode.name()).to_lowercase();
            if !keys.contains_key(action) {
                return Err(ConfigError::invalid(
                    &field,
                    "unsupported action for this mode",
                ));
            }
            if chords.len() > 16 {
                return Err(ConfigError::invalid(&field, "at most 16 chords per action"));
            }
            keys.insert(*action, chords.clone());
        }
    }
    let mut seen = HashSet::new();
    for (action, chords) in &keys {
        let field = format!("keys.{}.{action:?}", mode.name()).to_lowercase();
        for chord in chords {
            if matches!(mode, KeyMode::Search) && matches!(chord, KeyChord::Printable(_)) {
                return Err(ConfigError::invalid(
                    &field,
                    "printable search keys are reserved for query text",
                ));
            }
            if matches!(chord, KeyChord::Control(4))
                || matches!(chord, KeyChord::Control(3)) && *action != Cancel
            {
                return Err(ConfigError::invalid(
                    &field,
                    "Ctrl-C/Ctrl-D are reserved emergency exit keys (Ctrl-C may cancel search)",
                ));
            }
            if !seen.insert(*chord) {
                return Err(ConfigError::invalid(
                    &field,
                    "duplicate or aliased key chord after merging defaults",
                ));
            }
        }
    }
    Ok(keys)
}

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
        "behavior",
        "theme",
        "theme.colors",
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
    let k = config.keys.as_ref();
    resolved_keys(KeyMode::Normal, k.and_then(|k| k.normal.as_ref()))?;
    resolved_keys(KeyMode::Search, k.and_then(|k| k.search.as_ref()))?;
    Ok(())
}

/// Fully resolved behavior. Theme/input application belongs to later tasks.
#[derive(Debug, Clone, PartialEq)]
pub struct AppConfig {
    pub mode: DisplayMode,
    pub sidebar_width: u16,
    pub popup_width: u16,
    pub popup_height: PopupHeight,
    pub notifications: bool,
    pub hide_windows: Option<String>,
    pub theme: ThemeConfig,
    pub normal: BTreeMap<Action, Vec<KeyChord>>,
    pub search: BTreeMap<Action, Vec<KeyChord>>,
    /// Field name to the layer that decided it, named precisely enough to
    /// act on: a winning tmux option prints as `tmux @agenmux-width`.
    pub sources: BTreeMap<&'static str, String>,
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
    let mut result = AppConfig {
        mode: DisplayMode::Split,
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
        sources: BTreeMap::from([
            ("display.mode", "default".into()),
            ("display.sidebar_width", "default".into()),
            ("display.popup_width", "default".into()),
            ("display.popup_height", "default".into()),
            ("behavior.notifications", "default".into()),
            ("behavior.hide_windows", "default".into()),
        ]),
    };
    fn apply(r: &mut AppConfig, f: &FileConfig, source: &str) {
        macro_rules! set {
            ($field:ident, $value:expr, $name:literal) => {
                if let Some(v) = $value {
                    r.$field = v;
                    r.sources.insert($name, source.to_owned());
                }
            };
        }
        if let Some(d) = &f.display {
            set!(mode, d.mode, "display.mode");
            set!(sidebar_width, d.sidebar_width, "display.sidebar_width");
            set!(popup_width, d.popup_width, "display.popup_width");
            set!(popup_height, d.popup_height, "display.popup_height");
        }
        if let Some(b) = &f.behavior {
            set!(notifications, b.notifications, "behavior.notifications");
            if let Some(glob) = &b.hide_windows {
                r.hide_windows = Some(glob.clone());
                r.sources.insert("behavior.hide_windows", source.to_owned());
            }
        }
    }
    apply(&mut result, file, "file");
    result.sources.insert(
        "theme.base",
        if file.theme.as_ref().and_then(|t| t.base).is_some() {
            "file"
        } else {
            "default"
        }
        .into(),
    );
    macro_rules! color_sources {
        ($($field:ident),*) => { $(result.sources.insert(concat!("theme.colors.", stringify!($field)),
            if file.theme.as_ref().and_then(|t| t.colors.as_ref()).and_then(|c| c.$field.as_ref()).is_some() { "file" } else { "theme base" }.into());)* };
    }
    color_sources!(
        header_fg,
        header_bg,
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
        result
            .sources
            .insert(field, if supplied { "file" } else { "default" }.into());
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
        result.sources.insert("display.mode", "CLI".into());
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
    last_read: std::time::Instant,
    reported: HashSet<String>,
}
impl LiveConfig {
    pub fn new(settings: AppConfig) -> Self {
        Self {
            settings,
            file: snapshot().cloned().unwrap_or_default(),
            reload_token: None,
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
            .and_then(|options| resolve(&self.file, &options));
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
    let options = match read_options(|cmd| {
        crate::tmux::command(&cmd.split_whitespace().collect::<Vec<_>>())
    }) {
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
    let installed = crate::tmux::command(&["show-option", "-gqv", "@agenmux-nav-version"])
        .unwrap_or_default();
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

/// Lay names out in aligned columns so a long list stays readable in a pane
/// narrower than the list itself.
fn columns(names: &[&str], per_row: usize, indent: &str) -> String {
    let width = names.iter().map(|name| name.len()).max().unwrap_or(0);
    names
        .chunks(per_row)
        .map(|row| {
            let cells: Vec<String> = row
                .iter()
                .map(|name| format!("{name:width$}"))
                .collect();
            format!("{indent}{}", cells.join("  ").trim_end())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every configurable field, printed from one place so `--help` cannot drift
/// from the file the loader accepts.
pub fn help() -> i32 {
    let normal = |mode| {
        resolved_keys(mode, None)
            .unwrap_or_default()
            .into_keys()
            .map(|action| format!("{action:?}").to_lowercase())
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!(
        r##"agenmux application configuration

  file      $XDG_CONFIG_HOME/agenmux/config.toml
            otherwise $HOME/.config/agenmux/config.toml
  commands  agenmux config check [--effective]   validate, and report sources
            agenmux config reload                apply an edited file

Every key is optional and omitting one keeps its default. A present @agenmux-*
tmux option still wins over the file.

[display]
  mode            split | popup                       (split)
  sidebar_width   1..=10000 cells                     (30)
  popup_width     1..=10000 cells                     (40)
  popup_height    "auto" or 1..=10000 cells           (auto)

[behavior]
  notifications   true | false                        (true)
  hide_windows    glob for the prefix+w picker        (unset: picker untouched)

[theme]
  base            dark | light | terminal             (dark)

[theme.colors]    "default", 0..=255, or "#RRGGBB"; the base fills the rest
{}

[keys.normal]     {}
[keys.search]     {}
  gg and G jump to the first and last visible agent. They are not configurable;
  binding one of those keys to an action replaces that jump.
  Each value replaces that action's default list, and [] unbinds it. Chords are
  one printable ASCII character, Space, Up/Down/Left/Right, Home/End,
  PageUp/PageDown, Enter, Escape, Tab, BSpace, or a C- chord. Reserved:
  C-c and C-d always exit, C-@/C-a/C-b/C-l carry the sidebar's own key
  packets, and C-h/C-j cannot be told apart from BSpace and Enter.
  Printable chords cannot be bound in search mode, where typing owns them."##,
        columns(
            &Palette::default()
                .roles()
                .map(|(name, _)| name),
            3,
            "  ",
        ),
        normal(KeyMode::Normal),
        normal(KeyMode::Search),
    );
    0
}

/// One resolved setting as `check --effective` reports it.
pub struct Row {
    pub name: String,
    pub value: String,
    pub source: String,
}

/// Every setting with its resolved value and the layer that decided it, in the
/// order `--help` documents rather than alphabetically, so the report reads
/// like the file it describes.
pub fn rows(config: &AppConfig) -> Vec<Row> {
    let chords = |list: &Vec<KeyChord>| {
        if list.is_empty() {
            "(unbound)".to_string()
        } else {
            list.iter()
                .map(|chord| chord.tmux_name())
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    let mut out: Vec<(String, String)> = vec![
        (
            "display.mode".into(),
            match config.mode {
                DisplayMode::Split => "split".into(),
                DisplayMode::Popup => "popup".to_string(),
            },
        ),
        ("display.sidebar_width".into(), config.sidebar_width.to_string()),
        ("display.popup_width".into(), config.popup_width.to_string()),
        (
            "display.popup_height".into(),
            match config.popup_height {
                PopupHeight::Auto(_) => "auto".into(),
                PopupHeight::Cells(n) => n.to_string(),
            },
        ),
        ("behavior.notifications".into(), config.notifications.to_string()),
        (
            "behavior.hide_windows".into(),
            // Validated, but still user text: escape before it reaches a terminal.
            config
                .hide_windows
                .as_deref()
                .map_or("(unset)".to_string(), escaped),
        ),
        (
            "theme.base".into(),
            match config.theme.base.unwrap_or(ThemeBase::Dark) {
                ThemeBase::Dark => "dark".into(),
                ThemeBase::Light => "light".into(),
                ThemeBase::Terminal => "terminal".to_string(),
            },
        ),
    ];
    for (role, ink) in Palette::resolve(&config.theme).roles() {
        out.push((format!("theme.colors.{role}"), ink.describe()));
    }
    for (action, list) in &config.normal {
        out.push((
            format!("keys.normal.{action:?}").to_lowercase(),
            chords(list),
        ));
    }
    for (action, list) in &config.search {
        out.push((
            format!("keys.search.{action:?}").to_lowercase(),
            chords(list),
        ));
    }
    out.into_iter()
        .map(|(name, value)| {
            let source = config
                .sources
                .get(name.as_str())
                .cloned()
                .unwrap_or_else(|| "default".into());
            Row { name, value, source }
        })
        .collect()
}

/// Settings the user actually decided: everything a bare default did not.
fn customized(rows: &[Row]) -> usize {
    rows.iter()
        .filter(|row| row.source != "default" && row.source != "theme base")
        .count()
}

fn print_table(rows: &[Row]) {
    let name = rows.iter().map(|r| r.name.len()).max().unwrap_or(0).max(7);
    let value = rows.iter().map(|r| r.value.len()).max().unwrap_or(0).max(5);
    println!("{:name$}  {:value$}  source", "setting", "value");
    for row in rows {
        println!("{:name$}  {:value$}  {}", row.name, row.value, row.source);
    }
}

pub fn effective_check(all: bool) -> i32 {
    match current(None) {
        Ok(config) => {
            if let Some(path) = config_path() {
                println!("{}\n", escaped(&path.to_string_lossy()));
            }
            let all_rows = rows(&config);
            let shown: Vec<&Row> = if all {
                all_rows.iter().collect()
            } else {
                all_rows
                    .iter()
                    .filter(|row| row.source != "default" && row.source != "theme base")
                    .collect()
            };
            if shown.is_empty() {
                println!("every setting is at its default; --all lists them.");
                return 0;
            }
            let owned: Vec<Row> = shown
                .into_iter()
                .map(|row| Row {
                    name: row.name.clone(),
                    value: row.value.clone(),
                    source: row.source.clone(),
                })
                .collect();
            print_table(&owned);
            let rest = all_rows.len() - owned.len();
            if !all && rest > 0 {
                println!("\n{rest} settings are at their defaults; --all lists every one.");
            }
            0
        }
        Err(e) => {
            eprintln!("agenmux: {e}");
            e.exit_code()
        }
    }
}

/// Pure discovery: relative/empty roots never cause project-local searching.
pub fn discover(xdg: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    xdg.filter(|p| p.is_absolute())
        .map(|p| p.join("agenmux/config.toml"))
        .or_else(|| {
            home.filter(|p| p.is_absolute())
                .map(|p| p.join(".config/agenmux/config.toml"))
        })
}
pub fn config_path() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let home = std::env::var_os("HOME");
    discover(
        xdg.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
    )
}
pub fn load() -> Result<FileConfig, ConfigError> {
    match config_path() {
        Some(path) => load_path(&path),
        None => Ok(FileConfig::default()),
    }
}
/// Read-only, nonblocking open; inspect the opened descriptor, not a pre-open stat.
/// Symlinks to regular files are intentionally supported.
pub fn load_path(path: &Path) -> Result<FileConfig, ConfigError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return match fs::symlink_metadata(path) {
                Err(missing) if missing.kind() == io::ErrorKind::NotFound => {
                    // A missing leaf under a dangling directory symlink is not an absent config.
                    for parent in path.ancestors().skip(1) {
                        match fs::symlink_metadata(parent) {
                            Ok(meta) => {
                                if meta.file_type().is_symlink() {
                                    fs::metadata(parent).map_err(|e| ConfigError::io(path, e))?;
                                }
                                break;
                            }
                            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                            Err(e) => return Err(ConfigError::io(path, e)),
                        }
                    }
                    Ok(FileConfig::default())
                }
                Err(other) => Err(ConfigError::io(path, other)),
                Ok(_) => Err(ConfigError::io(path, e)), // includes a dangling final symlink
            };
        }
        Err(e) => return Err(ConfigError::io(path, e)),
    };
    let result = (|| {
        if !file
            .metadata()
            .map_err(|e| ConfigError::io(path, e))?
            .is_file()
        {
            return Err(ConfigError::invalid("file", "must be a regular file"));
        }
        let mut bytes = Vec::new();
        file.take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| ConfigError::io(path, e))?;
        if bytes.len() > MAX_BYTES {
            return Err(ConfigError::invalid("file", "exceeds 65536 bytes"));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| ConfigError::invalid("file", "must be strict UTF-8"))?;
        parse(text)
    })();
    result.map_err(|mut e| {
        e.location = path.to_string_lossy().into_owned();
        e
    })
}

/// Standalone validation: no tmux, detector, hook, or update dependencies.
pub fn check() -> i32 {
    let Some(path) = config_path() else {
        println!("no configuration file; using defaults");
        println!("no absolute XDG or HOME root to look in");
        return 0;
    };
    let shown = escaped(&path.to_string_lossy());
    let file = match load_path(&path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("agenmux: {e}");
            return e.exit_code();
        }
    };
    if !path.exists() {
        println!("no configuration file; using defaults");
        println!("looked for {shown}");
        return 0;
    }
    // No tmux here: count what the file alone decides, not what options win.
    let set = resolve(&file, &Default::default())
        .map(|config| customized(&rows(&config)))
        .unwrap_or(0);
    println!("{shown}: valid");
    match set {
        0 => println!("0 settings differ from the defaults"),
        n => println!(
            "{n} settings differ from the defaults; agenmux config check --effective shows them"
        ),
    }
    0
}

#[cfg(test)]
mod tests {
    #[test]
    fn palette_resolves_only_typed_colors_and_partial_roles() {
        use super::*;
        let dark = Palette::default();
        assert_eq!(dark, Palette::resolve(&parse("").unwrap().theme.unwrap_or_default()));
        let file = parse("[theme.colors]\nworking_bg = 7").unwrap();
        let actual = Palette::resolve(file.theme.as_ref().unwrap());
        let mut expected = dark.clone();
        expected.working_bg = Ink::Typed(Color::Indexed(7));
        assert_eq!(actual, expected);
        assert_eq!(dark.working_fg.fg("1"), "\x1b[1;33m");
        for (value, fg, bg) in [
            ("'default'", "39", "49"),
            ("0", "38;5;0", "48;5;0"),
            ("255", "38;5;255", "48;5;255"),
            ("'#00aAFF'", "38;2;0;170;255", "48;2;0;170;255"),
        ] {
            let file = parse(&format!(
                "[theme.colors]\nworking_fg = {value}\nworking_bg = {value}"
            ))
            .unwrap();
            let p = Palette::resolve(file.theme.as_ref().unwrap());
            assert_eq!(p.working_fg.fg("1"), format!("\x1b[1;{fg}m"));
            assert_eq!(p.working_bg.bg(), format!("\x1b[{bg}m"));
        }
        for value in [
            "'\x1b[31m'",
            "'\x1b]0;title\x07'",
            "'\x1bPpayload\x1b\\'",
            "\"\\u001b]52;c;AAAA\\u0007\"",
            "'red'",
            "'#ffffff\n'",
            "-1",
            "256",
        ] {
            assert!(parse(&format!("[theme.colors]\nworking_bg = {value}")).is_err());
        }
        let terminal = Palette::resolve(&ThemeConfig {
            base: Some(ThemeBase::Terminal),
            colors: None,
        });
        for state in ["blocked", "working", "idle", "done"] {
            for focused in [false, true] {
                assert_eq!(terminal.state_bg(state, focused), "\x1b[49m");
            }
        }
        assert_eq!(terminal.header_bg.bg(), "\x1b[49m");
        let light = Palette::resolve(&ThemeConfig {
            base: Some(ThemeBase::Light),
            colors: None,
        });
        assert_eq!(light.text_fg, Ink::Typed(Color::Rgb(32, 32, 32)));
        assert_eq!(light.working_bg, Ink::Typed(Color::Rgb(255, 240, 204)));
    }
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static ID: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "agenmux-schema-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn defaults_and_empty_layer_semantics() {
        let empty = parse("").unwrap();
        let default = resolve(&empty, &BTreeMap::new()).unwrap();
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
            .filter(|(field, _)| field.starts_with("display.")
                || field.starts_with("behavior."))
            .all(|(_, source)| source.starts_with("tmux @agenmux-")));
    }

    #[test]
    fn precedence_cli_canonical_legacy_file_defaults_and_unset() {
        let file = parse("[display]\nmode='popup'\nsidebar_width=22\n[behavior]\nnotifications=false").unwrap();
        let mut options = BTreeMap::new();
        assert_eq!(resolve(&file, &options).unwrap().sidebar_width, 22);
        options.insert("@agents-mon-width".into(), "44".into());
        assert_eq!(resolve(&file, &options).unwrap().sidebar_width, 44);
        options.insert("@agenmux-width".into(), "55".into());
        let config = resolve_cli(&file, &options, Some("split")).unwrap();
        assert_eq!(config.sidebar_width, 55);
        assert_eq!(config.mode, DisplayMode::Split);
        assert_eq!(config.sources["display.mode"], "CLI");
        assert_eq!(config.sources["display.sidebar_width"], "tmux @agenmux-width");
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
    fn discovery_uses_only_absolute_roots() {
        let p = |s| Some(Path::new(s));
        assert_eq!(
            discover(p("/xdg"), p("/home")),
            Some("/xdg/agenmux/config.toml".into())
        );
        for xdg in [None, p(""), p("relative")] {
            assert_eq!(
                discover(xdg, p("/home")),
                Some("/home/.config/agenmux/config.toml".into())
            );
            for home in [None, p(""), p("relative")] {
                assert_eq!(discover(xdg, home), None);
            }
        }
        assert_eq!(
            discover(p("/xdg"), None),
            Some("/xdg/agenmux/config.toml".into())
        );
    }

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
[theme]
base = "light"
[theme.colors]
header_bg = "#eeeeee"
header_fg = "#202020"
working_bg = "#fff0cc"
working_bg_unfocused = "#f7f2e5"
working_fg = "#775500"
[keys.normal]
down = ["j", "Down"]
up = ["k", "Up"]
jump = ["Enter", "l"]
search = ["/"]
filter = ["f"]
reset = ["Escape"]
help = ["?"]
versions = ["u"]
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
        for value in ["'off'", "0", "60000"] {
        }
        for value in [
            "'auto'",
            "'0'",
            "-1",
            "60001",
            "1.5",
            "nan",
            "inf",
            "true",
            "4294967296",
        ] {
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

    #[test]
    fn key_grammar_aliases_and_emergency_reservations() {
        for key in [
            "j", ";", "Space", "Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown",
            "Enter", "Escape", "Tab", "BSpace", "C-k", "C-p", "C-u", "C-g", "C-_",
        ] {
            assert!(KeyChord::parse(key).is_ok(), "{key}");
        }
        for key in [
            "", "é", "F1", "M-a", "C-M-a", "C-1", "C-S-a", "\u{1b}[A", "\n", "jj",
            // tmux binds these as their own keys, so the terminal's fold into
            // Enter/BSpace would install a binding for a different key.
            "C-j", "C-J", "C-h", "C-H",
        ] {
            assert!(KeyChord::parse(key).is_err(), "{key}");
        }
        for (a, b) in [
            ("C-i", "Tab"),
            ("C-m", "Enter"),
            ("C-[", "Escape"),
            ("C-?", "BSpace"),
            ("C-K", "C-k"),
            (" ", "Space"),
        ] {
            assert_eq!(KeyChord::parse(a), KeyChord::parse(b));
        }
        for source in [
            "[keys.normal]\nhelp = ['C-c']",
            "[keys.normal]\nclose = ['C-d']",
            "[keys.search]\ncancel = ['C-d']",
            "[keys.search]\nup = ['x']",
            "[keys.search]\nclear = ['Space']",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn intercepted_controls_are_rejected_in_every_mode() {
        for key in [
            "C-o", "C-q", "C-r", "C-s", "C-t", "C-v", "C-y", "C-z", "C-\\",
        ] {
            for key in [
                key.to_owned(),
                format!("C-{}", key[2..].to_ascii_uppercase()),
            ] {
                assert!(KeyChord::parse(&key).is_err(), "{key}");
                for (mode, action) in [
                    ("normal", "help"),
                    ("search", "clear"),
                ] {
                    assert!(parse(&format!("[keys.{mode}]\n{action} = ['{key}']")).is_err());
                }
            }
        }
        assert!(parse("").is_ok()); // Includes the default C-c cancel and C-u clear.
        assert!(parse("[keys.search]\ncancel = ['C-c']").is_ok());
    }

    #[test]
    fn merged_keys_replace_unbind_and_detect_collisions() {
        for source in [
            "[keys.normal]\ndown = ['k']",
            "[keys.normal]\njump = ['C-m', 'Enter']",
            "[keys.normal]\nhelp = ['C-j']",
            "[keys.search]\nclear = ['C-j']",
            "[keys.search]\nclear = ['C-h']",
            "[keys.search]\nbackspace = ['C-h', 'BSpace']",
            "[keys.normal]\ndown = ['Tab', 'C-i']",
            "[keys.prefix]\ntoggle = ['e']",
            "[keys.search]\nup = ['C-n']",
            "[keys.prefix]\ntoggle = ['A', 'A']",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
        let c = parse("[keys.normal]\ndown = ['k']\nup = []").unwrap();
        let resolved = resolved_keys(KeyMode::Normal, c.keys.unwrap().normal.as_ref()).unwrap();
        assert_eq!(resolved[&Action::Down], vec![KeyChord::Printable(b'k')]);
        assert!(resolved[&Action::Up].is_empty());
        assert_eq!(resolved[&Action::Close].len(), 2);
        let list = (b'0'..=b'?')
            .map(|b| format!("'{}'", b as char))
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse(&format!("[keys.normal]\nhelp = [{list}]\nsearch = []")).is_ok());
        assert!(parse(&format!("[keys.normal]\nhelp = [{list}, 'Z']\nsearch = []")).is_err());
    }

    #[test]
    fn loader_checks_opened_files_bytes_and_utf8() {
        let dir = Temp::new();
        let path = dir.0.join("config.toml");
        assert_eq!(load_path(&path).unwrap(), FileConfig::default());
        fs::write(&path, b"\xff").unwrap();
        assert_eq!(load_path(&path).unwrap_err().exit_code(), 2);
        fs::write(&path, " ".repeat(MAX_BYTES)).unwrap();
        assert!(load_path(&path).is_ok());
        fs::write(&path, " ".repeat(MAX_BYTES + 1)).unwrap();
        assert!(load_path(&path).is_err());
        assert!(parse(&" ".repeat(MAX_BYTES + 1)).is_err());
        fs::write(&path, "version = 1").unwrap();
        let link = dir.0.join("link");
        symlink(&path, &link).unwrap();
        assert_eq!(load_path(&link).unwrap().version, Some(1));
        fs::remove_file(&path).unwrap();
        assert_eq!(load_path(&link).unwrap_err().exit_code(), 1);
        assert_eq!(
            load_path(&link.join("nested/config.toml"))
                .unwrap_err()
                .exit_code(),
            1
        );
        assert_eq!(load_path(&dir.0).unwrap_err().exit_code(), 2);
        assert_eq!(
            load_path(Path::new("/dev/null")).unwrap_err().exit_code(),
            2
        );
        let fifo = dir.0.join("fifo");
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert_eq!(load_path(&fifo).unwrap_err().exit_code(), 2);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn diagnostics_escape_controls_and_do_not_render_source() {
        let dir = Temp::new();
        let path = dir.0.join("bad\n\u{1b}.toml");
        fs::write(
            &path,
            "[display]\nsidebar_width = 0\n# private source sentinel",
        )
        .unwrap();
        let diagnostic = load_path(&path).unwrap_err().to_string();
        assert!(diagnostic.contains("display.sidebar_width"));
        assert!(!diagnostic.contains('\n') && !diagnostic.contains('\u{1b}'));
        assert!(!diagnostic.contains("private source sentinel"));
        let diagnostic = parse("[theme]\ncommand = 'private source sentinel'")
            .unwrap_err()
            .to_string();
        assert!(!diagnostic.contains("private source sentinel"));
        for source in [
            "version = 'private source sentinel'",
            "[display]\nsidebar_width = 'private source sentinel'",
            "[behavior]\nnotifications = 'private source sentinel'",
            "[keys.normal]\nhelp = 'private source sentinel'",
            "[theme]\nbase = 'private source sentinel'",
            "[behavior]\nnotifications = '''\n[private source sentinel]\nprivate source sentinel = value\n'''",
            "version = @private source sentinel",
        ] {
            let diagnostic = parse(source).unwrap_err().to_string();
            assert!(!diagnostic.contains("private source sentinel"));
            assert!(diagnostic.contains("line ") && diagnostic.contains("column "));
            assert!(diagnostic.contains("invalid TOML syntax") || diagnostic.contains("invalid configuration schema"));
        }
    }
}

#[cfg(test)]
mod chord_tests {
    use super::*;

    #[test]
    fn chords_have_tmux_names_and_labels() {
        for (source, name, label, short) in [
            ("j", "j", "j", "j"),
            ("Space", "Space", "Space", "Space"),
            (";", "\\;", ";", ";"),
            ("C-u", "C-u", "C-u", "^u"),
            ("C-]", "C-]", "C-]", "^]"),
            ("C-m", "Enter", "Enter", "↵"),
            ("C-i", "Tab", "Tab", "Tab"),
            ("C-?", "BSpace", "BSpace", "BSpace"),
            ("Escape", "Escape", "Esc", "esc"),
            ("PageUp", "PageUp", "PgUp", "PgUp"),
            ("Up", "Up", "↑", "↑"),
        ] {
            let chord = KeyChord::parse(source).unwrap();
            assert_eq!(chord.tmux_name(), name, "{source}");
            assert_eq!(chord.label(false), label, "{source}");
            assert_eq!(chord.label(true), short, "{source}");
        }
        // Bytes the sidebar transport claims for its own packets: accepting
        // them would bind a key that only works in split mode.
        for reserved in ["C-@", "C-a", "C-b", "C-e", "C-f", "C-l"] {
            assert_eq!(
                KeyChord::parse(reserved),
                Err("control chord is reserved by the input protocol"),
                "{reserved}"
            );
        }
        let keys = resolved_keys(KeyMode::Normal, None).unwrap();
        assert_eq!(action_for(&keys, KeyChord::Printable(b'j')), Some(Action::Down));
        assert_eq!(action_for(&keys, KeyChord::Down), Some(Action::Down));
        assert_eq!(action_for(&keys, KeyChord::Printable(b'n')), None);
    }

    /// The shipped example spells out every default, so it must resolve to them.
    #[test]
    fn shipped_example_spells_out_the_defaults() {
        let example = parse(include_str!("../examples/config.toml")).unwrap();
        let mut resolved = resolve(&example, &Default::default()).unwrap();
        let defaults = resolve(&Default::default(), &Default::default()).unwrap();
        assert_eq!(Palette::resolve(&resolved.theme), Palette::resolve(&defaults.theme));
        resolved.theme = defaults.theme.clone();
        resolved.sources = defaults.sources.clone();
        assert_eq!(resolved, defaults);
    }
}
