#[cfg(not(target_arch = "wasm32"))]
pub use crossterm::event::*;

#[cfg(target_arch = "wasm32")]
pub use browser::*;

#[cfg(target_arch = "wasm32")]
mod browser {
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum KeyCode {
        BackTab,
        Backspace,
        Char(char),
        Delete,
        Down,
        End,
        Enter,
        Esc,
        F(u8),
        Home,
        Left,
        PageDown,
        PageUp,
        Right,
        Tab,
        Up,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum KeyEventKind {
        Press,
        Release,
    }

    #[derive(Clone, Copy)]
    pub struct KeyModifiers(u8);

    impl KeyModifiers {
        pub const NONE: Self = Self(0);
        pub const SHIFT: Self = Self(1);
        pub const CONTROL: Self = Self(2);
        pub const ALT: Self = Self(4);

        pub fn contains(self, other: Self) -> bool {
            self.0 & other.0 == other.0
        }
        pub fn intersects(self, other: Self) -> bool {
            self.0 & other.0 != 0
        }
    }

    impl std::ops::BitOr for KeyModifiers {
        type Output = Self;
        fn bitor(self, rhs: Self) -> Self {
            Self(self.0 | rhs.0)
        }
    }

    impl std::ops::BitOrAssign for KeyModifiers {
        fn bitor_assign(&mut self, rhs: Self) {
            self.0 |= rhs.0;
        }
    }

    pub struct KeyEvent {
        pub code: KeyCode,
        pub modifiers: KeyModifiers,
        pub kind: KeyEventKind,
    }

    impl KeyEvent {
        pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
            Self {
                code,
                modifiers,
                kind: KeyEventKind::Press,
            }
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum MouseButton {
        Left,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum MouseEventKind {
        Down(MouseButton),
        Moved,
        ScrollDown,
        ScrollUp,
    }

    pub struct MouseEvent {
        pub kind: MouseEventKind,
        pub column: u16,
        pub row: u16,
    }
}
