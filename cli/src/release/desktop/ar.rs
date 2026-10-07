//! The members of an `ar` archive: a `.deb` is `debian-binary`, then
//! `control.tar.*` and `data.tar.*`. `icm verify linux` reads a package
//! this way on any host, and `tar` unpacks the data member.

use std::path::Path;

/// One member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// Its name (`data.tar.xz`).
    pub name: String,
    /// Its bytes.
    pub data: Vec<u8>,
}

/// Reads every member of an archive.
pub fn read(path: &Path) -> Result<Vec<Member>, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    parse(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// Parses an archive.
pub fn parse(bytes: &[u8]) -> Result<Vec<Member>, String> {
    if !bytes.starts_with(b"!<arch>\n") {
        return Err("not an ar archive".to_string());
    }
    let mut members = Vec::new();
    let mut at = 8;
    while at < bytes.len() {
        let header = bytes
            .get(at..at + 60)
            .ok_or_else(|| format!("a truncated header at {at}"))?;
        if &header[58..60] != b"`\n" {
            return Err(format!("a malformed header at {at}"));
        }
        let name = String::from_utf8_lossy(&header[0..16])
            .trim_end()
            .trim_end_matches('/')
            .to_string();
        let size: usize = String::from_utf8_lossy(&header[48..58])
            .trim()
            .parse()
            .map_err(|_| format!("a malformed size at {at}"))?;
        let start = at + 60;
        let data = bytes
            .get(start..start + size)
            .ok_or_else(|| format!("{name} is truncated"))?;
        members.push(Member {
            name,
            data: data.to_vec(),
        });
        at = start + size + (size % 2);
    }
    Ok(members)
}

/// An archive of `(name, data)` members (tests and fake tools).
pub fn write(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        out.extend_from_slice(
            format!(
                "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
                name,
                0,
                0,
                0,
                "100644",
                data.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_round_trip() {
        let bytes = write(&[
            ("debian-binary", b"2.0\n"),
            ("control.tar.xz", b"odd"),
            ("data.tar.xz", b"data"),
        ]);
        let members = parse(&bytes).unwrap();
        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["debian-binary", "control.tar.xz", "data.tar.xz"]);
        assert_eq!(members[1].data, b"odd");
        assert_eq!(members[2].data, b"data");
        assert!(parse(b"nope").is_err());
        assert!(parse(&bytes[..70]).is_err());
    }
}
