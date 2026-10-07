//! A reader for XML property lists, into `serde_json::Value`.
//!
//! icm reads tool plists with `plutil -convert json` on macOS, but a
//! provisioning profile and the entitlements `codesign -d --xml` prints are
//! XML, and the profile gates run on any host (icm's tests run on Linux
//! too). `<date>` becomes its ISO 8601 string and `<data>` its base64 text
//! (whitespace removed); decode it with [`super::sha1::base64_decode`].

use serde_json::{Map, Number, Value};

/// Parses an XML plist document (or a bare value element).
pub fn parse(text: &str) -> Result<Value, String> {
    let mut reader = Reader {
        text,
        at: 0,
        depth: 0,
    };
    let Some(tag) = reader.open_tag()? else {
        return Err("no plist element".to_string());
    };
    if tag.name != "plist" {
        return reader.value(&tag);
    }
    if tag.empty {
        return Err("an empty <plist/>".to_string());
    }
    let Some(inner) = reader.open_tag()? else {
        return Err("an empty <plist>".to_string());
    };
    let value = reader.value(&inner)?;
    reader.close_tag("plist")?;
    Ok(value)
}

/// The XML plist inside a provisioning profile (a CMS envelope around the
/// plain plist bytes), parsed.
pub fn embedded(bytes: &[u8]) -> Result<Value, String> {
    let start = find(bytes, b"<?xml")
        .or_else(|| find(bytes, b"<plist"))
        .ok_or("no XML plist inside")?;
    let end = find(&bytes[start..], b"</plist>").ok_or("the XML plist is not closed")?;
    let slice = &bytes[start..start + end + "</plist>".len()];
    let text = std::str::from_utf8(slice).map_err(|e| format!("the plist is not UTF-8: {e}"))?;
    parse(text)
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

struct Tag {
    name: String,
    empty: bool,
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
    depth: usize,
}

impl<'a> Reader<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn error(&self, what: &str) -> String {
        let line = self.text[..self.at].matches('\n').count() + 1;
        format!("{what} at line {line}")
    }

    /// Skips whitespace, the XML declaration, the doctype and comments.
    fn skip_misc(&mut self) -> Result<(), String> {
        loop {
            let trimmed = self.rest().trim_start();
            self.at = self.text.len() - trimmed.len();
            let (open, close) = if trimmed.starts_with("<?") {
                ("<?", "?>")
            } else if trimmed.starts_with("<!--") {
                ("<!--", "-->")
            } else if trimmed.starts_with("<!") {
                ("<!", ">")
            } else {
                return Ok(());
            };
            let end = trimmed[open.len()..]
                .find(close)
                .ok_or_else(|| self.error("an unclosed declaration"))?;
            self.at += open.len() + end + close.len();
        }
    }

    /// The next start tag, or `None` at a close tag or the end.
    fn open_tag(&mut self) -> Result<Option<Tag>, String> {
        self.skip_misc()?;
        let rest = self.rest();
        if !rest.starts_with('<') || rest.starts_with("</") {
            return Ok(None);
        }
        let end = rest
            .find('>')
            .ok_or_else(|| self.error("an unclosed tag"))?;
        let inside = &rest[1..end];
        let empty = inside.ends_with('/');
        let inside = inside.trim_end_matches('/');
        let name = inside
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        self.at += end + 1;
        Ok(Some(Tag { name, empty }))
    }

    fn close_tag(&mut self, name: &str) -> Result<(), String> {
        self.skip_misc()?;
        let expected = format!("</{name}>");
        let rest = self.rest();
        // Tolerate whitespace before `>`.
        if let Some(after) = rest.strip_prefix("</")
            && let Some(end) = after.find('>')
            && after[..end].trim() == name
        {
            self.at += 2 + end + 1;
            return Ok(());
        }
        Err(self.error(&format!("expected {expected}")))
    }

    /// The text up to the next `<`, unescaped.
    fn text_until_tag(&mut self) -> Result<String, String> {
        let rest = self.rest();
        let end = rest
            .find('<')
            .ok_or_else(|| self.error("an unterminated value"))?;
        let raw = &rest[..end];
        self.at += end;
        unescape(raw).map_err(|e| self.error(&e))
    }

    fn value(&mut self, tag: &Tag) -> Result<Value, String> {
        self.depth += 1;
        if self.depth > 256 {
            return Err(self.error("nesting deeper than 256"));
        }
        let value = self.value_inner(tag);
        self.depth -= 1;
        value
    }

    fn value_inner(&mut self, tag: &Tag) -> Result<Value, String> {
        match tag.name.as_str() {
            "true" | "false" => {
                if !tag.empty {
                    self.close_tag(&tag.name)?;
                }
                Ok(Value::Bool(tag.name == "true"))
            }
            "dict" => {
                let mut map = Map::new();
                if tag.empty {
                    return Ok(Value::Object(map));
                }
                loop {
                    let Some(key) = self.open_tag()? else {
                        self.close_tag("dict")?;
                        return Ok(Value::Object(map));
                    };
                    if key.name != "key" {
                        return Err(self.error(&format!("<{}> where a <key> belongs", key.name)));
                    }
                    let name = if key.empty {
                        String::new()
                    } else {
                        let name = self.text_until_tag()?;
                        self.close_tag("key")?;
                        name
                    };
                    let Some(inner) = self.open_tag()? else {
                        return Err(self.error(&format!("the key {name} has no value")));
                    };
                    let value = self.value(&inner)?;
                    let _ = map.insert(name, value);
                }
            }
            "array" => {
                let mut items = Vec::new();
                if tag.empty {
                    return Ok(Value::Array(items));
                }
                loop {
                    let Some(inner) = self.open_tag()? else {
                        self.close_tag("array")?;
                        return Ok(Value::Array(items));
                    };
                    items.push(self.value(&inner)?);
                }
            }
            "string" | "date" | "data" | "integer" | "real" => {
                let text = if tag.empty {
                    String::new()
                } else {
                    let text = self.text_until_tag()?;
                    self.close_tag(&tag.name)?;
                    text
                };
                match tag.name.as_str() {
                    "string" => Ok(Value::String(text)),
                    "date" => Ok(Value::String(text.trim().to_string())),
                    "data" => Ok(Value::String(
                        text.chars().filter(|c| !c.is_whitespace()).collect(),
                    )),
                    "integer" => {
                        let trimmed = text.trim();
                        trimmed
                            .parse::<i64>()
                            .map(Value::from)
                            .or_else(|_| trimmed.parse::<u64>().map(Value::from))
                            .map_err(|_| self.error(&format!("<integer>{trimmed}</integer>")))
                    }
                    _ => {
                        let trimmed = text.trim();
                        trimmed
                            .parse::<f64>()
                            .ok()
                            .and_then(Number::from_f64)
                            .map(Value::Number)
                            .ok_or_else(|| self.error(&format!("<real>{trimmed}</real>")))
                    }
                }
            }
            other => Err(self.error(&format!("an unknown plist element <{other}>"))),
        }
    }
}

fn unescape(raw: &str) -> Result<String, String> {
    if !raw.contains('&') {
        return Ok(raw.to_string());
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end = after
            .find(';')
            .ok_or_else(|| "an unterminated entity".to_string())?;
        let entity = &after[..end];
        let ch = match entity {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(dec) = entity.strip_prefix('#') {
                    dec.parse().ok()
                } else {
                    None
                };
                code.and_then(char::from_u32)
                    .ok_or_else(|| format!("an unknown entity &{entity};"))?
            }
        };
        out.push(ch);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn documents_parse_into_json() {
        let text = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<!-- a comment -->
	<key>AppIDName</key>
	<string>Notes &amp; more &#x263A;</string>
	<key>ExpirationDate</key>
	<date>2027-09-29T12:00:00Z</date>
	<key>ProvisionsAllDevices</key>
	<false/>
	<key>TimeToLive</key>
	<integer>365</integer>
	<key>Ratio</key>
	<real>1.5</real>
	<key>Empty</key>
	<string/>
	<key>DeveloperCertificates</key>
	<array>
		<data>
		Zm9v
		YmFy
		</data>
	</array>
	<key>Entitlements</key>
	<dict>
		<key>get-task-allow</key>
		<true/>
		<key>keychain-access-groups</key>
		<array/>
		<key>nested</key>
		<dict/>
	</dict>
</dict>
</plist>
"#;
        let value = parse(text).unwrap();
        assert_eq!(
            value,
            json!({
                "AppIDName": "Notes & more \u{263A}",
                "ExpirationDate": "2027-09-29T12:00:00Z",
                "ProvisionsAllDevices": false,
                "TimeToLive": 365,
                "Ratio": 1.5,
                "Empty": "",
                "DeveloperCertificates": ["Zm9vYmFy"],
                "Entitlements": {"get-task-allow": true, "keychain-access-groups": [], "nested": {}}
            })
        );
    }

    #[test]
    fn icms_own_writer_round_trips() {
        let value = json!({"a": [1, "x < y", {"b": false}], "c": {}, "d": true});
        let xml = crate::platform::ios_sim::plist::to_xml(&value);
        assert_eq!(parse(&xml).unwrap(), value);
    }

    #[test]
    fn a_plist_inside_binary_bytes_is_found() {
        let mut bytes = vec![0x30, 0x82, 0x01, 0x00, 0xff, 0x00];
        bytes.extend_from_slice(b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>Name</key><string>P</string></dict></plist>");
        bytes.extend_from_slice(&[0xa0, 0x82, 0x00]);
        assert_eq!(embedded(&bytes).unwrap(), json!({"Name": "P"}));
        assert!(embedded(b"no plist here").is_err());
    }

    #[test]
    fn malformed_documents_are_errors() {
        for bad in [
            "<plist><dict><key>a</key></dict></plist>",
            "<plist><dict><string>x</string></dict></plist>",
            "<plist><integer>x</integer></plist>",
            "<plist><unknown/></plist>",
            "<plist><string>a &bogus; b</string></plist>",
            "<plist><array><string>x</string></plist>",
            "",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
