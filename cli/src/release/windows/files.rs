//! The Windows installer sources (design §9.6): `app.rc` (the icon and
//! VERSIONINFO), `app.wxs` (WiX v5, per-machine MSI) and `installer.nsi`
//! (NSIS, per-user), plus the version rules and the signing script.

use std::path::{Path, PathBuf};

/// What the installer sources are made of.
#[derive(Clone, Debug)]
pub struct Facts {
    /// `[app] name`.
    pub name: String,
    /// `[app] id`.
    pub id: String,
    /// `[app] publisher`, else the name.
    pub publisher: String,
    /// `[app] copyright`.
    pub copyright: Option<String>,
    /// `[app] description`.
    pub description: Option<String>,
    /// `X.Y.Z`.
    pub version: (u32, u32, u32),
    /// `[app] build`.
    pub build: u64,
    /// The executable's name without `.exe`.
    pub bin: String,
    /// The built (and signed) executable.
    pub exe: PathBuf,
    /// `app.ico`.
    pub icon: PathBuf,
    /// THIRD_PARTY_NOTICES.txt.
    pub notices: PathBuf,
    /// `[app] resources`: (path relative to the install directory, file).
    pub resources: Vec<(PathBuf, PathBuf)>,
}

impl Facts {
    /// `X.Y.Z`.
    pub fn version_string(&self) -> String {
        let (x, y, z) = self.version;
        format!("{x}.{y}.{z}")
    }

    /// `X.Y.Z.build`.
    pub fn file_version(&self) -> String {
        format!("{}.{}", self.version_string(), self.build)
    }
}

/// `windows.msi_version`: the Cargo version as an MSI ProductVersion
/// (major and minor at most 255, patch at most 65535; Windows Installer
/// ignores a fourth field) and `[app] build` within VERSIONINFO's 16 bits.
pub fn msi_version(version: &str, build: u64) -> Result<(u32, u32, u32), String> {
    let parts: Vec<&str> = version
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .split('.')
        .collect();
    let number = |part: Option<&&str>| part.and_then(|p| p.parse::<u32>().ok());
    let (Some(x), Some(y), Some(z)) = (
        number(parts.first()),
        number(parts.get(1)),
        number(parts.get(2)),
    ) else {
        return Err(format!("version {version} is not X.Y.Z"));
    };
    let mut problems = Vec::new();
    if x > 255 {
        problems.push(format!("major {x} is above 255"));
    }
    if y > 255 {
        problems.push(format!("minor {y} is above 255"));
    }
    if z > 65535 {
        problems.push(format!("patch {z} is above 65535"));
    }
    if build > 65535 {
        problems.push(format!(
            "[app] build {build} is above 65535, the largest VERSIONINFO field"
        ));
    }
    if problems.is_empty() {
        Ok((x, y, z))
    } else {
        Err(format!(
            "version {version} (build {build}) does not fit Windows' version fields: {}",
            problems.join("; ")
        ))
    }
}

/// The MSI UpgradeCode: a name-based GUID from the SHA-256 of `[app] id`,
/// so every release of the app upgrades the previous one.
pub fn upgrade_code(id: &str) -> String {
    let digest = crate::hash::sha256_hex(format!("icm.windows.upgrade-code:{id}").as_bytes());
    let mut bytes: Vec<u8> = (0..16)
        .map(|i| u8::from_str_radix(&digest[i * 2..i * 2 + 2], 16).unwrap_or(0))
        .collect();
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn rc_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\"\"")
}

/// `app.rc`: the icon and VERSIONINFO, in UTF-8 (no `#include`, so rc.exe
/// needs no include path). `icon` is the ICO's file name next to it.
pub fn rc(facts: &Facts, icon: &str) -> String {
    let (x, y, z) = facts.version;
    let build = facts.build;
    let mut values = vec![
        ("CompanyName", facts.publisher.clone()),
        (
            "FileDescription",
            facts
                .description
                .clone()
                .unwrap_or_else(|| facts.name.clone()),
        ),
        ("FileVersion", facts.file_version()),
        ("InternalName", facts.bin.clone()),
        ("OriginalFilename", format!("{}.exe", facts.bin)),
        ("ProductName", facts.name.clone()),
        ("ProductVersion", facts.version_string()),
    ];
    if let Some(copyright) = &facts.copyright {
        values.push(("LegalCopyright", copyright.clone()));
    }
    let mut text = String::from("#pragma code_page(65001)\n\n");
    text.push_str(&format!("1 ICON \"{}\"\n\n", rc_string(icon)));
    text.push_str(&format!(
        "1 VERSIONINFO\nFILEVERSION {x},{y},{z},{build}\nPRODUCTVERSION {x},{y},{z},0\nFILEFLAGSMASK 0x3fL\nFILEFLAGS 0x0L\nFILEOS 0x40004L\nFILETYPE 0x1L\nFILESUBTYPE 0x0L\nBEGIN\n    BLOCK \"StringFileInfo\"\n    BEGIN\n        BLOCK \"040904b0\"\n        BEGIN\n"
    ));
    for (key, value) in values {
        text.push_str(&format!(
            "            VALUE \"{key}\", \"{}\"\n",
            rc_string(&value)
        ));
    }
    text.push_str(
        "        END\n    END\n    BLOCK \"VarFileInfo\"\n    BEGIN\n        VALUE \"Translation\", 0x409, 1200\n    END\nEND\n",
    );
    text
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

/// The directories and files of `[app] resources`, as a tree.
#[derive(Default)]
struct Tree {
    files: Vec<(String, PathBuf)>,
    dirs: std::collections::BTreeMap<String, Tree>,
}

fn tree(resources: &[(PathBuf, PathBuf)]) -> Tree {
    let mut root = Tree::default();
    for (relative, source) in resources {
        let parts: Vec<String> = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        let Some((file, dirs)) = parts.split_last() else {
            continue;
        };
        let mut node = &mut root;
        for dir in dirs {
            node = node.dirs.entry(dir.clone()).or_default();
        }
        node.files.push((file.clone(), source.clone()));
    }
    root
}

/// `app.wxs` (WiX v5): a per-machine MSI that installs the executable, the
/// notices and `[app] resources` into `Program Files\<Name>`, adds a
/// Start-menu shortcut, and upgrades any earlier version (same version
/// included).
pub fn wxs(facts: &Facts) -> String {
    let name = xml(&facts.name);
    let publisher = xml(&facts.publisher);
    let mut components: Vec<String> = vec!["MainExecutable".into(), "Notices".into()];
    let mut text = String::new();
    text.push_str("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    text.push_str("<!-- Generated by icm from icm.toml; edit icm.toml, not this file. -->\n");
    text.push_str("<Wix xmlns=\"http://wixtoolset.org/schemas/v4/wxs\">\n");
    text.push_str(&format!(
        "  <Package Name=\"{name}\" Manufacturer=\"{publisher}\" Version=\"{}\" UpgradeCode=\"{}\" Scope=\"perMachine\" Language=\"1033\" Compressed=\"yes\">\n",
        facts.version_string(),
        upgrade_code(&facts.id)
    ));
    text.push_str(&format!(
        "    <SummaryInformation Description=\"{name} {}\" Manufacturer=\"{publisher}\" />\n",
        facts.version_string()
    ));
    text.push_str("    <MajorUpgrade AllowSameVersionUpgrades=\"yes\" DowngradeErrorMessage=\"A newer version of [ProductName] is already installed.\" />\n");
    text.push_str("    <MediaTemplate EmbedCab=\"yes\" />\n");
    text.push_str(&format!(
        "    <Icon Id=\"AppIcon.ico\" SourceFile=\"{}\" />\n",
        xml(&path_text(&facts.icon))
    ));
    text.push_str("    <Property Id=\"ARPPRODUCTICON\" Value=\"AppIcon.ico\" />\n");
    text.push_str("    <StandardDirectory Id=\"ProgramFiles64Folder\">\n");
    text.push_str(&format!(
        "      <Directory Id=\"INSTALLFOLDER\" Name=\"{name}\">\n"
    ));
    text.push_str(&format!(
        "        <Component Id=\"MainExecutable\">\n          <File Id=\"MainExe\" Source=\"{}\" Name=\"{}.exe\" KeyPath=\"yes\" />\n        </Component>\n",
        xml(&path_text(&facts.exe)),
        xml(&facts.bin)
    ));
    text.push_str(&format!(
        "        <Component Id=\"Notices\">\n          <File Id=\"Notices\" Source=\"{}\" Name=\"{}\" KeyPath=\"yes\" />\n        </Component>\n",
        xml(&path_text(&facts.notices)),
        crate::release::notices::FILE
    ));
    let mut counter = 0usize;
    fn walk(
        node: &Tree,
        depth: usize,
        counter: &mut usize,
        components: &mut Vec<String>,
        text: &mut String,
    ) {
        let indent = "  ".repeat(depth);
        for (file, source) in &node.files {
            *counter += 1;
            let id = format!("Resource{counter}");
            text.push_str(&format!(
                "{indent}<Component Id=\"{id}\">\n{indent}  <File Id=\"{id}\" Source=\"{}\" Name=\"{}\" KeyPath=\"yes\" />\n{indent}</Component>\n",
                xml(&path_text(source)),
                xml(file)
            ));
            components.push(id);
        }
        for (dir, child) in &node.dirs {
            *counter += 1;
            text.push_str(&format!(
                "{indent}<Directory Id=\"ResourceDir{counter}\" Name=\"{}\">\n",
                xml(dir)
            ));
            walk(child, depth + 1, counter, components, text);
            text.push_str(&format!("{indent}</Directory>\n"));
        }
    }
    walk(
        &tree(&facts.resources),
        4,
        &mut counter,
        &mut components,
        &mut text,
    );
    text.push_str("      </Directory>\n    </StandardDirectory>\n");
    text.push_str("    <StandardDirectory Id=\"ProgramMenuFolder\">\n");
    text.push_str(&format!(
        "      <Component Id=\"StartMenuShortcut\">\n        <Shortcut Id=\"AppShortcut\" Name=\"{name}\" Target=\"[INSTALLFOLDER]{}.exe\" WorkingDirectory=\"INSTALLFOLDER\" Icon=\"AppIcon.ico\" />\n        <RegistryValue Root=\"HKMU\" Key=\"Software\\{publisher}\\{name}\" Name=\"installed\" Type=\"integer\" Value=\"1\" KeyPath=\"yes\" />\n      </Component>\n",
        xml(&facts.bin)
    ));
    text.push_str("    </StandardDirectory>\n");
    components.push("StartMenuShortcut".into());
    text.push_str(&format!("    <Feature Id=\"Main\" Title=\"{name}\">\n"));
    for component in components {
        text.push_str(&format!("      <ComponentRef Id=\"{component}\" />\n"));
    }
    text.push_str("    </Feature>\n  </Package>\n</Wix>\n");
    text
}

/// A string inside an NSIS script's double quotes.
fn nsis(text: &str) -> String {
    text.replace('$', "$$")
        .replace('"', "$\\\"")
        .replace('\n', "$\\n")
}

fn windows_path(relative: &Path) -> String {
    relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\\")
}

/// `installer.nsi`: a per-user installer into
/// `%LOCALAPPDATA%\Programs\<Name>` with a Start-menu shortcut, an entry
/// in Apps & features and an uninstaller. `out` is the setup `.exe`.
pub fn nsi(facts: &Facts, out: &Path) -> String {
    let name = nsis(&facts.name);
    let exe = format!("{}.exe", nsis(&facts.bin));
    let uninst = format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        nsis(&facts.id)
    );
    let mut text = String::new();
    text.push_str("; Generated by icm from icm.toml; edit icm.toml, not this file.\n");
    text.push_str("Unicode true\nManifestDPIAware true\nSetCompressor /SOLID lzma\nRequestExecutionLevel user\n\n");
    text.push_str(&format!("Name \"{name}\"\n"));
    text.push_str(&format!("OutFile \"{}\"\n", nsis(&path_text(out))));
    text.push_str(&format!("InstallDir \"$LOCALAPPDATA\\Programs\\{name}\"\n"));
    text.push_str(&format!(
        "InstallDirRegKey HKCU \"{uninst}\" \"InstallLocation\"\n"
    ));
    text.push_str(&format!(
        "Icon \"{0}\"\nUninstallIcon \"{0}\"\n",
        nsis(&path_text(&facts.icon))
    ));
    text.push_str(&format!("VIProductVersion \"{}\"\n", facts.file_version()));
    let mut keys = vec![
        ("ProductName", facts.name.clone()),
        ("CompanyName", facts.publisher.clone()),
        ("FileDescription", format!("{} installer", facts.name)),
        ("FileVersion", facts.file_version()),
        ("ProductVersion", facts.version_string()),
    ];
    if let Some(copyright) = &facts.copyright {
        keys.push(("LegalCopyright", copyright.clone()));
    }
    for (key, value) in keys {
        text.push_str(&format!("VIAddVersionKey \"{key}\" \"{}\"\n", nsis(&value)));
    }
    text.push_str(
        "\nPage directory\nPage instfiles\nUninstPage uninstConfirm\nUninstPage instfiles\n\n",
    );

    text.push_str("Section \"Install\"\n  SetOutPath \"$INSTDIR\"\n");
    text.push_str(&format!(
        "  File \"/oname={exe}\" \"{}\"\n",
        nsis(&path_text(&facts.exe))
    ));
    text.push_str(&format!(
        "  File \"/oname={}\" \"{}\"\n",
        crate::release::notices::FILE,
        nsis(&path_text(&facts.notices))
    ));
    let mut dirs: Vec<String> = Vec::new();
    for (relative, source) in &facts.resources {
        let parent = relative.parent().map(windows_path).unwrap_or_default();
        let file = relative
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let out_dir = if parent.is_empty() {
            "$INSTDIR".to_string()
        } else {
            format!("$INSTDIR\\{}", nsis(&parent))
        };
        text.push_str(&format!("  SetOutPath \"{out_dir}\"\n"));
        text.push_str(&format!(
            "  File \"/oname={}\" \"{}\"\n",
            nsis(&file),
            nsis(&path_text(source))
        ));
        let mut dir = relative.parent();
        while let Some(current) = dir.filter(|d| !d.as_os_str().is_empty()) {
            let shown = windows_path(current);
            if !dirs.contains(&shown) {
                dirs.push(shown);
            }
            dir = current.parent();
        }
    }
    text.push_str("  SetOutPath \"$INSTDIR\"\n");
    text.push_str("  WriteUninstaller \"$INSTDIR\\uninstall.exe\"\n");
    text.push_str(&format!(
        "  CreateShortcut \"$SMPROGRAMS\\{name}.lnk\" \"$INSTDIR\\{exe}\"\n"
    ));
    let registry = [
        ("DisplayName", facts.name.clone()),
        ("DisplayVersion", facts.version_string()),
        ("Publisher", facts.publisher.clone()),
    ];
    for (key, value) in registry {
        text.push_str(&format!(
            "  WriteRegStr HKCU \"{uninst}\" \"{key}\" \"{}\"\n",
            nsis(&value)
        ));
    }
    text.push_str(&format!(
        "  WriteRegStr HKCU \"{uninst}\" \"DisplayIcon\" \"$INSTDIR\\{exe}\"\n  WriteRegStr HKCU \"{uninst}\" \"InstallLocation\" \"$INSTDIR\"\n  WriteRegStr HKCU \"{uninst}\" \"UninstallString\" \"$\\\"$INSTDIR\\uninstall.exe$\\\"\"\n  WriteRegStr HKCU \"{uninst}\" \"QuietUninstallString\" \"$\\\"$INSTDIR\\uninstall.exe$\\\" /S\"\n  WriteRegDWORD HKCU \"{uninst}\" \"NoModify\" 1\n  WriteRegDWORD HKCU \"{uninst}\" \"NoRepair\" 1\nSectionEnd\n\n"
    ));

    text.push_str("Section \"Uninstall\"\n");
    text.push_str(&format!("  Delete \"$INSTDIR\\{exe}\"\n"));
    text.push_str(&format!(
        "  Delete \"$INSTDIR\\{}\"\n",
        crate::release::notices::FILE
    ));
    for (relative, _) in &facts.resources {
        text.push_str(&format!(
            "  Delete \"$INSTDIR\\{}\"\n",
            nsis(&windows_path(relative))
        ));
    }
    dirs.sort_by_key(|dir| std::cmp::Reverse(dir.matches('\\').count()));
    for dir in dirs {
        text.push_str(&format!("  RMDir \"$INSTDIR\\{}\"\n", nsis(&dir)));
    }
    text.push_str("  Delete \"$INSTDIR\\uninstall.exe\"\n  RMDir \"$INSTDIR\"\n");
    text.push_str(&format!("  Delete \"$SMPROGRAMS\\{name}.lnk\"\n"));
    text.push_str(&format!("  DeleteRegKey HKCU \"{uninst}\"\nSectionEnd\n"));
    text
}

/// The shell script that signs `file` with `[desktop.windows]
/// sign_command`: `{file}` becomes the quoted path and `%VAR%` becomes
/// `${VAR}`, so `sh -c` expands the variables (the secrets stay out of
/// icm's argv and logs); `$VAR`, `${VAR}` and `env:VAR` pass unchanged.
pub fn sign_script(command: &str, file: &Path) -> String {
    let quoted = crate::process::shell_quote(&file.display().to_string());
    let mut out = String::new();
    let mut rest = command;
    while let Some(start) = rest.find('%') {
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end)
                if end > 0
                    && after[..end]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_') =>
            {
                out.push_str(&rest[..start]);
                out.push_str(&format!("${{{}}}", &after[..end]));
                rest = &after[end + 1..];
            }
            _ => {
                out.push_str(&rest[..=start]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("{file}", &quoted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            name: "Notes & Co".into(),
            id: "com.acme.notes".into(),
            publisher: "Acme \"Ltd\"".into(),
            copyright: Some("© 2026 Acme".into()),
            description: Some("Takes notes".into()),
            version: (1, 2, 3),
            build: 45,
            bin: "notes".into(),
            exe: PathBuf::from("C:/work/notes.exe"),
            icon: PathBuf::from("C:/work/app.ico"),
            notices: PathBuf::from("C:/work/THIRD_PARTY_NOTICES.txt"),
            resources: vec![
                (
                    PathBuf::from("assets/fonts/a.ttf"),
                    PathBuf::from("C:/p/assets/fonts/a.ttf"),
                ),
                (
                    PathBuf::from("readme.txt"),
                    PathBuf::from("C:/p/readme.txt"),
                ),
            ],
        }
    }

    #[test]
    fn versions_must_fit_windows_fields() {
        assert_eq!(msi_version("1.2.3", 45), Ok((1, 2, 3)));
        assert_eq!(msi_version("255.255.65535", 65535), Ok((255, 255, 65535)));
        let error = msi_version("256.0.70000", 1).unwrap_err();
        assert!(error.contains("major 256"), "{error}");
        assert!(error.contains("patch 70000"), "{error}");
        assert!(
            msi_version("1.0.0", 70000)
                .unwrap_err()
                .contains("[app] build 70000")
        );
        assert!(msi_version("1.0", 1).is_err());
    }

    #[test]
    fn the_upgrade_code_is_stable_per_id() {
        let code = upgrade_code("com.acme.notes");
        assert_eq!(code, upgrade_code("com.acme.notes"));
        assert_ne!(code, upgrade_code("com.acme.other"));
        assert_eq!(code.len(), 36);
        assert_eq!(&code[14..15], "5", "a name-based (version 5) GUID: {code}");
        assert!(matches!(&code[19..20], "8" | "9" | "A" | "B"), "{code}");
    }

    #[test]
    fn the_rc_has_the_icon_and_versions() {
        let rc = rc(&facts(), "app.ico");
        assert!(rc.starts_with("#pragma code_page(65001)"));
        assert!(rc.contains("1 ICON \"app.ico\""));
        assert!(rc.contains("FILEVERSION 1,2,3,45"));
        assert!(rc.contains("PRODUCTVERSION 1,2,3,0"));
        assert!(
            rc.contains("VALUE \"CompanyName\", \"Acme \"\"Ltd\"\"\""),
            "{rc}"
        );
        assert!(rc.contains("VALUE \"FileVersion\", \"1.2.3.45\""));
        assert!(rc.contains("VALUE \"LegalCopyright\", \"© 2026 Acme\""));
        assert!(rc.contains("VALUE \"OriginalFilename\", \"notes.exe\""));
    }

    #[test]
    fn the_wxs_installs_per_machine_with_a_shortcut() {
        let wxs = wxs(&facts());
        assert!(wxs.contains("xmlns=\"http://wixtoolset.org/schemas/v4/wxs\""));
        assert!(wxs.contains("Name=\"Notes &amp; Co\""), "{wxs}");
        assert!(wxs.contains("Manufacturer=\"Acme &quot;Ltd&quot;\""));
        assert!(wxs.contains("Version=\"1.2.3\""));
        assert!(wxs.contains(&format!(
            "UpgradeCode=\"{}\"",
            upgrade_code("com.acme.notes")
        )));
        assert!(wxs.contains("Scope=\"perMachine\""));
        assert!(wxs.contains("<MajorUpgrade AllowSameVersionUpgrades=\"yes\""));
        assert!(wxs.contains("<StandardDirectory Id=\"ProgramFiles64Folder\">"));
        assert!(wxs.contains("Source=\"C:/work/notes.exe\" Name=\"notes.exe\""));
        assert!(wxs.contains("Name=\"THIRD_PARTY_NOTICES.txt\""));
        assert!(
            wxs.contains("<Directory Id=\"ResourceDir2\" Name=\"assets\">"),
            "{wxs}"
        );
        assert!(wxs.contains("Name=\"a.ttf\""));
        assert!(wxs.contains("Target=\"[INSTALLFOLDER]notes.exe\""));
        for component in [
            "MainExecutable",
            "Notices",
            "Resource1",
            "StartMenuShortcut",
        ] {
            assert!(
                wxs.contains(&format!("<ComponentRef Id=\"{component}\" />")),
                "{component}"
            );
        }
        // Every component is referenced.
        let defined = wxs.matches("<Component Id=").count();
        let referenced = wxs.matches("<ComponentRef Id=").count();
        assert_eq!(defined, referenced, "{wxs}");
    }

    #[test]
    fn the_nsi_installs_per_user_and_uninstalls_everything() {
        let nsi = nsi(&facts(), Path::new("C:/dist/Notes-1.2.3-setup.exe"));
        assert!(nsi.contains("RequestExecutionLevel user"));
        assert!(nsi.contains("OutFile \"C:/dist/Notes-1.2.3-setup.exe\""));
        assert!(nsi.contains("InstallDir \"$LOCALAPPDATA\\Programs\\Notes & Co\""));
        assert!(nsi.contains("VIProductVersion \"1.2.3.45\""));
        assert!(
            nsi.contains("VIAddVersionKey \"CompanyName\" \"Acme $\\\"Ltd$\\\"\""),
            "{nsi}"
        );
        assert!(nsi.contains("File \"/oname=notes.exe\" \"C:/work/notes.exe\""));
        assert!(nsi.contains("SetOutPath \"$INSTDIR\\assets\\fonts\""));
        assert!(nsi.contains("Delete \"$INSTDIR\\assets\\fonts\\a.ttf\""));
        let fonts = nsi.find("RMDir \"$INSTDIR\\assets\\fonts\"").unwrap();
        let assets = nsi.find("RMDir \"$INSTDIR\\assets\"").unwrap();
        assert!(fonts < assets, "deepest directories go first");
        assert!(nsi.contains("WriteRegStr HKCU \"Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\com.acme.notes\" \"DisplayVersion\" \"1.2.3\""));
        assert!(nsi.contains("WriteUninstaller \"$INSTDIR\\uninstall.exe\""));
        assert!(nsi.contains("CreateShortcut \"$SMPROGRAMS\\Notes & Co.lnk\""));
        // A dollar in a value is escaped.
        let mut dollar = facts();
        dollar.name = "Price$".into();
        assert!(super::nsi(&dollar, Path::new("o.exe")).contains("Name \"Price$$\""));
    }

    #[test]
    fn sign_scripts_keep_secrets_as_variables() {
        let file = Path::new("C:/dist/My App.msi");
        assert_eq!(
            sign_script(
                "jsign --storetype TRUSTEDSIGNING --storepass %AZURE_TOKEN% --keystore $ENDPOINT {file}",
                file
            ),
            "jsign --storetype TRUSTEDSIGNING --storepass ${AZURE_TOKEN} --keystore $ENDPOINT 'C:/dist/My App.msi'"
        );
        assert_eq!(
            sign_script("signtool sign /fd sha256 /a 50% {file}", Path::new("a.exe")),
            "signtool sign /fd sha256 /a 50% a.exe"
        );
    }
}
