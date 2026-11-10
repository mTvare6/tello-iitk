#[derive(Clone, Copy, Default)]
pub struct Cursor {
    pub position: usize,
}

#[derive(Clone, Copy)]
pub enum Edit {
    Insert(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
}

fn offset(value: &str, position: usize) -> usize {
    value
        .char_indices()
        .nth(position)
        .map(|(i, _)| i)
        .unwrap_or(value.len())
}

impl Cursor {
    pub fn apply(&mut self, value: &mut String, edit: Edit, max: usize) {
        let count = value.chars().count();
        self.position = self.position.min(count);

        match edit {
            Edit::Insert(c) if !c.is_control() && count < max => {
                value.insert(offset(value, self.position), c);
                self.position += 1;
            }
            Edit::Backspace if self.position > 0 => {
                let end = offset(value, self.position);
                self.position -= 1;
                value.replace_range(offset(value, self.position)..end, "");
            }
            Edit::Delete if self.position < count => {
                value.replace_range(
                    offset(value, self.position)..offset(value, self.position + 1),
                    "",
                );
            }
            Edit::Left => self.position = self.position.saturating_sub(1),
            Edit::Right => self.position = (self.position + 1).min(count),
            Edit::Home => self.position = 0,
            Edit::End => self.position = count,
            _ => {}
        }
    }

    pub fn display(&self, value: &str, width: usize, masked: bool, focused: bool) -> String {
        if width == 0 {
            return String::new();
        }

        let count = value.chars().count();
        let position = self.position.min(count);
        let room = width.saturating_sub(usize::from(focused));
        let start = if focused {
            position.saturating_sub(room)
        } else {
            0
        };

        let mut out = String::new();

        for (index, c) in value.chars().enumerate().skip(start).take(room) {
            if focused && index == position {
                out.push('▏');
            }

            out.push(if masked { '•' } else { c });
        }

        if focused && position >= start + room.min(count.saturating_sub(start)) {
            out.push('▏');
        }

        out
    }
}
