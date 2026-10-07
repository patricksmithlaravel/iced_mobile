//! The Mach-O facts the iOS gates read (`ios.macho.platform`,
//! `ios.macho.minos`): each slice's `LC_BUILD_VERSION` (or the older
//! `LC_VERSION_MIN_IPHONEOS`). A few dozen lines of parsing instead of the
//! `object` crate or `otool`/`vtool`.

use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

/// `PLATFORM_MACOS`.
pub const PLATFORM_MACOS: u32 = 1;
/// `PLATFORM_IOS`: devices.
pub const PLATFORM_IOS: u32 = 2;
/// `PLATFORM_IOSSIMULATOR`.
pub const PLATFORM_IOSSIMULATOR: u32 = 7;

const MH_MAGIC_64: u32 = 0xfeed_facf;
const MH_MAGIC: u32 = 0xfeed_face;
const FAT_MAGIC: u32 = 0xcafe_babe;
const FAT_MAGIC_64: u32 = 0xcafe_babf;
const LC_BUILD_VERSION: u32 = 0x32;
const LC_VERSION_MIN_IPHONEOS: u32 = 0x25;
const CPU_TYPE_ARM64: u32 = 0x0100_000c;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;

/// One slice's build version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildVersion {
    /// The slice's architecture (`arm64`, `x86_64`, ...).
    pub arch: String,
    /// The `PLATFORM_*` number.
    pub platform: u32,
    /// The minimum OS version, `(major, minor, patch)`.
    pub minos: (u32, u32, u32),
    /// The SDK version.
    pub sdk: (u32, u32, u32),
}

impl BuildVersion {
    /// `16.0` or `16.0.1`.
    pub fn minos_string(&self) -> String {
        version_string(self.minos)
    }

    /// `27.0`.
    pub fn sdk_string(&self) -> String {
        version_string(self.sdk)
    }
}

/// `IOSSIMULATOR`, `IOS`, `MACOS` or the number.
pub fn platform_name(platform: u32) -> String {
    match platform {
        PLATFORM_MACOS => "MACOS".to_string(),
        PLATFORM_IOS => "IOS".to_string(),
        PLATFORM_IOSSIMULATOR => "IOSSIMULATOR".to_string(),
        6 => "MACCATALYST".to_string(),
        other => format!("platform {other}"),
    }
}

fn version_string((major, minor, patch): (u32, u32, u32)) -> String {
    if patch == 0 {
        format!("{major}.{minor}")
    } else {
        format!("{major}.{minor}.{patch}")
    }
}

/// Decodes the `xxxx.yy.zz` nibble encoding.
fn decode_version(value: u32) -> (u32, u32, u32) {
    (value >> 16, (value >> 8) & 0xff, value & 0xff)
}

fn arch_name(cputype: u32) -> String {
    match cputype {
        CPU_TYPE_ARM64 => "arm64".to_string(),
        CPU_TYPE_X86_64 => "x86_64".to_string(),
        other => format!("cputype {other:#x}"),
    }
}

/// Reads every slice's build version. A slice without one is left out.
pub fn build_versions(path: &Path) -> Result<Vec<BuildVersion>, String> {
    let file =
        File::open(path).map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let read = |offset: u64, len: usize| -> Result<Vec<u8>, String> {
        let mut buffer = vec![0u8; len];
        file.read_exact_at(&mut buffer, offset)
            .map_err(|error| format!("{} is truncated: {error}", path.display()))?;
        Ok(buffer)
    };

    let head = read(0, 8)?;
    let magic_be = u32::from_be_bytes(head[0..4].try_into().expect("4 bytes"));
    let mut slices: Vec<u64> = Vec::new();
    match magic_be {
        FAT_MAGIC | FAT_MAGIC_64 => {
            let count = u32::from_be_bytes(head[4..8].try_into().expect("4 bytes"));
            let (entry, offset_at, wide) = if magic_be == FAT_MAGIC {
                (20u64, 8usize, false)
            } else {
                (32u64, 8usize, true)
            };
            for index in 0..u64::from(count.min(64)) {
                let arch = read(8 + index * entry, entry as usize)?;
                let offset = if wide {
                    u64::from_be_bytes(arch[offset_at..offset_at + 8].try_into().expect("8"))
                } else {
                    u64::from(u32::from_be_bytes(
                        arch[offset_at..offset_at + 4].try_into().expect("4"),
                    ))
                };
                slices.push(offset);
            }
        }
        _ => slices.push(0),
    }

    let mut versions = Vec::new();
    for base in slices {
        let header = read(base, 28)?;
        let magic = u32::from_le_bytes(header[0..4].try_into().expect("4"));
        let header_size = match magic {
            MH_MAGIC_64 => 32u64,
            MH_MAGIC => 28u64,
            _ => {
                return Err(format!(
                    "{} is not a Mach-O file (magic {magic:#x})",
                    path.display()
                ));
            }
        };
        let field = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("4"));
        let cputype = field(4);
        let ncmds = field(16);
        let sizeofcmds = field(20);
        let commands = read(base + header_size, sizeofcmds as usize)?;

        let mut at = 0usize;
        for _ in 0..ncmds {
            if at + 8 > commands.len() {
                break;
            }
            let word = |offset: usize| -> Option<u32> {
                commands
                    .get(offset..offset + 4)
                    .map(|bytes| u32::from_le_bytes(bytes.try_into().expect("4")))
            };
            let (Some(cmd), Some(size)) = (word(at), word(at + 4)) else {
                break;
            };
            match cmd {
                LC_BUILD_VERSION => {
                    if let (Some(platform), Some(minos), Some(sdk)) =
                        (word(at + 8), word(at + 12), word(at + 16))
                    {
                        versions.push(BuildVersion {
                            arch: arch_name(cputype),
                            platform,
                            minos: decode_version(minos),
                            sdk: decode_version(sdk),
                        });
                    }
                }
                LC_VERSION_MIN_IPHONEOS => {
                    if let (Some(minos), Some(sdk)) = (word(at + 8), word(at + 12)) {
                        versions.push(BuildVersion {
                            arch: arch_name(cputype),
                            platform: PLATFORM_IOS,
                            minos: decode_version(minos),
                            sdk: decode_version(sdk),
                        });
                    }
                }
                _ => {}
            }
            if size < 8 {
                break;
            }
            at += size as usize;
        }
    }
    Ok(versions)
}

/// Whether `minos` equals a `[ios] min_os` value such as `16.0` or `16`.
pub fn minos_matches(minos: (u32, u32, u32), min_os: &str) -> bool {
    let mut parts = min_os
        .trim()
        .split('.')
        .map(|part| part.parse::<u32>().ok());
    let major = parts.next().flatten();
    let minor = parts.next().flatten().unwrap_or(0);
    let patch = parts.next().flatten().unwrap_or(0);
    major == Some(minos.0) && minor == minos.1 && patch == minos.2
}

/// A minimal thin arm64 Mach-O with one `LC_BUILD_VERSION`, for tests and
/// fake tools.
pub fn synthetic(platform: u32, minos: (u32, u32, u32), sdk: (u32, u32, u32)) -> Vec<u8> {
    let encode = |(major, minor, patch): (u32, u32, u32)| (major << 16) | (minor << 8) | patch;
    let mut bytes = Vec::new();
    for word in [
        MH_MAGIC_64,
        CPU_TYPE_ARM64,
        0,
        2, // MH_EXECUTE
        1, // ncmds
        24,
        0,
        0,
    ] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    for word in [
        LC_BUILD_VERSION,
        24,
        platform,
        encode(minos),
        encode(sdk),
        0,
    ] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_versions_are_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app");
        std::fs::write(
            &path,
            synthetic(PLATFORM_IOSSIMULATOR, (16, 0, 0), (27, 0, 0)),
        )
        .unwrap();
        let versions = build_versions(&path).unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].arch, "arm64");
        assert_eq!(versions[0].platform, PLATFORM_IOSSIMULATOR);
        assert_eq!(versions[0].minos_string(), "16.0");
        assert_eq!(versions[0].sdk_string(), "27.0");
        assert_eq!(platform_name(versions[0].platform), "IOSSIMULATOR");
        assert!(minos_matches(versions[0].minos, "16.0"));
        assert!(minos_matches(versions[0].minos, "16"));
        assert!(!minos_matches(versions[0].minos, "17.0"));

        // A fat file with that slice at offset 4096.
        let slice = synthetic(PLATFORM_IOS, (17, 2, 1), (27, 0, 0));
        let mut fat = Vec::new();
        fat.extend_from_slice(&FAT_MAGIC.to_be_bytes());
        fat.extend_from_slice(&1u32.to_be_bytes());
        for word in [CPU_TYPE_ARM64, 0, 4096, slice.len() as u32, 14] {
            fat.extend_from_slice(&word.to_be_bytes());
        }
        fat.resize(4096, 0);
        fat.extend_from_slice(&slice);
        std::fs::write(&path, fat).unwrap();
        let versions = build_versions(&path).unwrap();
        assert_eq!(versions[0].platform, PLATFORM_IOS);
        assert_eq!(versions[0].minos_string(), "17.2.1");

        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        assert!(build_versions(&path).is_err());
    }

    #[test]
    fn the_host_build_of_icm_has_a_build_version() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let versions = build_versions(&exe).unwrap();
        assert!(versions.iter().any(|v| v.platform == PLATFORM_MACOS));
    }
}
