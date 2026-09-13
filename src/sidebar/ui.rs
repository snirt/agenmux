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
