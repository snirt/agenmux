use super::E;

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

pub(super) fn bar(line: &str, bg: &str, cols: usize, width: usize) -> String {
    if bg.is_empty() {
        return line.into();
    }
    let body = line.replace(&format!("{E}[0m"), &format!("{E}[0m{bg}"));
    format!("{bg}{body}{}{E}[0m", " ".repeat(cols.saturating_sub(width)))
}

pub(super) struct SelectedRow<'a> {
    palette: &'a crate::app_config::Palette,
    selected: bool,
    focused: bool,
}

impl<'a> SelectedRow<'a> {
    pub(super) fn new(
        palette: &'a crate::app_config::Palette,
        selected: bool,
        focused: bool,
    ) -> Self {
        Self {
            palette,
            selected,
            focused,
        }
    }

    pub(super) fn render(&self, line: &str, cols: usize) -> String {
        if !self.selected {
            return line.into();
        }
        let bg = self.palette.state_bg("idle", self.focused);
        if bg.is_empty() || bg == format!("{E}[49m") {
            let icon = self.palette.state_fg("idle").fg("");
            return format!(
                "{icon}{E}[7m{line}{}{E}[0m",
                " ".repeat(cols.saturating_sub(line.chars().count()))
            );
        }
        bar(line, &bg, cols, line.chars().count())
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

    pub(super) fn select(&mut self, index: usize) -> bool {
        if index >= self.options.len() {
            return false;
        }
        self.selected = index;
        true
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
    cursor: usize, // UTF-8 byte boundary
    limit: usize,  // character count
}

impl TextEdit {
    pub(super) fn new(value: String) -> Self {
        Self::with_limit(value, 512)
    }

    pub(super) fn with_limit(value: String, limit: usize) -> Self {
        let cursor = value.len();
        Self {
            value,
            cursor,
            limit,
        }
    }

    pub(super) fn value(&self) -> &str {
        &self.value
    }

    pub(super) fn display(&self, mark: &str) -> String {
        format!(
            "{}{mark}{}",
            &self.value[..self.cursor],
            &self.value[self.cursor..]
        )
    }
    pub(super) fn display_clipped(&self, mark: &str, width: usize) -> String {
        let before: Vec<char> = self.value[..self.cursor].chars().collect();
        let after: Vec<char> = self.value[self.cursor..].chars().collect();
        let room = width.saturating_sub(1);
        let right = after.len().min(room / 2);
        let left = before.len().min(room - right);
        let right = after.len().min(room - left);
        let mut left_part: Vec<char> = before[before.len() - left..].to_vec();
        let mut right_part: Vec<char> = after[..right].to_vec();
        if left < before.len() && !left_part.is_empty() {
            left_part[0] = '…';
        }
        if right < after.len() && !right_part.is_empty() {
            *right_part.last_mut().unwrap() = '…';
        }
        format!(
            "{}{mark}{}",
            left_part.iter().collect::<String>(),
            right_part.iter().collect::<String>()
        )
    }

    pub(super) fn push(&mut self, text: &str) {
        let room = self.limit.saturating_sub(self.value.chars().count());
        let clean: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(room)
            .collect();
        self.value.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    pub(super) fn backspace(&mut self) {
        if let Some((start, _)) = self.value[..self.cursor].char_indices().next_back() {
            self.value.drain(start..self.cursor);
            self.cursor = start;
        }
    }

    pub(super) fn delete(&mut self) {
        if let Some(c) = self.value[self.cursor..].chars().next() {
            self.value.drain(self.cursor..self.cursor + c.len_utf8());
        }
    }

    pub(super) fn move_left(&mut self) {
        if let Some((start, _)) = self.value[..self.cursor].char_indices().next_back() {
            self.cursor = start;
        }
    }

    pub(super) fn move_right(&mut self) {
        if let Some(c) = self.value[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    pub(super) fn home(&mut self) {
        self.cursor = 0;
    }

    pub(super) fn end(&mut self) {
        self.cursor = self.value.len();
    }
    pub(super) fn handle(&mut self, key: &crate::input::Key) -> bool {
        use crate::input::Key;
        match key {
            Key::Text(text) => self.push(text),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Left => self.move_left(),
            Key::Right => self.move_right(),
            Key::Home => self.home(),
            Key::End => self.end(),
            Key::ClearSearch => self.clear(),
            _ => return false,
        }
        true
    }

    pub(super) fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }
}

impl From<&str> for TextEdit {
    fn from(value: &str) -> Self {
        Self::with_limit(value.into(), 256)
    }
}

impl From<String> for TextEdit {
    fn from(value: String) -> Self {
        Self::with_limit(value, 256)
    }
}

impl std::ops::Deref for TextEdit {
    type Target = str;
    fn deref(&self) -> &str {
        self.value()
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
    fn selected_row_fills_width_with_theme_or_reverse_background() {
        let palette = crate::app_config::Palette::default();
        let selected = SelectedRow::new(&palette, true, true).render("❯ mode", 10);
        assert!(selected.starts_with(&palette.state_bg("idle", true)));
        assert!(selected.ends_with(&format!("    {E}[0m")));
        assert_eq!(
            SelectedRow::new(&palette, false, true).render("  mode", 10),
            "  mode"
        );

        let file = crate::app_config::parse("[theme]\nbase = 'terminal'").unwrap();
        let terminal = crate::app_config::Palette::resolve(file.theme.as_ref().unwrap());
        let selected = SelectedRow::new(&terminal, true, true).render("❯ mode", 10);
        assert_eq!(
            selected,
            format!("{}{E}[7m❯ mode    {E}[0m", terminal.state_fg("idle").fg(""))
        );
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
    fn text_edit_inserts_and_deletes_at_unicode_cursor() {
        let mut edit = TextEdit::with_limit("a界c".into(), 5);
        edit.move_left();
        edit.move_left();
        assert_eq!(edit.display("▏"), "a▏界c");
        edit.push("é\n🙂!");
        assert_eq!(edit.value(), "aé🙂界c");
        edit.backspace();
        edit.delete();
        assert_eq!(edit.value(), "aéc");
        edit.home();
        edit.move_right();
        assert_eq!(edit.display("▏"), "a▏éc");
        edit.end();
        edit.push("!");
        assert_eq!(edit.value(), "aéc!");
        edit.clear();
        edit.push(&"x".repeat(513));
        assert_eq!(edit.value().len(), 5);
        edit = TextEdit::with_limit("abcdefghijk".into(), 20);
        edit.home();
        edit.move_right();
        edit.move_right();
        assert_eq!(edit.display_clipped("▏", 5), "ab▏c…");
        edit.end();
        assert_eq!(edit.display_clipped("▏", 5), "…ijk▏");
    }
}
