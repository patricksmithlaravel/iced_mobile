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
//! what `text_input` moves and deletes by. The engine moves that caret one
//! cluster at a time in the order of the text ([`Key::Previous`] and
//! [`Key::Next`], not the keyboard's arrows): `text_editor`'s arrows move on
//! screen, backwards in a right-to-left line, so these reach it as moves of
//! their own (see `events` in the parent module). It deletes the clusters
//! the keyboard changed with Delete, from the start of the change, which
//! takes one cluster in both widgets. `text_editor`'s Backspace deletes one
//! character: it would leave the rest of a cluster the keyboard changed in
//! part (an emoji's skin tone, a flag's second half, a vowel sign), and the
//! commit of what the keyboard left of it would double that rest. A
//! Backspace reaches the field only for the keyboard's own Backspace with
//! nothing before its cursor, which deletes in the field's text before the
//! origin what the field's Backspace deletes.
//!
//! The app segments its whole value into clusters, so a character that joins
//! the cluster before it right after the origin joins the app's text before
//! the origin. The engine then asks the Java side to rebase
//! ([`Request::Forget`]): the mirror forgets its text, the origin moves to
//! the app's caret, and the keyboard never edits across that join (it would
//! otherwise take back, and lose, the app's text it joined). The engine
//! knows the text before the origin only where the keyboard typed it before
//! a rebase moved the origin past it (a request to forget, a password
//! field's, the cap's, a change of purpose), and keeps the end of it.
//! There, a character that joins only certain clusters is found too: a
//! consonant after a virama, which Indic scripts join into a conjunct
//! (Unicode's GB9c), or a regional indicator after an unpaired one.
//! Elsewhere (the focus, a tap, a caret moved from code, the keyboard's
//! Enter, Tab, Escape, arrows or Select all, its Backspace into the field's
//! text) the text before the origin is taken to end in an ordinary letter,
//! which only a character that joins any cluster joins (a combining mark, a
//! vowel sign or virama, a zero width joiner, an emoji modifier): a field
//! that ends in a virama there rarely gets a consonant typed after it,
//! while forgetting every session that starts with a consonant would cost
//! the keyboard its context in Indic scripts.
//!
//! The end of the session's text can join the app's text after it in the
//! same way, and nothing handles that: in Indic scripts a consonant and
//! virama typed right before a consonant of the field make one conjunct
//! with it. The engine still counts the virama's cluster as its own, so a
//! revision of it (the keyboard's Backspace on the virama, or another
//! consonant after it) deletes the whole conjunct from the app, the field's
//! consonant with it, and text committed there lands after the conjunct,
//! where `text_input` puts its caret. The engine cannot see the
//! text after the session's. `docs/agents/limitations.md` lists both.
//!
//! Line breaks and tabs are never part of the text the engine compares or
//! commits: Enter and Tab reach the app as keys (the Java side turns a commit
//! of only a line break into Enter and of only a tab into Tab, as iOS does),
//! and inside longer text they are dropped, as `text_input` drops them from
//! such an insertion on iOS. The Java side keeps them out of what the
//! keyboard commits, composes and types, so that the keyboard does not
//! delete a character the app did not get; the engine drops them from what
//! it reads too, which also covers `replaceText` (Android 14), whose text
//! the Java side does not filter.

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
/// The keyboard's Select all (`performContextMenuAction` with
/// `android.R.id.selectAll`), which the Java side sends as a key: Android
/// has no key code for it, and none is this large.
pub(crate) const SELECT_ALL: i32 = 0x0001_0000;

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
    /// Escape, an arrow, Home or End, or its Select all. `code` is Android's
    /// key code, or [`SELECT_ALL`].
    Key { generation: i32, code: i32 },
    /// The Java side emptied the mirror without a reset (a secure field
    /// after each report, a mirror grown past its cap, a key that moves the
    /// app's caret or may submit the field, a change of purpose, or
    /// [`Request::Forget`]): the app's caret becomes the session's origin.
    Rebase { generation: i32 },
}

/// What the caller does after [`Engine::apply`], before it applies another
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    /// Nothing.
    None,
    /// Gives the field the edits so far, and lets the application handle
    /// them and build its view again, before the next message is applied:
    /// after an Enter, Tab or Escape key, the field may have been submitted
    /// (and cleared) or the focus moved, and the text the keyboard typed
    /// after the key belongs to what the field is then (the application's
    /// turn, in `feed`).
    Yield,
    /// Asks the Java side to rebase at the engine's generation
    /// (`Keyboard.forget`), with no new generation: the session's text
    /// starts with a character that joins the cluster before the origin in
    /// the app. Messages sent before the rebase still apply; the
    /// [`Message::Rebase`] that follows moves the origin to the app's caret.
    Forget,
}

/// A key press for the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Key {
    Backspace,
    Delete,
    /// The engine's own caret move: back one cluster in the order of the
    /// text, whatever the direction of the line.
    Previous,
    /// The engine's own caret move: on one cluster in the order of the text.
    Next,
    /// The keyboard's arrow keys, which move as a hardware keyboard's do.
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Enter,
    Tab,
    Escape,
    /// Selects all of the field's text: its own shortcut.
    SelectAll,
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
            SELECT_ALL => Self::SelectAll,
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
/// the edits to the window that has the input method, and does what `apply`
/// [requests](Request) before it applies another message. It calls
/// [`Engine::interrupt`] (then hands a new generation to the Java side)
/// when the caret moves some other way too: a tap, a hardware key, or the
/// application moving it from code ([`Engine::follows_caret`]). Messages
/// queued before such an event belong before it, so the caller applies
/// them first, and a composition is committed before the caret moves.
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
    /// A [`Request::Forget`] was made and its rebase has not arrived.
    forgetting: bool,
    /// The end of the app's text before the origin, as far as the engine
    /// knows it: the last cluster of what the keyboard typed there before
    /// a rebase moved the origin past it (see [`context`]). Empty when
    /// unknown.
    context: String,
}

impl Engine {
    /// An engine at generation 0 with nothing typed.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The generation the keyboard's messages must carry to be applied, and
    /// that a [`Request::Forget`] names.
    pub(crate) fn generation(&self) -> i32 {
        self.generation
    }

    /// Applies one message from the keyboard; pushes the edits for the app,
    /// and returns what the caller does next.
    ///
    /// An Enter, Tab or Escape key from the keyboard first commits a shown
    /// preedit, so a field submitted by Enter has the word being composed,
    /// and yields. An arrow, Home or End key, and Select all, commit it too.
    /// After any of them the Java side rebases when the mirror held text,
    /// and an empty mirror leaves the engine idle, so either way the origin
    /// follows the app's caret, with no new generation: what the keyboard
    /// sends after the key counts. A message of another generation predates
    /// a reset the caller made: it is dropped.
    pub(crate) fn apply(
        &mut self,
        message: Message,
        edits: &mut Vec<Edit>,
    ) -> Request {
        match message {
            Message::State {
                generation,
                text,
                selection,
                composing,
            } => {
                if generation != self.generation {
                    return Request::None;
                }

                self.state(&text, selection, composing, edits);

                if !self.forgetting && joins(&self.context, &self.base) {
                    self.forgetting = true;

                    Request::Forget
                } else {
                    Request::None
                }
            }
            Message::Key { generation, code } => {
                if generation == self.generation {
                    self.key(code, edits)
                } else {
                    Request::None
                }
            }
            Message::Rebase { generation } => {
                if generation == self.generation {
                    self.clear_preedit(edits);

                    // The origin moves to the app's caret, past what the
                    // keyboard typed before it.
                    let typed = prefix(&self.base, self.caret);
                    self.context = context(&self.context, typed);

                    self.base.clear();
                    self.caret = 0;
                    self.forgetting = false;
                }

                Request::None
            }
        }
    }

    /// Native-initiated reset (a tap, a hardware key, a caret moved from
    /// code): commits a shown composition the way Android does (Preedit
    /// cleared, then Commit of the composing text), forgets the text, and
    /// returns the new generation to give the keyboard.
    pub(crate) fn reset(&mut self, edits: &mut Vec<Edit>) -> i32 {
        self.commit_preedit(edits);

        self.base.clear();
        self.caret = 0;
        self.forgetting = false;
        self.context.clear();
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// The field lost the keyboard: what the keyboard typed and composes
    /// there is dropped, with the `dropped` messages, which were still
    /// queued and are not applied. Returns the new generation to give the
    /// keyboard (`Keyboard.reset`) when it may hold text the engine has not
    /// dropped, else none.
    ///
    /// It may whenever the engine is not idle, and whenever a message was
    /// dropped: the first report since the origin leaves the engine idle
    /// until it is applied, while the keyboard's mirror and composition
    /// already hold its text, which would otherwise reach the next field.
    pub(crate) fn abandon(&mut self, dropped: &[Message]) -> Option<i32> {
        self.context.clear();

        if dropped.is_empty() && self.is_idle() {
            return None;
        }

        Some(self.reset(&mut Vec::new()))
    }

    /// The app's caret moves some other way than through the keyboard (a
    /// tap, a hardware key, the application moving it): unless the engine
    /// is idle, it is [reset](Engine::reset), which commits a composition,
    /// and the new generation to give the keyboard is returned. Either way
    /// the origin moves with the caret, and what the engine knew of the
    /// text before it no longer holds.
    pub(crate) fn interrupt(&mut self, edits: &mut Vec<Edit>) -> Option<i32> {
        self.context.clear();

        (!self.is_idle()).then(|| self.reset(edits))
    }

    /// Whether a caret the application moves must reach the engine
    /// ([`Engine::interrupt`]): it is not idle, or it knows the end of the
    /// text before the origin.
    pub(crate) fn follows_caret(&self) -> bool {
        !self.is_idle() || !self.context.is_empty()
    }

    /// Nothing to reset: no text typed since the last reset and no preedit.
    ///
    /// Precisely: the mirror's committed text, as the engine last applied
    /// it without line breaks and tabs, is empty and no preedit is shown.
    /// The app's caret may then move without a reset: the session's origin
    /// moves with it.
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

            // The old clusters go from the start of the change, one Delete
            // each: `text_input` and `text_editor` both delete a whole
            // cluster forward, where `text_editor`'s Backspace deletes one
            // character and would leave the rest of a cluster the keyboard
            // changed in part (an emoji's skin tone, a flag's second half).
            self.move_to(prefix, edits);
            edits.extend(iter::repeat_n(Edit::Key(Key::Delete), removed));

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

    fn key(&mut self, code: i32, edits: &mut Vec<Edit>) -> Request {
        let Some(key) = Key::from_code(code) else {
            return Request::None;
        };

        match key {
            Key::Backspace => {
                // The keyboard presses Backspace only when the mirror has
                // nothing before its cursor, so what it deletes is before the
                // origin, wherever the caret rests (normally right there):
                // the text there is no longer the one the engine knew.
                let caret = self.caret;

                self.move_to(0, edits);
                edits.push(Edit::Key(Key::Backspace));
                self.move_to(caret, edits);
                self.context.clear();

                Request::None
            }
            Key::Delete => {
                // Likewise, the mirror had nothing after its cursor.
                let caret = self.caret;
                let end = self.base.graphemes(true).count();

                self.move_to(end, edits);
                edits.push(Edit::Key(Key::Delete));
                self.move_to(caret, edits);

                Request::None
            }
            // The engine's own moves come from no key code.
            Key::Previous
            | Key::Next
            | Key::Left
            | Key::Right
            | Key::Up
            | Key::Down
            | Key::Home
            | Key::End
            | Key::Enter
            | Key::Tab
            | Key::Escape
            | Key::SelectAll => {
                // The rebase that follows a non-empty mirror, or the empty
                // one, makes the app's caret after the key the origin (after
                // Select all, the selection the next text replaces): no
                // new generation, so the keys and text the keyboard sends
                // meanwhile count. The engine knows nothing of the text
                // before it, and starts over at once.
                self.commit_preedit(edits);
                edits.push(Edit::Key(key));

                self.base.clear();
                self.caret = 0;
                self.context.clear();

                if matches!(key, Key::Enter | Key::Tab | Key::Escape) {
                    Request::Yield
                } else {
                    Request::None
                }
            }
        }
    }

    /// Moves the app's caret to `position` (clusters after the origin), in
    /// the order of the text.
    fn move_to(&mut self, position: usize, edits: &mut Vec<Edit>) {
        let key = if position < self.caret {
            Key::Previous
        } else {
            Key::Next
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

/// What stands for the app's text before what the engine knows of it: a
/// Devanagari consonant. It is an ordinary letter to every character that
/// joins any cluster, and when the known text starts with characters that
/// join it (a virama typed first), the consonant before them makes it the
/// worst case: a consonant typed after that virama joins.
const UNKNOWN: char = '\u{915}';

/// The most characters [`context`] keeps: more than any cluster typed in
/// practice holds.
const CONTEXT: usize = 32;

/// Whether the first character of `text` joins the last cluster of the
/// app's text before it, of which `before` is the end the engine knows
/// (none: an ordinary letter).
///
/// With nothing known, that is a character that joins any cluster: a
/// combining or spacing mark (accents, Thai and Indic vowel signs and
/// viramas), a zero width joiner, an emoji modifier or a variation selector.
/// With `before` known, also one that joins only certain clusters: a
/// consonant after a virama, a regional indicator after an unpaired one, a
/// Hangul vowel jamo after a leading one. The end of the text is never
/// checked against the app's text after it (see the module's
/// documentation).
fn joins(before: &str, text: &str) -> bool {
    let Some(first) = text.chars().next() else {
        return false;
    };

    let mut probe = String::from(UNKNOWN);
    probe.push_str(before);
    let clusters = probe.graphemes(true).count();

    probe.push(first);

    probe.graphemes(true).count() == clusters
}

/// What the engine knows of the app's text before the origin once the
/// origin moves past `typed`, with `before` known before it: the last
/// cluster of the two, without the part that joins the unknown text before
/// them, and at most its last [`CONTEXT`] characters. When the whole of it
/// joins that unknown text, [`joins`] puts [`UNKNOWN`] back before it.
fn context(before: &str, typed: &str) -> String {
    if typed.is_empty() {
        return before.to_owned();
    }

    let mut probe = String::from(UNKNOWN);
    probe.push_str(before);
    probe.push_str(typed);

    let start = probe
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(start, _)| start)
        .max(UNKNOWN.len_utf8());

    let last = &probe[start..];
    let skipped = last.chars().count().saturating_sub(CONTEXT);

    last.chars().skip(skipped).collect()
}

/// The first `clusters` grapheme clusters of `text`.
fn prefix(text: &str, clusters: usize) -> &str {
    let end = text
        .grapheme_indices(true)
        .nth(clusters)
        .map_or(text.len(), |(start, _)| start);

    &text[..end]
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

    /// The edits one message gives, and what it requests.
    fn apply(engine: &mut Engine, message: Message) -> (Vec<Edit>, Request) {
        let mut edits = Vec::new();
        let request = engine.apply(message, &mut edits);

        (edits, request)
    }

    /// The edits one message gives when it requests nothing.
    fn edits(engine: &mut Engine, message: Message) -> Vec<Edit> {
        let (edits, request) = apply(engine, message);
        assert_eq!(request, Request::None, "unexpected request: {edits:?}");

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

    /// What deletes the `count` clusters right before the app's caret: the
    /// caret goes back over them, then a Delete takes each.
    fn deleted(count: usize) -> Vec<Edit> {
        [keys(Key::Previous, count), keys(Key::Delete, count)].concat()
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
        /// All of the value is selected (Select all), which the next text,
        /// Backspace or Delete replaces and a move ends.
        selected: bool,
        /// A line of a `text_editor`: its Backspace deletes one character,
        /// as cosmic-text's does, where `text_input`'s deletes a cluster.
        /// Its Delete and moves go by cluster too.
        editor: bool,
        /// A right-to-left line of a `text_editor`: its arrows move on
        /// screen, so Left goes on in the order of the text and Right back,
        /// while the engine's own moves go in that order.
        rtl: bool,
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
                selected: false,
                editor: false,
                rtl: false,
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
                if std::mem::take(&mut self.selected) {
                    match edit {
                        Edit::Key(Key::Backspace | Key::Delete) => {
                            self.value.clear();
                            self.caret = 0;
                            continue;
                        }
                        Edit::Commit(_) => {
                            self.value.clear();
                            self.caret = 0;
                        }
                        // A move ends the selection at its start or end,
                        // in the order of the text, whatever the line.
                        Edit::Key(Key::Left | Key::Previous) => {
                            self.caret = 0;
                            continue;
                        }
                        Edit::Key(Key::Right | Key::Next) => {
                            self.caret = self.value.len();
                            continue;
                        }
                        Edit::Key(
                            Key::Enter
                            | Key::Tab
                            | Key::Escape
                            | Key::SelectAll,
                        )
                        | Edit::Preedit(..) => self.selected = true,
                        Edit::Key(
                            Key::Up | Key::Down | Key::Home | Key::End,
                        ) => {}
                    }
                }

                match edit {
                    Edit::Key(Key::Backspace) => self.backspace(),
                    Edit::Key(Key::Delete) => {
                        if self.caret < self.value.len() {
                            let _ = self.value.remove(self.caret);
                        }
                    }
                    Edit::Key(Key::Previous) => self.back(),
                    Edit::Key(Key::Next) => self.on(),
                    Edit::Key(Key::Left) if self.rtl => self.on(),
                    Edit::Key(Key::Right) if self.rtl => self.back(),
                    Edit::Key(Key::Left) => self.back(),
                    Edit::Key(Key::Right) => self.on(),
                    Edit::Key(Key::Home) => self.caret = 0,
                    Edit::Key(Key::End) => self.caret = self.value.len(),
                    Edit::Key(
                        Key::Up
                        | Key::Down
                        | Key::Enter
                        | Key::Tab
                        | Key::Escape,
                    ) => {}
                    Edit::Key(Key::SelectAll) => {
                        self.selected = !self.value.is_empty();
                        self.caret = self.value.len();
                    }
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

        /// Backspace: the cluster before the caret goes, or in a
        /// `text_editor` only its last character, the rest of it staying a
        /// cluster before the caret.
        fn backspace(&mut self) {
            if self.caret == 0 {
                return;
            }

            if self.editor {
                let cluster = &mut self.value[self.caret - 1];
                let _ = cluster.pop();

                if !cluster.is_empty() {
                    return;
                }
            }

            self.caret -= 1;
            let _ = self.value.remove(self.caret);
        }

        /// The caret back one cluster, in the order of the text.
        fn back(&mut self) {
            self.caret = self.caret.saturating_sub(1);
        }

        /// The caret on one cluster, in the order of the text.
        fn on(&mut self) {
            self.caret = (self.caret + 1).min(self.value.len());
        }

        /// A character typed on a hardware keyboard: `text_input` inserts it
        /// and segments its whole value again.
        fn type_char(&mut self, c: char) {
            self.value.insert(self.caret, c.to_string());
            self.resegment();
            self.caret = (self.caret + 1).min(self.value.len());
        }

        /// What the app's next `view` does: `text_input` builds its value
        /// from the string again, segmenting all of it, and keeps the
        /// caret's index.
        fn resegment(&mut self) {
            self.value = graphemes(&self.value.concat());
            self.caret = self.caret.min(self.value.len());
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

    /// What the keyboard was told last: the selection and the composing
    /// region (`InputMethodManager.updateSelection`).
    type Told = ((usize, usize), Option<(usize, usize)>);

    /// The key a text that is only a line break or a tab stands for
    /// (`KeyboardConnection.key`).
    fn key_for(text: &str) -> Option<i32> {
        match text {
            "\n" | "\r" | "\r\n" => Some(KEYCODE_ENTER),
            "\t" => Some(KEYCODE_TAB),
            _ => None,
        }
    }

    /// The text without its line breaks and tabs
    /// (`KeyboardConnection.plain`).
    fn plain(text: &str) -> String {
        text.chars()
            .filter(|c| !matches!(c, '\n' | '\r' | '\t'))
            .collect()
    }

    /// Whether the mirror starts over after a key: it moves the app's caret
    /// or may submit the field (`KeyboardView.startsOver`).
    fn starts_over(code: i32) -> bool {
        matches!(
            code,
            KEYCODE_DPAD_UP
                | KEYCODE_DPAD_DOWN
                | KEYCODE_DPAD_LEFT
                | KEYCODE_DPAD_RIGHT
                | KEYCODE_MOVE_HOME
                | KEYCODE_MOVE_END
                | KEYCODE_ENTER
                | KEYCODE_NUMPAD_ENTER
                | KEYCODE_TAB
                | KEYCODE_ESCAPE
                | SELECT_ALL
        )
    }

    /// The Java side: an editable mirror in UTF-16 units that the keyboard
    /// edits through the calls of Android's `BaseInputConnection` (over a
    /// `SpannableStringBuilder`), as `KeyboardConnection` and `KeyboardView`
    /// handle them. It reports its whole state when it changed, once per
    /// outermost batch edit, and tells the keyboard its selection.
    #[derive(Debug)]
    struct Mirror {
        units: Vec<u16>,
        /// Start and end; the start may be after the end.
        selection: (usize, usize),
        composing: Option<(usize, usize)>,
        generation: i32,
        /// A secure field: a composition is reported as typed text, and the
        /// mirror is emptied and rebased after each report once nothing
        /// composes.
        secure: bool,
        /// Empty the mirror and rebase when it grows past this many units
        /// with no composition.
        cap: usize,
        /// The native side asked to forget the text: rebase once nothing
        /// composes and no batch edit is open.
        forgetting: bool,
        batch: usize,
        reported: Snapshot,
        told: Told,
        /// Every selection the keyboard was told, in order.
        updates: Vec<Told>,
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
                forgetting: false,
                batch: 0,
                reported: (Vec::new(), (0, 0), None),
                told: ((0, 0), None),
                updates: Vec::new(),
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

        /// `commitText`. A commit of only a line break (what keyboards
        /// commit for Enter) or a tab is the key instead, and longer text
        /// goes in without its line breaks and tabs.
        fn commit_text(&mut self, text: &str, new_cursor: i64) {
            match key_for(text) {
                Some(code) => self.forward(code),
                None => self.replace(&plain(text), new_cursor, false),
            }
        }

        fn set_composing_text(&mut self, text: &str, new_cursor: i64) {
            self.replace(&plain(text), new_cursor, true);
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

        /// `KeyboardConnection.deleteSurroundingText`: in UTF-16 units (so
        /// it may split a surrogate pair), around the selection and the
        /// composing region together.
        fn delete_surrounding_text(&mut self, before: usize, after: usize) {
            self.delete_around(before, after, false);
        }

        /// `KeyboardConnection.deleteSurroundingTextInCodePoints`.
        fn delete_surrounding_text_in_code_points(
            &mut self,
            before: usize,
            after: usize,
        ) {
            self.delete_around(before, after, true);
        }

        /// Deletes `before` units (or code points) before the selection and
        /// the composing region, and `after` after them. A side the keyboard
        /// asked to delete that is empty, with no selection and no
        /// composition, is the Backspace or Delete key instead: some
        /// keyboards (AOSP's LatinIME and its forks) delete that way even at
        /// the start of the text.
        fn delete_around(
            &mut self,
            before: usize,
            after: usize,
            code_points: bool,
        ) {
            let bare = self.selection.0 == self.selection.1
                && self.composing.is_none();
            let (mut start, mut end) = self.ordered_selection();

            if let Some((composing_start, composing_end)) = self.composing {
                start = start.min(composing_start);
                end = end.max(composing_end);
            }

            let backward = bare && before > 0 && start == 0;
            let forward = bare && after > 0 && end == self.len();

            let (from, to) = if code_points {
                let mut from = start;

                for _ in 0..before {
                    if from == 0 {
                        break;
                    }

                    from -= 1;

                    if from > 0
                        && is_low(self.units[from])
                        && is_high(self.units[from - 1])
                    {
                        from -= 1;
                    }
                }

                let mut to = end;

                for _ in 0..after {
                    if to == self.len() {
                        break;
                    }

                    to += 1;

                    if to < self.len()
                        && is_low(self.units[to])
                        && is_high(self.units[to - 1])
                    {
                        to += 1;
                    }
                }

                (from, to)
            } else {
                (start.saturating_sub(before), (end + after).min(self.len()))
            };

            self.begin_batch_edit();
            self.delete(end, to);
            self.delete(from, start);
            self.end_batch_edit();

            if backward {
                self.forward(KEYCODE_DEL);
            }

            if forward {
                self.forward(KEYCODE_FORWARD_DEL);
            }
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
                _ => self.forward(code),
            }
        }

        /// A key with a character (`KeyboardConnection.type`): it finishes
        /// the composition and goes in at the selection, as a hardware key
        /// types into a text field. A line break or a tab is the key.
        fn send_char(&mut self, c: char) {
            let text = c.to_string();

            if let Some(code) = key_for(&text) {
                self.forward(code);
                return;
            }

            self.begin_batch_edit();
            self.composing = None;
            self.replace(&text, 1, false);
            self.end_batch_edit();
        }

        /// `KeyboardConnection.performContextMenuAction` with
        /// `android.R.id.paste`: the clipboard's text, without its line
        /// breaks and tabs, goes in at the selection after any composition
        /// is finished, as a key's character does; never a key.
        fn paste(&mut self, clip: &str) {
            let text = plain(clip);

            if text.is_empty() {
                return;
            }

            self.begin_batch_edit();
            self.composing = None;
            self.replace(&text, 1, false);
            self.end_batch_edit();
        }

        /// `performContextMenuAction` with `android.R.id.copy` (or `cut`):
        /// the mirror's selection, which a cut also deletes; none when
        /// nothing is selected or the field is a password.
        fn copy(&mut self, cut: bool) -> Option<String> {
            let (start, end) = self.ordered_selection();

            if self.secure || start == end {
                return None;
            }

            let text = String::from_utf16_lossy(&self.units[start..end]);

            if cut {
                self.begin_batch_edit();
                self.delete(start, end);
                self.end_batch_edit();
            }

            Some(text)
        }

        /// `performContextMenuAction` with `android.R.id.selectAll`: the
        /// field's own Select all, which the mirror cannot hold.
        fn select_all(&mut self) {
            self.forward(SELECT_ALL);
        }

        /// `KeyboardView.forward`: the composition is finished and reported
        /// (even inside a batch edit), then the key goes to the app, and a
        /// key that moves the caret or may submit the field rebases a mirror
        /// that holds text.
        fn forward(&mut self, code: i32) {
            self.composing = None;
            self.send_report();
            self.key(code);

            if starts_over(code) && !self.units.is_empty() {
                self.rebase();
            }
        }

        /// A raw `nativeKey`.
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

        /// The end of an edit: a report unless a batch edit is open.
        fn report(&mut self) {
            if self.batch == 0 {
                self.send_report();
            }
        }

        /// `KeyboardView.report`.
        fn send_report(&mut self) {
            // A password field never shows a composition: it is reported as
            // typed text.
            let composing = if self.secure { None } else { self.composing };
            let snapshot = (self.units.clone(), self.selection, composing);

            if snapshot != self.reported {
                let index = |index: usize| i32::try_from(index).unwrap();

                self.out.push_back(Message::State {
                    generation: self.generation,
                    text: String::from_utf16_lossy(&self.units),
                    selection: (
                        index(self.selection.0),
                        index(self.selection.1),
                    ),
                    composing: composing.map_or((-1, -1), |(start, end)| {
                        (index(start), index(end))
                    }),
                });
                self.reported = snapshot;
            }

            // The keyboard sees where its edit left the cursor, then the
            // rebase.
            self.tell();

            let clear = self.forgetting
                || if self.secure {
                    !self.units.is_empty()
                } else {
                    self.len() > self.cap
                };

            // Not while something composes, nor inside a batch edit, which
            // the keyboard worked out from the text it saw when it began.
            if clear && self.composing.is_none() && self.batch == 0 {
                self.rebase();
            }
        }

        /// `KeyboardView.sendSelection`.
        fn tell(&mut self) {
            let now = (self.selection, self.composing);

            if now != self.told {
                self.told = now;
                self.updates.push(now);
            }
        }

        /// `KeyboardView.rebase`: the mirror is emptied, the shell told,
        /// then the keyboard.
        fn rebase(&mut self) {
            self.clear();
            self.out.push_back(Message::Rebase {
                generation: self.generation,
            });
            self.tell();
        }

        fn clear(&mut self) {
            self.units.clear();
            self.selection = (0, 0);
            self.composing = None;
            self.forgetting = false;
            self.reported = (Vec::new(), (0, 0), None);
        }

        /// `Keyboard.reset(generation)` running on the UI thread: the
        /// keyboard starts over with the empty mirror.
        fn adopt(&mut self, generation: i32) {
            self.generation = generation;
            self.clear();
            self.told = ((0, 0), None);
        }

        /// `Keyboard.forget(generation)` running on the UI thread: the
        /// mirror is rebased once nothing composes, and inside a batch edit
        /// once the batch ends, after its report.
        fn forget(&mut self, generation: i32) {
            if generation != self.generation {
                return;
            }

            self.forgetting = true;

            if self.batch == 0 {
                self.send_report();
            }
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
        /// What the native side posted that the Java side has not run.
        posted: VecDeque<Posted>,
        /// The edits given to the app since the last `take`.
        log: Vec<Edit>,
        seen: Seen,
    }

    /// A call the native side posts to the Java side's UI thread.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Posted {
        Reset(i32),
        Forget(i32),
    }

    /// What a run went through, so the random runs show they reach every
    /// path.
    #[derive(Debug, Default, Clone, Copy)]
    struct Seen {
        /// Reports applied, with a composition, with U+FFFD in the text,
        /// and that replaced text (a Delete and a commit).
        reports: usize,
        compositions: usize,
        replacements: usize,
        revisions: usize,
        /// Messages dropped for their generation.
        dropped: usize,
        /// Backspace and Delete keys, Enter, Tab and Escape keys (after
        /// which the application takes its turn), and keys that moved the
        /// caret.
        deletions: usize,
        submits: usize,
        moves: usize,
        /// Taps and hardware keys that reset, rebases, and requests to
        /// forget the text.
        interruptions: usize,
        rebases: usize,
        forgets: usize,
        /// Requests to forget run inside a batch edit.
        batched_forgets: usize,
        /// Requests to forget for a character that joins only the text the
        /// engine knows before the origin (a consonant after a virama, a
        /// regional indicator after an unpaired one).
        known_joins: usize,
    }

    impl Seen {
        fn add(&mut self, other: Self) {
            self.reports += other.reports;
            self.compositions += other.compositions;
            self.replacements += other.replacements;
            self.revisions += other.revisions;
            self.dropped += other.dropped;
            self.deletions += other.deletions;
            self.submits += other.submits;
            self.moves += other.moves;
            self.interruptions += other.interruptions;
            self.rebases += other.rebases;
            self.forgets += other.forgets;
            self.batched_forgets += other.batched_forgets;
            self.known_joins += other.known_joins;
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
                ("submits", self.submits),
                ("moves", self.moves),
                ("interruptions", self.interruptions),
                ("rebases", self.rebases),
                ("forgets", self.forgets),
                ("batched forgets", self.batched_forgets),
                ("known joins", self.known_joins),
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

        /// The field is a line of a `text_editor` (see [`Field::editor`]).
        fn editor(mut self) -> Self {
            self.app.editor = true;
            self
        }

        /// The field is a right-to-left line of a `text_editor` (see
        /// [`Field::rtl`]).
        fn right_to_left(mut self) -> Self {
            self.app.editor = true;
            self.app.rtl = true;
            self
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

            let context = self.engine.context.clone();
            let mut edits = Vec::new();
            let request = self.engine.apply(message.clone(), &mut edits);

            if generation != current {
                assert!(
                    edits.is_empty() && request == Request::None,
                    "{label}: a message of generation {generation} gave {edits:?} and {request:?} in generation {current}"
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
                    assert_ne!(
                        request,
                        Request::Yield,
                        "{label}: a report yielded"
                    );
                    self.last = Expected::read(&text, selection, composing);
                    self.check();

                    if request == Request::Forget {
                        assert!(
                            joins(&context, &self.last.committed),
                            "{label}: forget without a joining character"
                        );
                        self.posted.push_back(Posted::Forget(generation));
                        self.seen.forgets += 1;
                        self.seen.known_joins +=
                            usize::from(!joins("", &self.last.committed));
                    }

                    let has =
                        |wanted: fn(&Edit) -> bool| edits.iter().any(wanted);
                    self.seen.reports += 1;
                    self.seen.compositions +=
                        usize::from(self.last.composing.is_some());
                    self.seen.replacements +=
                        usize::from(text.contains('\u{FFFD}'));
                    self.seen.revisions += usize::from(
                        has(|edit| *edit == Edit::Key(Key::Delete))
                            && has(|edit| matches!(edit, Edit::Commit(_))),
                    );
                }
                Message::Key { code, .. } => match code {
                    KEYCODE_DEL => {
                        // The field's own Backspace, before the origin: a
                        // cluster, or a `text_editor`'s last character.
                        assert_eq!(request, Request::None, "{label}");

                        if let Some(last) = self.before.last_mut() {
                            if self.app.editor {
                                let _ = last.pop();
                            } else {
                                last.clear();
                            }

                            if last.is_empty() {
                                let _ = self.before.pop();
                            }
                        }

                        self.check();
                        self.seen.deletions += 1;
                    }
                    KEYCODE_FORWARD_DEL => {
                        assert_eq!(request, Request::None, "{label}");
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
                        let submits = matches!(
                            code,
                            KEYCODE_ENTER
                                | KEYCODE_NUMPAD_ENTER
                                | KEYCODE_TAB
                                | KEYCODE_ESCAPE
                        );

                        assert_eq!(
                            request,
                            if submits {
                                Request::Yield
                            } else {
                                Request::None
                            },
                            "{label}: key {code}"
                        );
                        assert_eq!(
                            edits.last(),
                            Some(&Edit::Key(Key::from_code(code).unwrap())),
                            "{label}"
                        );
                        // The composition is committed where it was, before
                        // the key, and the key edits no text.
                        self.check_finished();

                        // The app's caret after the key is the origin: at
                        // once when the mirror was empty (the engine is
                        // idle), else at the rebase that follows. Nothing
                        // the keyboard sends meanwhile is dropped.
                        self.restart();

                        if submits {
                            self.seen.submits += 1;
                        } else {
                            self.seen.moves += 1;
                        }
                    }
                    _ => {
                        assert!(
                            edits.is_empty() && request == Request::None,
                            "{label}: unknown key {code} gave {edits:?}"
                        );
                    }
                },
                Message::Rebase { .. } => {
                    assert_eq!(
                        request,
                        Request::None,
                        "{label}: a rebase asked for {request:?}"
                    );
                    // The Java side rebases only once nothing composes; a
                    // shown preedit would be dropped. The caret stays where
                    // the report put it.
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
            assert!(
                self.before.concat().ends_with(&self.engine.context),
                "{label}: the engine's context {:?} does not end the text before the origin",
                self.engine.context,
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

            let idle = self.engine.is_idle();
            let mut edits = Vec::new();
            let generation = self.engine.interrupt(&mut edits);

            assert_eq!(generation.is_none(), idle, "{}", self.label);
            assert_eq!(self.engine.context, "", "{}", self.label);

            match generation {
                None => {
                    assert!(
                        self.last.committed.is_empty()
                            && self.last.composing.is_none()
                            && edits.is_empty(),
                        "{}: idle with text",
                        self.label
                    );
                }
                Some(generation) => {
                    self.app.apply(&edits);
                    self.log.extend(edits);
                    self.check_finished();
                    self.posted.push_back(Posted::Reset(generation));
                    self.seen.interruptions += 1;
                }
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

        /// The Java side runs the oldest call the native side posted.
        fn adopt(&mut self) {
            match self.posted.pop_front() {
                Some(Posted::Reset(generation)) => {
                    self.mirror.adopt(generation);
                }
                Some(Posted::Forget(generation)) => {
                    self.mirror.forget(generation);
                }
                None => {}
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

                // A report among these may post a request to forget.
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
                (vec![], Request::None)
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
            [deleted(1), vec![commit("e\u{301}")]].concat()
        );

        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "👍", (2, 2), (-1, -1)));
        assert_eq!(
            edits(&mut engine, state(0, "👍🏽", (4, 4), (-1, -1))),
            [deleted(1), vec![commit("👍🏽")]].concat()
        );

        // A change in the middle: the caret goes to its start, Delete
        // removes it a cluster at a time, the new text is committed, the
        // caret goes to the target.
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "abcd", (4, 4), (-1, -1)));
        assert_eq!(
            edits(&mut engine, state(0, "aXYd", (4, 4), (-1, -1))),
            [
                Edit::Key(Key::Previous),
                Edit::Key(Key::Previous),
                Edit::Key(Key::Previous),
                Edit::Key(Key::Delete),
                Edit::Key(Key::Delete),
                commit("XY"),
                Edit::Key(Key::Next),
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
            deleted(2)
        );

        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "abab", (4, 4), (-1, -1)));
        assert_eq!(
            edits(&mut engine, state(0, "ab", (2, 2), (-1, -1))),
            deleted(2)
        );
    }

    #[test]
    fn the_caret_follows_the_selection_in_the_order_of_the_text() {
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "hello", (5, 5), (-1, -1)));

        assert_eq!(
            edits(&mut engine, state(0, "hello", (2, 2), (-1, -1))),
            keys(Key::Previous, 3)
        );
        assert_eq!(
            edits(&mut engine, state(0, "hello", (1, 4), (-1, -1))),
            keys(Key::Next, 2)
        );
        // Inside a cluster, the caret rests before it.
        let mut engine = Engine::new();
        assert_eq!(
            edits(&mut engine, state(0, "e\u{301}", (1, 1), (-1, -1))),
            [commit("e\u{301}"), Edit::Key(Key::Previous)]
        );
        // Inside a surrogate pair too.
        let mut engine = Engine::new();
        assert_eq!(
            edits(&mut engine, state(0, "😀", (1, 1), (-1, -1))),
            [commit("😀"), Edit::Key(Key::Previous)]
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
            deleted(1)
        );
    }

    #[test]
    fn a_rebase_makes_the_caret_the_new_origin() {
        let mut engine = Engine::new();

        assert_eq!(
            edits(&mut engine, state(0, "abc", (1, 1), (-1, -1))),
            [
                commit("abc"),
                Edit::Key(Key::Previous),
                Edit::Key(Key::Previous)
            ]
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
            deleted(1)
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
            (SELECT_ALL, Key::SelectAll),
        ] {
            let mut engine = Engine::new();
            let request = if matches!(key, Key::Enter | Key::Tab | Key::Escape)
            {
                Request::Yield
            } else {
                Request::None
            };

            assert_eq!(
                apply(
                    &mut engine,
                    Message::Key {
                        generation: 0,
                        code
                    }
                ),
                (vec![Edit::Key(key)], request),
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
                (vec![], Request::None),
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
                Edit::Key(Key::Previous),
                Edit::Key(Key::Backspace),
                Edit::Key(Key::Next)
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
                keys(Key::Next, 3),
                vec![Edit::Key(Key::Delete)],
                keys(Key::Previous, 3)
            ]
            .concat()
        );
    }

    #[test]
    fn backspace_and_delete_key_events_commit_a_composition_at_the_edge() {
        // The keyboard's key event, unlike deleteSurroundingText, reaches
        // the field's text past a composition at that end of the mirror:
        // the composition is committed where it was and reported, then the
        // key deletes there, as in an `EditText`.
        let mut harness = Harness::new("ab", "cd");
        let _ = harness.run(|m| m.set_composing_text("xyz", 1));
        assert_eq!(harness.preedit(), "xyz");

        assert_eq!(
            harness.run(|m| m.send_key(KEYCODE_FORWARD_DEL)),
            [clear(), commit("xyz"), Edit::Key(Key::Delete)]
        );
        assert_eq!(
            (harness.value().as_str(), harness.preedit()),
            ("abxyzd", "")
        );

        // With the cursor before the composition.
        let mut harness = Harness::new("ab", "cd");
        let _ = harness.run(|m| m.set_composing_text("xyz", 0));
        assert_eq!(harness.mirror.selection, (0, 0));

        assert_eq!(
            harness.run(|m| m.send_key(KEYCODE_DEL)),
            [
                vec![clear(), commit("xyz")],
                keys(Key::Previous, 3),
                vec![Edit::Key(Key::Backspace)]
            ]
            .concat()
        );
        assert_eq!(
            (harness.value().as_str(), harness.preedit()),
            ("axyzcd", "")
        );
    }

    #[test]
    fn a_key_that_yields_commits_the_composition_first() {
        let mut engine = Engine::new();
        let _ = edits(&mut engine, state(0, "hello", (5, 5), (0, 5)));
        assert!(engine.preedit.is_some());

        assert_eq!(
            apply(
                &mut engine,
                Message::Key {
                    generation: 0,
                    code: KEYCODE_ENTER
                }
            ),
            (
                vec![clear(), commit("hello"), Edit::Key(Key::Enter)],
                Request::Yield
            )
        );
        assert!(engine.preedit.is_none());

        // The rebase that follows makes the caret the origin, in the same
        // generation.
        assert_eq!(edits(&mut engine, Message::Rebase { generation: 0 }), []);
        assert!(engine.is_idle());
        assert_eq!(engine.generation(), 0);
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
            [commit("a\u{FFFD}b"), Edit::Key(Key::Previous)]
        );
        assert_eq!(
            edits(&mut engine, state(0, "ab", (1, 1), (-1, -1))),
            deleted(1)
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
                Edit::Key(Key::Previous),
                Edit::Key(Key::Previous),
                Edit::Key(Key::Previous),
                Edit::Key(Key::Delete),
                Edit::Key(Key::Delete),
                commit("he"),
                Edit::Key(Key::Next),
            ]
        );
    }

    #[test]
    fn the_engines_caret_moves_go_in_the_order_of_the_text() {
        // A Hebrew line of a `text_editor`, whose arrows move on screen. The
        // keyboard puts a period before the space it added after a word, in
        // one batch edit (AOSP's LatinIME and Gboard do, also for a double
        // space): the caret goes back over the space and on again in the
        // order of the text, and the next word follows the period.
        let mut harness = Harness::new("", "").right_to_left();

        let _ = harness.run(|m| m.commit_text("שלום ", 1));
        let edits = harness.run(|m| {
            m.begin_batch_edit();
            m.delete_surrounding_text(1, 0);
            m.commit_text(". ", 1);
            m.end_batch_edit();
        });

        assert_eq!(
            edits,
            [Edit::Key(Key::Previous), commit("."), Edit::Key(Key::Next)]
        );
        assert_eq!(harness.value(), "שלום. ");

        let _ = harness.run(|m| m.commit_text("עוד", 1));
        assert_eq!(harness.value(), "שלום. עוד");

        // The keyboard's own arrow keys stay arrows, which move on screen
        // there, as in an Android `EditText`: Right goes back.
        assert_eq!(
            harness.run(|m| m.send_key(KEYCODE_DPAD_RIGHT)),
            [Edit::Key(Key::Right)]
        );
        assert_eq!(harness.app.caret, harness.app.value.len() - 1);
    }

    #[test]
    fn the_keyboards_deletions_take_whole_clusters_in_a_text_editor() {
        // A `text_editor`'s Backspace deletes one character. The keyboard
        // deletes one code point of a cluster (its Backspace key, AOSP's
        // LatinIME) or a whole one (Gboard on an emoji): the engine moves to
        // the start of what changed and deletes forward, which takes a whole
        // cluster in `text_editor` too, then commits what is left of it.
        for (typed, left) in [
            ("ok👍🏽", "ok👍"),
            ("oke\u{301}", "oke"),
            ("ok🇩🇪", "ok🇩"),
            ("okकि", "okक"),
            ("ok1\u{FE0F}\u{20E3}", "ok1\u{FE0F}"),
        ] {
            let mut harness = Harness::new("", "").editor();
            let _ = harness.run(|m| m.commit_text(typed, 1));

            assert_eq!(
                harness.run(|m| m.send_key(KEYCODE_DEL)),
                [
                    Edit::Key(Key::Previous),
                    Edit::Key(Key::Delete),
                    commit(&left[2..]),
                ],
                "{typed}"
            );
            assert_eq!(harness.value(), left);

            let _ = harness.run(|m| m.commit_text("x", 1));
            assert_eq!(harness.value(), format!("{left}x"));
        }

        // A whole emoji, then a letter: the field follows the keyboard, and
        // what it types next goes where its cursor is.
        let mut harness = Harness::new("", "").editor();
        let _ = harness.run(|m| m.commit_text("ok👍🏽", 1));

        assert_eq!(
            harness.run(|m| m.delete_surrounding_text(4, 0)),
            [Edit::Key(Key::Previous), Edit::Key(Key::Delete)]
        );
        assert_eq!(harness.value(), "ok");

        let _ = harness.run(|m| m.delete_surrounding_text(1, 0));
        let _ = harness.run(|m| m.commit_text("x", 1));
        assert_eq!(harness.value(), "ox");

        // A heart and its variation selector, one unit each.
        let _ = harness.run(|m| m.commit_text("❤\u{FE0F}", 1));
        let _ = harness.run(|m| m.delete_surrounding_text(1, 0));
        assert_eq!(harness.value(), "ox❤");
        let _ = harness.run(|m| m.delete_surrounding_text(1, 0));
        assert_eq!(harness.value(), "ox");

        // The keyboard's Backspace with nothing typed before its cursor is
        // the field's own, which takes one character there, as a hardware
        // keyboard's does.
        let mut harness = Harness::new("ok👍🏽", "").editor();
        assert_eq!(
            harness.run(|m| m.send_key(KEYCODE_DEL)),
            [Edit::Key(Key::Backspace)]
        );
        assert_eq!(harness.value(), "ok👍");
    }

    /// iced's own text editor over cosmic-text, the one a `text_editor`
    /// edits, which the keyboard types into from the start of its only line.
    /// Its Backspace deletes one character, its Delete and moves a cluster.
    struct TextEditor {
        mirror: Mirror,
        engine: Engine,
        editor: crate::graphics::text::Editor,
    }

    impl TextEditor {
        fn new() -> Self {
            use crate::core::text::Editor as _;

            Self {
                mirror: Mirror::new(false, 4096),
                engine: Engine::new(),
                editor: crate::graphics::text::Editor::with_text(""),
            }
        }

        /// Runs `op` on the keyboard's side and gives the edits to the
        /// editor as `text_editor` binds them: in a left-to-right line, where
        /// its arrows move in the order of the text as `Motion::Previous`
        /// and `Motion::Next` do on Android.
        fn run(&mut self, op: impl FnOnce(&mut Mirror)) {
            use crate::core::text::Editor as _;
            use crate::core::text::editor::{Action, Edit as Change, Motion};

            op(&mut self.mirror);

            while let Some(message) = self.mirror.out.pop_front() {
                let mut edits = Vec::new();

                assert_eq!(
                    self.engine.apply(message, &mut edits),
                    Request::None
                );

                for edit in edits {
                    let action = match edit {
                        Edit::Key(Key::Backspace) => {
                            Action::Edit(Change::Backspace)
                        }
                        Edit::Key(Key::Delete) => Action::Edit(Change::Delete),
                        Edit::Key(Key::Previous | Key::Left) => {
                            Action::Move(Motion::Left)
                        }
                        Edit::Key(Key::Next | Key::Right) => {
                            Action::Move(Motion::Right)
                        }
                        Edit::Commit(text) => Action::Edit(Change::Paste(
                            std::sync::Arc::new(text),
                        )),
                        Edit::Preedit(..) => continue,
                        Edit::Key(key) => {
                            panic!("{key:?} is not expected here")
                        }
                    };

                    self.editor.perform(action);
                }
            }
        }

        fn text(&self) -> String {
            use crate::core::text::Editor as _;

            self.editor
                .line(0)
                .map(|line| line.text.into_owned())
                .unwrap_or_default()
        }
    }

    #[test]
    fn a_real_text_editor_follows_the_keyboards_deletions() {
        for (typed, delete, left) in [
            ("ok👍🏽", 1, "ok👍"),
            ("ok👍🏽", 2, "ok👍"),
            ("ok👍🏽", 4, "ok"),
            ("oke\u{301}", 1, "oke"),
            ("ok🇩🇪", 2, "ok🇩"),
            ("okकि", 1, "okक"),
            ("ok❤\u{FE0F}", 2, "ok"),
            ("ok1\u{FE0F}\u{20E3}", 1, "ok1\u{FE0F}"),
            ("ok👨\u{200D}👩\u{200D}👧", 2, "ok👨\u{200D}👩\u{200D}"),
        ] {
            let mut editor = TextEditor::new();

            editor.run(|m| m.commit_text(typed, 1));
            assert_eq!(editor.text(), typed);

            // The Backspace key deletes a code point; deleteSurroundingText
            // deletes UTF-16 units.
            if delete == 1 {
                editor.run(|m| m.send_key(KEYCODE_DEL));
            } else {
                editor.run(|m| m.delete_surrounding_text(delete, 0));
            }
            assert_eq!(editor.text(), left, "{typed} less {delete}");

            // A letter goes in where the keyboard's cursor is, and so does
            // its next deletion.
            editor.run(|m| m.commit_text("x", 1));
            assert_eq!(editor.text(), format!("{left}x"));

            editor.run(|m| m.delete_surrounding_text(1, 0));
            editor.run(|m| m.send_key(KEYCODE_DEL));
            editor.run(|m| m.commit_text("y", 1));
            assert_eq!(
                editor.text(),
                String::from_utf16_lossy(&editor.mirror.units),
                "{typed}"
            );
        }
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
    fn marks_that_join_any_cluster_are_told_apart() {
        for mark in [
            "\u{0E35}",          // Thai sara ii
            "\u{093E}",          // Devanagari vowel sign aa (a spacing mark)
            "\u{094D}",          // Devanagari virama
            "\u{0301}",          // combining acute accent
            "\u{200D}",          // zero width joiner
            "\u{FE0F}",          // variation selector
            "\u{1F3FD}",         // emoji modifier
            "\u{0E35}\u{0E48}x", // the first character counts
        ] {
            assert!(joins("", mark), "{mark:?}");
            assert!(joins("x", mark), "{mark:?}");
        }

        // With nothing known before the origin, the characters that join
        // only certain clusters are taken not to.
        for text in ["", "a", " ", "\u{0915}", "\u{0E01}", "🇫", "가", "😀"]
        {
            assert!(!joins("", text), "{text:?}");
        }

        // With the text before it known, they are told apart too.
        for (before, text, joined) in [
            ("\u{0915}\u{094D}", "\u{0924}", true), // क् + त: a conjunct
            ("\u{094D}", "\u{0924}", true),         // ् after unknown text
            ("\u{094D}\u{0924}", "\u{093E}", true), // a vowel sign
            ("\u{0915}", "\u{0924}", false),        // no virama
            ("\u{0915}\u{093F}", "\u{0924}", false), // कि + त
            ("a\u{094D}", "\u{0924}", false),       // no consonant before it
            ("🇫", "🇷", true),                       // an unpaired indicator
            ("🇩🇪", "🇫", false),                     // a paired one
            ("\u{1100}", "\u{1161}", true),         // Hangul L + V
            ("x", "\u{0924}", false),
            ("x", "", false),
        ] {
            assert_eq!(joins(before, text), joined, "{before:?} {text:?}");
        }
    }

    /// The keyboard's messages applied in order, as the native side applies
    /// them, to a field that segments its value again after each turn of
    /// the event loop, as `text_input` does at `view`. The requests are
    /// collected, with the generation they name.
    fn pump(
        engine: &mut Engine,
        mirror: &mut Mirror,
        field: &mut Field,
    ) -> Vec<(Request, i32)> {
        let mut requests = Vec::new();

        while let Some(message) = mirror.out.pop_front() {
            let mut edits = Vec::new();
            let request = engine.apply(message, &mut edits);

            field.apply(&edits);

            if request != Request::None {
                requests.push((request, engine.generation()));
            }
        }

        field.resegment();
        requests
    }

    #[test]
    fn a_mark_that_joins_the_text_before_the_origin_is_forgotten() {
        // The field holds "क" and the user tapped after it: the origin. The
        // keyboard commits a word that starts with a vowel sign, which joins
        // "क" in the field: "का".
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(false, 4096);
        let mut field = Field::new("क", "");

        mirror.commit_text("\u{093E}म ", 1);
        assert_eq!(
            pump(&mut engine, &mut mirror, &mut field),
            [(Request::Forget, 0)]
        );
        assert_eq!(field.text(), "काम ");
        assert_eq!(field.value, ["का", "म", " "]);

        // The Java side forgets the text, so the keyboard cannot take the
        // word back into a composition: that would delete "का" from the
        // field, which the engine cannot put back.
        mirror.forget(0);
        assert!(mirror.units.is_empty());
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert!(engine.is_idle());

        mirror.delete_surrounding_text(1, 0);
        mirror.set_composing_region(-3, 0);
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert_eq!(
            (field.text().as_str(), field.preedit.clone()),
            ("काम", None)
        );
    }

    /// Runs the requests to forget `pump` collected, as the Java side runs
    /// them, then delivers the rebases they send.
    fn forget(
        engine: &mut Engine,
        mirror: &mut Mirror,
        field: &mut Field,
        requests: Vec<(Request, i32)>,
    ) {
        for (request, generation) in requests {
            assert_eq!(request, Request::Forget);
            mirror.forget(generation);
        }

        assert_eq!(pump(engine, mirror, field), []);
    }

    #[test]
    fn a_consonant_after_a_virama_the_keyboard_typed_joins_it() {
        // The field holds "क" and the origin is after it (a tap). A keyboard
        // that commits each letter types a virama, which joins "क": the
        // engine forgets it, and knows that the origin now follows it.
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(false, 4096);
        let mut field = Field::new("क", "");

        for (typed, value) in [
            ("\u{094D}", "क\u{094D}"),
            // The consonant joins the conjunct in the field (GB9c): it is
            // forgotten too, so no edit reaches back into the conjunct.
            ("\u{0924}", "क\u{094D}\u{0924}"),
            ("\u{093E}", "क\u{094D}\u{0924}\u{093E}"),
            ("म", "क\u{094D}\u{0924}\u{093E}म"),
        ] {
            mirror.commit_text(typed, 1);
            let requests = pump(&mut engine, &mut mirror, &mut field);
            assert_eq!(
                requests.len(),
                usize::from(typed != "म"),
                "{typed:?} {requests:?}"
            );
            forget(&mut engine, &mut mirror, &mut field, requests);
            assert_eq!(field.text(), value, "{typed:?}");
        }

        // "म" starts no conjunct: the keyboard keeps it, and revises it in
        // place.
        assert_eq!(field.value, ["क\u{094D}\u{0924}\u{093E}", "म"]);
        mirror.commit_text("\u{093F}", 1);
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert_eq!(field.text(), "क\u{094D}\u{0924}\u{093E}मि");
    }

    #[test]
    fn a_word_composed_after_a_virama_the_keyboard_typed_is_kept_whole() {
        // As before, but the keyboard composes the word after the virama,
        // commits it with a space, then Backspace takes it back into a
        // composition: the field keeps the conjunct.
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(false, 4096);
        let mut field = Field::new("क", "");

        mirror.commit_text("\u{094D}", 1);
        let requests = pump(&mut engine, &mut mirror, &mut field);
        forget(&mut engine, &mut mirror, &mut field, requests);

        for word in ["ष", "ष\u{093E}"] {
            mirror.set_composing_text(word, 1);
            assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        }

        mirror.begin_batch_edit();
        mirror.commit_text("ष\u{093E}", 1);
        mirror.commit_text(" ", 1);
        mirror.end_batch_edit();
        let requests = pump(&mut engine, &mut mirror, &mut field);
        assert_eq!(requests, [(Request::Forget, 0)]);
        forget(&mut engine, &mut mirror, &mut field, requests);
        assert_eq!(field.text(), "क\u{094D}ष\u{093E} ");

        mirror.delete_surrounding_text(1, 0);
        mirror.set_composing_region(-2, 0);
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert_eq!(
            (field.text().as_str(), field.preedit.clone()),
            ("क\u{094D}ष\u{093E}", None)
        );
    }

    #[test]
    fn the_context_is_the_last_cluster_before_the_origin() {
        assert_eq!(context("", ""), "");
        assert_eq!(context("x", ""), "x");
        assert_eq!(context("", "abc"), "c");
        assert_eq!(context("ab", "c"), "c");
        // Text that joins the unknown text before it is kept whole.
        assert_eq!(context("", "\u{094D}"), "\u{094D}");
        assert_eq!(context("\u{094D}", "\u{0924}"), "\u{094D}\u{0924}");
        assert_eq!(context("\u{094D}\u{0924}", "\u{093E}म"), "म");
        assert_eq!(context("🇩", "🇪"), "🇩🇪");
        assert_eq!(context("🇩🇪", "🇫"), "🇫");

        // A cluster is kept up to its last characters.
        let marks = "\u{0301}".repeat(100);
        assert_eq!(context("", &format!("e{marks}")), "\u{0301}".repeat(32));
        assert!(joins(&"\u{0301}".repeat(32), "\u{0301}"));

        assert_eq!(prefix("a\u{0301}bc", 0), "");
        assert_eq!(prefix("a\u{0301}bc", 1), "a\u{0301}");
        assert_eq!(prefix("a\u{0301}bc", 9), "a\u{0301}bc");
    }

    #[test]
    fn the_origin_known_is_forgotten_where_the_field_changes() {
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(false, 4096);
        let mut field = Field::new("क", "");

        mirror.commit_text("\u{094D}", 1);
        let requests = pump(&mut engine, &mut mirror, &mut field);
        forget(&mut engine, &mut mirror, &mut field, requests);
        assert_eq!(engine.context, "\u{094D}");

        // Idle, but a caret the application moves must reach it.
        assert!(engine.is_idle() && engine.follows_caret());

        // The keyboard's Backspace deletes the conjunct's start in the field:
        // nothing is known before the origin any more.
        mirror.send_key(KEYCODE_DEL);
        let _ = pump(&mut engine, &mut mirror, &mut field);
        assert_eq!(engine.context, "");
        assert!(!engine.follows_caret());

        // Nor after a key that moves the caret, a key that may submit the
        // field, or a reset.
        for code in [KEYCODE_DPAD_LEFT, KEYCODE_ENTER] {
            mirror.commit_text("a\u{094D}", 1);
            let _ = pump(&mut engine, &mut mirror, &mut field);
            mirror.forget(engine.generation());
            let _ = pump(&mut engine, &mut mirror, &mut field);
            assert_eq!(engine.context, "a\u{094D}");

            mirror.send_key(code);
            let _ = pump(&mut engine, &mut mirror, &mut field);
            assert_eq!(engine.context, "", "{code}");
        }

        mirror.commit_text("b", 1);
        let _ = pump(&mut engine, &mut mirror, &mut field);
        mirror.forget(engine.generation());
        let _ = pump(&mut engine, &mut mirror, &mut field);
        assert_eq!(engine.context, "b");
        let generation = engine.reset(&mut Vec::new());
        assert_eq!(engine.context, "");
        mirror.adopt(generation);

        // A tap or a caret moved from code with the engine idle: no reset,
        // and nothing known.
        mirror.commit_text("d", 1);
        let _ = pump(&mut engine, &mut mirror, &mut field);
        mirror.forget(engine.generation());
        let _ = pump(&mut engine, &mut mirror, &mut field);
        assert_eq!(engine.context, "d");
        assert_eq!(engine.interrupt(&mut Vec::new()), None);
        assert_eq!(engine.context, "");
        assert_eq!(engine.generation(), generation);

        // The field losing the keyboard too.
        mirror.commit_text("e", 1);
        let _ = pump(&mut engine, &mut mirror, &mut field);
        mirror.forget(engine.generation());
        let _ = pump(&mut engine, &mut mirror, &mut field);
        assert_eq!(engine.context, "e");

        // A rebase with the caret at the origin leaves it as it was.
        mirror.commit_text("c", 1);
        mirror.set_selection(0, 0);
        let _ = pump(&mut engine, &mut mirror, &mut field);
        mirror.forget(engine.generation());
        let _ = pump(&mut engine, &mut mirror, &mut field);
        assert_eq!(engine.context, "e");

        assert_eq!(engine.abandon(&[]), None);
        assert_eq!(engine.context, "");
    }

    #[test]
    fn a_conjunct_typed_at_the_end_of_the_text_is_revised_whole() {
        // A virama asks for no rebase: the consonant after it joins the
        // engine's own cluster, which every edit revises in the field. (In
        // the middle of the text it would join the field's next consonant
        // instead, which the engine cannot see: a known limitation.)
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(false, 4096);
        let mut field = Field::new("म", "");

        for (typed, value) in [
            ("स", ["म", "स"]),
            ("\u{094D}", ["म", "स\u{094D}"]),
            ("त", ["म", "स\u{094D}त"]),
        ] {
            mirror.commit_text(typed, 1);
            assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
            assert_eq!(field.value, value);
        }

        for value in ["मस\u{094D}", "मस", "म"] {
            mirror.send_key(KEYCODE_DEL);
            assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
            assert_eq!(field.text(), value);
        }
    }

    #[test]
    fn a_request_to_forget_always_ends_in_a_rebase() {
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(false, 4096);
        let mut field = Field::new("e", "");

        mirror.commit_text("\u{0301}", 1);
        assert_eq!(
            pump(&mut engine, &mut mirror, &mut field),
            [(Request::Forget, 0)]
        );

        // The keyboard deleted the mark before the request ran: the empty
        // mirror is rebased all the same, so the next mark asks again.
        mirror.send_key(KEYCODE_DEL);
        mirror.forget(0);
        assert_eq!(mirror.out.back(), Some(&Message::Rebase { generation: 0 }));
        let _ = pump(&mut engine, &mut mirror, &mut field);

        mirror.commit_text("\u{0301}", 1);
        assert_eq!(
            pump(&mut engine, &mut mirror, &mut field),
            [(Request::Forget, 0)]
        );
    }

    #[test]
    fn forgetting_waits_for_the_composition_to_end() {
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(false, 4096);
        let mut field = Field::new("ก", "");

        mirror.commit_text("\u{0E35}", 1);
        assert_eq!(
            pump(&mut engine, &mut mirror, &mut field),
            [(Request::Forget, 0)]
        );
        assert_eq!(field.value, ["กี"]);

        // The keyboard composes before the request runs: the request waits,
        // and the engine asks once.
        mirror.set_composing_text("ab", 1);
        mirror.forget(0);
        mirror.set_composing_text("abc", 1);
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert_eq!(mirror.composing_text(), "abc");
        assert_eq!(field.preedit, Some(("abc".to_owned(), Some(3..3))));

        mirror.finish_composing_text();
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert!(mirror.units.is_empty());
        assert!(engine.is_idle());
        assert_eq!(
            (field.text().as_str(), field.preedit.clone()),
            ("กีabc", None)
        );

        // A request that a reset overtook does nothing.
        mirror.commit_text("\u{0E48}", 1);
        assert_eq!(
            pump(&mut engine, &mut mirror, &mut field),
            [(Request::Forget, 0)]
        );
        let generation = engine.reset(&mut Vec::new());
        mirror.adopt(generation);
        mirror.forget(0);
        assert_eq!(mirror.out, []);
    }

    #[test]
    fn a_request_to_forget_waits_for_the_keyboards_batch_edit() {
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(false, 4096);
        let mut field = Field::new("e", "");

        mirror.commit_text("\u{301}", 1);
        assert_eq!(
            pump(&mut engine, &mut mirror, &mut field),
            [(Request::Forget, 0)]
        );

        // The request runs between two calls of the keyboard's next batch
        // edit: the batch goes on with the text the keyboard saw when it
        // began it, and the report when it ends rebases.
        mirror.begin_batch_edit();
        mirror.commit_text("x", 1);
        mirror.forget(0);
        assert_eq!(mirror.out, []);
        assert_eq!(String::from_utf16_lossy(&mirror.units), "\u{301}x");
        mirror.commit_text("y", 1);
        mirror.end_batch_edit();
        assert_eq!(mirror.out.back(), Some(&Message::Rebase { generation: 0 }));

        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert_eq!(field.text(), "e\u{301}xy");
        assert!(engine.is_idle());
        assert!(mirror.units.is_empty() && !mirror.forgetting);

        // A key the batch sends reports what it edited before the key, and
        // the rebase still waits for the batch to end.
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(false, 4096);
        let mut field = Field::new("e", "");

        mirror.commit_text("\u{301}", 1);
        assert_eq!(
            pump(&mut engine, &mut mirror, &mut field),
            [(Request::Forget, 0)]
        );
        mirror.begin_batch_edit();
        mirror.forget(0);
        mirror.commit_text("x", 1);
        mirror.delete_surrounding_text(0, 1);
        mirror.commit_text("y", 1);
        mirror.end_batch_edit();

        let kinds: Vec<&str> = mirror
            .out
            .iter()
            .map(|message| match message {
                Message::State { .. } => "state",
                Message::Key { .. } => "key",
                Message::Rebase { .. } => "rebase",
            })
            .collect();
        assert_eq!(kinds, ["state", "key", "state", "rebase"]);
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert_eq!(field.text(), "e\u{301}xy");
        assert!(engine.is_idle());

        // A secure field's mirror is empty after each report: the request
        // rebases it all the same, and leaves nothing to forget later.
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(true, 4096);
        let mut field = Field::new("e", "");

        mirror.commit_text("\u{301}", 1);
        assert_eq!(
            pump(&mut engine, &mut mirror, &mut field),
            [(Request::Forget, 0)]
        );
        mirror.forget(0);
        assert_eq!(mirror.out, [Message::Rebase { generation: 0 }]);
        assert!(!mirror.forgetting);
        let _ = pump(&mut engine, &mut mirror, &mut field);
        assert!(engine.is_idle());
        assert_eq!(field.text(), "e\u{301}");
    }

    #[test]
    fn a_batch_edit_the_request_to_forget_meets_edits_the_text_it_saw() {
        // The field holds "क" and the user tapped after it. A keyboard that
        // commits each letter types a vowel sign, which joins "क" in the
        // field, then " teh". Its autocorrection on the next space is a
        // batch edit worked out from that text, and the request to forget
        // runs inside it: the batch still deletes "teh" and commits "the ",
        // or selects "teh" and commits over it, and does not delete only
        // "h", or insert, as it would in a mirror emptied in between.
        let batches: [(&str, fn(&mut Mirror)); 2] = [
            ("कि the ", |mirror| {
                mirror.delete_surrounding_text(3, 0);
                mirror.commit_text("the ", 1);
            }),
            ("कि the", |mirror| {
                mirror.set_selection(2, 5);
                mirror.commit_text("the", 1);
            }),
        ];

        for (value, batch) in batches {
            let mut engine = Engine::new();
            let mut mirror = Mirror::new(false, 4096);
            let mut field = Field::new("क", "");

            mirror.commit_text("\u{093F}", 1);
            assert_eq!(
                pump(&mut engine, &mut mirror, &mut field),
                [(Request::Forget, 0)]
            );

            for letter in [" ", "t", "e", "h"] {
                mirror.commit_text(letter, 1);
                assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
            }
            assert_eq!(field.text(), "कि teh");

            mirror.begin_batch_edit();
            mirror.forget(0);
            batch(&mut mirror);
            mirror.end_batch_edit();

            assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
            assert_eq!(field.text(), value);
            assert!(engine.is_idle() && mirror.units.is_empty());
        }
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
        let mut harness = Harness::new("x", "");

        assert_eq!(
            harness.run(|m| m.commit_text("Dear Sam,\nthanks", 1)),
            [commit("Dear Sam,thanks")]
        );
        // The mirror holds what the field has, so every Backspace deletes
        // one character there, also across the dropped line break.
        assert_eq!(harness.mirror.units.len(), "Dear Sam,thanks".len());
        for expected in ["xDear Sam,thank", "xDear Sam,", "xDear Sam"] {
            while harness.value().len() > expected.len() {
                assert_eq!(
                    harness.run(|m| m.send_key(KEYCODE_DEL)),
                    deleted(1)
                );
            }
            assert_eq!(harness.value(), expected);
        }

        // A paste from the keyboard's clipboard, deleted with
        // deleteSurroundingText: no Backspace is lost.
        let mut harness = Harness::new("x", "");
        assert_eq!(
            harness.run(|m| m.commit_text("a\r\nb\t", 1)),
            [commit("ab")]
        );
        for (expected, edits) in [
            ("xa", deleted(1)),
            ("x", deleted(1)),
            // The mirror is empty: the field's own Backspace.
            ("", vec![Edit::Key(Key::Backspace)]),
        ] {
            assert_eq!(harness.run(|m| m.delete_surrounding_text(1, 0)), edits);
            assert_eq!(harness.value(), expected);
        }

        // A composition keeps none either.
        let mut harness = Harness::new("", "");
        assert_eq!(
            harness.run(|m| m.set_composing_text("a\nb", 1)),
            [preedit("ab", 2)]
        );
        assert_eq!(harness.mirror.composing_text(), "ab");
    }

    #[test]
    fn a_lone_tab_or_line_break_is_a_key() {
        for (text, key) in [
            ("\t", Key::Tab),
            ("\n", Key::Enter),
            ("\r", Key::Enter),
            ("\r\n", Key::Enter),
        ] {
            let mut harness = Harness::new("hello", "");

            let _ = harness.run(|m| m.set_composing_text("wor", 1));
            // The composition is committed first, and the mirror starts
            // over after the key; nothing stays in it.
            assert_eq!(
                harness.run(|m| m.commit_text(text, 1)),
                [clear(), commit("wor"), Edit::Key(key)],
                "{text:?}"
            );
            assert_eq!(harness.posted, [], "{text:?}");
            assert!(harness.mirror.units.is_empty(), "{text:?}");

            // The next Backspace deletes in the field.
            assert_eq!(
                harness.run(|m| m.send_key(KEYCODE_DEL)),
                [Edit::Key(Key::Backspace)],
                "{text:?}"
            );
            assert_eq!(harness.value(), "hellowo", "{text:?}");
        }

        // A key typing a tab (`sendKeyEvent` with its character) too.
        let mut harness = Harness::new("", "");
        assert_eq!(harness.run(|m| m.send_char('\t')), [Edit::Key(Key::Tab)]);
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
        assert_eq!(harness.posted, []);

        // The mirror starts over at once, in the same generation.
        assert_eq!(harness.run(|m| m.commit_text("!", 1)), [commit("!")]);
        assert_eq!(harness.value(), "hi!");
        assert_eq!(
            harness.mirror.units,
            "!".encode_utf16().collect::<Vec<_>>()
        );
    }

    #[test]
    fn text_typed_right_after_enter_tab_or_escape_counts() {
        for code in [
            KEYCODE_ENTER,
            KEYCODE_NUMPAD_ENTER,
            KEYCODE_TAB,
            KEYCODE_ESCAPE,
        ] {
            // The keyboard sends the key, then text, all before the event
            // loop turns: nothing is dropped, and each piece of text lands
            // after the key.
            let mut harness = Harness::new("", "");

            let _ = harness.run(|m| m.set_composing_text("line", 1));
            harness.mirror.send_key(code);
            harness.mirror.set_composing_text("de", 1);
            harness.mirror.commit_text("def", 1);

            let mut requests = Vec::new();

            while let Some(message) = harness.mirror.out.pop_front() {
                let mut edits = Vec::new();
                requests.push(harness.engine.apply(message, &mut edits));
                harness.app.apply(&edits);
                harness.log.extend(edits);
            }

            assert_eq!(harness.value(), "linedef", "{code}");

            let key = Key::from_code(code).unwrap();

            assert_eq!(
                harness.take(),
                [
                    clear(),
                    commit("line"),
                    Edit::Key(key),
                    preedit("de", 2),
                    clear(),
                    commit("def"),
                ],
                "{code}"
            );
            // The native side gives the field the key before the text after
            // it.
            assert_eq!(
                requests,
                [
                    Request::None,
                    Request::Yield,
                    Request::None,
                    Request::None,
                    Request::None
                ],
                "{code}"
            );
            assert_eq!(harness.engine.generation(), 0);
        }

        // AOSP's keyboard and others commit a username, Tab, then a
        // password in one go: in a field the focus stays in, all of it
        // lands there. An application that moves the focus at the Tab or
        // Enter gets the rest in the next field (`feed`).
        let mut harness = Harness::new("", "");
        harness.mirror.commit_text("sam", 1);
        harness.mirror.commit_text("\t", 1);
        harness.mirror.commit_text("pw", 1);
        harness.drain();
        assert_eq!(harness.value(), "sampw");
        assert_eq!(
            harness.mirror.units,
            "pw".encode_utf16().collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_keyboards_paste_goes_in_at_the_caret() {
        let mut harness = Harness::new("x", "!");

        // The composition is finished first, and line breaks and tabs are
        // dropped, as from the keyboard's clipboard.
        let _ = harness.run(|m| m.set_composing_text("wo", 1));
        assert_eq!(
            harness.run(|m| m.paste("rld\nnext\t")),
            [clear(), commit("worldnext")]
        );
        assert_eq!(harness.value(), "xworldnext!");

        // A clip of only a line break pastes nothing: it is never Enter.
        assert_eq!(harness.run(|m| m.paste("\n")), []);
        assert_eq!(harness.run(|m| m.paste("")), []);

        // The mirror holds the pasted text, which Backspace deletes.
        assert_eq!(harness.run(|m| m.send_key(KEYCODE_DEL)), deleted(1));
        assert_eq!(harness.value(), "xworldnex!");
    }

    #[test]
    fn the_keyboards_cut_and_copy_take_its_own_selection() {
        let mut harness = Harness::new("> ", "");

        let _ = harness.run(|m| m.commit_text("hello", 1));
        let _ = harness.run(|m| m.set_selection(1, 3));

        // Copy leaves the text.
        assert_eq!(harness.mirror.copy(false).as_deref(), Some("el"));
        harness.drain();
        assert_eq!(harness.take(), []);
        assert_eq!(harness.value(), "> hello");

        // Cut deletes it in the field too.
        assert_eq!(harness.mirror.copy(true).as_deref(), Some("el"));
        harness.drain();
        assert_eq!(harness.take(), deleted(2));
        assert_eq!(harness.value(), "> hlo");

        // Nothing selected: nothing to take.
        assert_eq!(harness.mirror.copy(false), None);
        assert_eq!(harness.mirror.copy(true), None);

        // A password is never taken, not even while it is composed.
        let mut harness = Harness::with("", "", true, 4096);
        let _ = harness.run(|m| m.set_composing_text("pw", 1));
        let _ = harness.run(|m| m.set_selection(0, 2));
        assert_eq!(harness.mirror.copy(false), None);
        assert_eq!(harness.mirror.copy(true), None);
        assert_eq!(harness.value(), "pw");
    }

    #[test]
    fn select_all_selects_the_fields_text_for_the_next_edit() {
        let mut engine = Engine::new();
        let mut mirror = Mirror::new(false, 4096);
        let mut field = Field::new("abc", "");

        // The composition is committed, then the field selects all of its
        // text, and the mirror starts over.
        mirror.set_composing_text("de", 1);
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        mirror.select_all();
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert_eq!((field.text().as_str(), field.selected), ("abcde", true));
        assert!(mirror.units.is_empty());
        assert!(engine.is_idle());

        // The next text replaces it.
        mirror.commit_text("x", 1);
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert_eq!((field.text().as_str(), field.selected), ("x", false));

        // The keyboard's Backspace deletes it.
        mirror.select_all();
        mirror.send_key(KEYCODE_DEL);
        assert_eq!(pump(&mut engine, &mut mirror, &mut field), []);
        assert_eq!(field.text(), "");
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
    fn a_secure_field_tells_the_keyboard_its_text_is_gone() {
        let mut harness = Harness::with("", "", true, 4096);

        // AOSP's LatinIME commits each letter of a password and deletes
        // with deleteSurroundingText, after the text it believes is there.
        for c in ["p", "a", "s", "s"] {
            let _ = harness.run(|m| m.commit_text(c, 1));
        }

        // After each letter, the keyboard saw its cursor after it, then
        // back at the start of the empty field: its cache is empty.
        let after = ((1, 1), None);
        let start = ((0, 0), None);
        assert_eq!(harness.mirror.updates, [after, start].repeat(4));

        assert_eq!(
            harness.run(|m| m.delete_surrounding_text(1, 0)),
            [Edit::Key(Key::Backspace)]
        );
        assert_eq!(harness.value(), "pas");
    }

    #[test]
    fn a_composition_in_a_secure_field_is_typed_text() {
        let mut harness = Harness::with("", "", true, 4096);

        // A keyboard that composes in a password field: the field never
        // shows the composition in clear, and the mirror keeps it only
        // until it is committed.
        assert_eq!(
            harness.run(|m| m.set_composing_text("pw", 1)),
            [commit("pw")]
        );
        assert_eq!(
            harness.run(|m| m.set_composing_text("pwq", 1)),
            [commit("q")]
        );
        assert_eq!(harness.mirror.composing_text(), "pwq");
        assert_eq!(
            harness.run(|m| m.set_composing_text("pwx", 1)),
            [deleted(1), vec![commit("x")]].concat()
        );

        assert_eq!(harness.run(Mirror::finish_composing_text), []);
        assert!(harness.mirror.units.is_empty());
        assert_eq!((harness.value().as_str(), harness.preedit()), ("pwx", ""));

        let _ = harness.run(|m| m.commit_text("1", 1));
        assert_eq!(harness.value(), "pwx1");
    }

    #[test]
    fn backspace_on_the_empty_mirror_reaches_the_text_before_it() {
        // AOSP's LatinIME and its forks delete before an empty field with
        // deleteSurroundingText, not with the Backspace key.
        let mut harness = Harness::new("hello", " world");

        assert_eq!(
            harness.run(|m| m.delete_surrounding_text(1, 0)),
            [Edit::Key(Key::Backspace)]
        );
        assert_eq!(
            harness.run(|m| m.delete_surrounding_text_in_code_points(1, 0)),
            [Edit::Key(Key::Backspace)]
        );
        assert_eq!(harness.value(), "hel world");

        // Forward deletion at the end of the mirror is the Delete key.
        assert_eq!(
            harness.run(|m| m.delete_surrounding_text(0, 1)),
            [Edit::Key(Key::Delete)]
        );
        assert_eq!(harness.value(), "helworld");

        // With text in the mirror, only the mirror's text is deleted.
        let _ = harness.run(|m| m.commit_text("p", 1));
        assert_eq!(
            harness.run(|m| m.delete_surrounding_text(1, 0)),
            deleted(1)
        );
        assert_eq!(harness.mirror.units, []);
        assert_eq!(harness.value(), "helworld");

        // A selection, or a composition, is not an empty side: the
        // keyboard edits what it sees.
        let _ = harness.run(|m| m.set_composing_text("ab", 1));
        let _ = harness.run(|m| m.delete_surrounding_text(1, 0));
        assert_eq!(
            (harness.value().as_str(), harness.preedit()),
            ("helworld", "ab")
        );
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
        assert_eq!(harness.run(|m| m.send_key(KEYCODE_DEL)), deleted(1));
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
        assert_eq!(harness.posted, [Posted::Reset(1)]);

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
        // A tap resets; the keyboard, which saw its text before the tap,
        // types on before the reset runs.
        harness.tap(1);
        harness.mirror.commit_text("b", 1);
        harness.drain();
        assert_eq!(harness.value(), "a");

        harness.mirror.commit_text("c", 1);
        harness.adopt();
        harness.mirror.commit_text("d", 1);
        harness.drain();
        assert_eq!(harness.value(), "ad");
    }

    /// The native side drops what is queued when the field loses the
    /// keyboard (`Session::abandon`), and posts the reset it asks for.
    fn abandon(harness: &mut Harness) {
        let dropped: Vec<Message> = harness.mirror.out.drain(..).collect();

        if let Some(generation) = harness.engine.abandon(&dropped) {
            harness.posted.push_back(Posted::Reset(generation));
        }
    }

    #[test]
    fn a_field_losing_the_keyboard_drops_what_it_sent() {
        // Nothing typed, nothing queued: the keyboard holds nothing.
        let mut harness = Harness::new("", "");
        abandon(&mut harness);
        assert_eq!(harness.posted, []);

        // A composition the engine applied is dropped, not committed.
        let _ = harness.run(|m| m.set_composing_text("wor", 1));
        abandon(&mut harness);
        assert_eq!(harness.posted, [Posted::Reset(1)]);
        assert_eq!(harness.value(), "");
    }

    #[test]
    fn a_report_still_queued_when_the_field_loses_the_keyboard_is_dropped() {
        for secure_next in [false, true] {
            // The user taps field A (the engine is idle) and types "h";
            // the application moves the focus to field B before the event
            // loop turns.
            let mut harness = Harness::new("", "");
            harness.mirror.set_composing_text("h", 1);
            assert!(harness.engine.is_idle());

            abandon(&mut harness);
            assert_eq!(harness.posted, [Posted::Reset(1)]);
            harness.settle();

            // B's keyboard starts with no text: what was typed for A
            // reaches neither field, and the next key is B's alone.
            harness.app = Field::new("", "");
            harness.restart();
            assert!(harness.mirror.units.is_empty());

            if secure_next {
                // A password field's keyboard finishes a composition when
                // its purpose changes: there is none to finish.
                assert_eq!(harness.run(Mirror::finish_composing_text), []);
            }

            assert_eq!(
                harness.run(|m| m.set_composing_text("i", 1)),
                [preedit("i", 1)]
            );
            assert_eq!(
                (harness.value().as_str(), harness.preedit()),
                ("", "i")
            );
        }

        // A dropped report of a committed letter too.
        let mut harness = Harness::new("", "");
        harness.mirror.commit_text("x", 1);
        abandon(&mut harness);
        harness.settle();
        harness.app = Field::new("", "");
        harness.restart();
        assert_eq!(harness.run(|m| m.commit_text("y", 1)), [commit("y")]);
        assert_eq!(harness.value(), "y");
    }

    #[test]
    fn caret_moving_keys_keep_the_generation_and_every_key_counts() {
        let mut harness = Harness::new("", "!");

        let _ = harness.run(|m| m.set_composing_text("abc", 1));
        // Two quick arrows, then a letter, all before the event loop turns:
        // the first finishes the composition and rebases the mirror.
        harness.mirror.send_key(KEYCODE_DPAD_LEFT);
        harness.mirror.send_key(KEYCODE_DPAD_LEFT);
        harness.mirror.send_char('x');
        harness.drain();

        assert_eq!(harness.value(), "axbc!");
        assert_eq!(harness.preedit(), "");
        assert_eq!(harness.posted, []);
        assert_eq!(harness.engine.generation(), 0);

        // Home and End go on from where the caret is.
        let _ = harness.run(|m| m.send_key(KEYCODE_MOVE_END));
        let _ = harness.run(|m| m.send_char('?'));
        let _ = harness.run(|m| m.send_key(KEYCODE_MOVE_HOME));
        let _ = harness.run(|m| m.commit_text("> ", 1));
        assert_eq!(harness.value(), "> axbc!?");
        assert_eq!(harness.posted, []);
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
    fn keyboard_op(harness: &mut Harness, rng: &mut Rng) {
        let m = &mut harness.mirror;

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
                    text_op(&mut harness.mirror, rng);

                    // The UI thread runs a request to forget between two
                    // calls of the batch, with its edits not reported yet.
                    // A reset never runs there: it restarts the keyboard,
                    // whose old connection edits nothing afterwards.
                    if rng.chance(80)
                        && matches!(
                            harness.posted.front(),
                            Some(Posted::Forget(_))
                        )
                    {
                        // The rest of the batch edits the text the keyboard
                        // worked it out from: the request sends nothing and
                        // changes nothing until the batch ends.
                        let seen = |m: &Mirror| {
                            (
                                m.units.clone(),
                                m.selection,
                                m.composing,
                                m.out.len(),
                            )
                        };
                        let before = seen(&harness.mirror);

                        harness.adopt();
                        assert_eq!(
                            seen(&harness.mirror),
                            before,
                            "{}: a request to forget changed a batch edit's text",
                            harness.label
                        );
                        harness.seen.batched_forgets += 1;
                    }
                }

                harness.mirror.end_batch_edit();
            }
        }
    }

    fn simulate(
        seed: u64,
        steps: usize,
        secure: bool,
        cap: usize,
        rtl: bool,
    ) -> Seen {
        let mut rng = Rng::new(seed);
        let before = random_text(&mut rng);
        let after = if rng.chance(50) {
            random_text(&mut rng)
        } else {
            String::new()
        };
        let mut harness = Harness::with(&before, &after, secure, cap);
        harness.app.editor = rtl;
        harness.app.rtl = rtl;

        for step in 0..steps {
            harness.label = format!("seed {seed} step {step}");

            match rng.below(100) {
                0..=79 => keyboard_op(&mut harness, &mut rng),
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
            // runs a posted reset or request to forget.
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

            seen.add(simulate(seed, 400, false, cap, false));
        }

        // A request to forget meets an open batch edit more rarely, and a
        // character that joins only the text the engine knows before the
        // origin (after the cap's rebase) more rarely still.
        seen.assert_reached(100, &["batched forgets", "known joins"]);
        seen.assert_reached(20, &["known joins"]);
        seen.assert_reached(1, &[]);
    }

    #[test]
    fn random_keyboards_keep_a_right_to_left_editor_in_step() {
        // A right-to-left line of a `text_editor`, whose arrows move on
        // screen and whose Backspace deletes one character: the engine's
        // own moves go in the order of the text, and its own deletions take
        // whole clusters.
        let mut seen = Seen::default();

        for seed in 2000..2060 {
            let cap = [4096, 64][seed as usize % 2];

            seen.add(simulate(seed, 300, false, cap, true));
        }

        seen.assert_reached(20, &["batched forgets", "known joins"]);
        seen.assert_reached(1, &["known joins"]);
    }

    #[test]
    fn random_keyboards_on_a_secure_field_keep_the_app_in_step() {
        let mut seen = Seen::default();

        for seed in 1000..1060 {
            seen.add(simulate(seed, 300, true, 4096, false));
        }

        // A composition reaches the app as typed text, never as a preedit
        // drawn in clear; the mirror keeps it until it is committed.
        assert_eq!(seen.compositions, 0, "{seen:?}");
        seen.assert_reached(20, &["compositions", "known joins"]);
        seen.assert_reached(1, &["compositions"]);
    }
}
