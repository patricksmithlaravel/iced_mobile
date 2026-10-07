//! What the APK gates need from a shared library (design §10.4 step 5):
//! the machine type, the alignment of every `PT_LOAD` segment, and whether
//! a symbol is exported. Reads only the headers and the dynamic symbol
//! table, so a 200 MB debug library costs a few reads.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

/// `EM_386`.
pub const EM_386: u16 = 3;
/// `EM_ARM`.
pub const EM_ARM: u16 = 40;
/// `EM_X86_64`.
pub const EM_X86_64: u16 = 62;
/// `EM_AARCH64`.
pub const EM_AARCH64: u16 = 183;

const PT_LOAD: u32 = 1;
const SHT_DYNSYM: u32 = 11;

/// A shared library's facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Library {
    /// `e_machine`.
    pub machine: u16,
    /// Whether it is a 64-bit ELF.
    pub is_64: bool,
    /// The `p_align` of each `PT_LOAD` segment.
    pub load_aligns: Vec<u64>,
    /// The defined global and weak symbols in `.dynsym`.
    pub exports: Vec<String>,
}

impl Library {
    /// The smallest `PT_LOAD` alignment (0 when there is none).
    pub fn min_load_align(&self) -> u64 {
        self.load_aligns.iter().copied().min().unwrap_or(0)
    }

    /// Whether `name` is exported.
    pub fn exports(&self, name: &str) -> bool {
        self.exports.iter().any(|symbol| symbol == name)
    }
}

/// The `e_machine` for an Android ABI.
pub fn machine_for_abi(abi: &str) -> Option<u16> {
    match abi {
        "arm64-v8a" => Some(EM_AARCH64),
        "x86_64" => Some(EM_X86_64),
        "armeabi-v7a" => Some(EM_ARM),
        "x86" => Some(EM_386),
        _ => None,
    }
}

/// A readable name for an `e_machine`.
pub fn machine_name(machine: u16) -> String {
    match machine {
        EM_AARCH64 => "AArch64".to_string(),
        EM_X86_64 => "x86-64".to_string(),
        EM_ARM => "ARM".to_string(),
        EM_386 => "x86".to_string(),
        other => format!("machine {other}"),
    }
}

struct Reader {
    file: File,
    len: u64,
}

impl Reader {
    fn bytes(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
        if offset
            .checked_add(len as u64)
            .is_none_or(|end| end > self.len)
        {
            return Err(io::Error::other(
                "ELF structure points past the end of the file",
            ));
        }
        let mut buffer = vec![0u8; len];
        self.file.read_exact_at(&mut buffer, offset)?;
        Ok(buffer)
    }
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap_or([0; 4]))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or([0; 8]))
}

/// Reads a little-endian ELF shared library (32- or 64-bit).
pub fn read(path: &Path) -> io::Result<Library> {
    let file = File::open(path)?;
    let len = file.metadata()?.len();
    let reader = Reader { file, len };

    let ident = reader.bytes(0, 64.min(len as usize))?;
    if ident.len() < 52 || &ident[..4] != b"\x7fELF" {
        return Err(io::Error::other("not an ELF file"));
    }
    if ident[5] != 1 {
        return Err(io::Error::other("not a little-endian ELF file"));
    }
    let is_64 = match ident[4] {
        1 => false,
        2 if ident.len() >= 64 => true,
        _ => return Err(io::Error::other("unknown ELF class")),
    };
    let machine = u16_at(&ident, 18);

    let (phoff, shoff, phentsize, phnum, shentsize, shnum) = if is_64 {
        (
            u64_at(&ident, 32),
            u64_at(&ident, 40),
            u16_at(&ident, 54),
            u16_at(&ident, 56),
            u16_at(&ident, 58),
            u16_at(&ident, 60),
        )
    } else {
        (
            u64::from(u32_at(&ident, 28)),
            u64::from(u32_at(&ident, 32)),
            u16_at(&ident, 42),
            u16_at(&ident, 44),
            u16_at(&ident, 46),
            u16_at(&ident, 48),
        )
    };

    let mut load_aligns = Vec::new();
    for index in 0..u64::from(phnum) {
        let header = reader.bytes(phoff + index * u64::from(phentsize), phentsize as usize)?;
        let kind = u32_at(&header, 0);
        if kind != PT_LOAD {
            continue;
        }
        let align = if is_64 {
            u64_at(&header, 48)
        } else {
            u64::from(u32_at(&header, 28))
        };
        load_aligns.push(align);
    }

    // Section headers: find .dynsym and its string table.
    let mut sections = Vec::new();
    for index in 0..u64::from(shnum) {
        let header = reader.bytes(shoff + index * u64::from(shentsize), shentsize as usize)?;
        let section = if is_64 {
            (
                u32_at(&header, 4),
                u64_at(&header, 24),
                u64_at(&header, 32),
                u32_at(&header, 40),
                u64_at(&header, 56),
            )
        } else {
            (
                u32_at(&header, 4),
                u64::from(u32_at(&header, 16)),
                u64::from(u32_at(&header, 20)),
                u32_at(&header, 24),
                u64::from(u32_at(&header, 36)),
            )
        };
        sections.push(section);
    }

    let mut exports = Vec::new();
    if let Some(&(_, offset, size, link, entsize)) =
        sections.iter().find(|(kind, ..)| *kind == SHT_DYNSYM)
    {
        let (_, str_offset, str_size, _, _) = sections
            .get(link as usize)
            .copied()
            .ok_or_else(|| io::Error::other("dynsym links to no string table"))?;
        let strings = reader.bytes(str_offset, str_size as usize)?;
        let table = reader.bytes(offset, size as usize)?;
        let entsize = if entsize == 0 {
            if is_64 { 24 } else { 16 }
        } else {
            entsize as usize
        };
        for symbol in table.chunks_exact(entsize) {
            let (name, info, shndx) = if is_64 {
                (u32_at(symbol, 0), symbol[4], u16_at(symbol, 6))
            } else {
                (u32_at(symbol, 0), symbol[12], u16_at(symbol, 14))
            };
            let binding = info >> 4;
            if shndx == 0 || !(binding == 1 || binding == 2) {
                continue;
            }
            let start = name as usize;
            if start >= strings.len() {
                continue;
            }
            let end = strings[start..]
                .iter()
                .position(|b| *b == 0)
                .map_or(strings.len(), |p| start + p);
            exports.push(String::from_utf8_lossy(&strings[start..end]).into_owned());
        }
    }

    Ok(Library {
        machine,
        is_64,
        load_aligns,
        exports,
    })
}

/// Builds a minimal 64-bit ELF shared library, for icm's tests (the unit
/// tests and the fake toolchains of `tests/android_release.rs`).
#[doc(hidden)]
pub fn synthetic(machine: u16, align: u64, symbols: &[&str]) -> Vec<u8> {
    // Layout: ELF header (64) | 1 program header (56) | dynstr | dynsym |
    // 3 section headers (null, dynsym, dynstr).
    let mut dynstr = vec![0u8];
    let mut offsets = Vec::new();
    for name in symbols {
        offsets.push(dynstr.len() as u32);
        dynstr.extend_from_slice(name.as_bytes());
        dynstr.push(0);
    }
    let mut dynsym = vec![0u8; 24]; // the null symbol
    for offset in &offsets {
        let mut symbol = vec![0u8; 24];
        symbol[0..4].copy_from_slice(&offset.to_le_bytes());
        symbol[4] = (1 << 4) | 2; // GLOBAL FUNC
        symbol[6..8].copy_from_slice(&9u16.to_le_bytes()); // defined
        dynsym.extend_from_slice(&symbol);
    }
    // An undefined global, which is not an export.
    let mut undefined = vec![0u8; 24];
    undefined[4] = 1 << 4;
    dynsym.extend_from_slice(&undefined);

    let phoff = 64u64;
    let dynstr_off = phoff + 56;
    let dynsym_off = dynstr_off + dynstr.len() as u64;
    let shoff = dynsym_off + dynsym.len() as u64;

    let mut out = vec![0u8; 64];
    out[..4].copy_from_slice(b"\x7fELF");
    out[4] = 2;
    out[5] = 1;
    out[6] = 1;
    out[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
    out[18..20].copy_from_slice(&machine.to_le_bytes());
    out[32..40].copy_from_slice(&phoff.to_le_bytes());
    out[40..48].copy_from_slice(&shoff.to_le_bytes());
    out[52..54].copy_from_slice(&64u16.to_le_bytes());
    out[54..56].copy_from_slice(&56u16.to_le_bytes());
    out[56..58].copy_from_slice(&1u16.to_le_bytes());
    out[58..60].copy_from_slice(&64u16.to_le_bytes());
    out[60..62].copy_from_slice(&3u16.to_le_bytes());

    let mut program = vec![0u8; 56];
    program[0..4].copy_from_slice(&PT_LOAD.to_le_bytes());
    program[48..56].copy_from_slice(&align.to_le_bytes());
    out.extend_from_slice(&program);
    out.extend_from_slice(&dynstr);
    out.extend_from_slice(&dynsym);

    out.extend_from_slice(&[0u8; 64]); // null section
    let mut sym = vec![0u8; 64];
    sym[4..8].copy_from_slice(&SHT_DYNSYM.to_le_bytes());
    sym[24..32].copy_from_slice(&dynsym_off.to_le_bytes());
    sym[32..40].copy_from_slice(&(dynsym.len() as u64).to_le_bytes());
    sym[40..44].copy_from_slice(&2u32.to_le_bytes());
    sym[56..64].copy_from_slice(&24u64.to_le_bytes());
    out.extend_from_slice(&sym);
    let mut strtab = vec![0u8; 64];
    strtab[4..8].copy_from_slice(&3u32.to_le_bytes()); // SHT_STRTAB
    strtab[24..32].copy_from_slice(&dynstr_off.to_le_bytes());
    strtab[32..40].copy_from_slice(&(dynstr.len() as u64).to_le_bytes());
    out.extend_from_slice(&strtab);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_machine_alignment_and_exports() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("libapp.so");
        std::fs::write(
            &path,
            synthetic(
                EM_AARCH64,
                0x4000,
                &["ANativeActivity_onCreate", "android_main"],
            ),
        )
        .unwrap();
        let library = read(&path).unwrap();
        assert_eq!(library.machine, EM_AARCH64);
        assert!(library.is_64);
        assert_eq!(library.min_load_align(), 0x4000);
        assert!(library.exports("ANativeActivity_onCreate"));
        assert!(library.exports("android_main"));
        assert!(!library.exports("missing"));
        assert_eq!(library.exports.len(), 2, "{:?}", library.exports);

        std::fs::write(&path, synthetic(EM_X86_64, 0x1000, &[])).unwrap();
        let library = read(&path).unwrap();
        assert_eq!(machine_name(library.machine), "x86-64");
        assert_eq!(library.min_load_align(), 0x1000);
        assert!(library.exports.is_empty());

        std::fs::write(&path, b"not elf").unwrap();
        assert!(read(&path).is_err());
    }

    #[test]
    fn abis_map_to_machines() {
        assert_eq!(machine_for_abi("arm64-v8a"), Some(EM_AARCH64));
        assert_eq!(machine_for_abi("x86_64"), Some(EM_X86_64));
        assert_eq!(machine_for_abi("mips"), None);
    }
}
