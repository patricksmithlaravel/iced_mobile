//! The embedded dex: its bytes, and the names the shell loads from it.
//!
//! The Java sources are in `winit/android/java/iced_mobile/` (Java 8, only
//! framework APIs of API 26, no androidx). `winit/android/build-dex.sh`
//! builds them into `winit/android/classes.dex`, which is committed, and
//! writes `winit/android/classes.dex.inputs`: the sources' SHA-256, then the
//! javac and d8 versions. The tests here fail when a source changed without
//! a rebuild, and when the dex no longer defines what this module names.
//!
//! # The contract between the Java side and the shell
//!
//! The shell loads [`DEX`] with `dalvik.system.InMemoryDexClassLoader`
//! (API 26), whose parent is the Activity's class loader, loads the class
//! [`KEYBOARD`] from it, and registers [`NATIVE_STATE`], [`NATIVE_KEY`] and
//! [`NATIVE_REBASE`] on it with `RegisterNatives`, before it calls
//! [`INSTALL`].
//!
//! ## The shell calls Java on `android_main`'s thread
//!
//! Each method posts its work to the UI thread with
//! `Activity.runOnUiThread` and returns at once, so the work runs in the
//! order of the calls.
//!
//! | Method | Signature | Work on the UI thread |
//! |---|---|---|
//! | [`INSTALL`]: `public static Keyboard install(Activity)` | `(Landroid/app/Activity;)Liced_mobile/Keyboard;` | creates the view (1x1 pixels, transparent) and adds it with `Activity.addContentView` |
//! | [`SHOW`]: `public void show(int purpose)` | `(I)V` | focuses the view, makes it a text editor and shows the keyboard for `purpose`; a changed purpose first finishes a composition and rebases a mirror that holds text, then restarts the keyboard (`InputMethodManager.restartInput`), as the first `show` after a `hide` does. Until `hide`, the window gaining the focus shows the keyboard again |
//! | [`HIDE`]: `public void hide()` | `()V` | hides the keyboard and clears the mirror (the shell has reset or knows it holds no text); the view keeps the focus but is no text editor (`onCheckIsTextEditor` false, no input connection) and restarts the keyboard, which then sees an editor of type `TYPE_NULL` and lets a hardware keyboard's keys through to the app |
//! | [`RESET`]: `public void reset(int generation)` | `(I)V` | adopts `generation`, clears the mirror and restarts the keyboard (`InputMethodManager.restartInput`) |
//! | [`FORGET`]: `public void forget(int generation)` | `(I)V` | unless a reset ran since the shell had `generation`: rebases the mirror (below), an empty one too, at once or, while something composes, once the composition ends; no new generation, no restart |
//! | [`UNINSTALL`]: `public void uninstall()` | `()V` | hides the keyboard if the view had the focus, removes the view and lets go of the Activity |
//!
//! `install` throws `IllegalArgumentException` for a null Activity, and the
//! others throw nothing. Once the work of `uninstall` ran, nothing the shell
//! calls does anything and no native call follows; the shell then deletes
//! its global reference.
//!
//! ## Java calls the shell on the UI thread
//!
//! The three methods are `private static native` methods of [`KEYBOARD`].
//!
//! | Method | Signature |
//! |---|---|
//! | [`NATIVE_STATE`]: `nativeState(int generation, String text, int selStart, int selEnd, int composingStart, int composingEnd)` | `(ILjava/lang/String;IIII)V` |
//! | [`NATIVE_KEY`]: `nativeKey(int generation, int keyCode)` | `(II)V` |
//! | [`NATIVE_REBASE`]: `nativeRebase(int generation)` | `(I)V` |
//!
//! They run inside the keyboard's `InputConnection` calls, so they must not
//! throw and must not call into Java. A call that throws is logged once and
//! otherwise ignored.
//!
//! ## The mirror
//!
//! The view keeps the keyboard's text, the mirror: what was typed since the
//! last reset or rebase, never the field's earlier text. The keyboard edits
//! it through a `BaseInputConnection` (`fullEditor` true), and the view tells
//! the keyboard its selection and composing region after each change
//! (`InputMethodManager.updateSelection`).
//!
//! `nativeState` reports the whole mirror after each edit, once the
//! keyboard's outermost batch edit ends, and only when something changed
//! since the last report, reset or rebase:
//!
//! - `text`: the mirror, the composing text included. What the keyboard
//!   commits, composes or types puts no line break or tab into it: a
//!   commit of only a line break or only a tab is a key instead (below),
//!   and they are dropped from longer text. `replaceText` (Android 14) is
//!   not filtered, so its text can hold them, and one of only a line break
//!   is no Enter; the engine drops them from what it reads, so the field
//!   never gets one.
//! - `selStart`, `selEnd`: the selection in UTF-16 units, as Android's
//!   `Selection` has it: `selStart` is the anchor and `selEnd` the cursor,
//!   so `selEnd` may be less than `selStart`. Equal when nothing is
//!   selected.
//! - `composingStart`, `composingEnd`: the composing region, with
//!   `composingStart < composingEnd`, or -1 and -1 when nothing is
//!   composing. Always -1 and -1 for a secure field: a composition there is
//!   reported as typed text, so the field never draws it in clear, and the
//!   mirror keeps it until it is committed.
//!
//! `nativeKey` sends a key the mirror cannot hold, an Android `KeyEvent`
//! key code. Right before it, any composition is finished and the mirror
//! reported, so a key never overtakes the text typed before it:
//!
//! - `KEYCODE_DEL` (67) and `KEYCODE_FORWARD_DEL` (112): only when the
//!   mirror had no selection, no composition and nothing to delete on that
//!   side, so the mirror did not change there: for the keyboard's key event,
//!   and for `deleteSurroundingText` (and its code point form) asking to
//!   delete on that side, which is how AOSP's LatinIME and its forks press
//!   Backspace even at the start of the text. Otherwise the key deletes in
//!   the mirror (by code point) and is reported as a state.
//! - `KEYCODE_ENTER` (66): the keyboard's key event, its editor action
//!   (`performEditorAction`), or a commit of only `"\n"`, `"\r"` or
//!   `"\r\n"`. `KEYCODE_NUMPAD_ENTER` (160) is sent as it is.
//! - `KEYCODE_TAB` (61): the key event, or a commit of only `"\t"`.
//! - `KEYCODE_ESCAPE` (111).
//! - `KEYCODE_DPAD_UP` (19), `KEYCODE_DPAD_DOWN` (20), `KEYCODE_DPAD_LEFT`
//!   (21), `KEYCODE_DPAD_RIGHT` (22), `KEYCODE_MOVE_HOME` (122) and
//!   `KEYCODE_MOVE_END` (123).
//!
//! After an Enter, Tab or Escape key, or a caret-moving one, the app's
//! caret moves, or its field may be submitted or left, where the mirror
//! cannot follow, so when the mirror was not empty a rebase follows at
//! once, with no new generation: what the keyboard sends after the key
//! counts.
//!
//! Other keys with a character, unless pressed with Ctrl or Meta and
//! unless a dead key, are typed into the mirror at the cursor after any
//! composition is finished (a line break or a tab as the key above); the
//! rest are ignored, as are key releases.
//!
//! `nativeRebase` says the mirror is now empty while the text it had stays
//! in the field: the app's caret is where the next text goes. It follows:
//!
//! - every report for a secure field, so no password is kept in Java and
//!   its keyboard sees an empty field;
//! - a report whose text exceeds 4096 UTF-16 units;
//! - an Enter, Tab, Escape or caret-moving key, as above;
//! - [`FORGET`];
//! - a change of purpose while the mirror was not empty, after the
//!   composition was finished: the next field's keyboard does not see the
//!   last field's text.
//!
//! None of them happens while something is composing. Each report, and
//! each rebase, tells the keyboard the selection it leaves
//! (`updateSelection`), so a keyboard that keeps the text before its cursor
//! learns that the mirror is empty again.
//!
//! ## Generations
//!
//! The generation starts at 0. The work of `reset(generation)` adopts it, and
//! every native call made after that carries it; no call carries an older
//! one once that work ran. The shell drops calls whose generation is not its
//! current one: they come from before a reset it asked for. The keyboard's
//! connections from before a reset or a change of purpose edit nothing.
//!
//! ## Purposes
//!
//! `show`'s `purpose` is iced's `input_method::Purpose`, which sets the
//! `EditorInfo` the keyboard gets. Every purpose has the options
//! `IME_ACTION_NONE | IME_FLAG_NO_EXTRACT_UI | IME_FLAG_NO_FULLSCREEN`.
//!
//! | Purpose | `inputType` | Also |
//! |---|---|---|
//! | [`PURPOSE_NORMAL`] (0, and any unknown value) | `TYPE_CLASS_TEXT` | |
//! | [`PURPOSE_SECURE`] (1) | `TYPE_CLASS_TEXT \| TYPE_TEXT_VARIATION_PASSWORD` | `IME_FLAG_NO_PERSONALIZED_LEARNING`; a composition reported as typed text; a rebase after every report once nothing composes |
//! | [`PURPOSE_TERMINAL`] (2) | `TYPE_CLASS_TEXT \| TYPE_TEXT_FLAG_NO_SUGGESTIONS \| TYPE_TEXT_VARIATION_VISIBLE_PASSWORD` | |
//!
//! ## Failures
//!
//! The work on the UI thread catches every exception and logs it under the
//! tag `iced_mobile`. Nothing logs the mirror's text. When the view cannot
//! be added, `show` and `hide` use the window's decor view instead, and the
//! keyboard can only send key events, as without the dex.

/// The dex `winit/android/build-dex.sh` built.
pub(crate) const DEX: &[u8] = include_bytes!("../../android/classes.dex");

/// The class the shell loads from [`DEX`], as `ClassLoader.loadClass` names
/// it.
pub(crate) const KEYBOARD: &str = "iced_mobile.Keyboard";

/// A method of [`KEYBOARD`], as JNI names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Method {
    /// The method's name.
    pub(crate) name: &'static str,
    /// The method's JNI signature.
    pub(crate) signature: &'static str,
}

/// `public static Keyboard install(Activity activity)`.
pub(crate) const INSTALL: Method = Method {
    name: "install",
    signature: "(Landroid/app/Activity;)Liced_mobile/Keyboard;",
};

/// `public void show(int purpose)`.
pub(crate) const SHOW: Method = Method {
    name: "show",
    signature: "(I)V",
};

/// `public void hide()`.
pub(crate) const HIDE: Method = Method {
    name: "hide",
    signature: "()V",
};

/// `public void reset(int generation)`.
pub(crate) const RESET: Method = Method {
    name: "reset",
    signature: "(I)V",
};

/// `public void forget(int generation)`.
pub(crate) const FORGET: Method = Method {
    name: "forget",
    signature: "(I)V",
};

/// `public void uninstall()`.
pub(crate) const UNINSTALL: Method = Method {
    name: "uninstall",
    signature: "()V",
};

/// `private static native void nativeState(int generation, String text,
/// int selStart, int selEnd, int composingStart, int composingEnd)`.
pub(crate) const NATIVE_STATE: Method = Method {
    name: "nativeState",
    signature: "(ILjava/lang/String;IIII)V",
};

/// `private static native void nativeKey(int generation, int keyCode)`.
pub(crate) const NATIVE_KEY: Method = Method {
    name: "nativeKey",
    signature: "(II)V",
};

/// `private static native void nativeRebase(int generation)`.
pub(crate) const NATIVE_REBASE: Method = Method {
    name: "nativeRebase",
    signature: "(I)V",
};

/// [`SHOW`]'s purpose for a normal field.
pub(crate) const PURPOSE_NORMAL: i32 = 0;

/// [`SHOW`]'s purpose for a secure field: a password.
pub(crate) const PURPOSE_SECURE: i32 = 1;

/// [`SHOW`]'s purpose for a terminal: no suggestions. Unlike
/// [`PURPOSE_SECURE`], it does not ask the keyboard not to learn.
pub(crate) const PURPOSE_TERMINAL: i32 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    use sha2::{Digest, Sha256};

    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};

    const PUBLIC: u32 = 0x1;
    const PRIVATE: u32 = 0x2;
    const STATIC: u32 = 0x8;
    const FINAL: u32 = 0x10;
    const NATIVE: u32 = 0x100;

    #[test]
    fn dex_is_built_from_the_java_sources() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate is in the repository");

        let inputs = root.join("winit/android/classes.dex.inputs");
        let inputs = fs::read_to_string(&inputs).unwrap_or_else(|error| {
            panic!("{}: {error}", inputs.display());
        });

        let recorded: BTreeMap<String, String> = inputs
            .lines()
            .filter_map(|line| {
                let (digest, path) = line.split_once("  ")?;

                let is_digest = digest.len() == 64
                    && digest.bytes().all(|byte| byte.is_ascii_hexdigit());

                is_digest.then(|| (path.to_owned(), digest.to_owned()))
            })
            .collect();

        let mut sources = Vec::new();
        java_sources(&root.join("winit/android/java"), &mut sources);

        let found: BTreeMap<String, String> = sources
            .iter()
            .map(|source| {
                let path = source
                    .strip_prefix(root)
                    .expect("a source in the repository")
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");

                let bytes = fs::read(source).unwrap_or_else(|error| {
                    panic!("{}: {error}", source.display());
                });

                let digest = Sha256::digest(&bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();

                (path, digest)
            })
            .collect();

        assert!(!found.is_empty(), "no Java sources in winit/android/java");

        if found != recorded {
            let mut differences = Vec::new();

            for (path, digest) in &found {
                match recorded.get(path) {
                    Some(old) if old == digest => {}
                    Some(_) => differences.push(format!("changed: {path}")),
                    None => differences.push(format!("new: {path}")),
                }
            }

            for path in recorded.keys() {
                if !found.contains_key(path) {
                    differences.push(format!("gone: {path}"));
                }
            }

            panic!(
                "winit/android/classes.dex was not built from the Java \
                sources in winit/android/java ({}): run \
                winit/android/build-dex.sh, then commit classes.dex and \
                classes.dex.inputs",
                differences.join(", ")
            );
        }

        assert!(
            inputs.lines().any(|line| line.starts_with("javac "))
                && inputs.lines().any(|line| line.starts_with("d8 ")),
            "classes.dex.inputs names no javac or d8 version: run \
            winit/android/build-dex.sh"
        );
    }

    #[test]
    fn dex_defines_the_contract() {
        let dex = Dex(DEX);

        assert!(
            DEX.len() > 0x70
                && DEX.starts_with(b"dex\n")
                && DEX[4..7].iter().all(u8::is_ascii_digit)
                && DEX[7] == 0,
            "classes.dex has no dex header"
        );
        assert_eq!(dex.u32(0x20), DEX.len(), "the dex's file size");
        assert_eq!(
            dex.u32(0x08) as u32,
            adler32(&DEX[12..]),
            "the dex's checksum: is classes.dex intact?"
        );

        let keyboard = dex
            .class(&descriptor(KEYBOARD))
            .expect("the dex defines iced_mobile.Keyboard");

        assert_eq!(keyboard.access & (PUBLIC | FINAL), PUBLIC | FINAL);

        let method = |method: Method| {
            keyboard
                .methods
                .iter()
                .find(|defined| {
                    defined.name == method.name
                        && defined.signature == method.signature
                })
                .map(|defined| defined.access)
                .unwrap_or_else(|| {
                    panic!(
                        "iced_mobile.Keyboard has no {}{}",
                        method.name, method.signature
                    )
                })
        };

        assert_eq!(method(INSTALL) & (PUBLIC | STATIC), PUBLIC | STATIC);

        for called in [SHOW, HIDE, RESET, FORGET, UNINSTALL] {
            assert_eq!(
                method(called) & (PUBLIC | STATIC),
                PUBLIC,
                "{} is a public instance method",
                called.name
            );
        }

        for native in [NATIVE_STATE, NATIVE_KEY, NATIVE_REBASE] {
            assert_eq!(
                method(native) & (PRIVATE | STATIC | NATIVE),
                PRIVATE | STATIC | NATIVE,
                "{} is a private static native method",
                native.name
            );
        }

        assert_eq!(
            keyboard
                .methods
                .iter()
                .filter(|defined| defined.access & NATIVE != 0)
                .count(),
            3,
            "iced_mobile.Keyboard has exactly the three native methods"
        );

        for (class, superclass) in [
            ("Liced_mobile/KeyboardView;", "Landroid/view/View;"),
            (
                "Liced_mobile/KeyboardConnection;",
                "Landroid/view/inputmethod/BaseInputConnection;",
            ),
        ] {
            let defined = dex
                .class(class)
                .unwrap_or_else(|| panic!("the dex defines {class}"));

            assert_eq!(defined.superclass, superclass);
        }

        assert_eq!(
            (PURPOSE_NORMAL, PURPOSE_SECURE, PURPOSE_TERMINAL),
            (0, 1, 2)
        );
    }

    /// Every `.java` file under `directory`.
    fn java_sources(directory: &Path, sources: &mut Vec<PathBuf>) {
        let entries = fs::read_dir(directory).unwrap_or_else(|error| {
            panic!("{}: {error}", directory.display());
        });

        for entry in entries {
            let path = entry.expect("a directory entry").path();

            if path.is_dir() {
                java_sources(&path, sources);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "java")
            {
                sources.push(path);
            }
        }
    }

    /// `iced_mobile.Keyboard` → `Liced_mobile/Keyboard;`.
    fn descriptor(binary_name: &str) -> String {
        format!("L{};", binary_name.replace('.', "/"))
    }

    fn adler32(bytes: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);

        for &byte in bytes {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }

        (b << 16) | a
    }

    /// Enough of a dex reader to find its classes and their methods.
    struct Dex<'a>(&'a [u8]);

    struct Class {
        access: u32,
        superclass: String,
        methods: Vec<Defined>,
    }

    struct Defined {
        name: String,
        signature: String,
        access: u32,
    }

    impl Dex<'_> {
        fn u16(&self, at: usize) -> usize {
            usize::from(u16::from_le_bytes([self.0[at], self.0[at + 1]]))
        }

        fn u32(&self, at: usize) -> usize {
            let bytes =
                [self.0[at], self.0[at + 1], self.0[at + 2], self.0[at + 3]];

            u32::from_le_bytes(bytes) as usize
        }

        fn uleb128(&self, at: &mut usize) -> u32 {
            let mut value = 0;
            let mut shift = 0;

            loop {
                let byte = self.0[*at];
                *at += 1;
                value |= u32::from(byte & 0x7f) << shift;
                shift += 7;

                if byte & 0x80 == 0 {
                    return value;
                }
            }
        }

        /// A string of the string table: MUTF-8, which is ASCII here.
        fn string(&self, index: usize) -> String {
            let mut at = self.u32(self.u32(0x3c) + 4 * index);
            let _length = self.uleb128(&mut at);
            let end = at
                + self.0[at..]
                    .iter()
                    .position(|&byte| byte == 0)
                    .expect("a terminated string");

            String::from_utf8_lossy(&self.0[at..end]).into_owned()
        }

        fn type_name(&self, index: usize) -> String {
            self.string(self.u32(self.u32(0x44) + 4 * index))
        }

        /// A prototype as a JNI signature.
        fn signature(&self, proto: usize) -> String {
            let item = self.u32(0x4c) + 12 * proto;
            let parameters = self.u32(item + 8);

            let mut signature = String::from("(");

            if parameters != 0 {
                for k in 0..self.u32(parameters) {
                    signature.push_str(
                        &self.type_name(self.u16(parameters + 4 + 2 * k)),
                    );
                }
            }

            signature.push(')');
            signature.push_str(&self.type_name(self.u32(item + 4)));
            signature
        }

        fn class(&self, descriptor: &str) -> Option<Class> {
            let definitions = self.u32(0x64);

            let item = (0..self.u32(0x60))
                .map(|k| definitions + 32 * k)
                .find(|&item| self.type_name(self.u32(item)) == descriptor)?;

            let mut methods = Vec::new();
            let data = self.u32(item + 24);

            if data != 0 {
                let mut at = data;
                let static_fields = self.uleb128(&mut at);
                let instance_fields = self.uleb128(&mut at);
                let direct = self.uleb128(&mut at);
                let virtual_ = self.uleb128(&mut at);

                for _ in 0..(static_fields + instance_fields) * 2 {
                    let _ = self.uleb128(&mut at);
                }

                for count in [direct, virtual_] {
                    let mut index = 0;

                    for _ in 0..count {
                        index += self.uleb128(&mut at) as usize;
                        let access = self.uleb128(&mut at);
                        let _code = self.uleb128(&mut at);

                        let method = self.u32(0x5c) + 8 * index;

                        methods.push(Defined {
                            name: self.string(self.u32(method + 4)),
                            signature: self.signature(self.u16(method + 2)),
                            access,
                        });
                    }
                }
            }

            Some(Class {
                access: self.u32(item + 4) as u32,
                superclass: self.type_name(self.u32(item + 8)),
                methods,
            })
        }
    }
}
