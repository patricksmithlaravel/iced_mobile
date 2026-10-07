//! Known failure signatures (design §13.4): text that iced, the system and
//! the platform tools print when something common goes wrong, mapped to the
//! likely cause and the catalogue id that covers it.
//!
//! A command that fails scans what it has (the app's stderr, logcat, the
//! system log, a tool's output) with [`annotate`], which adds the causes to
//! the error's `likely_causes`; [`scan`] returns the matches with the line
//! they were found on, for evidence. Facts that are not text (a blank
//! screenshot of a live app) go in [`Facts`].
//!
//! ```ignore
//! let facts = Facts { platform: Some("ios-sim"), ..Facts::default() };
//! let error = signatures::annotate(error, &stderr, &facts);
//! ```

use crate::catalogue::CheckId;
use crate::error::{Evidence, IcmError};
use serde_json::Value;
use std::path::Path;

/// What is known besides the text.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts<'a> {
    /// The platform: `desktop`, `web`, `ios-sim`, `ios-device`, `android`,
    /// or `headless` for the test harness.
    pub platform: Option<&'a str>,
    /// The screenshot is a single colour.
    pub screen_blank: bool,
    /// The app is still running.
    pub alive: bool,
    /// A panic was found.
    pub panicked: bool,
    /// Android reported `FLAG_SECURE` on the window.
    pub flag_secure: bool,
}

/// One known signature.
#[derive(Clone, Copy, Debug)]
pub struct Signature {
    /// A stable name, e.g. `ios.scene_manifest`.
    pub name: &'static str,
    /// Any of these matches; each is a set of substrings that must all be
    /// on the same line (compared without case).
    pub patterns: &'static [&'static [&'static str]],
    /// The likely cause and its fix, in one sentence or two.
    pub cause: &'static str,
    /// Platform-specific causes that replace `cause`.
    pub per_platform: &'static [(&'static str, &'static str)],
    /// The catalogue id that covers it.
    pub related: Option<CheckId>,
}

/// The signatures, most specific first.
pub const SIGNATURES: &[Signature] = &[
    Signature {
        name: "ios.scene_manifest",
        patterns: &[
            &["no UIApplicationSceneManifest"],
            &["UIApplicationSceneManifest", "missing"],
            &["UIApplicationSceneManifest", "required"],
            &["TN3187"],
            &["failed to launch", "scene"],
            &["launch failed", "scene"],
        ],
        cause: "The Info.plist has no UIApplicationSceneManifest: an app built with the iOS 27 SDK must adopt UIKit's scene life cycle or UIKit stops it at launch (TN3187). icm generates the key; a hand-written Info.plist must declare it (ios.plist.scene_manifest).",
        per_platform: &[],
        related: Some(CheckId::IosPlistSceneManifest),
    },
    Signature {
        name: "android.recreation",
        patterns: &[
            &["RecreationAttempt"],
            &["android_main ran a second time"],
            &["android_main ran twice"],
        ],
        cause: "android_main ran a second time in one process (RecreationAttempt): Android recreated the Activity or kept the process after the app stopped. Keep the full android:configChanges list, never call iced::exit on Android, and use iced::android_main!, which ends the process when the app stops (run.activity_recreated).",
        per_platform: &[],
        related: Some(CheckId::RunActivityRecreated),
    },
    Signature {
        name: "android.no_android_app",
        patterns: &[
            &["No AndroidApp"],
            &["Call set_android_app"],
            &["call iced::mobile::set_android_app"],
        ],
        cause: "iced never received the AndroidApp: define the entry point with iced::android_main!(run). If it is, two copies of iced_winit are linked and the call filled the other one: put every iced line on the same git URL and rev (deps.single_iced).",
        per_platform: &[],
        related: Some(CheckId::DepsSingleIced),
    },
    Signature {
        name: "android.activity_feature",
        patterns: &[
            &["\"game-activity\" or \"native-activity\" must be enabled"],
            &["file not found for module `activity_impl`"],
            &[
                "\"game-activity\" and \"native-activity\" features cannot be enabled at the same time",
            ],
        ],
        cause: "Android needs exactly one activity backend: iced's default features include android-native-activity, so `default-features = false` on iced drops it (add \"android-native-activity\" back), and android-game-activity needs `default-features = false` on every crate that depends on iced (deps.android_activity_backend).",
        per_platform: &[],
        related: Some(CheckId::DepsAndroidActivityBackend),
    },
    Signature {
        name: "framework.display_server",
        patterns: &[&["No Unix display server backend"]],
        cause: "iced was built for Linux without the x11 or wayland feature: keep iced's default features, or enable \"x11\" or \"wayland\" next to `default-features = false`.",
        per_platform: &[],
        related: None,
    },
    Signature {
        name: "android.lib_name",
        patterns: &[
            &["dlopen failed", ".so", "not found"],
            &["UnsatisfiedLinkError"],
            &["Unable to find native library"],
        ],
        cause: "Android could not load the app's library: android.app.lib_name must name lib<lib>.so, which icm derives from [app] lib (android.manifest.lib_name), and the APK must contain it for the device's ABI.",
        per_platform: &[],
        related: Some(CheckId::AndroidManifestLibName),
    },
    Signature {
        name: "android.update_incompatible",
        patterns: &[&["INSTALL_FAILED_UPDATE_INCOMPATIBLE"]],
        cause: "The installed app was signed with another key: rerun with `--reinstall --wipe-data`, which deletes the app's data on the device (android.install.signature_mismatch).",
        per_platform: &[],
        related: Some(CheckId::AndroidInstallSignatureMismatch),
    },
    Signature {
        name: "android.no_matching_abis",
        patterns: &[&["INSTALL_FAILED_NO_MATCHING_ABIS"]],
        cause: "The APK has no library for the device's ABI: build that ABI (`--abi`, or [android] abis) (android.so.abis).",
        per_platform: &[],
        related: Some(CheckId::AndroidSoAbis),
    },
    Signature {
        name: "android.anr",
        patterns: &[&["ANR in "], &["Application Not Responding"]],
        cause: "The main thread was blocked for seconds (ANR): move slow work into a Task, and never block in update or view (run.anr).",
        per_platform: &[],
        related: Some(CheckId::RunAnr),
    },
    Signature {
        name: "android.activity_destroyed",
        patterns: &[
            &["am_destroy_activity"],
            &["am_relaunch_activity"],
            &["am_relaunch_resume_activity"],
            &["wm_relaunch_activity"],
            &["wm_relaunch_resume_activity"],
        ],
        cause: "Android destroyed or relaunched the Activity: a configuration change it does not handle (a resource overlay change is assetsPaths), or Back finishing it. Keep the full android:configChanges list and [android] back = \"key\" (run.activity_recreated).",
        per_platform: &[],
        related: Some(CheckId::RunActivityRecreated),
    },
    Signature {
        name: "gpu.adapter",
        patterns: &[
            &["Failed to find an appropriate adapter"],
            &["a suitable graphics adapter or device could not be found"],
            &["no adapter was found for the options requested"],
            &["GraphicsAdapterNotFound"],
            &["the surface creation failed"],
            &["no device request succeeded"],
            &["GraphicsCreationFailed"],
        ],
        cause: "No usable GPU adapter or surface: retry with the CPU renderer, `icm run <platform> --env ICED_BACKEND=tiny-skia`.",
        per_platform: &[
            (
                "android",
                "No usable GPU adapter or surface: retry with the CPU renderer, `adb shell setprop debug.iced.backend tiny-skia`, then relaunch.",
            ),
            (
                "headless",
                "The harness was asked for a GPU backend through ICED_TEST_BACKEND and found no adapter: unset ICED_TEST_BACKEND, and it draws with tiny-skia on the CPU.",
            ),
            (
                "web",
                "The browser gave wgpu no adapter: build with iced's \"webgl\" feature (the template does) or run Chrome with WebGPU enabled.",
            ),
        ],
        related: None,
    },
    Signature {
        name: "ios.codesign",
        patterns: &[
            &["CODESIGNING"],
            &["A valid provisioning profile for this executable was not found"],
            &["no provisioning profile"],
            &["Code Signature Invalid"],
        ],
        cause: "The system refused the app's code signature or provisioning profile; signing assets belong to the owner (exit 9).",
        per_platform: &[],
        related: Some(CheckId::IosSignNoProfile),
    },
    Signature {
        name: "web.mime",
        patterns: &[&["Incorrect response MIME type"]],
        cause: "The server sent the .wasm without `Content-Type: application/wasm`: serve the site with `icm run web`, or configure the host's MIME types (web.mime).",
        per_platform: &[],
        related: Some(CheckId::WebMime),
    },
    Signature {
        name: "web.wasm_bindgen",
        patterns: &[
            &["wasm-bindgen", "different bindgen format"],
            &["wasm-bindgen", "schema version"],
            &["rust wasm file schema version"],
        ],
        cause: "wasm-bindgen-cli does not match the wasm-bindgen version in Cargo.lock: `icm doctor web --fix --yes` installs the matching one (deps.wasm_bindgen_cli).",
        per_platform: &[],
        related: Some(CheckId::DepsWasmBindgenCli),
    },
    Signature {
        name: "fonts.default_missing",
        patterns: &[&["font.default_missing"]],
        cause: "The default font is missing, so text draws as nothing: keep `features = [\"fira-sans\"]` on iced and the Fira Sans default_font (run.font_missing).",
        per_platform: &[],
        related: Some(CheckId::RunFontMissing),
    },
];

/// A signature found in text (or in the facts).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// The signature's name (`panic` and `screen.blank` for the built-in
    /// ones).
    pub name: String,
    /// The likely cause.
    pub cause: String,
    /// The catalogue id that covers it.
    pub related: Option<CheckId>,
    /// The 1-based line it was found on.
    pub line: Option<u32>,
    /// The matching line, trimmed.
    pub excerpt: Option<String>,
}

/// A panic found in output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Panic {
    /// The panic message (may be empty).
    pub message: String,
    /// `file:line:column`, when printed.
    pub location: Option<String>,
    /// The thread's name.
    pub thread: Option<String>,
    /// The 1-based line of the text it was found on.
    pub line: u32,
    /// That line, trimmed.
    pub excerpt: String,
}

impl Panic {
    /// The location's file and line.
    pub fn file_line(&self) -> Option<(String, u32)> {
        let location = self.location.as_deref()?;
        let mut parts = location.rsplitn(3, ':');
        let last = parts.next()?;
        let middle = parts.next()?;
        match parts.next() {
            Some(file) => Some((file.to_string(), middle.parse().ok()?)),
            None => Some((middle.to_string(), last.parse().ok()?)),
        }
    }

    /// `panicked at src/lib.rs:41:9: index out of bounds`.
    pub fn describe(&self) -> String {
        let mut text = String::from("panicked");
        if let Some(location) = &self.location {
            text.push_str(&format!(" at {location}"));
        }
        if !self.message.is_empty() {
            text.push_str(&format!(": {}", self.message));
        }
        text
    }

    /// Evidence for the panic in `path` (the file the text came from).
    pub fn evidence(&self, path: &Path) -> Evidence {
        Evidence::line(path, self.line, self.excerpt.clone())
    }
}

/// The first panic in the text: Rust's `thread '…' panicked at …` (the
/// message on the same or the next line), or an `ICM_EVENT` `panic` event.
pub fn first_panic(text: &str) -> Option<Panic> {
    let lines: Vec<&str> = text.lines().collect();
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        let number = index as u32 + 1;

        if let Some(json) = icm_event(line)
            && json.get("kind").and_then(Value::as_str) == Some("panic")
        {
            return Some(Panic {
                message: json
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                location: json
                    .get("location")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                thread: json
                    .get("thread")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                line: number,
                excerpt: line.to_string(),
            });
        }

        let Some(at) = line.find(" panicked at ") else {
            continue;
        };
        // `thread 'main' panicked`, or `thread 'main' (1234) panicked`.
        let thread = line[..at]
            .rsplit_once("thread '")
            .and_then(|(_, rest)| rest.split_once('\''))
            .map(|(name, _)| name.to_string());
        let rest = &line[at + " panicked at ".len()..];

        // Before Rust 1.73: panicked at 'message', src/lib.rs:41:9
        if let Some(quoted) = rest.strip_prefix('\'')
            && let Some((message, location)) = quoted.rsplit_once("', ")
        {
            return Some(Panic {
                message: message.to_string(),
                location: Some(location.trim().to_string()),
                thread,
                line: number,
                excerpt: line.to_string(),
            });
        }

        // Rust 1.73+: panicked at src/lib.rs:41:9:\nmessage
        let (location, message) = match rest.split_once(": ") {
            Some((location, message)) if looks_like_location(location) => {
                (location.to_string(), message.to_string())
            }
            _ => (rest.trim_end_matches(':').to_string(), String::new()),
        };
        let message = if message.is_empty() {
            lines
                .get(index + 1)
                .map(|next| next.trim().to_string())
                .filter(|next| !next.starts_with("note:") && !next.contains(" panicked at "))
                .unwrap_or_default()
        } else {
            message
        };
        return Some(Panic {
            message,
            location: looks_like_location(&location).then_some(location),
            thread,
            line: number,
            excerpt: line.to_string(),
        });
    }
    None
}

fn looks_like_location(text: &str) -> bool {
    let mut parts = text.rsplitn(3, ':');
    let column = parts.next().unwrap_or("");
    let line = parts.next().unwrap_or("");
    let file = parts.next().unwrap_or("");
    !file.is_empty()
        && !line.is_empty()
        && line.chars().all(|c| c.is_ascii_digit())
        && column.chars().all(|c| c.is_ascii_digit())
        && !column.is_empty()
}

/// The JSON of an `ICM_EVENT <json>` line (also inside logcat lines).
fn icm_event(line: &str) -> Option<Value> {
    let at = line.find("ICM_EVENT")?;
    let rest = &line[at + "ICM_EVENT".len()..];
    let start = rest.find('{')?;
    serde_json::from_str(rest[start..].trim()).ok()
}

fn matches_line(line_lower: &str, pattern: &[&str]) -> bool {
    pattern
        .iter()
        .all(|part| line_lower.contains(&part.to_ascii_lowercase()))
}

fn cause_for(signature: &Signature, platform: Option<&str>) -> String {
    platform
        .and_then(|platform| {
            signature
                .per_platform
                .iter()
                .find(|(name, _)| *name == platform)
                .map(|(_, cause)| (*cause).to_string())
        })
        .unwrap_or_else(|| signature.cause.to_string())
}

/// Every signature found in the text and the facts, each once, in the
/// order they first appear (fact-based ones last).
pub fn scan(text: &str, facts: &Facts) -> Vec<Match> {
    let mut found: Vec<Match> = Vec::new();

    if let Some(panic) = first_panic(text) {
        let mut cause = format!("The app {}", panic.describe());
        if let Some((file, line)) = panic.file_line() {
            cause.push_str(&format!(" (fix the code at {file}:{line})"));
        }
        found.push(Match {
            name: "panic".to_string(),
            cause,
            related: Some(CheckId::RunAppPanicked),
            line: Some(panic.line),
            excerpt: Some(panic.excerpt.clone()),
        });
    }

    let mut seen = vec![false; SIGNATURES.len()];
    for (index, line) in text.lines().enumerate() {
        let lower = line.to_ascii_lowercase();
        for (which, signature) in SIGNATURES.iter().enumerate() {
            if seen[which] {
                continue;
            }
            if signature
                .patterns
                .iter()
                .any(|pattern| matches_line(&lower, pattern))
            {
                seen[which] = true;
                found.push(Match {
                    name: signature.name.to_string(),
                    cause: cause_for(signature, facts.platform),
                    related: signature.related,
                    line: Some(index as u32 + 1),
                    excerpt: Some(truncate(line.trim(), 300)),
                });
            }
        }
    }

    let panicked = facts.panicked || found.iter().any(|m| m.name == "panic");
    if facts.screen_blank && facts.alive && !panicked {
        let cause = if facts.flag_secure && facts.platform == Some("android") {
            "The window has FLAG_SECURE, so Android captures it as black; the app itself may be fine (android.screen.secure)."
        } else if facts.platform == Some("headless") {
            "The view drew a single colour: check that App::view returns its content and that the theme is not drawing over it. Text needs a font: keep `features = [\"fira-sans\"]` on iced."
        } else {
            "The app is alive but drew a single colour: usually text with no font or a theme problem. Compare with `icm shot --headless`; if that shows the content, keep `features = [\"fira-sans\"]` and the Fira Sans default_font."
        };
        found.push(Match {
            name: "screen.blank".to_string(),
            cause: cause.to_string(),
            related: Some(if facts.flag_secure {
                CheckId::AndroidScreenSecure
            } else {
                CheckId::RunScreenBlank
            }),
            line: None,
            excerpt: None,
        });
    }

    found
}

/// The likely causes for the text and facts, without duplicates.
pub fn likely_causes(text: &str, facts: &Facts) -> Vec<String> {
    let mut causes: Vec<String> = Vec::new();
    for found in scan(text, facts) {
        if !causes.contains(&found.cause) {
            causes.push(found.cause);
        }
    }
    causes
}

/// Adds the likely causes found in the text and facts to an error.
pub fn annotate(mut error: IcmError, text: &str, facts: &Facts) -> IcmError {
    for cause in likely_causes(text, facts) {
        if !error.likely_causes.contains(&cause) {
            error.likely_causes.push(cause);
        }
    }
    error
}

/// [`scan`] over a file (its last 4 MiB); nothing when it cannot be read.
pub fn scan_file(path: &Path, facts: &Facts) -> Vec<Match> {
    read_tail(path, 4 << 20)
        .map(|text| scan(&text, facts))
        .unwrap_or_default()
}

/// The last `limit` bytes of a file as text, from a line start.
pub fn read_tail(path: &Path, limit: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > limit {
        let _ = file.seek(SeekFrom::Start(len - limit)).ok()?;
    }
    let mut bytes = Vec::new();
    let _ = file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if len > limit {
        Some(
            text.split_once('\n')
                .map(|(_, rest)| rest.to_string())
                .unwrap_or(text),
        )
    } else {
        Some(text)
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(text: &str, facts: &Facts) -> Vec<String> {
        scan(text, facts).into_iter().map(|m| m.name).collect()
    }

    #[test]
    fn the_common_causes_are_recognised() {
        let none = Facts::default();
        let cases: &[(&str, &str)] = &[
            (
                "iced: the app's Info.plist has no UIApplicationSceneManifest. An app built with the iOS 27 SDK must adopt",
                "ios.scene_manifest",
            ),
            (
                "thread 'main' panicked at winit/src/lib.rs:190:39:\nCreate event loop: android_main ran a second time in this process (RecreationAttempt)",
                "android.recreation",
            ),
            (
                "E RustStdoutStderr: No AndroidApp: define the entry point with iced::android_main!(run)",
                "android.no_android_app",
            ),
            (
                "error: Either \"game-activity\" or \"native-activity\" must be enabled as features",
                "android.activity_feature",
            ),
            (
                "error[E0583]: file not found for module `activity_impl`",
                "android.activity_feature",
            ),
            ("Failed to find an appropriate adapter", "gpu.adapter"),
            (
                "Error: GraphicsCreationFailed(GraphicsAdapterNotFound { backend: \"wgpu\", reason: RequestFailed(\"no adapter was found\") })",
                "gpu.adapter",
            ),
            (
                "java.lang.IllegalArgumentException: Unable to find native library app using classloader",
                "android.lib_name",
            ),
            (
                "Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: Package com.example.app signatures do not match]",
                "android.update_incompatible",
            ),
            (
                "I am_destroy_activity: [0,1234,5,com.example.app/android.app.NativeActivity]",
                "android.activity_destroyed",
            ),
            (
                "ICM_EVENT {\"v\":1,\"kind\":\"warning\",\"code\":\"font.default_missing\",\"message\":\"x\"}",
                "fonts.default_missing",
            ),
            (
                "TypeError: Failed to execute 'compile' on 'WebAssembly': Incorrect response MIME type. Expected 'application/wasm'.",
                "web.mime",
            ),
        ];
        for (text, expected) in cases {
            let found = names(text, &none);
            assert!(
                found.iter().any(|name| name == expected),
                "{expected} not found in {text:?}: {found:?}"
            );
        }
        assert!(names("all good\nready", &none).is_empty());
    }

    #[test]
    fn causes_depend_on_the_platform() {
        let text = "Failed to find an appropriate adapter";
        let android = likely_causes(
            text,
            &Facts {
                platform: Some("android"),
                ..Facts::default()
            },
        );
        assert!(android[0].contains("debug.iced.backend"), "{android:?}");
        let headless = likely_causes(
            text,
            &Facts {
                platform: Some("headless"),
                ..Facts::default()
            },
        );
        assert!(headless[0].contains("ICED_TEST_BACKEND"), "{headless:?}");
        let desktop = likely_causes(
            text,
            &Facts {
                platform: Some("desktop"),
                ..Facts::default()
            },
        );
        assert!(desktop[0].contains("ICED_BACKEND=tiny-skia"), "{desktop:?}");
    }

    #[test]
    fn panics_are_parsed_in_every_form() {
        let new = "noise\nthread 'main' panicked at src/lib.rs:41:9:\nindex out of bounds: the len is 0\nnote: run with `RUST_BACKTRACE=1`";
        let panic = first_panic(new).unwrap();
        assert_eq!(panic.location.as_deref(), Some("src/lib.rs:41:9"));
        assert_eq!(panic.message, "index out of bounds: the len is 0");
        assert_eq!(panic.thread.as_deref(), Some("main"));
        assert_eq!(panic.line, 2);
        assert_eq!(panic.file_line(), Some(("src/lib.rs".to_string(), 41)));
        assert_eq!(
            panic.describe(),
            "panicked at src/lib.rs:41:9: index out of bounds: the len is 0"
        );

        let old = "thread 'tests::a' panicked at 'boom', src/main.rs:3:5";
        let panic = first_panic(old).unwrap();
        assert_eq!(panic.message, "boom");
        assert_eq!(panic.location.as_deref(), Some("src/main.rs:3:5"));
        assert_eq!(panic.thread.as_deref(), Some("tests::a"));

        let event = "ICM_EVENT {\"v\":1,\"kind\":\"panic\",\"message\":\"oops\",\"location\":\"src/lib.rs:7:1\",\"thread\":\"main\"}";
        let panic = first_panic(event).unwrap();
        assert_eq!(panic.message, "oops");
        assert_eq!(panic.file_line(), Some(("src/lib.rs".to_string(), 7)));

        let logcat = "10-06 21:03:11.123  4321  4321 E RustPanic: thread 'main' (4321) panicked at src/lib.rs:12:5: explicit panic";
        let panic = first_panic(logcat).unwrap();
        assert_eq!(panic.message, "explicit panic");
        assert_eq!(panic.location.as_deref(), Some("src/lib.rs:12:5"));
        assert_eq!(panic.thread.as_deref(), Some("main"));

        assert!(first_panic("all fine").is_none());
    }

    #[test]
    fn a_blank_live_screen_has_a_cause() {
        let facts = Facts {
            platform: Some("ios-sim"),
            screen_blank: true,
            alive: true,
            ..Facts::default()
        };
        let found = scan("", &facts);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "screen.blank");
        assert!(found[0].cause.contains("icm shot --headless"));

        let secure = Facts {
            platform: Some("android"),
            flag_secure: true,
            ..facts
        };
        assert!(scan("", &secure)[0].cause.contains("FLAG_SECURE"));

        // A panic explains a blank screen better.
        let found = scan("thread 'main' panicked at src/lib.rs:1:1:\nx", &facts);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "panic");
    }

    #[test]
    fn annotate_adds_each_cause_once() {
        let error = IcmError::new(CheckId::RunAppDied, "it died");
        let text = "RecreationAttempt\nRecreationAttempt\nam_destroy_activity";
        let error = annotate(error, text, &Facts::default());
        let error = annotate(error, text, &Facts::default());
        assert_eq!(error.likely_causes.len(), 2, "{:?}", error.likely_causes);
    }

    #[test]
    fn every_signature_is_well_formed() {
        let mut names: Vec<&str> = SIGNATURES.iter().map(|s| s.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SIGNATURES.len(), "duplicate signature names");
        for signature in SIGNATURES {
            assert!(!signature.patterns.is_empty(), "{}", signature.name);
            assert!(
                signature.patterns.iter().all(|p| !p.is_empty()),
                "{}",
                signature.name
            );
            assert!(signature.cause.ends_with('.') || signature.cause.ends_with(')'));
        }
    }

    #[test]
    fn tails_start_at_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        std::fs::write(&path, "first line\nsecond line\nthird\n").unwrap();
        assert_eq!(read_tail(&path, 14).unwrap(), "third\n");
        assert_eq!(
            read_tail(&path, 1000).unwrap(),
            "first line\nsecond line\nthird\n"
        );
        let found = scan_file(&path, &Facts::default());
        assert!(found.is_empty());
    }
}
