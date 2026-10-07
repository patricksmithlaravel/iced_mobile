//! A deterministic ZIP writer for APKs: every entry stored (no
//! compression), 1980-01-01 timestamps, Unix mode 0644, entries in the
//! order given. `zipalign -P 16` then pads the native libraries to 16 KB
//! pages and `apksigner` signs the result; both validate the layout.

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, Write};
use std::path::{Path, PathBuf};

/// Where an entry's bytes come from.
#[derive(Clone, Debug)]
pub enum Source {
    /// A file on disk.
    File(PathBuf),
    /// Bytes in memory.
    Bytes(Vec<u8>),
}

/// One entry.
#[derive(Clone, Debug)]
pub struct Entry {
    /// The name inside the archive (`/`-separated, no leading `/`).
    pub name: String,
    /// The contents.
    pub source: Source,
}

struct Written {
    name: String,
    crc: u32,
    size: u32,
    offset: u32,
}

/// DOS date for 1980-01-01 (day 1, month 1, year 0).
const DOS_DATE: u16 = (1 << 5) | 1;

/// Writes `entries` to `path` as a stored ZIP archive.
pub fn write(path: &Path, entries: &[Entry]) -> io::Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        if !seen.insert(entry.name.as_str()) {
            return Err(io::Error::other(format!(
                "duplicate zip entry {}",
                entry.name
            )));
        }
    }

    let mut out = BufWriter::new(File::create(path)?);
    let mut written: Vec<Written> = Vec::with_capacity(entries.len());

    for entry in entries {
        let offset = position(&mut out)?;
        let (crc, size) = match &entry.source {
            Source::Bytes(bytes) => (crc32(bytes), bytes.len() as u64),
            Source::File(file) => crc32_file(file)?,
        };
        let size = u32::try_from(size)
            .map_err(|_| io::Error::other(format!("{} is larger than 4 GiB", entry.name)))?;
        let name = entry.name.as_bytes();

        out.write_all(&0x0403_4b50u32.to_le_bytes())?;
        out.write_all(&10u16.to_le_bytes())?; // version needed: 1.0 (stored)
        out.write_all(&0u16.to_le_bytes())?; // flags
        out.write_all(&0u16.to_le_bytes())?; // method: stored
        out.write_all(&0u16.to_le_bytes())?; // time
        out.write_all(&DOS_DATE.to_le_bytes())?;
        out.write_all(&crc.to_le_bytes())?;
        out.write_all(&size.to_le_bytes())?; // compressed
        out.write_all(&size.to_le_bytes())?; // uncompressed
        out.write_all(&(name.len() as u16).to_le_bytes())?;
        out.write_all(&0u16.to_le_bytes())?; // extra
        out.write_all(name)?;

        match &entry.source {
            Source::Bytes(bytes) => out.write_all(bytes)?,
            Source::File(file) => {
                let mut input = File::open(file)?;
                let _ = io::copy(&mut input, &mut out)?;
            }
        }

        written.push(Written {
            name: entry.name.clone(),
            crc,
            size,
            offset: u32::try_from(offset)
                .map_err(|_| io::Error::other("the archive is larger than 4 GiB"))?,
        });
    }

    let directory_start = position(&mut out)?;
    for entry in &written {
        let name = entry.name.as_bytes();
        out.write_all(&0x0201_4b50u32.to_le_bytes())?;
        out.write_all(&((3u16 << 8) | 10).to_le_bytes())?; // made by: Unix, 1.0
        out.write_all(&10u16.to_le_bytes())?; // version needed
        out.write_all(&0u16.to_le_bytes())?; // flags
        out.write_all(&0u16.to_le_bytes())?; // method
        out.write_all(&0u16.to_le_bytes())?; // time
        out.write_all(&DOS_DATE.to_le_bytes())?;
        out.write_all(&entry.crc.to_le_bytes())?;
        out.write_all(&entry.size.to_le_bytes())?;
        out.write_all(&entry.size.to_le_bytes())?;
        out.write_all(&(name.len() as u16).to_le_bytes())?;
        out.write_all(&0u16.to_le_bytes())?; // extra
        out.write_all(&0u16.to_le_bytes())?; // comment
        out.write_all(&0u16.to_le_bytes())?; // disk
        out.write_all(&0u16.to_le_bytes())?; // internal attributes
        out.write_all(&((0o100_644u32) << 16).to_le_bytes())?; // external: -rw-r--r--
        out.write_all(&entry.offset.to_le_bytes())?;
        out.write_all(name)?;
    }
    let directory_end = position(&mut out)?;

    let count =
        u16::try_from(written.len()).map_err(|_| io::Error::other("too many zip entries"))?;
    out.write_all(&0x0605_4b50u32.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    out.write_all(&count.to_le_bytes())?;
    out.write_all(&count.to_le_bytes())?;
    out.write_all(&((directory_end - directory_start) as u32).to_le_bytes())?;
    out.write_all(&(directory_start as u32).to_le_bytes())?;
    out.write_all(&0u16.to_le_bytes())?;
    out.flush()?;
    Ok(())
}

fn position(out: &mut BufWriter<File>) -> io::Result<u64> {
    out.stream_position()
}

/// The names in a ZIP archive's central directory (for tests and checks).
pub fn names(path: &Path) -> io::Result<Vec<String>> {
    let bytes = std::fs::read(path)?;
    let end = bytes
        .windows(4)
        .rposition(|w| w == 0x0605_4b50u32.to_le_bytes())
        .ok_or_else(|| io::Error::other("no end of central directory"))?;
    let read16 = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
    let read32 = |at: usize| {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
    };
    let count = read16(end + 10);
    let mut at = read32(end + 16);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if read32(at) != 0x0201_4b50 {
            return Err(io::Error::other("bad central directory entry"));
        }
        let name_len = read16(at + 28);
        let extra = read16(at + 30);
        let comment = read16(at + 32);
        out.push(String::from_utf8_lossy(&bytes[at + 46..at + 46 + name_len]).into_owned());
        at += 46 + name_len + extra + comment;
    }
    Ok(out)
}

fn crc_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        table
    })
}

fn crc_update(mut crc: u32, bytes: &[u8]) -> u32 {
    let table = crc_table();
    for &byte in bytes {
        crc = table[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

/// CRC-32 (IEEE), as ZIP uses it.
pub fn crc32(bytes: &[u8]) -> u32 {
    !crc_update(!0, bytes)
}

fn crc32_file(path: &Path) -> io::Result<(u32, u64)> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0u8; 1 << 20];
    let mut crc = !0u32;
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        crc = crc_update(crc, &buffer[..read]);
        size += read as u64;
    }
    Ok((!crc, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn writes_a_readable_deterministic_archive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.so");
        std::fs::write(&file, vec![7u8; 5000]).unwrap();
        let entries = vec![
            Entry {
                name: "AndroidManifest.xml".into(),
                source: Source::Bytes(b"<manifest/>".to_vec()),
            },
            Entry {
                name: "lib/arm64-v8a/libapp.so".into(),
                source: Source::File(file),
            },
        ];
        let a = dir.path().join("a.zip");
        let b = dir.path().join("b.zip");
        write(&a, &entries).unwrap();
        write(&b, &entries).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
        assert_eq!(
            names(&a).unwrap(),
            vec!["AndroidManifest.xml", "lib/arm64-v8a/libapp.so"]
        );

        // The system unzip agrees, when there is one.
        if let Ok(output) = std::process::Command::new("unzip")
            .arg("-t")
            .arg(&a)
            .output()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(output.status.success(), "{text}");
            assert!(text.contains("No errors detected"), "{text}");
        }

        let duplicate = vec![entries[0].clone(), entries[0].clone()];
        assert!(write(&dir.path().join("c.zip"), &duplicate).is_err());
    }
}
