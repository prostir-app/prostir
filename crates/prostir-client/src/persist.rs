//! The headset's own files: which there are, and which of them a pass of the main loop
//! changed.
//!
//! The model in memory is what counts from the start on. The files are read once, at
//! the start ([`start`]), and after that only written, off the frame, by the
//! [`Saver`] on a thread of its own; nothing reads them back, so a write that lands
//! later breaks nothing. A pass of the loop only marks what changed ([`Dirty`]); after
//! it each marked file's text is written once into the saver's buffers and handed over
//! (the newest text of a file replaces one still waiting). The texts are written by
//! the parts they belong to: `Settings::write_text`, `layout::Desk::write_text`,
//! `profiles::write_text`, `layout::write_keyboard`.
//!
//! Every file but `settings.txt` starts with [`FORMAT`]. Nothing older is carried
//! over: a file this version does not read (an older version's, an empty one, one cut
//! short) is set aside as `*.old` at the start, and the app starts without it.

#![forbid(unsafe_code)]

mod disk;

use std::fmt;

pub use disk::{Loaded, PersistEvent, Saver, start, write_now};

/// The first line of the files this version writes in its own way (all but
/// `settings.txt`, whose `key value` lines every version reads). A file without it is
/// not read: it is set aside as `*.old` and the app starts without it, so the next
/// change of a format is a new number here, not a guess at what a file holds.
pub const FORMAT: &str = "format 2";

/// Why a file is not read at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    /// It does not start with [`FORMAT`]: an older version's file, a newer format
    /// this version must not misread, or an empty file.
    Format,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format => write!(f, "not \"{FORMAT}\""),
        }
    }
}

/// The lines of a file this version passed over (a newer version's additions, a value
/// edited by hand), for one line in the log: how many, and the first.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Skipped {
    pub count: usize,
    pub first: Option<usize>,
}

impl Skipped {
    /// Line `n` (from 1) is passed over.
    pub fn line(&mut self, n: usize) {
        self.count += 1;
        self.first.get_or_insert(n);
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// The lines of `text` after its [`FORMAT`] line, each with its number (the first is
/// 2); an error only when the first line is not [`FORMAT`]. Within, lines end with
/// `\n` or `\r\n`, and the last one may have no end. The header is strict, the
/// contents are not: a reader passes over a line it does not know, so a change an
/// older reader would misread is a new [`FORMAT`].
pub fn body(text: &str) -> Result<impl Iterator<Item = (usize, &str)>, LoadError> {
    let mut lines = text.lines();
    if lines.next() != Some(FORMAT) {
        return Err(LoadError::Format);
    }
    Ok(lines.enumerate().map(|(i, line)| (i + 2, line)))
}

/// The [`FORMAT`] line, added to `out`: the first of a file.
pub fn write_format(out: &mut String) {
    out.push_str(FORMAT);
    out.push('\n');
}

/// A file the headset keeps in its own folder, in the order they are written when
/// several wait: what is hardest to make again first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum File {
    /// `layout.txt`: the desk arrangement.
    Desk,
    /// `profiles.txt`: the saved setups.
    Profiles,
    /// `keyboard.txt`: where the keyboard window lies.
    Keyboard,
    /// `settings.txt`: what is chosen in the menu.
    Settings,
}

impl File {
    pub const ALL: [File; 4] = [File::Desk, File::Profiles, File::Keyboard, File::Settings];

    /// Its name in the app's folder.
    pub fn name(self) -> &'static str {
        match self {
            File::Settings => "settings.txt",
            File::Desk => "layout.txt",
            File::Profiles => "profiles.txt",
            File::Keyboard => "keyboard.txt",
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    fn bit(self) -> u8 {
        1 << self.index()
    }
}

/// Files, each at most once: those to be written after this pass, however often each
/// changed in it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Dirty(u8);

impl Dirty {
    pub fn mark(&mut self, file: File) {
        self.0 |= file.bit();
    }

    pub fn has(self, file: File) -> bool {
        self.0 & file.bit() != 0
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The files `other` marks are marked too.
    pub fn add(&mut self, other: Dirty) {
        self.0 |= other.0;
    }

    /// The marked files, in the order of [`File::ALL`]; none is marked after.
    pub fn take(&mut self) -> impl Iterator<Item = File> + use<> {
        let taken = std::mem::take(self);
        File::ALL.into_iter().filter(move |f| taken.has(*f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hundred changes of a pass are one write of each file changed.
    #[test]
    fn a_pass_writes_each_file_once() {
        let mut dirty = Dirty::default();
        assert!(dirty.is_empty());
        for i in 0..100 {
            dirty.mark(if i % 2 == 0 {
                File::Desk
            } else {
                File::Settings
            });
        }
        dirty.mark(File::Desk);
        assert!(dirty.has(File::Desk) && !dirty.has(File::Keyboard));
        assert_eq!(
            dirty.take().collect::<Vec<_>>(),
            [File::Desk, File::Settings]
        );
        assert!(dirty.is_empty());
        assert_eq!(dirty.take().count(), 0);
        // What another part changed in the pass is written with the rest, once.
        let mut world = Dirty::default();
        world.mark(File::Keyboard);
        world.mark(File::Desk);
        dirty.mark(File::Desk);
        dirty.add(world);
        assert_eq!(
            dirty.take().collect::<Vec<_>>(),
            [File::Desk, File::Keyboard]
        );
    }

    /// A file of this format gives its lines after the first, numbered as an editor
    /// numbers them, whatever ends its lines; an older one or an empty one gives none.
    #[test]
    fn the_format_line() {
        let lines: Vec<(usize, &str)> = body("format 2\na\n\nb\n").unwrap().collect();
        assert_eq!(lines, [(2, "a"), (3, ""), (4, "b")]);
        let crlf: Vec<(usize, &str)> = body("format 2\r\na\r\nb").unwrap().collect();
        assert_eq!(crlf, [(2, "a"), (3, "b")]);
        assert_eq!(body("format 2\n").unwrap().count(), 0);
        assert_eq!(body("format 2").unwrap().count(), 0);
        assert_eq!(body("").err(), Some(LoadError::Format));
        assert_eq!(body("center 0 1 0 0\n").err(), Some(LoadError::Format));
        assert_eq!(body("format 20\n").err(), Some(LoadError::Format));
        assert_eq!(body("format 3\n").err(), Some(LoadError::Format));
        let mut out = String::from("old");
        out.clear();
        write_format(&mut out);
        assert_eq!(out, "format 2\n");
        let mut skipped = Skipped::default();
        assert!(skipped.is_empty());
        skipped.line(4);
        skipped.line(2);
        assert_eq!((skipped.count, skipped.first), (2, Some(4)));
    }

    #[test]
    fn names_are_those_of_before() {
        let names: Vec<&str> = File::ALL.iter().map(|f| f.name()).collect();
        assert_eq!(
            names,
            ["layout.txt", "profiles.txt", "keyboard.txt", "settings.txt"]
        );
    }
}
