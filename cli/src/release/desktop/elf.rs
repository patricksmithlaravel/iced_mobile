//! What the Linux gates read from an ELF executable (design §11.6 step 1):
//! its machine, and the symbol versions it needs from each shared library
//! (`.gnu.version_r`). The highest `GLIBC_x.y` it needs is the oldest
//! glibc it runs on, which `linux.glibc_floor` compares with `[desktop.linux]
//! glibc_floor`. Bounds-checked: any file can be handed to `icm verify`.

use std::path::Path;

/// `EM_X86_64`.
pub const EM_X86_64: u16 = 62;
/// `EM_AARCH64`.
pub const EM_AARCH64: u16 = 183;

const SHT_GNU_VERNEED: u32 = 0x6fff_fffe;

/// An executable's facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Elf {
    /// `e_machine`.
    pub machine: u16,
    /// Whether it is a 64-bit ELF.
    pub is_64: bool,
    /// `(library, version names)` from `.gnu.version_r`.
    pub needs: Vec<(String, Vec<String>)>,
}

impl Elf {
    /// The highest `GLIBC_x.y[.z]` version needed, as numbers.
    pub fn max_glibc(&self) -> Option<Vec<u32>> {
        self.needs
            .iter()
            .flat_map(|(_, versions)| versions)
            .filter_map(|name| parse_version(name.strip_prefix("GLIBC_")?))
            .max()
    }

    /// The Debian architecture (`amd64`, `arm64`).
    pub fn deb_arch(&self) -> Option<&'static str> {
        match self.machine {
            EM_X86_64 => Some("amd64"),
            EM_AARCH64 => Some("arm64"),
            _ => None,
        }
    }

    /// The AppImage architecture (`x86_64`, `aarch64`).
    pub fn appimage_arch(&self) -> Option<&'static str> {
        match self.machine {
            EM_X86_64 => Some("x86_64"),
            EM_AARCH64 => Some("aarch64"),
            _ => None,
        }
    }
}

/// A readable machine name.
pub fn machine_name(machine: u16) -> String {
    match machine {
        EM_X86_64 => "x86-64".to_string(),
        EM_AARCH64 => "AArch64".to_string(),
        3 => "x86".to_string(),
        40 => "ARM".to_string(),
        other => format!("machine {other}"),
    }
}

/// `2.35` or `2.2.5` as numbers; `None` for `GLIBC_PRIVATE` and the like.
pub fn parse_version(text: &str) -> Option<Vec<u32>> {
    let parts: Option<Vec<u32>> = text.split('.').map(|part| part.parse().ok()).collect();
    parts.filter(|parts| !parts.is_empty())
}

/// A version as text.
pub fn version_string(version: &[u32]) -> String {
    version
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

/// Reads an ELF file.
pub fn read(path: &Path) -> Result<Elf, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// Parses a little-endian ELF image.
pub fn parse(bytes: &[u8]) -> Result<Elf, String> {
    if bytes.get(0..4) != Some(b"\x7fELF") {
        return Err("not an ELF file".to_string());
    }
    let is_64 = match bytes.get(4) {
        Some(1) => false,
        Some(2) => true,
        _ => return Err("an unknown ELF class".to_string()),
    };
    if bytes.get(5) != Some(&1) {
        return Err("a big-endian ELF file".to_string());
    }
    let u16_at = |at: usize| -> Result<u16, String> {
        bytes
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .ok_or_else(|| format!("truncated at {at:#x}"))
    };
    let u32_at = |at: usize| -> Result<u32, String> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| format!("truncated at {at:#x}"))
    };
    let word = |at: usize| -> Result<u64, String> {
        if is_64 {
            Ok(u64::from(u32_at(at)?) | (u64::from(u32_at(at + 4)?) << 32))
        } else {
            Ok(u64::from(u32_at(at)?))
        }
    };
    let machine = u16_at(18)?;
    let (shoff, shentsize, shnum) = if is_64 {
        (word(0x28)?, u16_at(0x3a)?, u16_at(0x3c)?)
    } else {
        (word(0x20)?, u16_at(0x2e)?, u16_at(0x30)?)
    };

    struct Section {
        kind: u32,
        offset: u64,
        size: u64,
        link: u32,
        info: u32,
    }
    let mut sections = Vec::new();
    for index in 0..usize::from(shnum).min(4096) {
        let at = shoff as usize + index * usize::from(shentsize);
        let section = if is_64 {
            Section {
                kind: u32_at(at + 4)?,
                offset: word(at + 24)?,
                size: word(at + 32)?,
                link: u32_at(at + 40)?,
                info: u32_at(at + 44)?,
            }
        } else {
            Section {
                kind: u32_at(at + 4)?,
                offset: word(at + 16)?,
                size: word(at + 20)?,
                link: u32_at(at + 24)?,
                info: u32_at(at + 28)?,
            }
        };
        sections.push(section);
    }

    let mut needs = Vec::new();
    for section in sections.iter().filter(|s| s.kind == SHT_GNU_VERNEED) {
        let strings = sections
            .get(section.link as usize)
            .ok_or("the version needs point at no string table")?;
        let string = |offset: u32| -> Result<String, String> {
            let start = (strings.offset + u64::from(offset)) as usize;
            let end = (strings.offset + strings.size) as usize;
            let slice = bytes
                .get(start..end.min(bytes.len()))
                .ok_or("a version name is outside the file")?;
            let len = slice
                .iter()
                .position(|&b| b == 0)
                .ok_or("an unterminated version name")?;
            Ok(String::from_utf8_lossy(&slice[..len]).into_owned())
        };
        let mut at = section.offset as usize;
        for _ in 0..section.info.min(4096) {
            let count = u16_at(at + 2)?;
            let file = string(u32_at(at + 4)?)?;
            let aux = u32_at(at + 8)? as usize;
            let next = u32_at(at + 12)? as usize;
            let mut versions = Vec::new();
            let mut aux_at = at + aux;
            for _ in 0..count.min(4096) {
                versions.push(string(u32_at(aux_at + 8)?)?);
                let next_aux = u32_at(aux_at + 12)? as usize;
                if next_aux == 0 {
                    break;
                }
                aux_at += next_aux;
            }
            needs.push((file, versions));
            if next == 0 {
                break;
            }
            at += next;
        }
    }
    Ok(Elf {
        machine,
        is_64,
        needs,
    })
}

/// A minimal 64-bit ELF with a `.gnu.version_r` section, for tests and
/// fake tools.
pub fn synthetic(machine: u16, needs: &[(&str, &[&str])]) -> Vec<u8> {
    // Strings: an empty first one, then every name.
    let mut strings = vec![0u8];
    let name = |text: &str, strings: &mut Vec<u8>| -> u32 {
        let at = strings.len() as u32;
        strings.extend_from_slice(text.as_bytes());
        strings.push(0);
        at
    };
    let mut verneed = Vec::new();
    for (index, (file, versions)) in needs.iter().enumerate() {
        let file_at = name(file, &mut strings);
        let size = 16 + 16 * versions.len();
        let next = if index + 1 == needs.len() { 0 } else { size };
        verneed.extend_from_slice(&1u16.to_le_bytes());
        verneed.extend_from_slice(&(versions.len() as u16).to_le_bytes());
        verneed.extend_from_slice(&file_at.to_le_bytes());
        verneed.extend_from_slice(&16u32.to_le_bytes());
        verneed.extend_from_slice(&(next as u32).to_le_bytes());
        for (aux, version) in versions.iter().enumerate() {
            let version_at = name(version, &mut strings);
            let next_aux = if aux + 1 == versions.len() { 0 } else { 16 };
            verneed.extend_from_slice(&0u32.to_le_bytes());
            verneed.extend_from_slice(&0u16.to_le_bytes());
            verneed.extend_from_slice(&0u16.to_le_bytes());
            verneed.extend_from_slice(&version_at.to_le_bytes());
            verneed.extend_from_slice(&(next_aux as u32).to_le_bytes());
        }
    }
    let strings_at = 64u64;
    let verneed_at = strings_at + strings.len() as u64;
    let shoff = verneed_at + verneed.len() as u64;

    let mut bytes = vec![0u8; 64];
    bytes[0..4].copy_from_slice(b"\x7fELF");
    bytes[4] = 2;
    bytes[5] = 1;
    bytes[6] = 1;
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[0x28..0x30].copy_from_slice(&shoff.to_le_bytes());
    bytes[0x34..0x36].copy_from_slice(&64u16.to_le_bytes());
    bytes[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
    bytes[0x3c..0x3e].copy_from_slice(&3u16.to_le_bytes());
    bytes.extend_from_slice(&strings);
    bytes.extend_from_slice(&verneed);
    let section = |kind: u32, offset: u64, size: u64, link: u32, info: u32| -> Vec<u8> {
        let mut header = vec![0u8; 64];
        header[4..8].copy_from_slice(&kind.to_le_bytes());
        header[24..32].copy_from_slice(&offset.to_le_bytes());
        header[32..40].copy_from_slice(&size.to_le_bytes());
        header[40..44].copy_from_slice(&link.to_le_bytes());
        header[44..48].copy_from_slice(&info.to_le_bytes());
        header
    };
    bytes.extend(section(0, 0, 0, 0, 0));
    bytes.extend(section(3, strings_at, strings.len() as u64, 0, 0));
    bytes.extend(section(
        SHT_GNU_VERNEED,
        verneed_at,
        verneed.len() as u64,
        1,
        needs.len() as u32,
    ));
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glibc_needs_are_read() {
        let bytes = synthetic(
            EM_X86_64,
            &[
                ("libc.so.6", &["GLIBC_2.2.5", "GLIBC_2.34", "GLIBC_PRIVATE"]),
                ("libgcc_s.so.1", &["GCC_3.0", "GCC_4.2.0"]),
                ("libm.so.6", &["GLIBC_2.29"]),
            ],
        );
        let elf = parse(&bytes).unwrap();
        assert_eq!(elf.machine, EM_X86_64);
        assert!(elf.is_64);
        assert_eq!(elf.needs.len(), 3);
        assert_eq!(elf.needs[0].0, "libc.so.6");
        assert_eq!(elf.needs[1].1, ["GCC_3.0", "GCC_4.2.0"]);
        assert_eq!(elf.max_glibc(), Some(vec![2, 34]));
        assert_eq!(elf.deb_arch(), Some("amd64"));
        assert_eq!(elf.appimage_arch(), Some("x86_64"));
        assert_eq!(version_string(&[2, 2, 5]), "2.2.5");
        assert!(parse_version("2.35").unwrap() > parse_version("2.2.5").unwrap());
        assert!(parse_version("2.35").unwrap() < parse_version("2.39").unwrap());
        assert_eq!(parse_version("PRIVATE"), None);
    }

    #[test]
    fn executables_without_needs_and_garbage() {
        let elf = parse(&synthetic(EM_AARCH64, &[])).unwrap();
        assert!(elf.needs.is_empty());
        assert_eq!(elf.max_glibc(), None);
        assert_eq!(elf.deb_arch(), Some("arm64"));
        assert!(parse(b"\x7fELF").is_err());
        assert!(parse(b"MZ").is_err());
        let mut truncated = synthetic(EM_X86_64, &[("libc.so.6", &["GLIBC_2.17"])]);
        truncated.truncate(100);
        assert!(parse(&truncated).is_err());
    }
}
