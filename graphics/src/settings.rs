use crate::Antialiasing;
use crate::core::{self, Font, Pixels};

/// The settings of a renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    /// The default [`Font`] to use.
    pub default_font: Font,

    /// The default size of text.
    ///
    /// By default, it will be set to `16.0`.
    pub default_text_size: Pixels,

    /// The antialiasing strategy that will be used for triangle primitives.
    ///
    /// By default, it is `None`.
    pub antialiasing: Option<Antialiasing>,

    /// Whether or not to synchronize frames.
    ///
    /// By default, it is `true`.
    pub vsync: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            default_font: Font::default(),
            default_text_size: Pixels(16.0),
            antialiasing: None,
            vsync: true,
        }
    }
}

impl From<core::Settings> for Settings {
    fn from(settings: core::Settings) -> Self {
        Self {
            // The web has no system font that the text stack finds for the
            // default `SansSerif` family, so text would render as nothing;
            // Android and iOS have one only with `mobile-system-fonts`. Fira
            // Sans is used instead when it is embedded: through `fira-sans`,
            // or `mobile-fira-sans` (on by default in `iced`) on Android and
            // iOS.
            default_font: if cfg!(any(
                all(target_arch = "wasm32", feature = "fira-sans"),
                all(
                    any(target_os = "android", target_os = "ios"),
                    any(feature = "fira-sans", feature = "mobile-fira-sans")
                )
            )) && settings.default_font == Font::default()
            {
                Font::with_name("Fira Sans")
            } else {
                settings.default_font
            },
            default_text_size: settings.default_text_size,
            antialiasing: settings.antialiasing.then_some(Antialiasing::MSAAx4),
            vsync: settings.vsync,
        }
    }
}
