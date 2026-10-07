//! The Linux package sources (design §9.6): `DEBIAN/control`, the
//! `.desktop` entry, `AppRun`, the copyright file, and the checks a
//! `.desktop` file must pass.

/// What the package sources are made of.
#[derive(Clone, Debug)]
pub struct Facts {
    /// `[app] name`.
    pub name: String,
    /// `[app] id` (the `.desktop` and icon file names).
    pub id: String,
    /// The Debian package name.
    pub package: String,
    /// `<version>-<build>`.
    pub deb_version: String,
    /// The Debian architecture (`amd64`, `arm64`).
    pub arch: String,
    /// `[desktop.linux] maintainer`.
    pub maintainer: String,
    /// `[app] description`.
    pub description: Option<String>,
    /// `[app] category`.
    pub category: Option<String>,
    /// `[app] copyright`, else the publisher.
    pub copyright: String,
    /// `[store] marketing_url`.
    pub homepage: Option<String>,
    /// The executable's name.
    pub bin: String,
}

/// The libraries winit and wgpu load with `dlopen` (so `ldd` and
/// dpkg-shlibdeps cannot see them): the `.deb` recommends them. wgpu takes
/// Vulkan (`libvulkan.so.1`) first and falls back to GLES through EGL
/// (`libEGL.so.1`, libegl1); it has no GLX path, so `libgl1` would not
/// give it a fallback.
pub const RECOMMENDS: &[&str] = &[
    "libxkbcommon0",
    "libxkbcommon-x11-0",
    "libwayland-client0",
    "libvulkan1",
    "libegl1",
];

/// The libraries the AppImage bundles from the build host (glibc-floor
/// matched when built in the ubuntu:22.04 container), with the Debian
/// package whose copyright file covers each. None may be on
/// [`EXCLUDELIST`]: `libwayland-client.so.0` is not bundled, since the
/// host's Mesa needs symbols an older copy lacks, and it is ABI-stable and
/// present on every Wayland host.
pub const BUNDLED: &[(&str, &str)] = &[
    ("libxkbcommon.so.0", "libxkbcommon0"),
    ("libxkbcommon-x11.so.0", "libxkbcommon-x11-0"),
    ("libwayland-cursor.so.0", "libwayland-cursor0"),
];

/// The AppImage project's excludelist
/// (<https://github.com/AppImageCommunity/pkg2appimage/blob/master/excludelist>):
/// libraries an AppImage must take from the host, never bundle, because
/// they belong to the C library, the graphics drivers or the session.
pub const EXCLUDELIST: &[&str] = &[
    "ld-linux.so.2",
    "ld-linux-x86-64.so.2",
    "libanl.so.1",
    "libBrokenLocale.so.1",
    "libcidn.so.1",
    "libc.so.6",
    "libdl.so.2",
    "libm.so.6",
    "libmvec.so.1",
    "libnss_compat.so.2",
    "libnss_dns.so.2",
    "libnss_files.so.2",
    "libnss_hesiod.so.2",
    "libnss_nisplus.so.2",
    "libnss_nis.so.2",
    "libpthread.so.0",
    "libresolv.so.2",
    "librt.so.1",
    "libthread_db.so.1",
    "libutil.so.1",
    "libstdc++.so.6",
    "libGL.so.1",
    "libEGL.so.1",
    "libGLdispatch.so.0",
    "libGLX.so.0",
    "libOpenGL.so.0",
    "libdrm.so.2",
    "libglapi.so.0",
    "libgbm.so.1",
    "libxcb.so.1",
    "libX11.so.6",
    "libX11-xcb.so.1",
    "libwayland-client.so.0",
    "libasound.so.2",
    "libfontconfig.so.1",
    "libfreetype.so.6",
    "libharfbuzz.so.0",
    "libcom_err.so.2",
    "libexpat.so.1",
    "libgcc_s.so.1",
    "libgpg-error.so.0",
    "libICE.so.6",
    "libSM.so.6",
    "libusb-1.0.so.0",
    "libuuid.so.1",
    "libz.so.1",
    "libjack.so.0",
    "libpipewire-0.3.so.0",
    "libxcb-dri3.so.0",
    "libxcb-dri2.so.0",
    "libfribidi.so.0",
    "libgmp.so.10",
];

/// A Debian package name from a Cargo package name: lower case, `_` as
/// `-`, only `[a-z0-9+.-]`, starting with a letter or digit.
pub fn deb_package_name(name: &str) -> String {
    let mapped: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '+' | '.' | '-' => c,
            _ => '-',
        })
        .collect();
    let trimmed = mapped.trim_start_matches(['+', '.', '-']).to_string();
    if trimmed.len() < 2 {
        format!("app-{trimmed}")
    } else {
        trimmed
    }
}

/// Whether `name` is a valid Debian package name.
pub fn is_deb_package_name(name: &str) -> bool {
    name.len() >= 2
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '.' | '-'))
}

/// The `Depends:` dpkg-shlibdeps found (`shlibs:Depends=...` on its
/// stdout).
pub fn shlibs_depends(output: &str) -> Vec<String> {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("shlibs:Depends="))
        .map(|list| {
            list.split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn merge(base: &[String], extra: &[String]) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    for item in base.iter().chain(extra) {
        if !all.contains(item) {
            all.push(item.clone());
        }
    }
    all
}

/// `DEBIAN/control`.
pub fn control(
    facts: &Facts,
    depends: &[String],
    recommends: &[String],
    installed_kib: u64,
) -> String {
    let recommends = merge(
        &RECOMMENDS
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        recommends,
    );
    let synopsis = facts
        .description
        .as_deref()
        .and_then(|d| d.lines().next())
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(&facts.name)
        .to_string();
    let mut text = format!(
        "Package: {}\nVersion: {}\nArchitecture: {}\nMaintainer: {}\nInstalled-Size: {installed_kib}\n",
        facts.package, facts.deb_version, facts.arch, facts.maintainer
    );
    if !depends.is_empty() {
        text.push_str(&format!("Depends: {}\n", depends.join(", ")));
    }
    text.push_str(&format!("Recommends: {}\n", recommends.join(", ")));
    text.push_str(&format!(
        "Section: {}\nPriority: optional\n",
        crate::release::desktop::deb_section(facts.category.as_deref())
    ));
    if let Some(homepage) = &facts.homepage {
        text.push_str(&format!("Homepage: {homepage}\n"));
    }
    text.push_str(&format!("Description: {synopsis}\n"));
    text.push_str(&format!(
        " {} is an iced application. The licences of the components it ships\n are in /usr/share/doc/{}/THIRD_PARTY_NOTICES.txt.\n",
        facts.name, facts.package
    ));
    text
}

/// The `.desktop` entry (`Exec` is the executable's name, found on `PATH`
/// for the `.deb` and through `AppRun` in the AppImage).
pub fn desktop_entry(facts: &Facts) -> String {
    let escape = |text: &str| text.replace('\\', "\\\\").replace('\n', " ");
    let mut text = format!(
        "[Desktop Entry]\nType=Application\nName={}\n",
        escape(&facts.name)
    );
    if let Some(description) = &facts.description {
        text.push_str(&format!(
            "Comment={}\n",
            escape(description.lines().next().unwrap_or(""))
        ));
    }
    text.push_str(&format!(
        "Exec={}\nIcon={}\nTerminal=false\nCategories={}\n",
        facts.bin,
        facts.id,
        crate::release::desktop::linux_categories(facts.category.as_deref())
    ));
    text
}

/// `AppRun`: the AppImage's entry point, with the bundled libraries first.
pub fn app_run(bin: &str) -> String {
    format!(
        "#!/bin/sh\n# Generated by icm: runs the app with the libraries bundled in usr/lib.\nHERE=\"$(dirname \"$(readlink -f \"$0\")\")\"\nexport LD_LIBRARY_PATH=\"$HERE/usr/lib${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}\"\nexec \"$HERE/usr/bin/{bin}\" \"$@\"\n"
    )
}

/// `usr/share/doc/<package>/copyright`.
pub fn copyright(facts: &Facts, version: &str) -> String {
    format!(
        "{} {version}\nCopyright: {}\n\nThe components this package ships and their licences are listed in\nTHIRD_PARTY_NOTICES.txt in this directory.\n",
        facts.name, facts.copyright
    )
}

/// The registered main categories of the Desktop Menu Specification.
const MAIN_CATEGORIES: &[&str] = &[
    "AudioVideo",
    "Audio",
    "Video",
    "Development",
    "Education",
    "Game",
    "Graphics",
    "Network",
    "Office",
    "Science",
    "Settings",
    "System",
    "Utility",
];

/// The problems of a `.desktop` file (empty: valid). A subset of
/// `desktop-file-validate`, which icm runs as well when it is installed.
pub fn desktop_problems(text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    if lines.next() != Some("[Desktop Entry]") {
        problems.push("the first group is not [Desktop Entry]".to_string());
    }
    let mut keys: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.starts_with('[') {
            break;
        }
        match line.split_once('=') {
            Some((key, value)) => {
                let key = key.trim();
                if !key.chars().all(|c| {
                    c.is_ascii_alphanumeric() || matches!(c, '-' | '[' | ']' | '@' | '_' | '.')
                }) {
                    problems.push(format!("the key `{key}` has invalid characters"));
                }
                if keys.iter().any(|(k, _)| k == key) {
                    problems.push(format!("the key `{key}` appears twice"));
                }
                keys.push((key.to_string(), value.trim().to_string()));
            }
            None => problems.push(format!("`{line}` is not a key=value line")),
        }
    }
    let get = |key: &str| keys.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    match get("Type") {
        Some("Application") => {}
        Some(other) => problems.push(format!("Type is {other}, not Application")),
        None => problems.push("Type is missing".to_string()),
    }
    for key in ["Name", "Exec", "Icon"] {
        if get(key).is_none_or(str::is_empty) {
            problems.push(format!("{key} is missing or empty"));
        }
    }
    match get("Categories") {
        Some(categories) => {
            if !categories.ends_with(';') {
                problems.push("Categories does not end with `;`".to_string());
            }
            if !categories
                .split(';')
                .any(|category| MAIN_CATEGORIES.contains(&category))
            {
                problems.push(format!(
                    "Categories `{categories}` has no registered main category"
                ));
            }
        }
        None => problems.push("Categories is missing".to_string()),
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            name: "Notes".into(),
            id: "com.acme.notes".into(),
            package: "notes".into(),
            deb_version: "1.2.3-45".into(),
            arch: "amd64".into(),
            maintainer: "Acme Ltd <dev@acme.example>".into(),
            description: Some("Takes notes.".into()),
            category: Some("productivity".into()),
            copyright: "© 2026 Acme".into(),
            homepage: Some("https://acme.example".into()),
            bin: "notes".into(),
        }
    }

    #[test]
    fn package_names_are_debian_names() {
        assert_eq!(deb_package_name("My_App"), "my-app");
        assert_eq!(deb_package_name("release-app"), "release-app");
        assert_eq!(deb_package_name("x"), "app-x");
        assert_eq!(deb_package_name("_x1"), "x1");
        assert!(is_deb_package_name("notes"));
        assert!(is_deb_package_name("lib2.0+x-y"));
        assert!(!is_deb_package_name("Notes"));
        assert!(!is_deb_package_name("-notes"));
        assert!(!is_deb_package_name("n"));
    }

    #[test]
    fn the_control_file_has_the_fields_dpkg_needs() {
        let depends = shlibs_depends(
            "dpkg-shlibdeps: warning: something\nshlibs:Depends=libc6 (>= 2.34), libgcc-s1 (>= 4.2)\n",
        );
        assert_eq!(depends, ["libc6 (>= 2.34)", "libgcc-s1 (>= 4.2)"]);
        let control = control(
            &facts(),
            &merge(&depends, &["libssl3".to_string()]),
            &["fonts-noto".to_string(), "libxkbcommon0".to_string()],
            1234,
        );
        assert!(control.starts_with("Package: notes\nVersion: 1.2.3-45\nArchitecture: amd64\nMaintainer: Acme Ltd <dev@acme.example>\nInstalled-Size: 1234\n"));
        assert!(control.contains("Depends: libc6 (>= 2.34), libgcc-s1 (>= 4.2), libssl3\n"));
        assert!(control.contains(
            "Recommends: libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libvulkan1, libegl1, fonts-noto\n"
        ));
        assert!(control.contains("Section: misc\n"), "{control}");
        assert!(control.contains("Homepage: https://acme.example\n"));
        assert!(control.contains("Description: Takes notes.\n Notes is an iced application."));
        assert!(control.ends_with(".txt.\n"));
        assert!(shlibs_depends("nothing").is_empty());
    }

    #[test]
    fn the_desktop_entry_validates() {
        let entry = desktop_entry(&facts());
        assert!(entry.contains("Exec=notes\nIcon=com.acme.notes\n"));
        assert!(entry.contains("Categories=Office;\n"));
        assert!(
            desktop_problems(&entry).is_empty(),
            "{:?}",
            desktop_problems(&entry)
        );
        let broken = "[Desktop Entry]\nType=Link\nName=\nCategories=Gadgets\nName=x\n";
        let problems = desktop_problems(broken);
        assert!(problems.iter().any(|p| p.contains("Type is Link")));
        assert!(problems.iter().any(|p| p.contains("Exec is missing")));
        assert!(problems.iter().any(|p| p.contains("does not end with")));
        assert!(
            problems
                .iter()
                .any(|p| p.contains("no registered main category"))
        );
        assert!(problems.iter().any(|p| p.contains("appears twice")));
        assert!(!desktop_problems("Type=Application\n").is_empty());
    }

    #[test]
    fn the_appimage_bundles_nothing_the_excludelist_names() {
        for (lib, _) in BUNDLED {
            assert!(
                !EXCLUDELIST.contains(lib),
                "{lib} is on the AppImage excludelist"
            );
        }
        assert!(EXCLUDELIST.contains(&"libwayland-client.so.0"));
        // The libraries the .deb only recommends stay the host's.
        assert!(EXCLUDELIST.contains(&"libEGL.so.1"));
    }

    #[test]
    fn app_run_puts_bundled_libraries_first() {
        let script = app_run("notes");
        assert!(script.starts_with("#!/bin/sh\n"));
        assert!(
            script
                .contains("LD_LIBRARY_PATH=\"$HERE/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}\"")
        );
        assert!(script.ends_with("exec \"$HERE/usr/bin/notes\" \"$@\"\n"));
        assert!(copyright(&facts(), "1.2.3").contains("THIRD_PARTY_NOTICES.txt"));
    }
}
