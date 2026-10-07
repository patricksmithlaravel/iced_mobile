//! The Mach-O facts the device and release gates read beyond the build
//! version ([`crate::platform::ios_sim::macho`]): each slice's
//! architecture, `LC_UUID` (the dSYM gate) and undefined external symbols
//! (the privacy scan, design §11.1 step 4). Parsed directly, like the
//! simulator's gates, instead of through `object`, `otool` or `nm`.

use crate::platform::ios_sim::macho::{self as sim, BuildVersion};
use std::path::Path;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_MAGIC: u32 = 0xfeed_face;
const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const LC_SYMTAB: u32 = 0x2;
const LC_UUID: u32 = 0x1b;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_SUBTYPE_ARM64E: u32 = 2;
const N_STAB: u8 = 0xe0;
const N_TYPE: u8 = 0x0e;
const N_EXT: u8 = 0x01;

/// One architecture slice.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Slice {
    /// `arm64`, `arm64e`, `x86_64`, ...
    pub arch: String,
    /// `LC_UUID`, upper case with dashes (as `dwarfdump --uuid` prints it).
    pub uuid: Option<String>,
    /// Undefined external symbols (`_stat`, `_OBJC_CLASS_$_NSUserDefaults`).
    pub undefined: Vec<String>,
}

/// Every slice of a Mach-O file (thin or fat).
pub fn slices(path: &Path) -> Result<Vec<Slice>, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// The build versions of every slice (the simulator module's reader).
pub fn build_versions(path: &Path) -> Result<Vec<BuildVersion>, String> {
    sim::build_versions(path)
}

fn u32_at(bytes: &[u8], at: usize, big: bool) -> Option<u32> {
    let word: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(if big {
        u32::from_be_bytes(word)
    } else {
        u32::from_le_bytes(word)
    })
}

fn u64_at(bytes: &[u8], at: usize, big: bool) -> Option<u64> {
    let word: [u8; 8] = bytes.get(at..at + 8)?.try_into().ok()?;
    Some(if big {
        u64::from_be_bytes(word)
    } else {
        u64::from_le_bytes(word)
    })
}

/// Parses a Mach-O image in memory.
pub fn parse(bytes: &[u8]) -> Result<Vec<Slice>, String> {
    let magic = u32_at(bytes, 0, true).ok_or("too short for a Mach-O header")?;
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    match magic {
        FAT_MAGIC | FAT_MAGIC_64 => {
            let count = u32_at(bytes, 4, true).ok_or("a truncated fat header")?;
            let wide = magic == FAT_MAGIC_64;
            let entry = if wide { 32 } else { 20 };
            for index in 0..count.min(64) as usize {
                let at = 8 + index * entry;
                let (offset, size) = if wide {
                    (
                        u64_at(bytes, at + 8, true).ok_or("a truncated fat entry")? as usize,
                        u64_at(bytes, at + 16, true).ok_or("a truncated fat entry")? as usize,
                    )
                } else {
                    (
                        u32_at(bytes, at + 8, true).ok_or("a truncated fat entry")? as usize,
                        u32_at(bytes, at + 12, true).ok_or("a truncated fat entry")? as usize,
                    )
                };
                ranges.push((offset, size));
            }
        }
        _ => ranges.push((0, bytes.len())),
    }

    let mut slices = Vec::new();
    for (offset, size) in ranges {
        let end = offset.checked_add(size).ok_or("a fat entry overflows")?;
        let image = bytes
            .get(offset..end.min(bytes.len()))
            .ok_or("a fat entry points outside the file")?;
        slices.push(parse_thin(image)?);
    }
    Ok(slices)
}

fn parse_thin(image: &[u8]) -> Result<Slice, String> {
    let magic = u32_at(image, 0, false).ok_or("too short for a Mach-O header")?;
    let (header, wide) = match magic {
        MH_MAGIC_64 => (32usize, true),
        MH_MAGIC => (28usize, false),
        other => return Err(format!("not a Mach-O file (magic {other:#x})")),
    };
    let field = |at: usize| u32_at(image, at, false).ok_or("a truncated Mach-O header");
    let cputype = field(4)?;
    let subtype = field(8)? & 0x00ff_ffff;
    let ncmds = field(16)?;
    let arch = match (cputype, subtype) {
        (CPU_TYPE_ARM64, CPU_SUBTYPE_ARM64E) => "arm64e".to_string(),
        (CPU_TYPE_ARM64, _) => "arm64".to_string(),
        (CPU_TYPE_X86_64, _) => "x86_64".to_string(),
        (other, _) => format!("cputype {other:#x}"),
    };

    let mut slice = Slice {
        arch,
        ..Slice::default()
    };
    let mut at = header;
    for _ in 0..ncmds.min(4096) {
        let (Some(cmd), Some(size)) = (u32_at(image, at, false), u32_at(image, at + 4, false))
        else {
            break;
        };
        match cmd {
            LC_UUID => {
                if let Some(raw) = image.get(at + 8..at + 24) {
                    slice.uuid = Some(uuid_string(raw));
                }
            }
            LC_SYMTAB if wide => {
                let word = |offset: usize| u32_at(image, at + offset, false).unwrap_or(0) as usize;
                slice.undefined = undefined_symbols(image, word(8), word(12), word(16), word(20));
            }
            _ => {}
        }
        if size < 8 {
            break;
        }
        at += size as usize;
    }
    Ok(slice)
}

fn uuid_string(raw: &[u8]) -> String {
    let hex: String = raw.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn undefined_symbols(
    image: &[u8],
    symoff: usize,
    nsyms: usize,
    stroff: usize,
    strsize: usize,
) -> Vec<String> {
    let Some(strings) = image.get(stroff..stroff.saturating_add(strsize)) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for index in 0..nsyms.min(4_000_000) {
        let at = symoff + index * 16;
        let Some(entry) = image.get(at..at + 16) else {
            break;
        };
        let n_type = entry[4];
        let undefined = n_type & N_STAB == 0 && n_type & N_TYPE == 0 && n_type & N_EXT != 0 && {
            // A common symbol has a size in n_value; it is defined.
            entry[8..16].iter().all(|b| *b == 0)
        };
        if !undefined {
            continue;
        }
        let strx = u32::from_le_bytes([entry[0], entry[1], entry[2], entry[3]]) as usize;
        let Some(tail) = strings.get(strx..) else {
            continue;
        };
        let end = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
        names.push(String::from_utf8_lossy(&tail[..end]).into_owned());
    }
    names.sort();
    names.dedup();
    names
}

/// Whether `needle` occurs anywhere in the file's bytes (the ObjC class
/// and selector names the privacy scan looks for, the agent-bridge
/// marker).
pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// A thin arm64 Mach-O for tests and fake tools: `LC_BUILD_VERSION`,
/// `LC_UUID`, `LC_SYMTAB` with the given undefined symbols, then `extra`
/// bytes (strings the privacy scan finds).
pub fn synthetic(
    platform: u32,
    minos: (u32, u32, u32),
    sdk: (u32, u32, u32),
    uuid: [u8; 16],
    undefined: &[&str],
    extra: &[u8],
) -> Vec<u8> {
    let encode = |(major, minor, patch): (u32, u32, u32)| (major << 16) | (minor << 8) | patch;
    const BUILD: u32 = 0x32;
    let commands_size = 24 + 24 + 24;
    let header_size = 32;
    let symoff = header_size + commands_size;
    let nsyms = undefined.len();
    let stroff = symoff + nsyms * 16;
    let mut strings = vec![b' ', 0];
    let mut offsets = Vec::new();
    for name in undefined {
        offsets.push(strings.len() as u32);
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
    }

    fn push(bytes: &mut Vec<u8>, words: &[u32]) {
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
    }
    let mut bytes = Vec::new();
    push(
        &mut bytes,
        &[
            MH_MAGIC_64,
            CPU_TYPE_ARM64,
            0,
            2,
            3,
            commands_size as u32,
            0,
            0,
        ],
    );
    push(
        &mut bytes,
        &[BUILD, 24, platform, encode(minos), encode(sdk), 0],
    );
    push(&mut bytes, &[LC_UUID, 24]);
    bytes.extend_from_slice(&uuid);
    push(
        &mut bytes,
        &[
            LC_SYMTAB,
            24,
            symoff as u32,
            nsyms as u32,
            stroff as u32,
            strings.len() as u32,
        ],
    );
    for offset in offsets {
        bytes.extend_from_slice(&offset.to_le_bytes());
        bytes.push(N_EXT); // n_type: undefined, external
        bytes.push(0); // n_sect
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
    }
    bytes.extend_from_slice(&strings);
    bytes.extend_from_slice(extra);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::ios_sim::macho::PLATFORM_IOS;

    #[test]
    fn uuid_symbols_and_arch_are_read() {
        let uuid = [
            0x02, 0xF0, 0xF6, 0x19, 0xEE, 0x0D, 0x37, 0x69, 0x89, 0xD5, 0xE3, 0x2E, 0xFA, 0x6B,
            0xC5, 0x2C,
        ];
        let bytes = synthetic(
            PLATFORM_IOS,
            (16, 0, 0),
            (27, 0, 0),
            uuid,
            &["_stat", "_mach_absolute_time", "_stat"],
            b"activeInputModes",
        );
        let slices = parse(&bytes).unwrap();
        assert_eq!(slices.len(), 1);
        assert_eq!(slices[0].arch, "arm64");
        assert_eq!(
            slices[0].uuid.as_deref(),
            Some("02F0F619-EE0D-3769-89D5-E32EFA6BC52C")
        );
        assert_eq!(slices[0].undefined, ["_mach_absolute_time", "_stat"]);
        assert!(contains(&bytes, b"activeInputModes"));
        assert!(!contains(&bytes, b"NSUserDefaults"));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app");
        std::fs::write(&path, &bytes).unwrap();
        let versions = build_versions(&path).unwrap();
        assert_eq!(versions[0].platform, PLATFORM_IOS);
        assert_eq!(versions[0].sdk_string(), "27.0");
        assert_eq!(super::slices(&path).unwrap(), slices);
    }

    #[test]
    fn fat_files_list_every_slice() {
        let thin = synthetic(PLATFORM_IOS, (16, 0, 0), (27, 0, 0), [7; 16], &[], b"");
        let mut fat = Vec::new();
        fat.extend_from_slice(&FAT_MAGIC.to_be_bytes());
        fat.extend_from_slice(&2u32.to_be_bytes());
        for (index, subtype) in [0u32, CPU_SUBTYPE_ARM64E].iter().enumerate() {
            let offset = 4096 * (index as u32 + 1);
            for word in [CPU_TYPE_ARM64, *subtype, offset, thin.len() as u32, 14] {
                fat.extend_from_slice(&word.to_be_bytes());
            }
        }
        fat.resize(4096, 0);
        fat.extend_from_slice(&thin);
        fat.resize(8192, 0);
        let mut arm64e = thin.clone();
        arm64e[8..12].copy_from_slice(&CPU_SUBTYPE_ARM64E.to_le_bytes());
        fat.extend_from_slice(&arm64e);
        let slices = parse(&fat).unwrap();
        let archs: Vec<&str> = slices.iter().map(|s| s.arch.as_str()).collect();
        assert_eq!(archs, ["arm64", "arm64e"]);
        assert!(parse(b"#!/bin/sh\n").is_err());
        assert!(parse(b"").is_err());
    }

    #[test]
    fn the_host_build_of_icm_has_a_uuid() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let slices = super::slices(&exe).unwrap();
        assert!(slices.iter().all(|s| s.uuid.is_some()));
        assert!(slices.iter().any(|s| !s.undefined.is_empty()));
    }
}
