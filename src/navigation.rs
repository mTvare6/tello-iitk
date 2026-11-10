use crate::model::*;

#[derive(Clone)]
pub struct View {
    pub location: Location,
    pub page: Page,
    pub selected: usize,
    pub filter: String,
    pub scroll: u16,
    pub text_focus: bool,
}

impl View {
    pub fn new(location: Location, page: Page) -> Self {
        let text_focus = !page.text.is_empty() && page.entries.is_empty();

        Self {
            location,
            page,
            selected: 0,
            filter: String::new(),
            scroll: 0,
            text_focus,
        }
    }

    pub fn visible(&self) -> Vec<&Entry> {
        let query = self.filter.to_lowercase();

        self.page
            .entries
            .iter()
            .filter(|e| {
                format!("{} {}", e.title, e.subtitle)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.visible().get(self.selected).copied()
    }

    pub fn move_by(&mut self, delta: isize) {
        let n = self.visible().len();
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(n.saturating_sub(1));
    }

    pub fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.selected = 0;
    }
}

#[derive(Default)]
pub struct Navigation {
    pub history: Vec<View>,
    pub current: Option<View>,
}

impl Navigation {
    pub fn push(&mut self, location: Location, page: Page) {
        if let Some(old) = self.current.take() {
            self.history.push(old);
        }

        self.current = Some(View::new(location, page));
    }

    pub fn back(&mut self) -> bool {
        if let Some(previous) = self.history.pop() {
            self.current = Some(previous);

            true
        } else {
            false
        }
    }
}
