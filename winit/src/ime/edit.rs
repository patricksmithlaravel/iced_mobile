//! What the keyboard did to its text, as edits for the focused field.
//!
//! The keyboard edits a mirror of its own: the text of the Java side's view,
//! which is empty when a field gets the keyboard and after every reset. The
//! Java side reports the whole mirror after each change, and the engine turns
//! the difference between two reports into what iced's widgets understand:
//! key presses that move the caret and delete, commits that insert text at
//! the caret, and a preedit for the text the keyboard is composing.
//!
//! The engine never reads the app's text. It knows the text typed since the
//! session's origin: where the app's caret was when the mirror was last
//! empty (a reset or a rebase). In the app, the mirror's committed text (its
//! text without the composing part) follows the origin, and the engine keeps
//! the app's caret inside that text, counted in grapheme clusters, which is
//! what `text_input` moves and deletes by. `text_editor`'s Backspace may
//! delete by character instead, so a combining sequence the keyboard revises
//! can come out wrong there.
//!
//! Line breaks and tabs are never part of the text the engine compares or
//! commits: Enter and Tab reach the app as keys (the Java side turns a commit
//! of exactly `"\n"` into Enter), and inside longer text they are dropped, as
//! `text_input` drops them from such an insertion on iOS.
// The JNI glue that feeds the engine is not in the crate yet, so outside the
// tests nothing calls it.
#![cfg_attr(
    not(test),
    allow(dead_code, reason = "the JNI glue will call the engine")
)]

use std::iter;
use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// Android's `KeyEvent.KEYCODE_DEL`: Backspace.
pub(crate) const KEYCODE_DEL: i32 = 67;
/// Android's `KeyEvent.KEYCODE_FORWARD_DEL`: Delete.
pub(crate) const KEYCODE_FORWARD_DEL: i32 = 112;
/// Android's `KeyEvent.KEYCODE_ENTER`.
pub(crate) const KEYCODE_ENTER: i32 = 66;
/// Android's `KeyEvent.KEYCODE_NUMPAD_ENTER`.
pub(crate) const KEYCODE_NUMPAD_ENTER: i32 = 160;
/// Android's `KeyEvent.KEYCODE_TAB`.
pub(crate) const KEYCODE_TAB: i32 = 61;
/// Android's `KeyEvent.KEYCODE_ESCAPE`.
pub(crate) const KEYCODE_ESCAPE: i32 = 111;
/// Android's `KeyEvent.KEYCODE_DPAD_UP`.
pub(crate) const KEYCODE_DPAD_UP: i32 = 19;
/// Android's `KeyEvent.KEYCODE_DPAD_DOWN`.
pub(crate) const KEYCODE_DPAD_DOWN: i32 = 20;
/// Android's `KeyEvent.KEYCODE_DPAD_LEFT`.
pub(crate) const KEYCODE_DPAD_LEFT: i32 = 21;
/// Android's `KeyEvent.KEYCODE_DPAD_RIGHT`.
pub(crate) const KEYCODE_DPAD_RIGHT: i32 = 22;
/// Android's `KeyEvent.KEYCODE_MOVE_HOME`.
pub(crate) const KEYCODE_MOVE_HOME: i32 = 122;
/// Android's `KeyEvent.KEYCODE_MOVE_END`.
pub(crate) const KEYCODE_MOVE_END: i32 = 123;

/// A message from the keyboard's Java side, in the order it was sent.
///
/// Each carries the generation the Java side had when it sent it: the one
/// of the last reset it ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Message {
    /// The mirror after a change: its whole text, its selection and its
    /// composing region, as UTF-16 indices (-1 for none).
    State {
        generation: i32,
        text: String,
        selection: (i32, i32),
        composing: (i32, i32),
    },
    /// A key the keyboard pressed that does not edit the mirror: Backspace
    /// or Delete with nothing to delete in the mirror, or Enter, Tab,
    /// Escape, an arrow, Home or End. `code` is Android's key code.
    Key { generation: i32, code: i32 },
    /// The Java side emptied the mirror without a reset (a secure field
    /// after each report, or a mirror grown past its cap): the app's caret
    /// becomes the session's origin.
    Rebase { generation: i32 },
}

/// A key press for the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Enter,
    Tab,
    Escape,
}

impl Key {
    /// The key for one of Android's key codes, if the engine passes it on.
    fn from_code(code: i32) -> Option<Self> {
        Some(match code {
            KEYCODE_DEL => Self::Backspace,
            KEYCODE_FORWARD_DEL => Self::Delete,
            KEYCODE_DPAD_LEFT => Self::Left,
            KEYCODE_DPAD_RIGHT => Self::Right,
            KEYCODE_DPAD_UP => Self::Up,
            KEYCODE_DPAD_DOWN => Self::Down,
            KEYCODE_MOVE_HOME => Self::Home,
            KEYCODE_MOVE_END => Self::End,
            KEYCODE_ENTER | KEYCODE_NUMPAD_ENTER => Self::Enter,
            KEYCODE_TAB => Self::Tab,
            KEYCODE_ESCAPE => Self::Escape,
            _ => return None,
        })
    }
}

/// An edit for the app, in the order it is to be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Edit {
    /// A press (and release) of a key.
    Key(Key),
    /// Text to insert at the caret.
    Commit(String),
    /// The composing text and the cursor inside it (a byte range); an empty
    /// text with None clears a shown preedit.
    Preedit(String, Option<Range<usize>>),
}

/// Turns the keyboard's messages into edits for the focused field.
///
/// The caller applies every message in order with [`Engine::apply`], gives
/// the edits to the window that has the input method, and calls
/// [`Engine::reset`] (then hands the new generation to the Java side) when
/// `apply` asks for it, before it applies another message, and when the user
/// moves the caret some other way: a tap, or a hardware key, unless the
/// engine [is idle](Engine::is_idle). Messages queued before such an event
/// belong before it, so the caller applies them first.
#[derive(Debug, Default)]
pub(crate) struct Engine {
    /// The generation of the last reset; messages of any other are stale.
    generation: i32,
    /// The mirror's committed text as last applied, without line breaks and
    /// tabs: the text that follows the origin in the app.
    base: String,
    /// The app's caret: grapheme clusters of `base` after the origin.
    caret: usize,
    /// The preedit the app shows, and its cursor (a byte offset).
    preedit: Option<(String, usize)>,
}

impl Engine {
    /// An engine at generation 0 with nothing typed.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The generation the keyboard's messages must carry to be applied.
    pub(crate) fn generation(&self) -> i32 {
        self.generation
    }

    /// Applies one message from the keyboard; pushes the edits for the app.
    /// Returns true when the keyboard's state should be reset (after an
    /// Enter, Tab, Escape, arrow, Home or End key from the keyboard).
    ///
    /// Such a key first commits a shown preedit, so a field submitted by
    /// Enter has the word being composed, and the caller then calls
    /// [`Engine::reset`] before it applies another message. A message of
    /// another generation predates a reset the caller asked for: it is
    /// dropped.
    pub(crate) fn apply(
        &mut self,
        message: Message,
        edits: &mut Vec<Edit>,
    ) -> bool {
        match message {
            Message::State {
                generation,
                text,
                selection,
                composing,
            } => {
                if generation == self.generation {
                    self.state(&text, selection, composing, edits);
                }

                false
            }
            Message::Key { generation, code } => {
                generation == self.generation && self.key(code, edits)
            }
            Message::Rebase { generation } => {
                if generation == self.generation {
                    self.clear_preedit(edits);
                    self.base.clear();
                    self.caret = 0;
                }

                false
            }
        }
    }

    /// Native-initiated reset (a tap, a hardware key, or after apply
    /// returned true): commits a shown composition the way Android does
    /// (Preedit cleared, then Commit of the composing text), forgets the
    /// text, and returns the new generation to give the keyboard.
    pub(crate) fn reset(&mut self, edits: &mut Vec<Edit>) -> i32 {
        self.commit_preedit(edits);

        self.base.clear();
        self.caret = 0;
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// Nothing to reset: no text typed since the last reset and no preedit.
    ///
    /// Precisely: the mirror's committed text, as the engine last applied
    /// it, is empty (it may hold line breaks and tabs, which the app never
    /// gets) and no preedit is shown. The app's caret may then move without
    /// a reset: the session's origin moves with it.
    pub(crate) fn is_idle(&self) -> bool {
        self.base.is_empty() && self.preedit.is_none()
    }

    fn state(
        &mut self,
        text: &str,
        selection: (i32, i32),
        composing: (i32, i32),
        edits: &mut Vec<Edit>,
    ) {
        let report = Report::read(text, selection, composing);

        let old: Vec<&str> = self.base.graphemes(true).collect();
        let new: Vec<&str> = report.committed.graphemes(true).collect();

        // The common prefix, and the common suffix of what remains, so the
        // two never overlap.
        let prefix = common(old.iter(), new.iter());
        let suffix =
            common(old[prefix..].iter().rev(), new[prefix..].iter().rev());

        let removed = old.len() - prefix - suffix;
        let inserted = new[prefix..new.len() - suffix].concat();
        let inserted_len = new.len() - suffix - prefix;
        let changed_end = old.len() - suffix;

        // Where the app's caret rests, rounded down to a cluster.
        let target = new
            .iter()
            .scan(0, |end, grapheme| {
                *end += grapheme.len();
                Some(*end)
            })
            .take_while(|end| *end <= report.target)
            .count();

        if removed > 0 || inserted_len > 0 {
            if inserted_len > 0 {
                // As on the desktop backends, a shown preedit is cleared
                // before the commit that replaces it, so it never shows
                // twice.
                self.clear_preedit(edits);
            }

            self.move_to(changed_end, edits);
            edits.extend(iter::repeat_n(Edit::Key(Key::Backspace), removed));
            self.caret = prefix;

            if inserted_len > 0 {
                edits.push(Edit::Commit(inserted));
                self.caret += inserted_len;
            }
        }

        self.base = report.committed;
        self.move_to(target, edits);

        match report.preedit {
            Some(preedit) => {
                if self.preedit.as_ref() != Some(&preedit) {
                    let (text, cursor) = &preedit;

                    edits.push(Edit::Preedit(
                        text.clone(),
                        Some(*cursor..*cursor),
                    ));
                    self.preedit = Some(preedit);
                }
            }
            None => self.clear_preedit(edits),
        }
    }

    fn key(&mut self, code: i32, edits: &mut Vec<Edit>) -> bool {
        let Some(key) = Key::from_code(code) else {
            return false;
        };

        match key {
            Key::Backspace => {
                // The keyboard presses Backspace only when the mirror has
                // nothing before its cursor, so what it deletes is before the
                // origin, wherever the caret rests (normally right there).
                let caret = self.caret;

                self.move_to(0, edits);
                edits.push(Edit::Key(Key::Backspace));
                self.move_to(caret, edits);

                false
            }
            Key::Delete => {
                // Likewise, the mirror had nothing after its cursor.
                let caret = self.caret;
                let end = self.base.graphemes(true).count();

                self.move_to(end, edits);
                edits.push(Edit::Key(Key::Delete));
                self.move_to(caret, edits);

                false
            }
            Key::Left
            | Key::Right
            | Key::Up
            | Key::Down
            | Key::Home
            | Key::End
            | Key::Enter
            | Key::Tab
            | Key::Escape => {
                self.commit_preedit(edits);
                edits.push(Edit::Key(key));

                true
            }
        }
    }

    /// Moves the app's caret to `position` (clusters after the origin).
    fn move_to(&mut self, position: usize, edits: &mut Vec<Edit>) {
        let key = if position < self.caret {
            Key::Left
        } else {
            Key::Right
        };

        edits.extend(iter::repeat_n(
            Edit::Key(key),
            self.caret.abs_diff(position),
        ));
        self.caret = position;
    }

    fn clear_preedit(&mut self, edits: &mut Vec<Edit>) {
        if self.preedit.take().is_some() {
            edits.push(Edit::Preedit(String::new(), None));
        }
    }

    /// Commits a shown preedit at the caret, the way Android finishes a
    /// composition: its text stays where it was.
    fn commit_preedit(&mut self, edits: &mut Vec<Edit>) {
        if let Some((text, _)) = self.preedit.take() {
            edits.push(Edit::Preedit(String::new(), None));
            edits.push(Edit::Commit(text));
        }
    }
}

/// A report of the mirror, read for the app.
#[derive(Debug, PartialEq, Eq)]
struct Report {
    /// The text without its composing region, line breaks and tabs.
    committed: String,
    /// Where the app's caret rests, a byte offset into `committed`: the
    /// composing region's start, or the selection's end.
    target: usize,
    /// The composing text, without line breaks and tabs, and the selection's
    /// end inside it (a byte offset); none when it is empty.
    preedit: Option<(String, usize)>,
}

impl Report {
    /// Reads a report from the Java side.
    ///
    /// Never panics: an index out of range is clamped into the text (a
    /// negative selection index counts as 0, as Android's
    /// `BaseInputConnection` counts it), one inside a surrogate pair rounds
    /// down, an inverted range is swapped, and a composing region with a
    /// negative index or no characters is none.
    fn read(text: &str, selection: (i32, i32), composing: (i32, i32)) -> Self {
        let cursor = byte_offset(text, selection.1);

        let region = if composing.0 < 0 || composing.1 < 0 {
            None
        } else {
            let start = byte_offset(text, composing.0);
            let end = byte_offset(text, composing.1);

            Some((start.min(end), start.max(end)))
                .filter(|(start, end)| start < end)
        };

        let mut committed = String::with_capacity(text.len());

        match region {
            Some((start, end)) => {
                push_text(&mut committed, &text[..start]);
                let target = committed.len();
                push_text(&mut committed, &text[end..]);

                let cursor = cursor.clamp(start, end);
                let mut preedit = String::new();
                push_text(&mut preedit, &text[start..cursor]);
                let offset = preedit.len();
                push_text(&mut preedit, &text[cursor..end]);

                Self {
                    committed,
                    target,
                    preedit: (!preedit.is_empty()).then_some((preedit, offset)),
                }
            }
            None => {
                push_text(&mut committed, &text[..cursor]);
                let target = committed.len();
                push_text(&mut committed, &text[cursor..]);

                Self {
                    committed,
                    target,
                    preedit: None,
                }
            }
        }
    }
}

/// The byte offset in `text` of a UTF-16 index, clamped into the text and
/// rounded down to a character boundary.
///
/// Each character counts its UTF-16 length. U+FFFD, which stands for an
/// unpaired surrogate when the Java string is read as UTF-16, counts one
/// unit, like the surrogate did.
fn byte_offset(text: &str, index: i32) -> usize {
    let Ok(index) = usize::try_from(index) else {
        return 0;
    };

    let mut units = 0;

    for (offset, c) in text.char_indices() {
        units += c.len_utf16();

        if units > index {
            return offset;
        }
    }

    text.len()
}

/// Appends `text` without its line breaks and tabs.
fn push_text(out: &mut String, text: &str) {
    out.extend(text.chars().filter(|c| !matches!(c, '\n' | '\r' | '\t')));
}

/// The length of the common start of two sequences.
fn common<T: PartialEq>(
    a: impl Iterator<Item = T>,
    b: impl Iterator<Item = T>,
) -> usize {
    a.zip(b).take_while(|(a, b)| a == b).count()
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::VecDeque;

    fn graphemes(text: &str) -> Vec<String> {
        text.graphemes(true).map(String::from).collect()
    }

    fn state(
        generation: i32,
        text: &str,
        selection: (i32, i32),
        composing: (i32, i32),
    ) -> Message {
        Message::State {
            generation,
            text: text.to_owned(),
            selection,
            composing,
        }
    }

    /// The edits one message gives, and whether it asked for a reset.
    fn apply(engine: &mut Engine, message: Message) -> (Vec<Edit>, bool) {
        let mut edits = Vec::new();
        let reset = engine.apply(message, &mut edits);

        (edits, reset)
    }

    /// The edits one message gives when it does not ask for a reset.
    fn edits(engine: &mut Engine, message: Message) -> Vec<Edit> {
        let (edits, reset) = apply(engine, message);
        assert!(!reset, "unexpected reset request: {edits:?}");

        edits
    }

    fn commit(text: &str) -> Edit {
        Edit::Commit(text.to_owned())
    }

    fn preedit(text: &str, cursor: usize) -> Edit {
        Edit::Preedit(text.to_owned(), Some(cursor..cursor))
    }

    fn clear() -> Edit {
        Edit::Preedit(String::new(), None)
    }

    fn keys(key: Key, count: usize) -> Vec<Edit> {
        vec![Edit::Key(key); count]
    }

    // The app's side: a field like iced's `text_input`.

    /// A single-line field that edits its value the way iced's `text_input`
    /// does: a list of grapheme clusters a commit splices into without
    /// segmenting the whole value again, a caret between clusters, and a
    /// preedit drawn at the caret that is not part of the value.
    #[derive(Debug, Default)]
    struct Field {
        value: Vec<String>,
        caret: usize,
        preedit: Option<(String, Option<Range<usize>>)>,
        /// While the last edits applied: the lowest caret, and the fewest
        /// clusters after it.
        low: usize,
        tail: usize,
    }

    impl Field {
        fn new(before: &str, after: &str) -> Self {
            let mut value = graphemes(before);
            let caret = value.len();
            value.extend(graphemes(after));

            Self {
                tail: value.len() - caret,
                value,
                caret,
                preedit: None,
                low: caret,
            }
        }

        fn text(&self) -> String {
            self.value.concat()
        }

        fn apply(&mut self, edits: &[Edit]) {
            self.low = self.caret;
            self.tail = self.value.len() - self.caret;

            for edit in edits {
                match edit {
                    Edit::Key(Key::Backspace) => {
                        if self.caret > 0 {
                            self.caret -= 1;
                            let _ = self.value.remove(self.caret);
                        }
                    }
                    Edit::Key(Key::Delete) => {
                        if self.caret < self.value.len() {
                            let _ = self.value.remove(self.caret);
                        }
                    }
                    Edit::Key(Key::Left) => {
                        self.caret = self.caret.saturating_sub(1);
                    }
                    Edit::Key(Key::Right) => {
                        self.caret = (self.caret + 1).min(self.value.len());
                    }
                    Edit::Key(Key::Home) => self.caret = 0,
                    Edit::Key(Key::End) => self.caret = self.value.len(),
                    Edit::Key(
                        Key::Up
                        | Key::Down
                        | Key::Enter
                        | Key::Tab
                        | Key::Escape,
                    ) => {}
                    Edit::Commit(text) => {
                        let inserted = graphemes(text);
                        let count = inserted.len();

                        let _ =
                            self.value.splice(self.caret..self.caret, inserted);
                        self.caret += count;
                    }
                    Edit::Preedit(text, cursor) => {
                        self.preedit = (!text.is_empty())
                            .then(|| (text.clone(), cursor.clone()));
                    }
                }

                self.low = self.low.min(self.caret);
                self.tail = self.tail.min(self.value.len() - self.caret);
            }
        }

        /// A character typed on a hardware keyboard: `text_input` inserts it
        /// and segments its whole value again.
        fn type_char(&mut self, c: char) {
            self.value.insert(self.caret, c.to_string());
            self.value = graphemes(&self.value.concat());
            self.caret = (self.caret + 1).min(self.value.len());
        }
    }

    // The keyboard's side: the Java mirror.

    fn is_high(unit: u16) -> bool {
        (0xD800..0xDC00).contains(&unit)
    }

    fn is_low(unit: u16) -> bool {
        (0xDC00..0xE000).contains(&unit)
    }

    type Snapshot = (Vec<u16>, (usize, usize), Option<(usize, usize)>);

    /// The Java side: an editable mirror in UTF-16 units that the keyboard
    /// edits through the calls of Android's `BaseInputConnection` (over a
    /// `SpannableStringBuilder`), and that reports its whole state when it
    /// changed, once per outermost batch edit.
    #[derive(Debug)]
    struct Mirror {
        units: Vec<u16>,
        /// Start and end; the start may be after the end.
        selection: (usize, usize),
        composing: Option<(usize, usize)>,
        generation: i32,
        /// A secure field: empty the mirror and rebase after each report.
        secure: bool,
        /// Empty the mirror and rebase when it grows past this many units
        /// with no composition.
        cap: usize,
        batch: usize,
        reported: Snapshot,
        out: VecDeque<Message>,
    }

    impl Mirror {
        fn new(secure: bool, cap: usize) -> Self {
            Self {
                units: Vec::new(),
                selection: (0, 0),
                composing: None,
                generation: 0,
                secure,
                cap,
                batch: 0,
                reported: (Vec::new(), (0, 0), None),
                out: VecDeque::new(),
            }
        }

        fn len(&self) -> usize {
            self.units.len()
        }

        fn ordered_selection(&self) -> (usize, usize) {
            let (start, end) = self.selection;

            (start.min(end), start.max(end))
        }

        fn composing_text(&self) -> String {
            self.composing.map_or_else(String::new, |(start, end)| {
                String::from_utf16_lossy(&self.units[start..end])
            })
        }

        fn begin_batch_edit(&mut self) {
            self.batch += 1;
        }

        fn end_batch_edit(&mut self) {
            self.batch -= 1;
            self.report();
        }

        /// `commitText`. A commit of exactly `"\n"` (what keyboards commit
        /// for Enter) finishes the composition, reports it and sends Enter
        /// as a key instead.
        fn commit_text(&mut self, text: &str, new_cursor: i64) {
            if text == "\n" {
                self.composing = None;
                self.report();
                self.key(KEYCODE_ENTER);
            } else {
                self.replace(text, new_cursor, false);
            }
        }

        fn set_composing_text(&mut self, text: &str, new_cursor: i64) {
            self.replace(text, new_cursor, true);
        }

        /// `BaseInputConnection.replaceText`: the composing region, else the
        /// selection, becomes `text`; the cursor goes relative to the end of
        /// the text minus one when `new_cursor` is positive, else relative
        /// to its start, clamped into the mirror.
        fn replace(&mut self, text: &str, new_cursor: i64, composing: bool) {
            let (start, end) =
                self.composing.unwrap_or(self.ordered_selection());
            let inserted: Vec<u16> = text.encode_utf16().collect();
            let count = inserted.len();

            let _ = self.units.splice(start..end, inserted);

            let cursor = if new_cursor > 0 {
                (start + count) as i64 + new_cursor - 1
            } else {
                start as i64 + new_cursor
            };
            let cursor = cursor.clamp(0, self.len() as i64) as usize;

            self.selection = (cursor, cursor);
            // An empty composing span is no span.
            self.composing =
                (composing && count > 0).then_some((start, start + count));
            self.report();
        }

        fn set_composing_region(&mut self, start: i64, end: i64) {
            let clamp = |index: i64| index.clamp(0, self.len() as i64) as usize;
            let (start, end) = (clamp(start.min(end)), clamp(start.max(end)));

            self.composing = (start < end).then_some((start, end));
            self.report();
        }

        fn finish_composing_text(&mut self) {
            self.composing = None;
            self.report();
        }

        /// `BaseInputConnection.deleteSurroundingText`: in UTF-16 units
        /// (so it may split a surrogate pair), around the selection and the
        /// composing region together.
        fn delete_surrounding_text(&mut self, before: usize, after: usize) {
            let (mut start, mut end) = self.ordered_selection();

            if let Some((composing_start, composing_end)) = self.composing {
                start = start.min(composing_start);
                end = end.max(composing_end);
            }

            let from = start.saturating_sub(before);
            self.delete(from, start);

            let end = end - (start - from);
            self.delete(end, (end + after).min(self.len()));

            self.report();
        }

        /// `BaseInputConnection.setSelection`: ignored out of range.
        fn set_selection(&mut self, start: i64, end: i64) {
            let range = 0..=self.len() as i64;

            if range.contains(&start) && range.contains(&end) {
                self.selection = (start as usize, end as usize);
            }

            self.report();
        }

        /// A key press from the keyboard, as `KeyboardConnection` handles
        /// it: Backspace and Delete edit the mirror by code point when it
        /// has something to delete, other keys go to the app.
        fn send_key(&mut self, code: i32) {
            let (start, end) = self.ordered_selection();

            match code {
                KEYCODE_DEL | KEYCODE_FORWARD_DEL if start < end => {
                    self.delete(start, end);
                    self.report();
                }
                KEYCODE_DEL if start > 0 => {
                    let pair = start >= 2
                        && is_low(self.units[start - 1])
                        && is_high(self.units[start - 2]);

                    self.delete(start - if pair { 2 } else { 1 }, start);
                    self.report();
                }
                KEYCODE_FORWARD_DEL if end < self.len() => {
                    let pair = end + 2 <= self.len()
                        && is_high(self.units[end])
                        && is_low(self.units[end + 1]);

                    self.delete(end, end + if pair { 2 } else { 1 });
                    self.report();
                }
                _ => self.key(code),
            }
        }

        /// A key with a character: committed like `commitText`.
        fn send_char(&mut self, c: char) {
            self.commit_text(c.encode_utf8(&mut [0; 4]), 1);
        }

        fn key(&mut self, code: i32) {
            self.out.push_back(Message::Key {
                generation: self.generation,
                code,
            });
        }

        /// Deletes units, moving the selection and the composing region the
        /// way a `SpannableStringBuilder` moves its spans.
        fn delete(&mut self, start: usize, end: usize) {
            let moved = |index: usize| {
                if index <= start {
                    index
                } else if index >= end {
                    index - (end - start)
                } else {
                    start
                }
            };

            let _ = self.units.drain(start..end);
            self.selection = (moved(self.selection.0), moved(self.selection.1));
            self.composing = self
                .composing
                .map(|(start, end)| (moved(start), moved(end)))
                .filter(|(start, end)| start < end);
        }

        fn report(&mut self) {
            if self.batch > 0 {
                return;
            }

            let snapshot = (self.units.clone(), self.selection, self.composing);

            if snapshot == self.reported {
                return;
            }

            let index = |index: usize| i32::try_from(index).unwrap();

            self.out.push_back(Message::State {
                generation: self.generation,
                text: String::from_utf16_lossy(&self.units),
                selection: (index(self.selection.0), index(self.selection.1)),
                composing: self.composing.map_or((-1, -1), |(start, end)| {
                    (index(start), index(end))
                }),
            });
            self.reported = snapshot;

            if self.secure
                || (self.len() > self.cap && self.composing.is_none())
            {
                self.clear();
                self.out.push_back(Message::Rebase {
                    generation: self.generation,
                });
            }
        }

        fn clear(&mut self) {
            self.units.clear();
            self.selection = (0, 0);
            self.composing = None;
            self.reported = (Vec::new(), (0, 0), None);
        }

        /// `Keyboard.reset(generation)` running on the UI thread.
        fn adopt(&mut self, generation: i32) {
            self.generation = generation;
            self.clear();
        }
    }

    // The test's own reading of a report.

    /// What the app should show for a report: the rules of the engine's
    /// module, written apart from the engine (characters and a unit table
    /// instead of byte offsets).
    #[derive(Debug, Clone, Default)]
    struct Expected {
        committed: String,
        /// Where the app's caret rests: a byte offset into `committed`.
        target: usize,
        composing: Option<(String, usize)>,
    }

    impl Expected {
        fn read(
            text: &str,
            selection: (i32, i32),
            composing: (i32, i32),
        ) -> Self {
            let chars: Vec<char> = text.chars().collect();

            // The character each UTF-16 unit belongs to, then the end.
            let mut table: Vec<usize> = chars
                .iter()
                .enumerate()
                .flat_map(|(n, c)| iter::repeat_n(n, c.len_utf16()))
                .collect();
            table.push(chars.len());

            let at = |index: i32| {
                usize::try_from(index)
                    .map_or(0, |index| table[index.min(table.len() - 1)])
            };

            let text_of = |range: Range<usize>| -> String {
                chars[range]
                    .iter()
                    .filter(|c| !matches!(c, '\n' | '\r' | '\t'))
                    .collect()
            };

            let region = (composing.0 >= 0 && composing.1 >= 0)
                .then(|| {
                    let (a, b) = (at(composing.0), at(composing.1));
                    (a.min(b), a.max(b))
                })
                .filter(|(a, b)| a < b);

            match region {
                Some((start, end)) => {
                    let before = text_of(0..start);
                    let cursor = at(selection.1).clamp(start, end);
                    let composing = text_of(start..end);

                    Self {
                        target: before.len(),
                        committed: before + &text_of(end..chars.len()),
                        composing: (!composing.is_empty())
                            .then(|| (composing, text_of(start..cursor).len())),
                    }
                }
                None => {
                    let cursor = at(selection.1);
                    let before = text_of(0..cursor);

                    Self {
                        target: before.len(),
                        committed: before + &text_of(cursor..chars.len()),
                        composing: None,
                    }
                }
            }
        }

        /// The end of the last cluster of `committed` that ends at or before
        /// the target: where the app's caret rests, in bytes.
        fn rest(&self) -> usize {
            self.committed
                .grapheme_indices(true)
                .map(|(start, grapheme)| start + grapheme.len())
                .take_while(|end| *end <= self.target)
                .last()
                .unwrap_or(0)
        }

        /// The app's caret, in clusters after the origin.
        fn caret(&self) -> usize {
            self.committed[..self.rest()].graphemes(true).count()
        }

        /// The committed text with the composing text committed where the
        /// app's caret rests.
        fn finished(&self) -> String {
            let rest = self.rest();
            let composing =
                self.composing.as_ref().map_or("", |(text, _)| text);

            format!(
                "{}{composing}{}",
                &self.committed[..rest],
                &self.committed[rest..]
            )
        }
    }

    // Both sides together.

    /// The keyboard's mirror, the engine and the app, with the native side's
    /// queue between them and the test's account of where the session's
    /// text sits in the app.
    struct Harness {
        label: String,
        mirror: Mirror,
        engine: Engine,
        app: Field,
        /// The app's clusters before the origin and after the session's text.
        before: Vec<String>,
        after: Vec<String>,
        /// The last report the engine applied in this session.
        last: Expected,
        /// Resets the native side posted that the Java side has not run.
        posted: VecDeque<i32>,
        /// The edits given to the app since the last `take`.
        log: Vec<Edit>,
        seen: Seen,
    }

    /// What a run went through, so the random runs show they reach every
    /// path.
    #[derive(Debug, Default, Clone, Copy)]
    struct Seen {
        /// Reports applied, with a composition, with U+FFFD in the text,
        /// and that replaced text (a Backspace and a commit).
        reports: usize,
        compositions: usize,
        replacements: usize,
        revisions: usize,
        /// Messages dropped for their generation.
        dropped: usize,
        /// Backspace and Delete keys, and keys that asked for a reset.
        deletions: usize,
        resets: usize,
        /// Taps and hardware keys that reset, and rebases.
        interruptions: usize,
        rebases: usize,
    }

    impl Seen {
        fn add(&mut self, other: Self) {
            self.reports += other.reports;
            self.compositions += other.compositions;
            self.replacements += other.replacements;
            self.revisions += other.revisions;
            self.dropped += other.dropped;
            self.deletions += other.deletions;
            self.resets += other.resets;
            self.interruptions += other.interruptions;
            self.rebases += other.rebases;
        }

        /// Each path but the `skipped` ones was taken `at_least` times.
        fn assert_reached(&self, at_least: usize, skipped: &[&str]) {
            for (name, count) in [
                ("reports", self.reports),
                ("compositions", self.compositions),
                ("replacements", self.replacements),
                ("revisions", self.revisions),
                ("dropped", self.dropped),
                ("deletions", self.deletions),
                ("resets", self.resets),
                ("interruptions", self.interruptions),
                ("rebases", self.rebases),
            ] {
                assert!(
                    count >= at_least || skipped.contains(&name),
                    "only {count} {name}: {self:?}"
                );
            }
        }
    }

    impl Harness {
        fn new(before: &str, after: &str) -> Self {
            Self::with(before, after, false, 4096)
        }

        fn with(before: &str, after: &str, secure: bool, cap: usize) -> Self {
            Self {
                label: String::from("harness"),
                mirror: Mirror::new(secure, cap),
                engine: Engine::new(),
                app: Field::new(before, after),
                before: graphemes(before),
                after: graphemes(after),
                last: Expected::default(),
                posted: VecDeque::new(),
                log: Vec::new(),
                seen: Seen::default(),
            }
        }

        /// Runs `op` on the keyboard's side, delivers what it sent, and
        /// returns the edits the app got.
        fn run(&mut self, op: impl FnOnce(&mut Mirror)) -> Vec<Edit> {
            op(&mut self.mirror);
            self.drain();
            self.take()
        }

        fn take(&mut self) -> Vec<Edit> {
            std::mem::take(&mut self.log)
        }

        fn value(&self) -> String {
            self.app.text()
        }

        fn preedit(&self) -> &str {
            self.app.preedit.as_ref().map_or("", |(text, _)| text)
        }

        /// The native side's event loop: applies every queued message.
        fn drain(&mut self) {
            while let Some(message) = self.mirror.out.pop_front() {
                self.deliver(message);
            }
        }

        fn deliver(&mut self, message: Message) {
            let label = self.label.clone();
            let current = self.engine.generation();
            let (Message::State { generation, .. }
            | Message::Key { generation, .. }
            | Message::Rebase { generation }) = message;

            let mut edits = Vec::new();
            let reset = self.engine.apply(message.clone(), &mut edits);

            if generation != current {
                assert!(
                    edits.is_empty() && !reset,
                    "{label}: a message of generation {generation} gave {edits:?} in generation {current}"
                );
                self.seen.dropped += 1;
                return;
            }

            self.app.apply(&edits);
            self.log.extend(edits.iter().cloned());

            match message {
                Message::State {
                    text,
                    selection,
                    composing,
                    ..
                } => {
                    assert!(!reset, "{label}: a report asked for a reset");
                    self.last = Expected::read(&text, selection, composing);
                    self.check();

                    let has =
                        |wanted: fn(&Edit) -> bool| edits.iter().any(wanted);
                    self.seen.reports += 1;
                    self.seen.compositions +=
                        usize::from(self.last.composing.is_some());
                    self.seen.replacements +=
                        usize::from(text.contains('\u{FFFD}'));
                    self.seen.revisions += usize::from(
                        has(|edit| *edit == Edit::Key(Key::Backspace))
                            && has(|edit| matches!(edit, Edit::Commit(_))),
                    );
                }
                Message::Key { code, .. } => match code {
                    KEYCODE_DEL => {
                        assert!(!reset, "{label}: Backspace asked for a reset");
                        let _ = self.before.pop();
                        self.check();
                        self.seen.deletions += 1;
                    }
                    KEYCODE_FORWARD_DEL => {
                        assert!(!reset, "{label}: Delete asked for a reset");
                        if !self.after.is_empty() {
                            let _ = self.after.remove(0);
                        }
                        self.check();
                        self.seen.deletions += 1;
                    }
                    KEYCODE_ENTER | KEYCODE_NUMPAD_ENTER | KEYCODE_TAB
                    | KEYCODE_ESCAPE | KEYCODE_DPAD_UP | KEYCODE_DPAD_DOWN
                    | KEYCODE_DPAD_LEFT | KEYCODE_DPAD_RIGHT
                    | KEYCODE_MOVE_HOME | KEYCODE_MOVE_END => {
                        assert!(
                            reset,
                            "{label}: key {code} did not ask for a reset"
                        );
                        assert_eq!(
                            edits.last(),
                            Some(&Edit::Key(Key::from_code(code).unwrap())),
                            "{label}"
                        );
                        // The composition is committed where it was, before
                        // the key, and the key edits no text.
                        self.check_finished();

                        let mut more = Vec::new();
                        let generation = self.engine.reset(&mut more);
                        assert_eq!(
                            more,
                            [],
                            "{label}: the reset after a key gave edits"
                        );

                        self.posted.push_back(generation);
                        self.restart();
                        self.seen.resets += 1;
                    }
                    _ => {
                        assert!(
                            edits.is_empty() && !reset,
                            "{label}: unknown key {code} gave {edits:?}"
                        );
                    }
                },
                Message::Rebase { .. } => {
                    assert!(!reset, "{label}: a rebase asked for a reset");
                    // The composition is dropped (the Java side rebases a
                    // secure field even while a keyboard composes) and the
                    // caret stays where the report put it.
                    self.last.composing = None;
                    assert_eq!(self.app.preedit, None, "{label}");
                    assert_eq!(
                        self.value(),
                        self.expected_value(&self.last.committed),
                        "{label}"
                    );
                    assert_eq!(
                        self.app.caret,
                        self.before.len() + self.last.caret(),
                        "{label}"
                    );
                    self.restart();
                    self.seen.rebases += 1;
                }
            }
        }

        fn expected_value(&self, session: &str) -> String {
            format!("{}{session}{}", self.before.concat(), self.after.concat())
        }

        /// The app shows what the engine's last report says.
        fn check(&self) {
            let label = &self.label;
            let last = &self.last;
            let app = &self.app;
            let end = app.value.len() - self.after.len();

            assert_eq!(
                app.text(),
                self.expected_value(&last.committed),
                "{label}"
            );
            assert_eq!(app.value[..self.before.len()], self.before, "{label}");
            assert_eq!(app.value[end..], self.after, "{label}");
            assert_eq!(
                app.value[self.before.len()..end],
                graphemes(&last.committed),
                "{label}: the app's clusters differ from the session's text"
            );
            assert_eq!(self.engine.base, last.committed, "{label}");
            assert_eq!(self.engine.caret, last.caret(), "{label}");
            assert_eq!(
                app.caret,
                self.before.len() + self.engine.caret,
                "{label}"
            );
            assert!(
                app.low >= self.before.len(),
                "{label}: the caret went left of the origin ({} < {})",
                app.low,
                self.before.len(),
            );
            assert!(
                app.tail >= self.after.len(),
                "{label}: the caret went past the session's text ({} < {})",
                app.tail,
                self.after.len(),
            );

            let preedit = last
                .composing
                .as_ref()
                .map(|(text, cursor)| (text.clone(), Some(*cursor..*cursor)));
            assert_eq!(app.preedit, preedit, "{label}");
        }

        /// The composition was committed where the app's caret rests.
        fn check_finished(&self) {
            assert_eq!(
                self.value(),
                self.expected_value(&self.last.finished()),
                "{}",
                self.label
            );
            assert_eq!(self.app.preedit, None, "{}", self.label);
        }

        /// The app's caret becomes the session's origin, and the session's
        /// text is empty.
        fn restart(&mut self) {
            self.before = self.app.value[..self.app.caret].to_vec();
            self.after = self.app.value[self.app.caret..].to_vec();
            self.last = Expected::default();
        }

        /// What the native side does before a tap or a hardware key reaches
        /// the app: deliver what is queued, then reset unless idle.
        fn interrupt(&mut self) {
            self.drain();

            if self.engine.is_idle() {
                assert!(
                    self.last.committed.is_empty()
                        && self.last.composing.is_none(),
                    "{}: idle with text",
                    self.label
                );
            } else {
                let mut edits = Vec::new();
                let generation = self.engine.reset(&mut edits);

                self.app.apply(&edits);
                self.log.extend(edits);
                self.check_finished();
                self.posted.push_back(generation);
                self.seen.interruptions += 1;
            }
        }

        /// A tap that puts the app's caret at cluster `at`.
        fn tap(&mut self, at: usize) {
            self.interrupt();
            self.app.caret = at.min(self.app.value.len());
            self.restart();
        }

        /// A character from a hardware keyboard.
        fn hardware(&mut self, c: char) {
            self.interrupt();
            self.app.type_char(c);
            self.restart();
        }

        /// The Java side runs the oldest reset the native side posted.
        fn adopt(&mut self) {
            if let Some(generation) = self.posted.pop_front() {
                self.mirror.adopt(generation);
            }
        }

        /// Everything posted runs and everything sent is delivered.
        fn settle(&mut self) {
            loop {
                while !self.posted.is_empty() {
                    self.adopt();
                }

                if self.mirror.out.is_empty() {
                    break;
                }

                // A key among these may post another reset.
                self.drain();
            }
        }
    }

    // The rules, one at a time.

    #[test]
    fn a_new_engine_is_idle_at_generation_zero() {
        let engine = Engine::new();

        assert_eq!(engine.generation(), 0);
        assert!(engine.is_idle());
    }

    #[test]
    fn messages_of_another_generation_are_dropped() {
        let mut engine = Engine::new();

        for generation in [1, -1, i32::MAX] {
            assert_eq!(
                edits(&mut engine, state(generation, "abc", (3, 3), (-1, -1))),
                []
            );
            assert_eq!(
                edits(&mut engine, state(generation, "ab", (2, 2), (0, 2))),
                []
            );
            assert_eq!(
                apply(
                    &mut engine,
                    Message::Key {
                        generation,
                        code: KEYCODE_ENTER
                    }
                ),
                (vec![], false)
            );
            assert_eq!(
                edits(
                    &mut engine,
                    Message::Key {
                        generation,
                        code: KEYCODE_DEL
                    }
                ),
                []
            );
            assert_eq!(edits(&mut engine, Message::Rebase { generation }), []);
        }
        assert!(engine.is_idle());

        assert_eq!(
            edits(&mut engine, state(0, "ab", (2, 2), (0, 2))),
            [preedit("ab", 2)]
        );
        assert_eq!(engine.reset(&mut Vec::new()), 1);

        // The keyboard had not run the reset yet.
        assert_eq!(edits(&mut engine, state(0, "abc", (3, 3), (0, 3))), []);
        assert_eq!(edits(&mut engine, Message::Rebase { generation: 0 }), []);
        assert_eq!(
            edits(&mut engine, state(1, "x", (1, 1), (-1, -1))),
            [commit("x")]
        );
    }

    #[test]
    fn utf16_indices_become_char_boundaries() {
        // a (1 unit), 😀 (2 units, 4 bytes), b (1 unit).
        let text = "a😀b";

        for (index, offset) in [
            (0, 0),
            (1, 1),
            (2, 1), // inside the surrogate pair: rounded down
            (3, 5),
            (4, 6),
            (5, 6), // past the end: clamped
            (i32::MAX, 6),
            (-1, 0),
            (i32::MIN, 0),
        ] {
            assert_eq!(byte_offset(text, index), offset, "index {index}");
        }

        // U+FFFD stands for an unpaired surrogate: one unit.
        assert_eq!(byte_offset("\u{FFFD}x", 1), 3);
        assert_eq!(byte_offset("\u{FFFD}x", 2), 4);
        assert_eq!(byte_offset("", 3), 0);
    }

    #[test]
    fn a_report_leaves_out_the_composition() {
        let report = Report::read("hello wor", (9, 9), (6, 9));

        assert_eq!(report.committed, "hello ");
        assert_eq!(report.target, 6);
        assert_eq!(report.preedit, Some((String::from("wor"), 3)));

        // An inverted region is swapped.
        assert_eq!(Report::read("hello wor", (9, 9), (9, 6)), report);

        // The caret rests at the selection's end without a composition.
        let report = Report::read("hello", (5, 2), (-1, -1));
        assert_eq!((report.committed.as_str(), report.target), ("hello", 2));

        // A region with no characters, or a negative end, is none.
        for composing in [(2, 2), (-1, 3), (3, -1), (1, 1), (0, 1)] {
            let report = Report::read("😀b", (3, 3), composing);
            assert_eq!(report.preedit, None, "{composing:?}");
            assert_eq!(report.committed, "😀b");
        }

        // An end inside a surrogate pair rounds down too.
        let report = Report::read("😀b", (3, 3), (1, 2));
        assert_eq!(report.preedit, Some((String::from("😀"), 4)));
        assert_eq!(report.committed, "b");

        // The preedit's cursor is the selection's end clamped into the
        // region, in bytes of the composing text.
        assert_eq!(
            Report::read("xhéllo", (3, 3), (1, 6)).preedit,
            Some((String::from("héllo"), 3))
        );
        assert_eq!(
            Report::read("xhéllo", (0, 0), (1, 6)).preedit,
            Some((String::from("héllo"), 0))
        );
        assert_eq!(
            Report::read("xhéllo", (9, 9), (1, 6)).preedit,
            Some((String::from("héllo"), 6))
        );
    }

    #[test]
    fn odd_indices_never_panic_and_keep_the_app_in_step() {
        let texts = [
            "",
            "a",
            "a😀b",
            "e\u{301}\u{FFFD}x",
            "👨\u{200D}👩\u{200D}👧 hi",
            "line\nbreak\t",
            "你好世界",
        ];
        let indices = [i32::MIN, -2, -1, 0, 1, 2, 3, 4, 5, 7, 11, 40, i32::MAX];
        let mut rng = Rng::new(7);

        for round in 0..2000 {
            let mut harness = Harness::new("pre ", " post");
            harness.label = format!("round {round}");

            for _ in 0..4 {
                let text = rng.pick(&texts);
                let mut index = || rng.pick(&indices);
                let message =
                    state(0, text, (index(), index()), (index(), index()));

                harness.deliver(message);
            }
        }
    }

    #[test]
    fn changes_are_found_by_grapheme_cluster() {
        let mut engine = Engine::new();

        assert_eq!(
            edits(&mut engine, state(0, "e", (1, 1), (-1, -1))),
            [commit("e")]
        );
        // A combining accent joins the cluster before it: the whole cluster
        // is replaced.
        assert_eq!(
            edits(&mut engine, state(0, "e\u{301}", (2, 2), (-1, -1))),
            [Edit::Key(Key::Backspace), commit("e\u{301}")]
        );

        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "👍", (2, 2), (-1, -1)));
        assert_eq!(
            edits(&mut engine, state(0, "👍🏽", (4, 4), (-1, -1))),
            [Edit::Key(Key::Backspace), commit("👍🏽")]
        );

        // A change in the middle: the caret goes to its end, Backspace
        // removes it, the new text is committed, the caret goes to the
        // target.
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "abcd", (4, 4), (-1, -1)));
        assert_eq!(
            edits(&mut engine, state(0, "aXYd", (4, 4), (-1, -1))),
            [
                Edit::Key(Key::Left),
                Edit::Key(Key::Backspace),
                Edit::Key(Key::Backspace),
                commit("XY"),
                Edit::Key(Key::Right),
            ]
        );
    }

    #[test]
    fn the_common_suffix_never_overlaps_the_prefix() {
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "aa", (2, 2), (-1, -1)));

        assert_eq!(
            edits(&mut engine, state(0, "aaa", (3, 3), (-1, -1))),
            [commit("a")]
        );
        assert_eq!(
            edits(&mut engine, state(0, "a", (1, 1), (-1, -1))),
            keys(Key::Backspace, 2)
        );

        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "abab", (4, 4), (-1, -1)));
        assert_eq!(
            edits(&mut engine, state(0, "ab", (2, 2), (-1, -1))),
            keys(Key::Backspace, 2)
        );
    }

    #[test]
    fn the_caret_follows_the_selection_with_arrows() {
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "hello", (5, 5), (-1, -1)));

        assert_eq!(
            edits(&mut engine, state(0, "hello", (2, 2), (-1, -1))),
            keys(Key::Left, 3)
        );
        assert_eq!(
            edits(&mut engine, state(0, "hello", (1, 4), (-1, -1))),
            keys(Key::Right, 2)
        );
        // Inside a cluster, the caret rests before it.
        let mut engine = Engine::new();
        assert_eq!(
            edits(&mut engine, state(0, "e\u{301}", (1, 1), (-1, -1))),
            [commit("e\u{301}"), Edit::Key(Key::Left)]
        );
        // Inside a surrogate pair too.
        let mut engine = Engine::new();
        assert_eq!(
            edits(&mut engine, state(0, "😀", (1, 1), (-1, -1))),
            [commit("😀"), Edit::Key(Key::Left)]
        );
    }

    #[test]
    fn line_breaks_and_tabs_are_dropped() {
        let mut engine = Engine::new();

        assert_eq!(
            edits(&mut engine, state(0, "a\nb\tc\r\n", (7, 7), (-1, -1))),
            [commit("abc")]
        );
        // A line break added between them changes nothing for the app.
        assert_eq!(
            edits(&mut engine, state(0, "a\n\nb\tc\r\n", (8, 8), (-1, -1))),
            []
        );
        // Nor does a text that is nothing else: no empty commit.
        let mut engine = Engine::new();
        assert_eq!(edits(&mut engine, state(0, "\n\t", (2, 2), (-1, -1))), []);
        assert!(engine.is_idle());
        // The composing text drops them too.
        assert_eq!(
            edits(&mut engine, state(0, "\n\ta\nb", (5, 5), (2, 5))),
            [preedit("ab", 2)]
        );
    }

    #[test]
    fn the_preedit_shows_the_composition_and_its_cursor() {
        let mut engine = Engine::new();

        assert_eq!(
            edits(&mut engine, state(0, "h", (1, 1), (0, 1))),
            [preedit("h", 1)]
        );
        assert_eq!(
            edits(&mut engine, state(0, "hé", (2, 2), (0, 2))),
            [preedit("hé", 3)]
        );
        // The same report again: nothing to show.
        assert_eq!(edits(&mut engine, state(0, "hé", (2, 2), (0, 2))), []);
        // The cursor moves inside the composition.
        assert_eq!(
            edits(&mut engine, state(0, "hé", (1, 1), (0, 2))),
            [preedit("hé", 1)]
        );
        // The composition goes away: the preedit is cleared.
        assert_eq!(
            edits(&mut engine, state(0, "", (0, 0), (-1, -1))),
            [clear()]
        );
        assert!(engine.is_idle());
    }

    #[test]
    fn the_preedit_is_cleared_before_a_commit_and_shown_again() {
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "hello", (5, 5), (0, 5)));

        assert_eq!(
            edits(&mut engine, state(0, "hello world", (11, 11), (6, 11))),
            [clear(), commit("hello "), preedit("world", 5)]
        );
        // A deletion before the composition leaves the preedit shown.
        assert_eq!(
            edits(&mut engine, state(0, "helloworld", (10, 10), (5, 10))),
            [Edit::Key(Key::Backspace)]
        );
    }

    #[test]
    fn a_rebase_makes_the_caret_the_new_origin() {
        let mut engine = Engine::new();

        assert_eq!(
            edits(&mut engine, state(0, "abc", (1, 1), (-1, -1))),
            [commit("abc"), Edit::Key(Key::Left), Edit::Key(Key::Left)]
        );
        assert_eq!(edits(&mut engine, Message::Rebase { generation: 0 }), []);
        assert!(engine.is_idle());
        // The mirror starts empty again: its text goes at the app's caret.
        assert_eq!(
            edits(&mut engine, state(0, "x", (1, 1), (-1, -1))),
            [commit("x")]
        );
        assert_eq!(
            edits(&mut engine, state(0, "", (0, 0), (-1, -1))),
            [Edit::Key(Key::Backspace)]
        );

        // A shown preedit is cleared.
        let _ = edits(&mut engine, state(0, "wo", (2, 2), (0, 2)));
        assert_eq!(
            edits(&mut engine, Message::Rebase { generation: 0 }),
            [clear()]
        );
        assert!(engine.is_idle());
    }

    #[test]
    fn android_key_codes_become_keys() {
        for (code, key) in [
            (KEYCODE_DEL, Key::Backspace),
            (KEYCODE_FORWARD_DEL, Key::Delete),
            (KEYCODE_ENTER, Key::Enter),
            (KEYCODE_NUMPAD_ENTER, Key::Enter),
            (KEYCODE_TAB, Key::Tab),
            (KEYCODE_ESCAPE, Key::Escape),
            (KEYCODE_DPAD_UP, Key::Up),
            (KEYCODE_DPAD_DOWN, Key::Down),
            (KEYCODE_DPAD_LEFT, Key::Left),
            (KEYCODE_DPAD_RIGHT, Key::Right),
            (KEYCODE_MOVE_HOME, Key::Home),
            (KEYCODE_MOVE_END, Key::End),
        ] {
            let mut engine = Engine::new();
            let resets = !matches!(key, Key::Backspace | Key::Delete);

            assert_eq!(
                apply(
                    &mut engine,
                    Message::Key {
                        generation: 0,
                        code
                    }
                ),
                (vec![Edit::Key(key)], resets),
                "code {code}"
            );
        }

        for code in [0, 1, 4, 29, 62, 82, -1, i32::MAX] {
            let mut engine = Engine::new();

            assert_eq!(
                apply(
                    &mut engine,
                    Message::Key {
                        generation: 0,
                        code
                    }
                ),
                (vec![], false),
                "code {code}"
            );
        }
    }

    #[test]
    fn backspace_and_delete_keys_leave_the_text_alone() {
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "abc", (0, 0), (-1, -1)));

        assert_eq!(
            edits(
                &mut engine,
                Message::Key {
                    generation: 0,
                    code: KEYCODE_DEL
                }
            ),
            [Edit::Key(Key::Backspace)]
        );
        assert_eq!((engine.base.as_str(), engine.caret), ("abc", 0));

        let _ = edits(&mut engine, state(0, "abc", (3, 3), (-1, -1)));
        assert_eq!(
            edits(
                &mut engine,
                Message::Key {
                    generation: 0,
                    code: KEYCODE_FORWARD_DEL
                }
            ),
            [Edit::Key(Key::Delete)]
        );
        assert_eq!((engine.base.as_str(), engine.caret), ("abc", 3));
    }

    #[test]
    fn backspace_and_delete_keys_act_at_the_edges_of_the_text() {
        // The mirror's cursor is at its start, but its composition (where
        // the caret rests) is later.
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "abc", (0, 0), (1, 3)));
        assert_eq!(engine.caret, 1);

        assert_eq!(
            edits(
                &mut engine,
                Message::Key {
                    generation: 0,
                    code: KEYCODE_DEL
                }
            ),
            [
                Edit::Key(Key::Left),
                Edit::Key(Key::Backspace),
                Edit::Key(Key::Right)
            ]
        );

        let _ = edits(&mut engine, state(0, "abcd", (4, 4), (0, 1)));
        assert_eq!(engine.caret, 0);
        assert_eq!(
            edits(
                &mut engine,
                Message::Key {
                    generation: 0,
                    code: KEYCODE_FORWARD_DEL
                }
            ),
            [
                keys(Key::Right, 3),
                vec![Edit::Key(Key::Delete)],
                keys(Key::Left, 3)
            ]
            .concat()
        );
    }

    #[test]
    fn a_key_that_resets_commits_the_composition_first() {
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "hello", (5, 5), (0, 5)));

        assert_eq!(
            apply(
                &mut engine,
                Message::Key {
                    generation: 0,
                    code: KEYCODE_ENTER
                }
            ),
            (vec![clear(), commit("hello"), Edit::Key(Key::Enter)], true)
        );

        let mut edits = Vec::new();
        assert_eq!(engine.reset(&mut edits), 1);
        assert_eq!(edits, []);
        assert!(engine.is_idle());
    }

    #[test]
    fn a_reset_commits_the_preedit_and_starts_a_generation() {
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "hello wor", (9, 9), (6, 9)));

        let mut edits = Vec::new();
        assert_eq!(engine.reset(&mut edits), 1);
        assert_eq!(edits, [clear(), commit("wor")]);
        assert!(engine.is_idle());
        assert_eq!(engine.generation(), 1);

        // Nothing shown: nothing to commit.
        let mut edits = Vec::new();
        assert_eq!(engine.reset(&mut edits), 2);
        assert_eq!(edits, []);

        // The generation wraps.
        engine.generation = i32::MAX;
        assert_eq!(engine.reset(&mut Vec::new()), i32::MIN);
    }

    #[test]
    fn idle_means_no_text_and_no_preedit() {
        let mut engine = Engine::new();

        let _ = edits(&mut engine, state(0, "a", (1, 1), (0, 1)));
        assert!(!engine.is_idle());
        let _ = edits(&mut engine, state(0, "a", (1, 1), (-1, -1)));
        assert!(!engine.is_idle());
        let _ = edits(&mut engine, state(0, "", (0, 0), (-1, -1)));
        assert!(engine.is_idle());
    }

    #[test]
    fn replacement_characters_count_one_unit() {
        let mut engine = Engine::new();

        // The Java side split an emoji: the string holds U+FFFD for the
        // unpaired surrogate.
        assert_eq!(
            edits(&mut engine, state(0, "a\u{FFFD}b", (2, 2), (-1, -1))),
            [commit("a\u{FFFD}b"), Edit::Key(Key::Left)]
        );
        assert_eq!(
            edits(&mut engine, state(0, "ab", (1, 1), (-1, -1))),
            [Edit::Key(Key::Backspace)]
        );
    }

    // Keyboards as they are.

    #[test]
    fn typing_a_word_and_a_space() {
        let mut harness = Harness::new("", "");

        for (n, word) in
            ["h", "he", "hel", "hell", "hello"].into_iter().enumerate()
        {
            assert_eq!(
                harness.run(|m| m.set_composing_text(word, 1)),
                [preedit(word, n + 1)]
            );
        }
        assert_eq!(harness.value(), "");

        // Space: the word is committed, then the space.
        assert_eq!(
            harness.run(|m| {
                m.begin_batch_edit();
                m.commit_text("hello", 1);
                m.commit_text(" ", 1);
                m.end_batch_edit();
            }),
            [clear(), commit("hello ")]
        );
        assert_eq!(
            (harness.value().as_str(), harness.preedit()),
            ("hello ", "")
        );
    }

    #[test]
    fn autocorrect_replaces_the_composing_word() {
        let mut harness = Harness::new("I said ", "");

        let _ = harness.run(|m| m.set_composing_text("teh", 1));
        assert_eq!(harness.preedit(), "teh");

        let _ = harness.run(|m| {
            m.begin_batch_edit();
            m.commit_text("the", 1);
            m.commit_text(" ", 1);
            m.end_batch_edit();
        });
        assert_eq!(
            (harness.value().as_str(), harness.preedit()),
            ("I said the ", "")
        );
    }

    #[test]
    fn autocorrect_replaces_a_committed_word() {
        let mut harness = Harness::new("", "");

        let _ = harness.run(|m| m.commit_text("I like teh ", 1));
        let edits = harness.run(|m| {
            m.begin_batch_edit();
            m.set_composing_region(7, 10);
            m.commit_text("the", 1);
            m.set_selection(11, 11);
            m.end_batch_edit();
        });

        assert_eq!(harness.value(), "I like the ");
        assert_eq!(
            edits,
            [
                Edit::Key(Key::Left),
                Edit::Key(Key::Backspace),
                Edit::Key(Key::Backspace),
                commit("he"),
                Edit::Key(Key::Right),
            ]
        );
    }

    #[test]
    fn swipe_typing_composes_whole_words() {
        let mut harness = Harness::new("", "");

        assert_eq!(
            harness.run(|m| m.set_composing_text("hello", 1)),
            [preedit("hello", 5)]
        );
        // The next swipe commits the word and a space, and composes its own.
        assert_eq!(
            harness.run(|m| {
                m.begin_batch_edit();
                m.finish_composing_text();
                m.commit_text(" ", 1);
                m.set_composing_text("world", 1);
                m.end_batch_edit();
            }),
            [clear(), commit("hello "), preedit("world", 5)]
        );
        // A suggestion replaces the swiped word.
        let _ = harness.run(|m| m.commit_text("word", 1));
        assert_eq!(
            (harness.value().as_str(), harness.preedit()),
            ("hello word", "")
        );
    }

    #[test]
    fn backspacing_into_the_previous_word_composes_it_again() {
        let mut harness = Harness::new("> ", "");

        let _ = harness.run(|m| m.commit_text("hello ", 1));
        // Backspace removes the space, and the keyboard takes the word back
        // into its composition.
        let _ = harness.run(|m| m.delete_surrounding_text(1, 0));
        assert_eq!(harness.value(), "> hello");
        let _ = harness.run(|m| m.set_composing_region(0, 5));
        assert_eq!(
            (harness.value().as_str(), harness.preedit()),
            ("> ", "hello")
        );

        let _ = harness.run(|m| m.set_composing_text("hell", 1));
        assert_eq!(harness.preedit(), "hell");
        let _ = harness.run(|m| m.commit_text("help", 1));
        assert_eq!(
            (harness.value().as_str(), harness.preedit()),
            ("> help", "")
        );
    }

    #[test]
    fn pinyin_composes_and_commits_chinese() {
        let mut harness = Harness::new("", "!");

        for input in ["n", "ni", "nih", "niha", "nihao"] {
            let _ = harness.run(|m| m.set_composing_text(input, 1));
            assert_eq!(harness.preedit(), input);
        }
        assert_eq!(
            harness.run(|m| m.commit_text("你好", 1)),
            [clear(), commit("你好")]
        );
        assert_eq!(harness.value(), "你好!");
    }

    #[test]
    fn a_suggestion_with_a_line_break_is_committed_without_it() {
        let mut harness = Harness::new("", "");

        assert_eq!(
            harness.run(|m| m.commit_text("Dear Sam,\nthanks", 1)),
            [commit("Dear Sam,thanks")]
        );
        let _ = harness.run(|m| m.send_key(KEYCODE_DEL));
        assert_eq!(harness.value(), "Dear Sam,thank");
    }

    #[test]
    fn enter_from_the_keyboard_is_a_key() {
        let mut harness = Harness::new("", "");

        let _ = harness.run(|m| m.set_composing_text("hi", 1));
        // The Java side finishes the composition and sends Enter as a key.
        assert_eq!(
            harness.run(|m| m.commit_text("\n", 1)),
            [clear(), commit("hi"), Edit::Key(Key::Enter)]
        );
        assert_eq!(harness.value(), "hi");
        assert_eq!(harness.posted, [1]);

        // A message sent before the keyboard ran the reset is dropped.
        assert_eq!(harness.run(|m| m.commit_text("lost", 1)), []);
        harness.settle();
        assert_eq!(harness.run(|m| m.commit_text("!", 1)), [commit("!")]);
        assert_eq!(harness.value(), "hi!");
    }

    #[test]
    fn performing_the_editor_action_commits_the_composition_first() {
        let mut harness = Harness::new("", "");

        let _ = harness.run(|m| m.set_composing_text("send", 1));
        assert_eq!(
            harness.run(|m| m.send_key(KEYCODE_ENTER)),
            [clear(), commit("send"), Edit::Key(Key::Enter)]
        );
    }

    #[test]
    fn a_secure_field_rebases_after_each_report() {
        let mut harness = Harness::with("", "", true, 4096);

        for (n, c) in "pass".chars().enumerate() {
            assert_eq!(
                harness.run(|m| m.send_char(c)),
                [commit(&c.to_string())]
            );
            assert!(
                harness.mirror.units.is_empty(),
                "the mirror kept {n} units"
            );
        }
        // Backspace on the empty mirror is a key.
        assert_eq!(
            harness.run(|m| m.send_key(KEYCODE_DEL)),
            [Edit::Key(Key::Backspace)]
        );
        assert_eq!(harness.value(), "pas");

        // A keyboard that commits whole words.
        let _ = harness.run(|m| m.commit_text("word", 1));
        assert_eq!(harness.value(), "pasword");
    }

    #[test]
    fn the_mirror_cap_rebases_and_typing_goes_on() {
        let mut harness = Harness::with("", "", false, 8);

        // The ninth unit is past the cap.
        for c in "abcdefghi".chars() {
            let _ = harness.run(|m| m.send_char(c));
        }
        assert_eq!(harness.value(), "abcdefghi");
        assert!(harness.mirror.units.is_empty());

        // The mirror lost the text before the rebase: Backspace deletes
        // what it has, then becomes a key that still deletes in the app.
        let _ = harness.run(|m| m.send_char('j'));
        assert_eq!(
            harness.run(|m| m.send_key(KEYCODE_DEL)),
            [Edit::Key(Key::Backspace)]
        );
        for _ in 0..2 {
            let _ = harness.run(|m| m.send_key(KEYCODE_DEL));
        }
        assert_eq!(harness.value(), "abcdefg");
    }

    #[test]
    fn a_tap_commits_the_composition_and_restarts_there() {
        let mut harness = Harness::new("one two", "");

        let _ = harness.run(|m| m.set_composing_text(" thr", 1));
        harness.tap(3);
        assert_eq!(
            (harness.value().as_str(), harness.preedit()),
            ("one two thr", "")
        );
        assert_eq!(harness.posted, [1]);

        harness.settle();
        let _ = harness.run(|m| m.commit_text(",", 1));
        assert_eq!(harness.value(), "one, two thr");
    }

    #[test]
    fn a_hardware_key_commits_the_composition_first() {
        let mut harness = Harness::new("", "");

        let _ = harness.run(|m| m.set_composing_text("ab", 1));
        harness.hardware('c');
        assert_eq!(harness.value(), "abc");
    }

    #[test]
    fn a_reset_drops_what_the_keyboard_sent_before_it_ran() {
        let mut harness = Harness::new("", "");

        let _ = harness.run(|m| m.commit_text("a", 1));
        // An arrow from the keyboard asks for a reset; the keyboard types on
        // before it runs.
        harness.mirror.send_key(KEYCODE_DPAD_LEFT);
        harness.mirror.commit_text("b", 1);
        harness.drain();
        assert_eq!(harness.value(), "a");

        harness.mirror.commit_text("c", 1);
        harness.adopt();
        harness.mirror.commit_text("d", 1);
        harness.drain();
        // The caret moved left of "a" with the arrow.
        assert_eq!(harness.value(), "da");
    }

    // Random keyboards.

    /// A small deterministic generator (xorshift64*).
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }

        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }

        fn between(&mut self, low: i64, high: i64) -> i64 {
            low + self.below((high - low + 1) as usize) as i64
        }

        fn chance(&mut self, percent: usize) -> bool {
            self.below(100) < percent
        }

        fn pick<T: Copy>(&mut self, items: &[T]) -> T {
            items[self.below(items.len())]
        }
    }

    const PIECES: &[&str] = &[
        "a",
        "b",
        "e",
        "s",
        "hello",
        "world",
        "the",
        "teh",
        "I",
        " ",
        ".",
        ",",
        "é",
        "e\u{301}",
        "\u{301}",
        "\u{308}",
        "ñ",
        "naïve",
        "über",
        "ß",
        "你",
        "好",
        "你好",
        "世界",
        "日本語",
        "ni",
        "hao",
        "😀",
        "👍",
        "👍🏽",
        "🏽",
        "\u{200D}",
        "👨\u{200D}👩\u{200D}👧",
        "👩🏽\u{200D}💻",
        "🇩🇪",
        "🇫",
        "🇷",
        "한",
        "\u{1112}",
        "\u{1161}",
        "\u{11AB}",
        "क",
        "\u{94D}",
        "ष",
        "\n",
        "\r\n",
        "\t",
        "line\nbreak",
        "\u{FFFD}",
    ];

    const KEYS: &[i32] = &[
        KEYCODE_ENTER,
        KEYCODE_NUMPAD_ENTER,
        KEYCODE_TAB,
        KEYCODE_ESCAPE,
        KEYCODE_DPAD_UP,
        KEYCODE_DPAD_DOWN,
        KEYCODE_DPAD_LEFT,
        KEYCODE_DPAD_RIGHT,
        KEYCODE_MOVE_HOME,
        KEYCODE_MOVE_END,
    ];

    fn random_text(rng: &mut Rng) -> String {
        (0..rng.between(1, 3)).map(|_| rng.pick(PIECES)).collect()
    }

    /// One call a keyboard makes that edits the mirror.
    fn text_op(m: &mut Mirror, rng: &mut Rng) {
        let len = m.len() as i64;

        match rng.below(44) {
            // Typing: the composition grows.
            0..=17 => {
                let text = m.composing_text() + rng.pick(PIECES);
                m.set_composing_text(&text, 1);
            }
            // The composition, or a correction, is committed, often with a
            // space.
            18..=23 => {
                let mut text = if rng.chance(50) {
                    m.composing_text()
                } else {
                    random_text(rng)
                };

                if rng.chance(50) {
                    text.push(' ');
                }

                m.commit_text(&text, 1);
            }
            24..=25 => m.commit_text(&random_text(rng), rng.between(-3, 3)),
            26..=27 => {
                m.set_composing_text(&random_text(rng), rng.between(-3, 3));
            }
            28..=30 => m.finish_composing_text(),
            31..=35 => m.delete_surrounding_text(rng.below(4), rng.below(3)),
            // A word before the cursor composes again.
            36..=38 => {
                let end = m.selection.1 as i64;
                m.set_composing_region(end - rng.between(1, 6), end);
            }
            39..=40 => m.set_composing_region(
                rng.between(-2, len + 2),
                rng.between(-2, len + 2),
            ),
            41 => m.set_selection(
                rng.between(-1, len + 1),
                rng.between(-1, len + 1),
            ),
            _ => {
                let cursor = rng.between(0, len);
                m.set_selection(cursor, cursor);
            }
        }
    }

    /// One thing a keyboard does.
    fn keyboard_op(m: &mut Mirror, rng: &mut Rng) {
        match rng.below(64) {
            0..=43 => text_op(m, rng),
            44..=51 => m.send_key(KEYCODE_DEL),
            52..=53 => m.send_key(KEYCODE_FORWARD_DEL),
            54..=55 => m.send_key(rng.pick(KEYS)),
            // Codes the Java side does not send; the engine ignores them.
            56 => m.key(rng.pick(&[0, 4, 82, -1, 999])),
            57 => m.commit_text("\n", 1),
            58..=59 => m.send_char(rng.pick(&['a', 'é', 'ß', '1', ' ', '你'])),
            _ => {
                m.begin_batch_edit();
                for _ in 0..rng.between(2, 4) {
                    text_op(m, rng);
                }
                m.end_batch_edit();
            }
        }
    }

    fn simulate(seed: u64, steps: usize, secure: bool, cap: usize) -> Seen {
        let mut rng = Rng::new(seed);
        let before = random_text(&mut rng);
        let after = if rng.chance(50) {
            random_text(&mut rng)
        } else {
            String::new()
        };
        let mut harness = Harness::with(&before, &after, secure, cap);

        for step in 0..steps {
            harness.label = format!("seed {seed} step {step}");

            match rng.below(100) {
                0..=79 => keyboard_op(&mut harness.mirror, &mut rng),
                80..=83 => {
                    let at = rng.below(harness.app.value.len() + 1);
                    harness.tap(at);
                }
                84..=85 => {
                    harness.hardware(rng.pick(&['x', 'ñ', '\u{301}', '好']));
                }
                86..=92 => harness.drain(),
                _ => harness.adopt(),
            }

            // The event loop often runs right away, and the UI thread soon
            // runs a posted reset.
            if rng.chance(50) {
                harness.drain();
            }

            if rng.chance(40) {
                harness.adopt();
            }
        }

        harness.label = format!("seed {seed} end");
        harness.settle();

        // Every reset ran, and what the mirror holds is what the app shows.
        assert_eq!(harness.mirror.generation, harness.engine.generation());

        let (units, selection, composing) = &harness.mirror.reported;
        let index = |index: usize| i32::try_from(index).unwrap();
        let expected = Expected::read(
            &String::from_utf16_lossy(units),
            (index(selection.0), index(selection.1)),
            composing
                .map_or((-1, -1), |(start, end)| (index(start), index(end))),
        );

        assert_eq!(harness.engine.base, expected.committed, "seed {seed}");

        harness.seen
    }

    #[test]
    fn random_keyboards_keep_the_app_in_step() {
        let mut seen = Seen::default();

        for seed in 0..240 {
            let cap = [4096, 24, 64][seed as usize % 3];

            seen.add(simulate(seed, 400, false, cap));
        }

        seen.assert_reached(100, &[]);
    }

    #[test]
    fn random_keyboards_on_a_secure_field_keep_the_app_in_step() {
        let mut seen = Seen::default();

        for seed in 1000..1060 {
            seen.add(simulate(seed, 300, true, 4096));
        }

        // The mirror is empty after every report: nothing to revise, and
        // a tap finds the engine idle.
        seen.assert_reached(20, &["revisions", "interruptions"]);
    }
}
