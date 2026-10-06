//! The loggers [`init_logger`](super::init_logger) installs where the
//! platform has no log of its own: stderr on the desktop, the console on the
//! web. Both are minimal [`log::Log`] implementations, so that `iced` needs
//! no logging crate beyond `log`.
use log::{Level, LevelFilter, Log, Metadata, Record};

/// Which records pass: an `env_logger`-style list of directives, as
/// `RUST_LOG` holds them.
///
/// - `warn` sets the level of every target no directive names;
/// - `iced_wgpu=error` sets the level of the targets that start with
///   `iced_wgpu`, and the longest matching name wins;
/// - `my_app` alone lets every record of `my_app` pass.
///
/// Directives are separated by commas. With no directive at all, records
/// up to `Info` pass; with directives but no bare level, only the targets
/// they name pass. Directives that do not parse are ignored.
///
/// Once the application raises `log::max_level` above the most verbose level
/// the directives give, every record up to it passes, so that
/// `log::set_max_level` can raise the level as well as lower it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Filter {
    default: LevelFilter,
    directives: Vec<(String, LevelFilter)>,
    max: LevelFilter,
}

/// The directives used without `RUST_LOG`: `Info` records, except for those
/// of the shell and of the wgpu renderer, whose `Info` records report the
/// window attributes, the adapters and the surface formats on more than a
/// hundred lines at every start.
pub(super) const DEFAULT: &str = "info,iced_winit=warn,iced_wgpu=warn,wgpu_core=warn,wgpu_hal=warn,naga=warn";

impl Filter {
    /// The filter `RUST_LOG` holds, or [`DEFAULT`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_env() -> Self {
        match std::env::var("RUST_LOG") {
            Ok(spec) if !spec.trim().is_empty() => Self::parse(&spec),
            _ => Self::parse(DEFAULT),
        }
    }

    /// The filter the page's `rust_log` query parameter holds
    /// (`?rust_log=debug`, `?rust_log=info,my_app=trace`), or [`DEFAULT`].
    #[cfg(target_arch = "wasm32")]
    pub fn from_query() -> Self {
        let spec = web_sys::window()
            .and_then(|window| window.location().search().ok())
            .and_then(|search| query_value(&search, "rust_log"))
            .filter(|spec| !spec.trim().is_empty());

        Self::parse(spec.as_deref().unwrap_or(DEFAULT))
    }

    pub fn parse(spec: &str) -> Self {
        let mut default = None;
        let mut directives = Vec::new();

        for directive in spec.split(',').map(str::trim) {
            if directive.is_empty() {
                continue;
            }

            match directive.split_once('=') {
                Some((target, level)) => {
                    let Ok(level) = level.trim().parse() else {
                        continue;
                    };

                    directives.push((target.trim().to_owned(), level));
                }
                None => match directive.parse() {
                    Ok(level) => default = Some(level),
                    Err(_) => {
                        directives
                            .push((directive.to_owned(), LevelFilter::Trace));
                    }
                },
            }
        }

        let default = default.unwrap_or(if directives.is_empty() {
            LevelFilter::Info
        } else {
            LevelFilter::Off
        });

        // Longest first, so that the first match is the most specific.
        directives.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));

        let max = directives
            .iter()
            .map(|(_, level)| *level)
            .fold(default, Ord::max);

        Self {
            default,
            directives,
            max,
        }
    }

    /// The most verbose level any target gets.
    pub fn max(&self) -> LevelFilter {
        self.max
    }

    /// Whether a record passes, while `log::max_level` is `max_level`.
    pub fn enabled(
        &self,
        target: &str,
        level: Level,
        max_level: LevelFilter,
    ) -> bool {
        if level > max_level {
            return false;
        }

        if max_level > self.max {
            return true;
        }

        let filter = self
            .directives
            .iter()
            .find(|(name, _)| target.starts_with(name.as_str()))
            .map_or(self.default, |(_, level)| *level);

        level <= filter
    }
}

/// The decoded value of a parameter of a query string (`?a=b&c=d`).
#[cfg(any(test, target_arch = "wasm32"))]
fn query_value(search: &str, key: &str) -> Option<String> {
    let value = search
        .trim_start_matches('?')
        .split('&')
        .find_map(|pair| pair.strip_prefix(key)?.strip_prefix('='))?;

    let mut bytes = Vec::with_capacity(value.len());
    let mut rest = value.as_bytes();

    while let Some((&byte, tail)) = rest.split_first() {
        let escaped = (byte == b'%')
            .then(|| tail.get(..2))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());

        match escaped {
            Some(decoded) => {
                bytes.push(decoded);
                rest = &tail[2..];
            }
            None => {
                bytes.push(if byte == b'+' { b' ' } else { byte });
                rest = tail;
            }
        }
    }

    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Writes each record to stderr as one line:
/// `[2026-10-06T12:34:56.789Z INFO  iced_winit] message`.
#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_arch = "wasm32"
)))]
pub(super) struct Stderr {
    pub filter: Filter,
}

#[cfg(not(any(
    target_os = "android",
    target_os = "ios",
    target_arch = "wasm32"
)))]
impl Log for Stderr {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.filter.enabled(
            metadata.target(),
            metadata.level(),
            log::max_level(),
        )
    }

    fn log(&self, record: &Record<'_>) {
        use std::io::Write;

        if !self.enabled(record.metadata()) {
            return;
        }

        // One write per record, so that records from different threads do
        // not interleave.
        let line = format!(
            "[{} {:<5} {}] {}\n",
            timestamp(std::time::SystemTime::now()),
            record.level(),
            record.target(),
            record.args()
        );

        let _ = std::io::stderr().lock().write_all(line.as_bytes());
    }

    fn flush(&self) {
        use std::io::Write;

        let _ = std::io::stderr().flush();
    }
}

/// Writes each record to the browser's console, with the console method of
/// its level: `console.error`, `warn`, `info`, and `debug` for `Debug` and
/// `Trace` (shown under the console's "Verbose" level).
#[cfg(target_arch = "wasm32")]
pub(super) struct Console {
    pub filter: Filter,
}

#[cfg(target_arch = "wasm32")]
impl Log for Console {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.filter.enabled(
            metadata.target(),
            metadata.level(),
            log::max_level(),
        )
    }

    fn log(&self, record: &Record<'_>) {
        use web_sys::console;
        use web_sys::wasm_bindgen::JsValue;

        if !self.enabled(record.metadata()) {
            return;
        }

        let text = JsValue::from(format!(
            "[{} {}] {}",
            record.level(),
            record.target(),
            record.args()
        ));

        match record.level() {
            Level::Error => console::error_1(&text),
            Level::Warn => console::warn_1(&text),
            Level::Info => console::info_1(&text),
            Level::Debug | Level::Trace => console::debug_1(&text),
        }
    }

    fn flush(&self) {}
}

/// `time` in RFC 3339, in UTC, to the millisecond.
#[cfg(any(
    test,
    not(any(
        target_os = "android",
        target_os = "ios",
        target_arch = "wasm32"
    ))
))]
fn timestamp(time: std::time::SystemTime) -> String {
    let since_epoch = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();

    let seconds = since_epoch.as_secs();
    let (year, month, day) = civil_from_days(seconds / 86_400);
    let seconds = seconds % 86_400;

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60,
        since_epoch.subsec_millis()
    )
}

/// The date `days` after 1970-01-01, in the proleptic Gregorian calendar
/// (Howard Hinnant's `civil_from_days`).
#[cfg(any(
    test,
    not(any(
        target_os = "android",
        target_os = "ios",
        target_arch = "wasm32"
    ))
))]
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let days = days + 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524
        - day_of_era / 146_096)
        / 365;
    let day_of_year =
        day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);

    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether `filter` lets a record pass while `log::max_level` is what
    /// the filter set.
    fn passes(filter: &Filter, target: &str, level: Level) -> bool {
        filter.enabled(target, level, filter.max())
    }

    #[test]
    fn filters_parse_like_env_logger() {
        let filter = Filter::parse("");
        assert_eq!(filter.max(), LevelFilter::Info);
        assert!(passes(&filter, "anything", Level::Info));
        assert!(!passes(&filter, "anything", Level::Debug));

        let filter = Filter::parse(" debug ");
        assert_eq!(filter.max(), LevelFilter::Debug);
        assert!(passes(&filter, "anything", Level::Debug));

        let filter =
            Filter::parse("info,iced_wgpu=warn,iced_wgpu::image=trace");
        assert_eq!(filter.max(), LevelFilter::Trace);
        assert!(passes(&filter, "my_app", Level::Info));
        assert!(!passes(&filter, "my_app", Level::Debug));
        assert!(!passes(&filter, "iced_wgpu::window", Level::Info));
        assert!(passes(&filter, "iced_wgpu::window", Level::Warn));
        assert!(passes(&filter, "iced_wgpu::image::atlas", Level::Trace));

        let filter = Filter::parse("my_app=debug");
        assert!(passes(&filter, "my_app", Level::Debug));
        assert!(!passes(&filter, "iced_winit", Level::Error));

        let filter = Filter::parse("my_app");
        assert!(passes(&filter, "my_app::ui", Level::Trace));
        assert!(!passes(&filter, "other", Level::Error));

        let filter = Filter::parse("my_app=loud,warn");
        assert_eq!(filter.max(), LevelFilter::Warn);
        assert!(!passes(&filter, "my_app", Level::Info));

        let filter = Filter::parse(DEFAULT);
        assert_eq!(filter.max(), LevelFilter::Info);
        assert!(passes(&filter, "my_app", Level::Info));
        assert!(!passes(
            &filter,
            "iced_wgpu::window::compositor",
            Level::Info
        ));
        assert!(passes(&filter, "iced_winit", Level::Warn));
    }

    #[test]
    fn the_maximum_level_raises_and_lowers_the_filter() {
        let filter = Filter::parse(DEFAULT);

        // Lowered: the maximum level wins.
        assert!(!filter.enabled("my_app", Level::Info, LevelFilter::Warn));

        // Raised above every directive: everything up to it passes.
        assert!(filter.enabled("my_app", Level::Debug, LevelFilter::Debug));
        assert!(filter.enabled("iced_wgpu", Level::Info, LevelFilter::Debug));
        assert!(!filter.enabled("my_app", Level::Trace, LevelFilter::Debug));
    }

    #[test]
    fn query_values_are_decoded() {
        assert_eq!(
            query_value("?rust_log=info,my_app=debug&icm_events=1", "rust_log"),
            Some(String::from("info,my_app=debug"))
        );
        assert_eq!(
            query_value("?a=1&rust_log=info%2Cmy_app%3Dtrace", "rust_log"),
            Some(String::from("info,my_app=trace"))
        );
        assert_eq!(query_value("?rust_logs=debug", "rust_log"), None);
        assert_eq!(query_value("", "rust_log"), None);
        assert_eq!(
            query_value("?rust_log=100%", "rust_log"),
            Some(String::from("100%"))
        );
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        use std::time::{Duration, UNIX_EPOCH};

        assert_eq!(timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");

        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_millis(1_000_000_000_123)),
            "2001-09-09T01:46:40.123Z"
        );

        // A leap day, and the last moment of a year.
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(1_709_164_800)),
            "2024-02-29T00:00:00.000Z"
        );
        assert_eq!(
            timestamp(UNIX_EPOCH + Duration::from_secs(1_798_761_599)),
            "2026-12-31T23:59:59.000Z"
        );
    }
}
