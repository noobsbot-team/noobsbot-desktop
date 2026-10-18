//! The single-line text field's editing (D8): typing, Backspace/Delete, Left/Right/Home/End and select-all, on a
//! `String` and a caret. Drawing lives in `render::ui`; no clipboard (a secure field must not copy anyway).

use smithay::input::keyboard::Keysym;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextField {
    pub text: String,
    /// Byte index into `text`, always on a char boundary.
    pub caret: usize,
    /// Shown as bullets (passwords).
    pub secure: bool,
    /// Everything is selected (Ctrl+A); the next edit replaces it, an arrow key drops it.
    pub all: bool,
}

/// What a key did to the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// The text changed.
    Changed,
    /// Only the caret or selection moved: redraw.
    Moved,
    /// Return / Enter.
    Submit,
    Cancel,
    /// Not a field key: pass it on.
    Ignored,
}

impl TextField {
    pub fn secure() -> Self {
        Self { secure: true, ..Self::default() }
    }

    /// What is drawn: the text, or one bullet per char when secure.
    pub fn display(&self) -> String {
        if self.secure { "•".repeat(self.text.chars().count()) } else { self.text.clone() }
    }

    /// The caret as a byte index into `display()`.
    pub fn display_caret(&self) -> usize {
        if self.secure { self.text[..self.caret].chars().count() * '•'.len_utf8() } else { self.caret }
    }

        /// One key press: `utf8` is what the key types (xkb's string for it), `ctrl` whether Ctrl is held.
    pub fn key(&mut self, keysym: Keysym, utf8: Option<&str>, ctrl: bool) -> Edit {
        let prev = self.text[..self.caret].chars().next_back().map_or(0, char::len_utf8);
        let next = self.text[self.caret..].chars().next().map_or(0, char::len_utf8);
        match keysym {
            Keysym::Return | Keysym::KP_Enter => Edit::Submit,
            Keysym::Escape => Edit::Cancel,
            Keysym::a | Keysym::A if ctrl => self.moved(true, self.text.len()),
            Keysym::BackSpace | Keysym::Delete | Keysym::KP_Delete if self.all => self.replace(""),
            Keysym::BackSpace if prev > 0 => {
                self.caret -= prev;
                self.text.remove(self.caret);
                Edit::Changed
            }
            Keysym::Delete | Keysym::KP_Delete if next > 0 => {
                self.text.remove(self.caret);
                Edit::Changed
            }
            Keysym::BackSpace | Keysym::Delete | Keysym::KP_Delete => Edit::Moved,
            Keysym::Left | Keysym::KP_Left => self.moved(false, if self.all { 0 } else { self.caret - prev }),
            Keysym::Right | Keysym::KP_Right => self.moved(false, if self.all { self.text.len() } else { self.caret + next }),
            Keysym::Home | Keysym::KP_Home => self.moved(false, 0),
            Keysym::End | Keysym::KP_End => self.moved(false, self.text.len()),
            _ => match utf8.filter(|s| !ctrl && !s.is_empty() && !s.chars().any(char::is_control)) {
                Some(typed) => self.replace(typed),
                None => Edit::Ignored,
            },
        }
    }

        fn moved(&mut self, all: bool, caret: usize) -> Edit {
        (self.all, self.caret) = (all, caret);
        Edit::Moved
    }

    /// Types `typed` at the caret, over the selection if everything is selected.
        fn replace(&mut self, typed: &str) -> Edit {
        if self.all {
            (self.text, self.caret, self.all) = (String::new(), 0, false);
        }
        self.text.insert_str(self.caret, typed);
        self.caret += typed.len();
        Edit::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(field: &mut TextField, s: &str) {
        for c in s.chars() {
            assert_eq!(field.key(Keysym::NoSymbol, Some(&c.to_string()), false), Edit::Changed);
        }
    }

    #[test]
    fn typing_and_moving() {
        let mut f = TextField::default();
        typed(&mut f, "héllo");
        assert_eq!((f.text.as_str(), f.caret), ("héllo", 6));
        f.key(Keysym::Left, None, false);
        f.key(Keysym::Left, None, false);
        f.key(Keysym::Left, None, false);
        f.key(Keysym::Left, None, false);
        assert_eq!(f.caret, 1, "back over the two-byte é");
        typed(&mut f, "X");
        assert_eq!(f.text, "hXéllo");
        assert_eq!(f.key(Keysym::BackSpace, None, false), Edit::Changed);
        assert_eq!(f.key(Keysym::Delete, None, false), Edit::Changed);
        assert_eq!((f.text.as_str(), f.caret), ("hllo", 1));
        f.key(Keysym::Home, None, false);
        assert_eq!(f.key(Keysym::BackSpace, None, false), Edit::Moved, "nothing before the caret");
        assert_eq!(f.key(Keysym::Left, None, false), Edit::Moved);
        assert_eq!(f.caret, 0);
        f.key(Keysym::End, None, false);
        assert_eq!(f.key(Keysym::Delete, None, false), Edit::Moved, "nothing after the caret");
        assert_eq!(f.key(Keysym::Right, None, false), Edit::Moved);
        assert_eq!(f.caret, 4);
    }

    #[test]
    fn select_all() {
        let mut f = TextField { text: "secret".into(), caret: 2, ..TextField::default() };
        assert_eq!(f.key(Keysym::a, Some("\u{1}"), true), Edit::Moved);
        assert!(f.all);
        typed(&mut f, "n");
        assert_eq!((f.text.as_str(), f.caret, f.all), ("n", 1, false), "typing replaces the selection");
        f.key(Keysym::a, None, true);
        f.key(Keysym::Left, None, false);
        assert_eq!((f.caret, f.all), (0, false), "Left drops the selection at the start");
        f.key(Keysym::a, None, true);
        assert_eq!(f.key(Keysym::BackSpace, None, false), Edit::Changed);
        assert_eq!((f.text.as_str(), f.caret), ("", 0));
    }

    #[test]
    fn special_keys_and_control_chars() {
        let mut f = TextField::default();
        assert_eq!(f.key(Keysym::Return, Some("\r"), false), Edit::Submit);
        assert_eq!(f.key(Keysym::Escape, Some("\u{1b}"), false), Edit::Cancel);
        assert_eq!(f.key(Keysym::Tab, Some("\t"), false), Edit::Ignored);
        assert_eq!(f.key(Keysym::c, Some("\u{3}"), true), Edit::Ignored, "Ctrl+C types nothing");
        assert_eq!(f.key(Keysym::F5, None, false), Edit::Ignored);
        assert!(f.text.is_empty());
    }

    #[test]
    fn secure_shows_bullets() {
        let mut f = TextField::secure();
        typed(&mut f, "pä55");
        f.key(Keysym::Left, None, false);
        assert_eq!(f.display(), "••••");
        assert_eq!(f.display_caret(), 3 * '•'.len_utf8());
        assert_eq!(f.text, "pä55");
    }
}
