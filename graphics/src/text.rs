//! Draw text.
pub mod cache;
pub mod editor;
pub mod paragraph;

#[cfg(any(target_os = "android", target_os = "ios", test))]
mod mobile;

pub use cache::Cache;
pub use editor::Editor;
pub use paragraph::Paragraph;

pub use cosmic_text;

use crate::core::alignment;
use crate::core::font::{self, Font};
use crate::core::text::{Alignment, Shaping, Wrapping};
use crate::core::{Color, Pixels, Point, Rectangle, Size, Transformation};

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock, Weak};

/// A text primitive.
#[derive(Debug, Clone, PartialEq)]
pub enum Text {
    /// A paragraph.
    #[allow(missing_docs)]
    Paragraph {
        paragraph: paragraph::Weak,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    },
    /// An editor.
    #[allow(missing_docs)]
    Editor {
        editor: editor::Weak,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
        transformation: Transformation,
    },
    /// Some cached text.
    Cached {
        /// The contents of the text.
        content: String,
        /// The bounds of the text.
        bounds: Rectangle,
        /// The color of the text.
        color: Color,
        /// The size of the text in logical pixels.
        size: Pixels,
        /// The line height of the text.
        line_height: Pixels,
        /// The font of the text.
        font: Font,
        /// The horizontal alignment of the text.
        align_x: Alignment,
        /// The vertical alignment of the text.
        align_y: alignment::Vertical,
        /// The shaping strategy of the text.
        shaping: Shaping,
        /// The clip bounds of the text.
        clip_bounds: Rectangle,
    },
    /// Some raw text.
    #[allow(missing_docs)]
    Raw {
        raw: Raw,
        transformation: Transformation,
    },
}

impl Text {
    /// Returns the visible bounds of the [`Text`].
    pub fn visible_bounds(&self) -> Option<Rectangle> {
        match self {
            Text::Paragraph {
                position,
                paragraph,
                clip_bounds,
                transformation,
                ..
            } => Rectangle::new(*position, paragraph.min_bounds)
                .intersection(clip_bounds)
                .map(|bounds| bounds * *transformation),
            Text::Editor {
                editor,
                position,
                clip_bounds,
                transformation,
                ..
            } => Rectangle::new(*position, editor.bounds)
                .intersection(clip_bounds)
                .map(|bounds| bounds * *transformation),
            Text::Cached {
                bounds,
                clip_bounds,
                ..
            } => bounds.intersection(clip_bounds),
            Text::Raw { raw, .. } => Some(raw.clip_bounds),
        }
    }
}

/// The regular variant of the [Fira Sans] font.
///
/// It is loaded as part of the default fonts when the `fira-sans`
/// feature is enabled, and on Android and iOS also with the
/// `mobile-fira-sans` feature, which `iced` enables by default. Every
/// application built that way embeds it, so it must ship the font's notice:
/// Fira Sans is licensed under the SIL Open Font License 1.1, whose text is
/// in `graphics/fonts/OFL.txt`.
///
/// [Fira Sans]: https://mozilla.github.io/Fira/
#[cfg(any(
    feature = "fira-sans",
    all(
        feature = "mobile-fira-sans",
        any(target_os = "android", target_os = "ios")
    )
))]
pub const FIRA_SANS_REGULAR: &[u8] =
    include_bytes!("../fonts/FiraSans-Regular.ttf").as_slice();

/// Returns the global [`FontSystem`].
///
/// It is created on first use. On Android and iOS, with the
/// `mobile-system-fonts` feature, this also indexes the fonts of the
/// operating system, so that scripts the embedded fonts lack can fall back
/// to them.
pub fn font_system() -> &'static RwLock<FontSystem> {
    static FONT_SYSTEM: OnceLock<RwLock<FontSystem>> = OnceLock::new();

    FONT_SYSTEM.get_or_init(|| {
        let embedded = [
            cosmic_text::fontdb::Source::Binary(Arc::new(
                include_bytes!("../fonts/Iced-Icons.ttf").as_slice(),
            )),
            #[cfg(any(
                feature = "fira-sans",
                all(
                    feature = "mobile-fira-sans",
                    any(target_os = "android", target_os = "ios")
                )
            ))]
            cosmic_text::fontdb::Source::Binary(Arc::new(FIRA_SANS_REGULAR)),
        ];

        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        let raw = cosmic_text::FontSystem::new_with_fonts(embedded);

        #[cfg(any(target_os = "android", target_os = "ios"))]
        let raw = mobile::font_system(embedded);

        RwLock::new(FontSystem::new(raw))
    })
}

/// Whether the global [`FontSystem`] holds a face of `font`'s family. Only
/// a named family can be missing (one the application neither embeds nor
/// loads, and the system lacks); the generic families always resolve to
/// some face or to the fallback.
pub fn is_loaded(font: Font) -> bool {
    let font::Family::Name(name) = font.family else {
        return true;
    };

    let mut system = font_system().write().expect("Write font system");

    system.raw().db().faces().any(|face| {
        face.families
            .iter()
            .any(|(family, _)| family.eq_ignore_ascii_case(name))
    })
}

/// A set of system fonts.
pub struct FontSystem {
    raw: cosmic_text::FontSystem,
    /// The addresses of the borrowed fonts loaded so far.
    loaded_fonts: HashSet<usize>,
    /// The owned fonts loaded so far, by length.
    loaded_owned_fonts: HashMap<usize, Vec<Arc<Vec<u8>>>>,
    version: Version,
}

impl FontSystem {
    fn new(raw: cosmic_text::FontSystem) -> Self {
        Self {
            raw,
            loaded_fonts: HashSet::new(),
            loaded_owned_fonts: HashMap::new(),
            version: Version::default(),
        }
    }

    /// Returns the raw [`cosmic_text::FontSystem`].
    pub fn raw(&mut self) -> &mut cosmic_text::FontSystem {
        &mut self.raw
    }

    /// Loads a font from its bytes.
    ///
    /// A font loaded already is not loaded again: borrowed bytes are
    /// recognized by their address, and owned bytes by their content. The
    /// font system lives as long as the process, which on Android outlives
    /// an application: each new Activity runs a new one, which loads its
    /// fonts again.
    pub fn load_font(&mut self, bytes: Cow<'static, [u8]>) {
        let bytes = match bytes {
            Cow::Borrowed(bytes) => {
                let address = bytes.as_ptr() as usize;

                if !self.loaded_fonts.insert(address) {
                    return;
                }

                Arc::new(bytes.to_vec())
            }
            Cow::Owned(bytes) => {
                let same_length =
                    self.loaded_owned_fonts.entry(bytes.len()).or_default();

                if same_length.iter().any(|loaded| **loaded == bytes) {
                    return;
                }

                let bytes = Arc::new(bytes);
                same_length.push(bytes.clone());

                bytes
            }
        };

        let _ = self
            .raw
            .db_mut()
            .load_font_source(cosmic_text::fontdb::Source::Binary(bytes));

        self.version = Version(self.version.0 + 1);
    }

    /// Returns the current [`Version`] of the [`FontSystem`].
    ///
    /// Loading a font will increase the version of a [`FontSystem`].
    pub fn version(&self) -> Version {
        self.version
    }
}

/// A version number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Version(u32);

/// A weak reference to a [`cosmic_text::Buffer`] that can be drawn.
#[derive(Debug, Clone)]
pub struct Raw {
    /// A weak reference to a [`cosmic_text::Buffer`].
    pub buffer: Weak<cosmic_text::Buffer>,
    /// The position of the text.
    pub position: Point,
    /// The color of the text.
    pub color: Color,
    /// The clip bounds of the text.
    pub clip_bounds: Rectangle,
}

impl PartialEq for Raw {
    fn eq(&self, _other: &Self) -> bool {
        // TODO: There is no proper way to compare raw buffers
        // For now, no two instances of `Raw` text will be equal.
        // This should be fine, but could trigger unnecessary redraws
        // in the future.
        false
    }
}

/// Measures the dimensions of the given [`cosmic_text::Buffer`].
pub fn measure(buffer: &cosmic_text::Buffer) -> (Size, bool) {
    let (width, height, has_rtl) = buffer.layout_runs().fold(
        (0.0, 0.0, false),
        |(width, height, has_rtl), run| {
            (
                run.line_w.max(width),
                height + run.line_height,
                has_rtl || run.rtl,
            )
        },
    );

    (Size::new(width, height), has_rtl)
}

/// Aligns the given [`cosmic_text::Buffer`] with the given [`Alignment`]
/// and returns its minimum [`Size`].
pub fn align(
    buffer: &mut cosmic_text::Buffer,
    font_system: &mut cosmic_text::FontSystem,
    alignment: Alignment,
) -> Size {
    let (min_bounds, has_rtl) = measure(buffer);
    let mut needs_relayout = has_rtl;

    if let Some(align) = to_align(alignment) {
        let has_multiple_lines = buffer.lines.len() > 1
            || buffer.lines.first().is_some_and(|line| {
                line.layout_opt().is_some_and(|layout| layout.len() > 1)
            });

        if has_multiple_lines {
            for line in &mut buffer.lines {
                let _ = line.set_align(Some(align));
            }

            needs_relayout = true;
        } else if let Some(line) = buffer.lines.first_mut() {
            needs_relayout = line.set_align(None);
        }
    }

    // TODO: Avoid relayout with some changes to `cosmic-text` (?)
    if needs_relayout {
        log::trace!("Relayouting paragraph...");

        buffer.set_size(
            font_system,
            Some(min_bounds.width),
            Some(min_bounds.height),
        );
    }

    min_bounds
}

/// Returns the attributes of the given [`Font`].
pub fn to_attributes(font: Font) -> cosmic_text::Attrs<'static> {
    cosmic_text::Attrs::new()
        .family(to_family(font.family))
        .weight(to_weight(font.weight))
        .stretch(to_stretch(font.stretch))
        .style(to_style(font.style))
}

fn to_family(family: font::Family) -> cosmic_text::Family<'static> {
    match family {
        font::Family::Name(name) => cosmic_text::Family::Name(name),
        font::Family::SansSerif => cosmic_text::Family::SansSerif,
        font::Family::Serif => cosmic_text::Family::Serif,
        font::Family::Cursive => cosmic_text::Family::Cursive,
        font::Family::Fantasy => cosmic_text::Family::Fantasy,
        font::Family::Monospace => cosmic_text::Family::Monospace,
    }
}

fn to_weight(weight: font::Weight) -> cosmic_text::Weight {
    match weight {
        font::Weight::Thin => cosmic_text::Weight::THIN,
        font::Weight::ExtraLight => cosmic_text::Weight::EXTRA_LIGHT,
        font::Weight::Light => cosmic_text::Weight::LIGHT,
        font::Weight::Normal => cosmic_text::Weight::NORMAL,
        font::Weight::Medium => cosmic_text::Weight::MEDIUM,
        font::Weight::Semibold => cosmic_text::Weight::SEMIBOLD,
        font::Weight::Bold => cosmic_text::Weight::BOLD,
        font::Weight::ExtraBold => cosmic_text::Weight::EXTRA_BOLD,
        font::Weight::Black => cosmic_text::Weight::BLACK,
    }
}

fn to_stretch(stretch: font::Stretch) -> cosmic_text::Stretch {
    match stretch {
        font::Stretch::UltraCondensed => cosmic_text::Stretch::UltraCondensed,
        font::Stretch::ExtraCondensed => cosmic_text::Stretch::ExtraCondensed,
        font::Stretch::Condensed => cosmic_text::Stretch::Condensed,
        font::Stretch::SemiCondensed => cosmic_text::Stretch::SemiCondensed,
        font::Stretch::Normal => cosmic_text::Stretch::Normal,
        font::Stretch::SemiExpanded => cosmic_text::Stretch::SemiExpanded,
        font::Stretch::Expanded => cosmic_text::Stretch::Expanded,
        font::Stretch::ExtraExpanded => cosmic_text::Stretch::ExtraExpanded,
        font::Stretch::UltraExpanded => cosmic_text::Stretch::UltraExpanded,
    }
}

fn to_style(style: font::Style) -> cosmic_text::Style {
    match style {
        font::Style::Normal => cosmic_text::Style::Normal,
        font::Style::Italic => cosmic_text::Style::Italic,
        font::Style::Oblique => cosmic_text::Style::Oblique,
    }
}

fn to_align(alignment: Alignment) -> Option<cosmic_text::Align> {
    match alignment {
        Alignment::Default => None,
        Alignment::Left => Some(cosmic_text::Align::Left),
        Alignment::Center => Some(cosmic_text::Align::Center),
        Alignment::Right => Some(cosmic_text::Align::Right),
        Alignment::Justified => Some(cosmic_text::Align::Justified),
    }
}

/// Converts some [`Shaping`] strategy to a [`cosmic_text::Shaping`] strategy.
pub fn to_shaping(shaping: Shaping, text: &str) -> cosmic_text::Shaping {
    match shaping {
        Shaping::Auto => {
            if text.is_ascii() {
                cosmic_text::Shaping::Basic
            } else {
                cosmic_text::Shaping::Advanced
            }
        }
        Shaping::Basic => cosmic_text::Shaping::Basic,
        Shaping::Advanced => cosmic_text::Shaping::Advanced,
    }
}

/// Converts some [`Wrapping`] strategy to a [`cosmic_text::Wrap`] strategy.
pub fn to_wrap(wrapping: Wrapping) -> cosmic_text::Wrap {
    match wrapping {
        Wrapping::None => cosmic_text::Wrap::None,
        Wrapping::Word => cosmic_text::Wrap::Word,
        Wrapping::Glyph => cosmic_text::Wrap::Glyph,
        Wrapping::WordOrGlyph => cosmic_text::Wrap::WordOrGlyph,
    }
}

/// Converts some [`Color`] to a [`cosmic_text::Color`].
pub fn to_color(color: Color) -> cosmic_text::Color {
    let [r, g, b, a] = color.into_rgba8();

    cosmic_text::Color::rgba(r, g, b, a)
}

/// A text renderer coupled to `iced_graphics`.
pub trait Renderer {
    /// Draws the given [`Raw`] text.
    fn fill_raw(&mut self, raw: Raw);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICONS: &[u8] = include_bytes!("../fonts/Iced-Icons.ttf");
    const FIRA_SANS: &[u8] = include_bytes!("../fonts/FiraSans-Regular.ttf");

    fn empty() -> FontSystem {
        FontSystem::new(cosmic_text::FontSystem::new_with_locale_and_db(
            "en-US".to_owned(),
            cosmic_text::fontdb::Database::new(),
        ))
    }

    #[test]
    fn owned_fonts_are_loaded_once() {
        let mut fonts = empty();

        // What each new application of the process does with a font it
        // was given as owned bytes.
        for _ in 0..3 {
            fonts.load_font(Cow::Owned(ICONS.to_vec()));
        }

        assert_eq!(fonts.raw().db().len(), 1);
        assert_eq!(fonts.version(), Version(1));

        // Other bytes of the same length are another font.
        let mut other = ICONS.to_vec();
        *other.last_mut().expect("A byte") ^= 1;
        fonts.load_font(Cow::Owned(other));
        fonts.load_font(Cow::Owned(FIRA_SANS.to_vec()));

        assert_eq!(fonts.raw().db().len(), 3);
        assert_eq!(fonts.version(), Version(3));
    }

    #[test]
    fn borrowed_fonts_are_loaded_once() {
        let mut fonts = empty();

        for _ in 0..3 {
            fonts.load_font(Cow::Borrowed(ICONS));
        }

        assert_eq!(fonts.raw().db().len(), 1);
        assert_eq!(fonts.version(), Version(1));
    }
}
