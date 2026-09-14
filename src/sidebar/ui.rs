pub(super) struct TopBar<'a> {
    palette: &'a crate::app_config::Palette,
    focused: bool,
    inherited: bool,
}

impl<'a> TopBar<'a> {
    pub(super) fn new(
        palette: &'a crate::app_config::Palette,
        focused: bool,
        inherited: bool,
    ) -> Self {
        Self {
            palette,
            focused,
            inherited,
        }
    }

    pub(super) fn foreground(&self, effect: &str) -> String {
        if !self.focused && self.inherited {
            self.palette.header_bg.fg(effect)
        } else {
            self.palette.header_fg.fg(effect)
        }
    }

    pub(super) fn background(&self) -> String {
        if self.focused {
            self.palette.header_bg.bg()
        } else {
            String::new()
        }
    }
}

pub(super) struct Label<'a> {
    text: &'a str,
}

impl<'a> Label<'a> {
    pub(super) fn new(text: &'a str) -> Self {
        Self { text }
    }

    pub(super) fn render(&self, mark: &str, value: &str) -> String {
        format!("{mark} {}: {value}", self.text)
    }
}

pub(super) struct Action<'a> {
    label: &'a str,
}

impl<'a> Action<'a> {
    pub(super) fn new(label: &'a str) -> Self {
        Self { label }
    }

    pub(super) fn render(&self, selected: bool) -> String {
        format!("{} {}", if selected { "❯" } else { " " }, self.label)
    }
}

pub(super) struct Select {
    options: &'static [&'static str],
    selected: usize,
}

impl Select {
    pub(super) fn new(value: &str, options: &'static [&'static str]) -> Self {
        let selected = options
            .iter()
            .position(|option| *option == value)
            .unwrap_or(0);
        Self { options, selected }
    }

    pub(super) fn move_by(&mut self, direction: isize) {
        self.selected =
            (self.selected as isize + direction).rem_euclid(self.options.len() as isize) as usize;
    }

    pub(super) fn value(&self) -> &str {
        self.options[self.selected]
    }

    pub(super) fn render(&self) -> impl Iterator<Item = String> + '_ {
        self.options.iter().enumerate().map(|(index, option)| {
            format!(
                "    {} {option}",
                if index == self.selected { "❯" } else { " " }
            )
        })
    }

    pub(super) fn height(&self) -> usize {
        self.options.len()
    }
}

pub(super) struct TextEdit {
    value: String,
}

impl TextEdit {
    pub(super) fn new(value: String) -> Self {
        Self { value }
    }

    pub(super) fn value(&self) -> &str {
        &self.value
    }

    pub(super) fn push(&mut self, text: &str) {
        if self.value.len() + text.len() <= 512 {
            self.value.push_str(text);
        }
    }

    pub(super) fn backspace(&mut self) {
        self.value.pop();
    }

    pub(super) fn clear(&mut self) {
        self.value.clear();
    }
}

pub(super) enum Editor {
    Select(Select),
    TextEdit(TextEdit),
}

impl Editor {
    pub(super) fn value(&self) -> &str {
        match self {
            Self::Select(select) => select.value(),
            Self::TextEdit(edit) => edit.value(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_bar_centralizes_focus_aware_colors() {
        let palette = crate::app_config::Palette::default();
        let focused = TopBar::new(&palette, true, true);
        assert_eq!(focused.foreground("1"), palette.header_fg.fg("1"));
        assert_eq!(focused.background(), palette.header_bg.bg());

        let inherited = TopBar::new(&palette, false, true);
        assert_eq!(inherited.foreground("1"), palette.header_bg.fg("1"));
        assert_eq!(inherited.background(), "");

        let explicit = TopBar::new(&palette, false, false);
        assert_eq!(explicit.foreground("1"), palette.header_fg.fg("1"));
        assert_eq!(explicit.background(), "");
    }

    #[test]
    fn select_moves_wraps_and_renders_inline() {
        let mut select = Select::new("dark", &["dark", "light", "terminal"]);
        select.move_by(-1);

        assert_eq!(select.value(), "terminal");
        assert_eq!(
            select.render().collect::<Vec<_>>(),
            ["      dark", "      light", "    ❯ terminal"]
        );
    }

    #[test]
    fn text_edit_caps_input_and_edits_value() {
        let mut edit = TextEdit::new("value".into());
        edit.push("!");
        edit.backspace();
        assert_eq!(edit.value(), "value");
        edit.clear();
        edit.push(&"x".repeat(513));
        assert_eq!(edit.value(), "");
    }
}
