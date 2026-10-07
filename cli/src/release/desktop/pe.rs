//! What the Windows gates read from a PE executable (design Appendix C
//! item 20): the machine, the subsystem, and the DLLs it imports (the
//! import and delay-import directories). Rust's `x86_64-pc-windows-msvc`
//! links the VC++ runtime (`vcruntime140.dll`) dynamically unless built
//! with `+crt-static`, and a clean Windows does not have it; a release
//! machine with Visual Studio does, so only the imports tell.
//!
//! A few dozen lines over the headers, bounds-checked: any file can be
//! handed to `icm verify windows`.

use std::path::Path;

/// `IMAGE_FILE_MACHINE_AMD64`.
pub const MACHINE_AMD64: u16 = 0x8664;
/// `IMAGE_FILE_MACHINE_ARM64`.
pub const MACHINE_ARM64: u16 = 0xaa64;
/// `IMAGE_FILE_MACHINE_I386`.
pub const MACHINE_I386: u16 = 0x014c;
/// `IMAGE_SUBSYSTEM_WINDOWS_GUI`: no console window.
pub const SUBSYSTEM_GUI: u16 = 2;
/// `IMAGE_SUBSYSTEM_WINDOWS_CUI`: a console window opens with the app.
pub const SUBSYSTEM_CONSOLE: u16 = 3;

/// A PE image's facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pe {
    /// `IMAGE_FILE_HEADER.Machine`.
    pub machine: u16,
    /// `IMAGE_OPTIONAL_HEADER.Subsystem`.
    pub subsystem: u16,
    /// The DLLs in the import directory, as written.
    pub imports: Vec<String>,
    /// The DLLs in the delay-import directory.
    pub delay_imports: Vec<String>,
}

impl Pe {
    /// Every imported DLL, lower case.
    pub fn dlls(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .imports
            .iter()
            .chain(&self.delay_imports)
            .map(|dll| dll.to_ascii_lowercase())
            .collect();
        all.sort();
        all.dedup();
        all
    }

    /// The imported DLLs a clean Windows 10 or later does not have: the
    /// VC++ redistributable (`vcruntime*`, `msvcp*`, `concrt*`,
    /// `vccorlib*`, the old `msvcr*`) and MinGW's runtimes. The Universal
    /// CRT (`ucrtbase.dll`, `api-ms-win-crt-*`) is part of Windows 10.
    pub fn runtime_dlls(&self) -> Vec<String> {
        self.dlls()
            .into_iter()
            .filter(|dll| is_redistributable(dll))
            .collect()
    }

    /// Whether it imports the Universal CRT (fine on Windows 10 and later).
    pub fn uses_ucrt(&self) -> bool {
        self.dlls()
            .iter()
            .any(|dll| dll == "ucrtbase.dll" || dll.starts_with("api-ms-win-crt-"))
    }
}

/// Whether a (lower-case) DLL name belongs to a runtime that must be
/// redistributed with the app.
pub fn is_redistributable(dll: &str) -> bool {
    let stem = dll.strip_suffix(".dll").unwrap_or(dll);
    ["vcruntime", "msvcp", "concrt", "vccorlib", "vcomp"]
        .iter()
        .any(|prefix| stem.starts_with(prefix))
        || (stem.starts_with("msvcr") && stem != "msvcrt")
        || stem.starts_with("libgcc_s")
        || stem.starts_with("libstdc++")
        || stem.starts_with("libwinpthread")
}

/// A readable machine name.
pub fn machine_name(machine: u16) -> String {
    match machine {
        MACHINE_AMD64 => "x64".to_string(),
        MACHINE_ARM64 => "ARM64".to_string(),
        MACHINE_I386 => "x86".to_string(),
        other => format!("machine {other:#06x}"),
    }
}

/// A readable subsystem name.
pub fn subsystem_name(subsystem: u16) -> String {
    match subsystem {
        SUBSYSTEM_GUI => "windows (GUI)".to_string(),
        SUBSYSTEM_CONSOLE => "console".to_string(),
        other => format!("subsystem {other}"),
    }
}

/// Reads a PE file.
pub fn read(path: &Path) -> Result<Pe, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn u16(&self, at: usize) -> Result<u16, String> {
        self.0
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .ok_or_else(|| format!("truncated at {at:#x}"))
    }

    fn u32(&self, at: usize) -> Result<u32, String> {
        self.0
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| format!("truncated at {at:#x}"))
    }

    fn u64(&self, at: usize) -> Result<u64, String> {
        Ok(u64::from(self.u32(at)?) | (u64::from(self.u32(at + 4)?) << 32))
    }

    fn cstr(&self, at: usize) -> Result<String, String> {
        let rest = self
            .0
            .get(at..)
            .ok_or_else(|| format!("a name at {at:#x} is outside the file"))?;
        let end = rest
            .iter()
            .take(512)
            .position(|&b| b == 0)
            .ok_or_else(|| format!("an unterminated name at {at:#x}"))?;
        Ok(String::from_utf8_lossy(&rest[..end]).into_owned())
    }
}

struct Section {
    address: u32,
    size: u32,
    raw: u32,
    raw_size: u32,
}

/// Parses a PE image.
pub fn parse(bytes: &[u8]) -> Result<Pe, String> {
    let b = Bytes(bytes);
    if bytes.get(0..2) != Some(b"MZ") {
        return Err("not a PE file (no MZ header)".to_string());
    }
    let pe = b.u32(0x3c)? as usize;
    if bytes.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("not a PE file (no PE signature)".to_string());
    }
    let coff = pe + 4;
    let machine = b.u16(coff)?;
    let sections = b.u16(coff + 2)? as usize;
    let optional_size = b.u16(coff + 16)? as usize;
    let optional = coff + 20;
    let magic = b.u16(optional)?;
    let (image_base, count_at, dirs_at) = match magic {
        0x10b => (
            u64::from(b.u32(optional + 28)?),
            optional + 92,
            optional + 96,
        ),
        0x20b => (b.u64(optional + 24)?, optional + 108, optional + 112),
        other => return Err(format!("an unknown optional header magic {other:#x}")),
    };
    let subsystem = b.u16(optional + 68)?;
    let dir_count = b.u32(count_at)? as usize;
    let dir = |index: usize| -> Result<(u32, u32), String> {
        if index >= dir_count {
            return Ok((0, 0));
        }
        Ok((b.u32(dirs_at + index * 8)?, b.u32(dirs_at + index * 8 + 4)?))
    };

    let table = optional + optional_size;
    let mut list = Vec::with_capacity(sections.min(96));
    for index in 0..sections.min(96) {
        let at = table + index * 40;
        list.push(Section {
            size: b.u32(at + 8)?,
            address: b.u32(at + 12)?,
            raw_size: b.u32(at + 16)?,
            raw: b.u32(at + 20)?,
        });
    }
    let offset = |rva: u32| -> Result<usize, String> {
        list.iter()
            .find(|s| rva >= s.address && rva < s.address.saturating_add(s.size.max(s.raw_size)))
            .map(|s| (rva - s.address + s.raw) as usize)
            .ok_or_else(|| format!("the address {rva:#x} is in no section"))
    };

    let mut imports = Vec::new();
    let (rva, _) = dir(1)?;
    if rva != 0 {
        let mut at = offset(rva)?;
        for _ in 0..4096 {
            let name = b.u32(at + 12)?;
            let thunk = b.u32(at + 16)?;
            if name == 0 && thunk == 0 {
                break;
            }
            imports.push(b.cstr(offset(name)?)?);
            at += 20;
        }
    }

    let mut delay_imports = Vec::new();
    let (rva, _) = dir(13)?;
    if rva != 0 {
        let mut at = offset(rva)?;
        for _ in 0..4096 {
            let attributes = b.u32(at)?;
            let name = b.u32(at + 4)?;
            if name == 0 {
                break;
            }
            // Bit 0: the fields are RVAs; old images store addresses.
            let name = if attributes & 1 == 1 {
                name
            } else {
                u32::try_from(u64::from(name).saturating_sub(image_base)).unwrap_or(0)
            };
            delay_imports.push(b.cstr(offset(name)?)?);
            at += 32;
        }
    }

    Ok(Pe {
        machine,
        subsystem,
        imports,
        delay_imports,
    })
}

/// A minimal PE32+ image importing `dlls`, for tests and fake tools.
pub fn synthetic(machine: u16, subsystem: u16, dlls: &[&str]) -> Vec<u8> {
    const RAW: u32 = 0x200;
    const VA: u32 = 0x1000;
    let mut section = vec![0u8; (dlls.len() + 1) * 20];
    let mut names = Vec::new();
    for (index, dll) in dlls.iter().enumerate() {
        let name_rva = VA + section.len() as u32 + names.len() as u32;
        names.extend_from_slice(dll.as_bytes());
        names.push(0);
        let at = index * 20;
        section[at + 12..at + 16].copy_from_slice(&name_rva.to_le_bytes());
        section[at + 16..at + 20].copy_from_slice(&(VA + 0x800).to_le_bytes());
    }
    let imports_size = section.len() as u32;
    section.extend_from_slice(&names);
    section.resize(section.len().max(0x800) + 8, 0);

    let mut bytes = vec![0u8; RAW as usize];
    bytes[0..2].copy_from_slice(b"MZ");
    bytes[0x3c..0x40].copy_from_slice(&64u32.to_le_bytes());
    bytes[64..68].copy_from_slice(b"PE\0\0");
    let coff = 68;
    bytes[coff..coff + 2].copy_from_slice(&machine.to_le_bytes());
    bytes[coff + 2..coff + 4].copy_from_slice(&1u16.to_le_bytes());
    bytes[coff + 16..coff + 18].copy_from_slice(&240u16.to_le_bytes());
    bytes[coff + 18..coff + 20].copy_from_slice(&0x22u16.to_le_bytes());
    let optional = coff + 20;
    bytes[optional..optional + 2].copy_from_slice(&0x20bu16.to_le_bytes());
    bytes[optional + 24..optional + 32].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
    bytes[optional + 68..optional + 70].copy_from_slice(&subsystem.to_le_bytes());
    bytes[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes());
    let import_dir = optional + 112 + 8;
    if !dlls.is_empty() {
        bytes[import_dir..import_dir + 4].copy_from_slice(&VA.to_le_bytes());
        bytes[import_dir + 4..import_dir + 8].copy_from_slice(&imports_size.to_le_bytes());
    }
    let header = optional + 240;
    bytes[header..header + 6].copy_from_slice(b".idata");
    let size = section.len() as u32;
    bytes[header + 8..header + 12].copy_from_slice(&size.to_le_bytes());
    bytes[header + 12..header + 16].copy_from_slice(&VA.to_le_bytes());
    bytes[header + 16..header + 20].copy_from_slice(&size.to_le_bytes());
    bytes[header + 20..header + 24].copy_from_slice(&RAW.to_le_bytes());
    bytes.extend_from_slice(&section);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_and_subsystem_are_read() {
        let bytes = synthetic(
            MACHINE_AMD64,
            SUBSYSTEM_GUI,
            &[
                "KERNEL32.dll",
                "VCRUNTIME140.dll",
                "api-ms-win-crt-runtime-l1-1-0.dll",
            ],
        );
        let pe = parse(&bytes).unwrap();
        assert_eq!(pe.machine, MACHINE_AMD64);
        assert_eq!(pe.subsystem, SUBSYSTEM_GUI);
        assert_eq!(
            pe.imports,
            [
                "KERNEL32.dll",
                "VCRUNTIME140.dll",
                "api-ms-win-crt-runtime-l1-1-0.dll"
            ]
        );
        assert_eq!(pe.runtime_dlls(), ["vcruntime140.dll"]);
        assert!(pe.uses_ucrt());
        assert_eq!(machine_name(pe.machine), "x64");
        assert_eq!(subsystem_name(pe.subsystem), "windows (GUI)");
    }

    #[test]
    fn a_static_crt_build_imports_only_system_dlls() {
        let bytes = synthetic(
            MACHINE_AMD64,
            SUBSYSTEM_CONSOLE,
            &["KERNEL32.dll", "USER32.dll", "ntdll.dll", "msvcrt.dll"],
        );
        let pe = parse(&bytes).unwrap();
        assert!(pe.runtime_dlls().is_empty(), "{:?}", pe.runtime_dlls());
        assert!(!pe.uses_ucrt());
        assert_eq!(pe.subsystem, SUBSYSTEM_CONSOLE);
        let none = parse(&synthetic(MACHINE_ARM64, SUBSYSTEM_GUI, &[])).unwrap();
        assert!(none.imports.is_empty());
    }

    #[test]
    fn redistributable_runtimes_are_named() {
        for dll in [
            "vcruntime140.dll",
            "vcruntime140_1.dll",
            "msvcp140.dll",
            "msvcr120.dll",
            "concrt140.dll",
            "libgcc_s_seh-1.dll",
            "libwinpthread-1.dll",
        ] {
            assert!(is_redistributable(dll), "{dll}");
        }
        for dll in ["kernel32.dll", "msvcrt.dll", "ucrtbase.dll", "user32.dll"] {
            assert!(!is_redistributable(dll), "{dll}");
        }
    }

    #[test]
    fn garbage_is_refused_without_panicking() {
        assert!(parse(b"").is_err());
        assert!(parse(b"MZ").is_err());
        let mut bytes = synthetic(MACHINE_AMD64, SUBSYSTEM_GUI, &["a.dll"]);
        bytes.truncate(0x210);
        assert!(parse(&bytes).is_err());
        let mut wrong = synthetic(MACHINE_AMD64, SUBSYSTEM_GUI, &["a.dll"]);
        wrong[0x3c] = 0xff;
        assert!(parse(&wrong).is_err());
    }
}
