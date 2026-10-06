//! The fonts of the operating system on Android and iOS.
//!
//! fontdb, which cosmic-text uses to find fonts, scans no font directory on
//! either platform, and cosmic-text has no fallback lists for them. Fira Sans,
//! the default font there, covers only Latin, Greek and Cyrillic, so text in
//! any other script (CJK, Arabic, Hebrew, Indic, Thai, ...) would be drawn
//! with nothing. This module indexes the system fonts once, when the global
//! font system is created, and gives cosmic-text fallback lists that name
//! them. Fira Sans stays the default family; the system fonts are only used
//! for what it lacks.
//!
//! Indexing parses only the header tables of each file (fontdb maps the file
//! and reads its names and metrics, not its glyphs). See `font_system` for
//! measured times.
//!
//! Some system fonts are in formats that swash, the rasterizer, cannot draw:
//! - Emoji stay blank on both platforms: Android's Noto Color Emoji is COLRv1,
//!   and Apple Color Emoji stores its images as `emjc`, not PNG. The flags of
//!   Android's separate Noto Color Emoji Flags font do draw.
//! - PingFang, the Chinese font of iOS, has only `hvgl` outlines, so it is
//!   not loaded. Han characters fall back to Hiragino Sans, which has the
//!   Japanese sets but lacks many simplified characters (这, 们, ...). Those
//!   are drawn as the missing-glyph box of Fira Sans, once cosmic-text has
//!   tried every face for them, which costs about 1 ms per text layout in a
//!   release build and 20 ms in a debug build.
//!
//! Apps that need these must embed a font for them.
use cosmic_text::fontdb;

/// Creates the font system with the `embedded` fonts, the fonts of the
/// operating system, and fallback lists for them.
///
/// It runs once, when [`super::font_system`] is first used. Measured with the
/// font files in the page cache; the first run after a boot read them from
/// disk and took 20 ms on Android and 120 ms on iOS (release builds):
///
/// | Device              | System faces | Release | Debug |
/// |---------------------|--------------|---------|-------|
/// | Android 16 emulator | 214          | 2 ms    | 11 ms |
/// | iOS 27 simulator    | 454          | 11 ms   | 40 ms |
#[cfg(any(target_os = "android", target_os = "ios"))]
pub fn font_system(
    embedded: impl IntoIterator<Item = fontdb::Source>,
) -> cosmic_text::FontSystem {
    // `cosmic_text::FontSystem::new_with_fonts` would also look for the
    // fonts of Linux, the branch fontdb takes on iOS, and log a fontconfig
    // warning there.
    let locale =
        sys_locale::get_locale().unwrap_or_else(|| String::from("en-US"));
    let mut db = fontdb::Database::new();

    for source in embedded {
        let _ = db.load_font_source(source);
    }

    // The system fonts must be in the database before the font system is
    // built: it lists the monospaced faces only then.
    let embedded_faces = db.len();
    let start = std::time::Instant::now();

    platform::load_fonts(&mut db);

    if db.len() == embedded_faces {
        log::warn!(
            "No system fonts found; text in scripts that the embedded fonts \
            lack will not be drawn"
        );
    } else {
        log::debug!(
            "Indexed {} system font faces in {:?}",
            db.len() - embedded_faces,
            start.elapsed()
        );
    }

    add_missing_weights(&mut db, platform::CJK);

    #[cfg(feature = "fira-sans")]
    db.set_sans_serif_family("Fira Sans");

    #[cfg(not(feature = "fira-sans"))]
    db.set_sans_serif_family(platform::SANS_SERIF);

    db.set_serif_family(platform::SERIF);
    db.set_monospace_family(platform::MONOSPACE);

    cosmic_text::FontSystem::new_with_locale_and_db_and_fallback(
        locale, db, Fallback,
    )
}

/// The fallback lists of the platform.
#[cfg(any(target_os = "android", target_os = "ios"))]
struct Fallback;

#[cfg(any(target_os = "android", target_os = "ios"))]
impl cosmic_text::Fallback for Fallback {
    fn common_fallback(&self) -> &[&'static str] {
        platform::COMMON
    }

    fn forbidden_fallback(&self) -> &[&'static str] {
        platform::FORBIDDEN
    }

    fn script_fallback(
        &self,
        script: unicode_script::Script,
        locale: &str,
    ) -> &[&'static str] {
        platform::script_fallback(script, Han::from_locale(locale))
    }
}

/// The regional style of Han characters, which decides the CJK font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Han {
    Japanese,
    Korean,
    Simplified,
    Traditional,
    HongKong,
}

impl Han {
    /// Picks the style for a BCP 47 locale, like `en-US`, `ja-JP` or
    /// `zh-Hant-TW`. Simplified Chinese is the default.
    fn from_locale(locale: &str) -> Self {
        let mut subtags = locale.split(['-', '_']);
        let language = subtags.next().unwrap_or_default();

        if language.eq_ignore_ascii_case("ja") {
            Han::Japanese
        } else if language.eq_ignore_ascii_case("ko") {
            Han::Korean
        } else if language.eq_ignore_ascii_case("zh") {
            let subtags: Vec<&str> = subtags.collect();
            let has = |tag: &str| {
                subtags
                    .iter()
                    .any(|subtag| subtag.eq_ignore_ascii_case(tag))
            };

            if has("HK") || has("MO") {
                Han::HongKong
            } else if has("TW") || has("Hant") {
                Han::Traditional
            } else {
                Han::Simplified
            }
        } else {
            Han::Simplified
        }
    }
}

/// Gives each of `families` a regular (400) and a bold (700) face where it
/// has none, by adding its nearest face again under that weight.
///
/// cosmic-text only takes a script fallback whose face has exactly the
/// requested weight. Android's CJK fonts are variable, but fontdb registers
/// each at its default weight of 400 only, so bold Han text would skip the
/// face of the locale and take whichever CJK face comes first (the Japanese
/// one). cosmic-text draws a variable font at the requested weight, so the
/// added face is a real bold. Other weights still take the first CJK face.
fn add_missing_weights(db: &mut fontdb::Database, families: &[&str]) {
    for family in families {
        for weight in [fontdb::Weight::NORMAL, fontdb::Weight::BOLD] {
            let is_upright_face_of_family = |face: &&fontdb::FaceInfo| {
                face.style == fontdb::Style::Normal
                    && face.stretch == fontdb::Stretch::Normal
                    && face.families.iter().any(|(name, _)| name == family)
            };

            if db
                .faces()
                .filter(is_upright_face_of_family)
                .any(|face| face.weight == weight)
            {
                continue;
            }

            let alias = db
                .faces()
                .filter(is_upright_face_of_family)
                .min_by_key(|face| face.weight.0.abs_diff(weight.0))
                .map(|face| fontdb::FaceInfo {
                    weight,
                    ..face.clone()
                });

            if let Some(alias) = alias {
                let _ = db.push_face_info(alias);
            }
        }
    }
}

#[cfg(target_os = "android")]
mod platform {
    use super::Han;

    use cosmic_text::fontdb;
    use unicode_script::Script;

    #[cfg_attr(feature = "fira-sans", allow(dead_code))]
    pub const SANS_SERIF: &str = "Roboto";
    pub const SERIF: &str = "Noto Serif";
    pub const MONOSPACE: &str = "Droid Sans Mono";

    /// Symbols come before emoji, so that characters both fonts have are
    /// drawn as text: the color emoji cannot be drawn at all.
    pub const COMMON: &[&str] = &[
        "Noto Sans Symbols",
        "Noto Color Emoji Flags",
        "Noto Color Emoji",
    ];

    pub const FORBIDDEN: &[&str] = &[];

    pub const CJK: &[&str] = &[
        "Noto Sans CJK JP",
        "Noto Sans CJK KR",
        "Noto Sans CJK SC",
        "Noto Sans CJK TC",
        "Noto Sans CJK HK",
    ];

    pub fn load_fonts(db: &mut fontdb::Database) {
        db.load_fonts_dir("/system/fonts");
    }

    /// The families of `/system/etc/fonts.xml`, with its compact ("UI")
    /// variants first, as Android uses them for interface text. Checked on
    /// Android 16; scripts without an entry fall back to any face that has
    /// the glyphs.
    pub fn script_fallback(
        script: Script,
        han: Han,
    ) -> &'static [&'static str] {
        match script {
            Script::Arabic => &["Noto Naskh Arabic UI", "Noto Naskh Arabic"],
            Script::Armenian => &["Noto Sans Armenian"],
            Script::Bengali => &["Noto Sans Bengali UI", "Noto Sans Bengali"],
            Script::Bopomofo => &["Noto Sans CJK TC"],
            Script::Canadian_Aboriginal => &["Noto Sans Canadian Aboriginal"],
            Script::Cherokee => &["Noto Sans Cherokee"],
            Script::Devanagari => {
                &["Noto Sans Devanagari UI", "Noto Sans Devanagari"]
            }
            Script::Ethiopic => &["Noto Sans Ethiopic"],
            Script::Georgian => &["Noto Sans Georgian"],
            Script::Gujarati => {
                &["Noto Sans Gujarati UI", "Noto Sans Gujarati"]
            }
            Script::Gurmukhi => {
                &["Noto Sans Gurmukhi UI", "Noto Sans Gurmukhi"]
            }
            Script::Han => match han {
                Han::Japanese => &["Noto Sans CJK JP"],
                Han::Korean => &["Noto Sans CJK KR"],
                Han::Simplified => &["Noto Sans CJK SC"],
                Han::Traditional => &["Noto Sans CJK TC"],
                Han::HongKong => &["Noto Sans CJK HK"],
            },
            Script::Hangul => &["Noto Sans CJK KR"],
            Script::Hebrew => &["Noto Sans Hebrew"],
            Script::Hiragana | Script::Katakana => &["Noto Sans CJK JP"],
            Script::Kannada => &["Noto Sans Kannada UI", "Noto Sans Kannada"],
            Script::Khmer => &["Noto Sans Khmer UI", "Noto Sans Khmer"],
            Script::Lao => &["Noto Sans Lao UI", "Noto Sans Lao"],
            Script::Malayalam => {
                &["Noto Sans Malayalam UI", "Noto Sans Malayalam"]
            }
            Script::Mongolian => &["Noto Sans Mongolian"],
            Script::Myanmar => &["Noto Sans Myanmar UI", "Noto Sans Myanmar"],
            Script::Oriya => &["Noto Sans Oriya UI", "Noto Sans Oriya"],
            Script::Sinhala => &["Noto Sans Sinhala UI", "Noto Sans Sinhala"],
            Script::Syriac => &["Noto Sans Syriac Estrangela"],
            Script::Tamil => &["Noto Sans Tamil UI", "Noto Sans Tamil"],
            Script::Telugu => &["Noto Sans Telugu UI", "Noto Sans Telugu"],
            Script::Thaana => &["Noto Sans Thaana"],
            Script::Thai => &["Noto Sans Thai UI", "Noto Sans Thai"],
            Script::Tibetan => &["Noto Serif Tibetan"],
            Script::Yi => &["Noto Sans Yi"],
            _ => &[],
        }
    }
}

#[cfg(target_os = "ios")]
mod platform {
    use super::Han;

    use cosmic_text::fontdb;
    use unicode_script::Script;

    use std::path::PathBuf;

    #[cfg_attr(feature = "fira-sans", allow(dead_code))]
    pub const SANS_SERIF: &str = "Helvetica Neue";
    pub const SERIF: &str = "Times New Roman";
    pub const MONOSPACE: &str = "Menlo";

    /// Apple Color Emoji is left out: it cannot be drawn (see the module
    /// documentation).
    pub const COMMON: &[&str] = &["Apple Symbols"];

    /// `.LastResort` has a placeholder glyph for every code point. It is
    /// dropped when loading (see [`load_fonts`]); this is a second guard.
    pub const FORBIDDEN: &[&str] = &[".LastResort"];

    /// Both have a regular and a bold face already; listed in case a later
    /// iOS changes that.
    pub const CJK: &[&str] = &["Hiragino Sans", "Apple SD Gothic Neo"];

    pub fn load_fonts(db: &mut fontdb::Database) {
        // The simulator does not remap paths: the process sees the file
        // system of the Mac, and the simulated iOS lives under this root.
        let root = std::env::var_os("IPHONE_SIMULATOR_ROOT")
            .map_or_else(|| PathBuf::from("/"), PathBuf::from);

        db.load_fonts_dir(root.join("System/Library/Fonts"));

        // Families starting with a dot are private to the system:
        // `.LastResort`, keycaps, and interface variants of public families.
        // Apple does not support using them by name, and in cosmic-text's
        // last fallback step, which tries every face, they would draw
        // placeholders or keycaps.
        let private: Vec<fontdb::ID> = db
            .faces()
            .filter(|face| {
                face.families
                    .first()
                    .is_some_and(|(name, _)| name.starts_with('.'))
            })
            .map(|face| face.id)
            .collect();

        for id in private {
            db.remove_face(id);
        }
    }

    /// Public families present in both the iOS 18 and the iOS 27 runtime;
    /// scripts without an entry fall back to any face that has the glyphs.
    pub fn script_fallback(
        script: Script,
        han: Han,
    ) -> &'static [&'static str] {
        match script {
            Script::Arabic => &["Geeza Pro"],
            Script::Armenian => &["Noto Sans Armenian"],
            Script::Bengali => &["Kohinoor Bangla"],
            Script::Canadian_Aboriginal => &["Euphemia UCAS"],
            Script::Devanagari => &["Kohinoor Devanagari"],
            Script::Ethiopic => &["Kefa III", "Kefa"],
            Script::Georgian => &["Helvetica Neue"],
            Script::Gujarati => &["Kohinoor Gujarati"],
            Script::Gurmukhi => &["Mukta Mahee"],
            // PingFang, the Chinese system font, cannot be drawn, so every
            // locale but Korean shares the Japanese font.
            Script::Han => match han {
                Han::Korean => &["Apple SD Gothic Neo", "Hiragino Sans"],
                Han::Japanese
                | Han::Simplified
                | Han::Traditional
                | Han::HongKong => &["Hiragino Sans"],
            },
            Script::Hangul => &["Apple SD Gothic Neo"],
            Script::Hebrew => &["Arial Hebrew"],
            Script::Hiragana | Script::Katakana => &["Hiragino Sans"],
            Script::Kannada => &["Noto Sans Kannada"],
            Script::Khmer => &["Khmer Sangam MN"],
            Script::Lao => &["Lao Sangam MN"],
            Script::Malayalam => &["Malayalam Sangam MN"],
            Script::Mongolian => &["Noto Sans Mongolian"],
            Script::Myanmar => &["Noto Sans Myanmar"],
            Script::Oriya => &["Noto Sans Oriya"],
            Script::Sinhala => &["Sinhala Sangam MN"],
            Script::Syriac => &["Noto Sans Syriac"],
            Script::Tamil => &["Tamil Sangam MN"],
            Script::Telugu => &["Kohinoor Telugu"],
            Script::Thaana => &["Noto Sans Thaana"],
            Script::Thai => &["Thonburi"],
            Script::Tibetan => &["Kailasa"],
            Script::Yi => &["Noto Sans Yi"],
            _ => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    #[test]
    fn han_style_follows_the_locale() {
        assert_eq!(Han::from_locale("ja-JP"), Han::Japanese);
        assert_eq!(Han::from_locale("ko_KR"), Han::Korean);
        assert_eq!(Han::from_locale("zh-Hans-CN"), Han::Simplified);
        assert_eq!(Han::from_locale("zh-TW"), Han::Traditional);
        assert_eq!(Han::from_locale("zh-Hant"), Han::Traditional);
        assert_eq!(Han::from_locale("zh-Hant-HK"), Han::HongKong);
        assert_eq!(Han::from_locale("zh-MO"), Han::HongKong);
        assert_eq!(Han::from_locale("en-US"), Han::Simplified);
        assert_eq!(Han::from_locale(""), Han::Simplified);
    }

    #[test]
    fn missing_weights_are_added_once() {
        let mut db = fontdb::Database::new();
        let _ = db.load_font_source(fontdb::Source::Binary(Arc::new(
            include_bytes!("../../fonts/FiraSans-Regular.ttf").as_slice(),
        )));

        let weights = |db: &fontdb::Database| {
            let mut weights: Vec<u16> = db
                .faces()
                .filter(|face| {
                    face.families.iter().any(|(name, _)| name == "Fira Sans")
                })
                .map(|face| face.weight.0)
                .collect();
            weights.sort_unstable();
            weights
        };

        assert_eq!(weights(&db), [400]);

        add_missing_weights(&mut db, &["Fira Sans", "Not Installed"]);
        assert_eq!(weights(&db), [400, 700]);

        add_missing_weights(&mut db, &["Fira Sans"]);
        assert_eq!(weights(&db), [400, 700]);
    }
}
