//! Theme schema and the resolved palette the renderer draws with.

use super::*;

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
    pub pane_bg: Option<Color>,
    pub selected_bg: Option<Color>,
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
    pub pane_bg: Ink,
    pub selected_bg: Ink,
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
    pub fn roles(&self) -> [(&'static str, Ink); 20] {
        [
            ("header_fg", self.header_fg),
            ("header_bg", self.header_bg),
            ("pane_bg", self.pane_bg),
            ("selected_bg", self.selected_bg),
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
            pane_bg: Typed(Color::Indexed(236)),
            selected_bg: Typed(Color::Indexed(238)),
            text_fg: Inherited,
            muted_fg: Typed(Color::Indexed(245)),
            accent_fg: Basic(4),
            error_fg: Inherited,
            blocked_fg: Basic(1),
            blocked_bg: rgb(42, 16, 16),
            blocked_bg_unfocused: rgb(27, 10, 10),
            working_fg: Basic(3),
            working_bg: rgb(38, 32, 16),
            working_bg_unfocused: rgb(25, 20, 10),
            idle_fg: Basic(2),
            idle_bg: rgb(15, 36, 16),
            idle_bg_unfocused: rgb(9, 23, 10),
            done_fg: Basic(2),
            done_bg: rgb(15, 36, 16),
            done_bg_unfocused: rgb(9, 23, 10),
        };
        match theme.base.unwrap_or(ThemeBase::Dark) {
            ThemeBase::Dark => {}
            ThemeBase::Light => {
                p.header_fg = rgb(32, 32, 32);
                p.header_bg = rgb(238, 238, 238);
                p.pane_bg = rgb(238, 238, 238);
                p.selected_bg = rgb(218, 226, 242);
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
                p.done_fg = p.idle_fg;
                p.done_bg = p.idle_bg;
                p.done_bg_unfocused = p.idle_bg_unfocused;
            }
            ThemeBase::Terminal => {
                p.error_fg = Basic(1);
                p.header_fg = Typed(Color::Default);
                p.text_fg = Typed(Color::Default);
                p.muted_fg = Typed(Color::Default);
                p.header_bg = Typed(Color::Default);
                p.pane_bg = Typed(Color::Default);
                // Every other fill here is the default background; the
                // terminal's own grey keeps the selection visible.
                p.selected_bg = Typed(Color::Indexed(8));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_resolves_only_typed_colors_and_partial_roles() {
        let dark = Palette::default();
        assert_eq!(
            dark,
            Palette::resolve(&parse("").unwrap().theme.unwrap_or_default())
        );
        let file = parse("[theme.colors]\nworking_bg = 7").unwrap();
        let actual = Palette::resolve(file.theme.as_ref().unwrap());
        let mut expected = dark.clone();
        expected.working_bg = Ink::Typed(Color::Indexed(7));
        assert_eq!(actual, expected);
        let pane_file = parse("[theme.colors]\npane_bg = 238").unwrap();
        assert_eq!(
            Palette::resolve(pane_file.theme.as_ref().unwrap()).pane_bg,
            Ink::Typed(Color::Indexed(238))
        );
        assert_eq!(dark.pane_bg, Ink::Typed(Color::Indexed(236)));
        assert_eq!(dark.working_fg.fg("1"), "\x1b[1;33m");
        assert_eq!(
            dark.blocked_bg_unfocused,
            Ink::Typed(Color::Rgb(27, 10, 10))
        );
        assert_eq!(
            dark.working_bg_unfocused,
            Ink::Typed(Color::Rgb(25, 20, 10))
        );
        assert_eq!(dark.idle_bg_unfocused, Ink::Typed(Color::Rgb(9, 23, 10)));
        assert_eq!(dark.done_bg_unfocused, dark.idle_bg_unfocused);
        assert_eq!(dark.working_bg, Ink::Typed(Color::Rgb(38, 32, 16)));
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
        assert_eq!(terminal.pane_bg.bg(), "\x1b[49m");
        let light = Palette::resolve(&ThemeConfig {
            base: Some(ThemeBase::Light),
            colors: None,
        });
        assert_eq!(light.text_fg, Ink::Typed(Color::Rgb(32, 32, 32)));
        assert_eq!(light.pane_bg, Ink::Typed(Color::Rgb(238, 238, 238)));
        assert_eq!(light.working_bg, Ink::Typed(Color::Rgb(255, 240, 204)));
    }
}
