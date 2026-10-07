//! The release site (design §9.5, §11.3 steps 2 and 3, §12.4): what `icm
//! release web` writes into `dist/<version>+<build>/web/site/` and what
//! `icm verify web` reads back from any site directory.
//!
//! - Content-hashed module names, `pkg/app-<h8>.js` and
//!   `pkg/app_bg-<h8>.wasm` (`h8`: the first 8 hex digits of the file's
//!   sha256), passed explicitly to wasm-bindgen's `init` so neither name is
//!   derived at run time ([`index_html`]).
//! - `index.html` without the dev forwarder, with `<base href>` from `[web]
//!   public_url`; `404.html` (the same page, for hosts that serve it on
//!   unknown paths); `.nojekyll`; `manifest.webmanifest`; icons at 32, 180,
//!   192 and 512 px plus a maskable 512 px one ([`icons`]).
//! - Header suggestions: `_headers` in the site (Netlify and Cloudflare Pages
//!   read it: `application/wasm`, immutable caching of the hashed files,
//!   `no-cache` on the pages) and, for servers that do not, nginx, Apache
//!   and Caddy snippets beside the site ([`hosting_files`]).
//! - What the gates read: the hashed names in `index.html`
//!   ([`parse_index`], `web.hashed_assets`), the `.wasm`'s declared type in
//!   `_headers` (`web.mime`), the fonts embedded in the `.wasm` ([`fonts`],
//!   `web.fonts_embedded`), and the `wasm-opt` flags that match what rustc
//!   compiled for ([`wasm_opt_flags`]).

use crate::android::image::{Rgba, decode, encode};
use std::path::Path;

/// The module names inside the site, relative to it (`pkg/app-<h8>.js`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hashed {
    /// The JavaScript glue.
    pub js: String,
    /// The WebAssembly module.
    pub wasm: String,
}

/// The first 8 hex digits of a sha256.
pub fn h8(sha256: &str) -> &str {
    &sha256[..8.min(sha256.len())]
}

/// `pkg/<stem>-<h8>.<extension>`.
pub fn hashed_name(stem: &str, extension: &str, sha256: &str) -> String {
    format!("pkg/{stem}-{}.{extension}", h8(sha256))
}

/// Whether `name` (`pkg/app-0123abcd.js`) carries an 8-hex-digit hash
/// before its extension; returns the hash.
pub fn hash_in_name(name: &str) -> Option<&str> {
    let file = name.rsplit('/').next()?;
    let (stem, _) = file.rsplit_once('.')?;
    let (_, hash) = stem.rsplit_once('-')?;
    (hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit())).then_some(hash)
}

/// `<base href>`: `[web] public_url` with a trailing `/`, so relative
/// paths resolve inside it.
pub fn base_href(public_url: &str) -> String {
    if public_url.ends_with('/') {
        public_url.to_string()
    } else {
        format!("{public_url}/")
    }
}

/// The path part of `[web] public_url` (`/`, `/app/`), for the paths in
/// `_headers` and the server snippets, and for serving the site locally.
pub fn path_prefix(public_url: &str) -> String {
    let base = base_href(public_url);
    match base.strip_prefix("https://") {
        Some(rest) => match rest.find('/') {
            Some(at) => rest[at..].to_string(),
            None => "/".to_string(),
        },
        None => base,
    }
}

/// The release `index.html` (design §9.5).
pub fn index_html(
    name: &str,
    description: &str,
    background: &str,
    public_url: &str,
    hashed: &Hashed,
    icons: bool,
) -> String {
    use super::site::escape;
    let icons = if icons {
        "<link rel=\"icon\" href=\"icon-32.png\" sizes=\"32x32\" type=\"image/png\">\n<link rel=\"apple-touch-icon\" href=\"icon-180.png\">\n"
    } else {
        // An empty icon: the browser would otherwise ask for /favicon.ico.
        "<link rel=\"icon\" href=\"data:,\">\n"
    };
    let description = if description.trim().is_empty() {
        String::new()
    } else {
        format!(
            "<meta name=\"description\" content=\"{}\">\n",
            escape(description.trim())
        )
    };
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<base href="{base}">
<title>{title}</title>
{description}{icons}<link rel="manifest" href="manifest.webmanifest">
<meta name="theme-color" content="{background}">
<style>html,body{{margin:0;height:100%;background:{background}}}canvas{{display:block;width:100%;height:100%;outline:none}}</style>
</head><body>
<noscript>This app needs JavaScript and WebAssembly.</noscript>
<script type="module">
import init from "./{js}";
init({{ module_or_path: "./{wasm}" }}).catch(function (error) {{
  console.error("the app failed to start: " + (error && error.stack || error));
}});
</script>
</body></html>
"#,
        base = escape(&base_href(public_url)),
        title = escape(name),
        js = hashed.js,
        wasm = hashed.wasm,
    )
}

/// The module names a release `index.html` loads: the `import init from`
/// specifier and the `module_or_path` string, without a leading `./`.
pub fn parse_index(html: &str) -> Option<Hashed> {
    let quoted_after = |marker: &str| -> Option<String> {
        let rest = &html[html.find(marker)? + marker.len()..];
        let rest = rest.trim_start();
        let quote = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let inner = &rest[1..];
        let end = inner.find(quote)?;
        Some(inner[..end].trim_start_matches("./").to_string())
    };
    Some(Hashed {
        js: quoted_after("import init from")?,
        wasm: quoted_after("module_or_path:")?,
    })
}

/// The release `manifest.webmanifest`.
pub fn manifest(name: &str, description: &str, background: &str, icons: bool) -> String {
    let icons = if icons {
        serde_json::json!([
            {"src": "icon-192.png", "sizes": "192x192", "type": "image/png"},
            {"src": "icon-512.png", "sizes": "512x512", "type": "image/png"},
            {"src": "icon-maskable-512.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable"},
        ])
    } else {
        serde_json::json!([])
    };
    let mut manifest = serde_json::json!({
        "name": name,
        "short_name": name,
        "start_url": ".",
        "scope": ".",
        "display": "standalone",
        "background_color": background,
        "theme_color": background,
        "icons": icons,
    });
    if !description.trim().is_empty() {
        manifest["description"] = serde_json::json!(description.trim());
    }
    let mut text = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    text.push('\n');
    text
}

/// The icons (file name, PNG bytes) made from the app icon: 32 px (the
/// favicon) and 192 and 512 px (the manifest) keep transparency; 180 px
/// (Apple's touch icon, which iOS shows black where it is transparent) is
/// flattened onto the background; the maskable 512 px icon is the icon at
/// 66 % on the background, inside the safe zone launchers crop to (the
/// share Android's adaptive icon uses).
pub fn icons(source: &Path, background: [u8; 3]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let bytes = std::fs::read(source)
        .map_err(|error| format!("cannot read {}: {error}", source.display()))?;
    let image = decode(&bytes).map_err(|error| format!("{}: {error}", source.display()))?;
    let [r, g, b] = background;
    let mut out = Vec::new();
    for size in [32u32, 192, 512] {
        out.push((
            format!("icon-{size}.png"),
            encode(&image.resize(size, size))?,
        ));
    }
    out.push((
        "icon-180.png".to_string(),
        encode(&image.resize(180, 180).flatten(background))?,
    ));
    let inner = 338; // 66 % of 512
    let offset = (512 - inner) / 2;
    let mut maskable = Rgba::filled(512, 512, [r, g, b, 255]);
    maskable.draw(&image.resize(inner, inner), offset, offset);
    out.push((
        "icon-maskable-512.png".to_string(),
        encode(&maskable.flatten(background))?,
    ));
    Ok(out)
}

/// `_headers` (Netlify and Cloudflare Pages): the `.wasm` type, immutable
/// caching of the hashed modules, revalidation of the page and the
/// manifest. No two rules set the same header on one path, since the hosts
/// combine such rules differently.
pub fn headers(prefix: &str, hashed: &Hashed) -> String {
    format!(
        "# Written by icm release web. Netlify and Cloudflare Pages apply it;\n\
         # for other hosts see ../hosting/ and UPLOAD.md.\n\
         {prefix}*\n  X-Content-Type-Options: nosniff\n\n\
         {prefix}\n  Cache-Control: no-cache\n\n\
         {prefix}index.html\n  Cache-Control: no-cache\n\n\
         {prefix}manifest.webmanifest\n  Cache-Control: no-cache\n\n\
         {prefix}{wasm}\n  Content-Type: application/wasm\n  Cache-Control: public, max-age=31536000, immutable\n\n\
         {prefix}{js}\n  Cache-Control: public, max-age=31536000, immutable\n",
        js = hashed.js,
        wasm = hashed.wasm,
    )
}

/// The header suggestions for servers that do not read `_headers`, as
/// `(file name, contents)` for `dist/…/web/hosting/`.
pub fn hosting_files(prefix: &str, hashed: &Hashed) -> Vec<(&'static str, String)> {
    let hashed_regex = format!(
        "^{}pkg/app(_bg)?-[0-9a-f]{{8}}\\.(js|wasm)$",
        prefix.replace('.', "\\.")
    );
    vec![
        (
            "nginx.conf",
            format!(
                "# nginx: inside the server block that serves the site (icm release web).\n\
                 # The .wasm must be application/wasm: nginx's mime.types has it since\n\
                 # 1.21; on older versions add `application/wasm wasm;` to mime.types.\n\
                 location ~ {hashed_regex} {{\n    add_header Cache-Control \"public, max-age=31536000, immutable\";\n    add_header X-Content-Type-Options \"nosniff\";\n}}\n\
                 location {prefix} {{\n    add_header Cache-Control \"no-cache\";\n    add_header X-Content-Type-Options \"nosniff\";\n}}\n\
                 # This release: {js} and {wasm}\n",
                js = hashed.js,
                wasm = hashed.wasm,
            ),
        ),
        (
            "apache.htaccess",
            format!(
                "# Apache: save as .htaccess at the site's root (icm release web);\n\
                 # needs mod_headers.\n\
                 AddType application/wasm .wasm\n\
                 Header set X-Content-Type-Options \"nosniff\"\n\
                 Header set Cache-Control \"no-cache\"\n\
                 <FilesMatch \"^app(_bg)?-[0-9a-f]{{8}}\\.(js|wasm)$\">\n    Header set Cache-Control \"public, max-age=31536000, immutable\"\n</FilesMatch>\n\
                 # This release: {js} and {wasm}\n",
                js = hashed.js,
                wasm = hashed.wasm,
            ),
        ),
        (
            "Caddyfile",
            format!(
                "# Caddy: inside the site block (icm release web). Caddy serves .wasm\n\
                 # as application/wasm already.\n\
                 header X-Content-Type-Options nosniff\n\
                 @hashed path_regexp {hashed_regex}\n\
                 header @hashed Cache-Control \"public, max-age=31536000, immutable\"\n\
                 @other not path_regexp {hashed_regex}\n\
                 header @other Cache-Control \"no-cache\"\n\
                 # This release: {js} and {wasm}\n",
                js = hashed.js,
                wasm = hashed.wasm,
            ),
        ),
    ]
}

/// The `Content-Type` `_headers` sets for a path (exact paths only, which
/// is how icm writes them).
pub fn header_content_type(headers: &str, path: &str) -> Option<String> {
    let mut current: Option<&str> = None;
    for line in headers.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.starts_with(char::is_whitespace) {
            current = Some(line.trim());
            continue;
        }
        if current == Some(path)
            && let Some((name, value)) = line.trim().split_once(':')
            && name.trim().eq_ignore_ascii_case("content-type")
        {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// The `wasm-opt` flags for the target features rustc reports (`rustc
/// --print cfg --target wasm32-unknown-unknown`): each
/// `target_feature="x"` becomes `--enable-<binaryen name>`. A feature this
/// wasm-opt has no flag for (its `--help` does not list it) is returned in
/// the second list and left out.
pub fn wasm_opt_flags(cfg: &str, help: &str) -> (Vec<String>, Vec<String>) {
    let mut flags = Vec::new();
    let mut skipped = Vec::new();
    for line in cfg.lines() {
        let Some(feature) = line
            .trim()
            .strip_prefix("target_feature=\"")
            .and_then(|rest| rest.strip_suffix('"'))
        else {
            continue;
        };
        let name = match feature {
            "nontrapping-fptoint" => "nontrapping-float-to-int",
            "simd128" => "simd",
            "atomics" => "threads",
            other => other,
        };
        let flag = format!("--enable-{name}");
        let listed = help
            .split(|c: char| c.is_whitespace() || c == ',')
            .any(|word| word == flag);
        if listed {
            if !flags.contains(&flag) {
                flags.push(flag);
            }
        } else {
            skipped.push(feature.to_string());
        }
    }
    (flags, skipped)
}

/// The family names of the fonts a WebAssembly module carries in its
/// data. wasm-opt's memory packing drops runs of zeros from data segments
/// (memory starts zeroed), so a font is contiguous only in the linear
/// memory the segments initialise: [`data_image`] rebuilds it first. A
/// module that does not parse is scanned as it is.
pub fn wasm_fonts(wasm: &[u8]) -> Vec<String> {
    match data_image(wasm) {
        Some(images) => {
            let mut found = Vec::new();
            for image in images {
                for family in fonts(&image) {
                    if !found.contains(&family) {
                        found.push(family);
                    }
                }
            }
            found
        }
        None => fonts(wasm),
    }
}

fn leb_u32(bytes: &[u8], at: &mut usize) -> Option<u32> {
    let mut value = 0u64;
    for shift in (0..35).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return u32::try_from(value).ok();
        }
    }
    None
}

fn leb_i64(bytes: &[u8], at: &mut usize) -> Option<i64> {
    let mut value = 0i64;
    let mut shift = 0;
    loop {
        let byte = *bytes.get(*at)?;
        *at += 1;
        if shift < 64 {
            value |= i64::from(byte & 0x7f) << shift;
        }
        shift += 7;
        if byte & 0x80 == 0 {
            if shift < 64 && byte & 0x40 != 0 {
                value |= -1i64 << shift;
            }
            return Some(value);
        }
        if shift > 70 {
            return None;
        }
    }
}

/// The data a module puts in memory: its active segments laid out at
/// their offsets in one image (gaps zeroed), then each passive segment on
/// its own. `None` when the module does not parse.
pub fn data_image(wasm: &[u8]) -> Option<Vec<Vec<u8>>> {
    if wasm.get(..4)? != b"\0asm" {
        return None;
    }
    let mut at = 8;
    let mut active: Vec<(u64, &[u8])> = Vec::new();
    let mut passive: Vec<Vec<u8>> = Vec::new();
    while at < wasm.len() {
        let id = wasm[at];
        at += 1;
        let size = leb_u32(wasm, &mut at)? as usize;
        let end = at.checked_add(size).filter(|end| *end <= wasm.len())?;
        if id == 11 {
            let mut p = at;
            let count = leb_u32(wasm, &mut p)?;
            for _ in 0..count {
                let flags = leb_u32(wasm, &mut p)?;
                let offset = match flags {
                    0 | 2 => {
                        if flags == 2 {
                            let _memory = leb_u32(wasm, &mut p)?;
                        }
                        // i32.const or i64.const <n>, then end.
                        let opcode = *wasm.get(p)?;
                        p += 1;
                        if opcode != 0x41 && opcode != 0x42 {
                            return None;
                        }
                        let value = leb_i64(wasm, &mut p)?;
                        if *wasm.get(p)? != 0x0b {
                            return None;
                        }
                        p += 1;
                        Some(u64::try_from(value).ok()?)
                    }
                    1 => None,
                    _ => return None,
                };
                let length = leb_u32(wasm, &mut p)? as usize;
                let bytes = wasm.get(p..p.checked_add(length)?)?;
                p += length;
                match offset {
                    Some(offset) => active.push((offset, bytes)),
                    None => passive.push(bytes.to_vec()),
                }
            }
        }
        at = end;
    }
    let mut images = Vec::new();
    if let Some(first) = active.iter().map(|(offset, _)| *offset).min() {
        // Zeros before the first segment were dropped too (a TrueType file
        // starts with one).
        let start = first.saturating_sub(16);
        let end = active
            .iter()
            .map(|(offset, bytes)| offset + bytes.len() as u64)
            .max()?;
        // A layout spanning more than 1 GiB is not one a module could use.
        let span = usize::try_from(end - start)
            .ok()
            .filter(|span| *span <= 1 << 30)?;
        let mut image = vec![0u8; span];
        for (offset, bytes) in &active {
            let from = (offset - start) as usize;
            image[from..from + bytes.len()].copy_from_slice(bytes);
        }
        images.push(image);
    }
    images.extend(passive);
    Some(images)
}

/// The family names of the fonts embedded in a binary (TrueType or
/// OpenType files, found by their table directory, which is validated
/// field by field so random bytes do not match).
pub fn fonts(bytes: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    let mut at = 0;
    while at + 12 <= bytes.len() {
        let tag = &bytes[at..at + 4];
        if (tag == [0, 1, 0, 0] || tag == b"OTTO" || tag == b"true")
            && let Some(family) = font_at(bytes, at)
        {
            if !found.contains(&family) {
                found.push(family);
            }
            at += 12;
            continue;
        }
        at += 1;
    }
    found
}

fn be16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn be32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

/// The family name of the font whose table directory starts at `start`.
fn font_at(bytes: &[u8], start: usize) -> Option<String> {
    let tables = be16(bytes, start + 4)?;
    if !(4..=64).contains(&tables) {
        return None;
    }
    let power = 1u16 << (15 - tables.leading_zeros());
    if be16(bytes, start + 6)? != power * 16
        || be16(bytes, start + 8)? != power.trailing_zeros() as u16
        || be16(bytes, start + 10)? != tables * 16 - power * 16
    {
        return None;
    }
    let mut name = None;
    let mut has_cmap = false;
    let mut previous: &[u8] = b"";
    for index in 0..usize::from(tables) {
        let record = start + 12 + index * 16;
        let tag = bytes.get(record..record + 4)?;
        if !tag.iter().all(|c| (0x20..0x7f).contains(c)) || tag <= previous {
            return None;
        }
        previous = tag;
        let offset = be32(bytes, record + 8)? as usize;
        let length = be32(bytes, record + 12)? as usize;
        if start.checked_add(offset)?.checked_add(length)? > bytes.len() {
            return None;
        }
        match tag {
            b"cmap" => has_cmap = true,
            b"name" => name = Some((start + offset, length)),
            _ => {}
        }
    }
    if !has_cmap {
        return None;
    }
    let (table, length) = name?;
    family(bytes.get(table..table + length)?)
}

/// The family (name id 16, else 1) in a `name` table.
fn family(table: &[u8]) -> Option<String> {
    let count = usize::from(be16(table, 2)?);
    let strings = usize::from(be16(table, 4)?);
    let mut best: Option<(u8, String)> = None;
    for index in 0..count {
        let record = 6 + index * 12;
        let platform = be16(table, record)?;
        let id = be16(table, record + 6)?;
        let length = usize::from(be16(table, record + 8)?);
        let offset = usize::from(be16(table, record + 10)?);
        let rank = match id {
            16 => 2,
            1 => 1,
            _ => continue,
        };
        let raw = table.get(strings + offset..strings + offset + length)?;
        let text = match platform {
            0 | 3 => {
                let units: Vec<u16> = raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_be_bytes(*pair))
                    .collect();
                String::from_utf16_lossy(&units)
            }
            _ => raw.iter().map(|&b| char::from(b)).collect(),
        };
        if !text.trim().is_empty() && best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, text.trim().to_string()));
        }
    }
    best.map(|(_, name)| name)
}

/// The font families iced always embeds that draw no text: its icon font.
pub fn icon_only(family: &str) -> bool {
    family.eq_ignore_ascii_case("Iced-Icons")
}

/// Font files in a site (`.ttf`, `.otf`, `.woff`, `.woff2`), relative to
/// it: fonts an app loads at run time.
pub fn font_files(site: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![site.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let extension = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if ["ttf", "otf", "woff", "woff2"].contains(&extension.as_str())
                && let Ok(relative) = path.strip_prefix(site)
            {
                found.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hashed() -> Hashed {
        Hashed {
            js: "pkg/app-0123abcd.js".into(),
            wasm: "pkg/app_bg-89abcdef.wasm".into(),
        }
    }

    #[test]
    fn names_carry_their_hash() {
        let sha = "0123abcd".repeat(8);
        assert_eq!(hashed_name("app", "js", &sha), "pkg/app-0123abcd.js");
        assert_eq!(hash_in_name("pkg/app_bg-89abcdef.wasm"), Some("89abcdef"));
        assert_eq!(hash_in_name("pkg/app_bg.wasm"), None);
        assert_eq!(hash_in_name("pkg/app-xyz.js"), None);
        assert_eq!(hash_in_name("pkg/app-0123abc.js"), None);
    }

    #[test]
    fn base_and_prefix_come_from_public_url() {
        assert_eq!(base_href("/"), "/");
        assert_eq!(base_href("/app"), "/app/");
        assert_eq!(path_prefix("/app/"), "/app/");
        assert_eq!(path_prefix("https://cdn.example.com"), "/");
        assert_eq!(path_prefix("https://cdn.example.com/a/b"), "/a/b/");
    }

    #[test]
    fn the_index_loads_the_hashed_modules() {
        let html = index_html("Tom & Co", "A <b> app", "#112233", "/app", &hashed(), true);
        assert!(html.contains("<base href=\"/app/\">"), "{html}");
        assert!(html.contains("import init from \"./pkg/app-0123abcd.js\";"));
        assert!(html.contains("module_or_path: \"./pkg/app_bg-89abcdef.wasm\""));
        assert!(html.contains("<title>Tom &amp; Co</title>"));
        assert!(html.contains("content=\"A &lt;b&gt; app\""));
        assert!(html.contains("href=\"icon-32.png\""));
        assert!(!html.contains("/__icm/log"), "no dev forwarder");
        assert_eq!(parse_index(&html), Some(hashed()));
        let plain = index_html("A", "", "#FFFFFF", "/", &hashed(), false);
        assert!(plain.contains("href=\"data:,\""));
        assert!(!plain.contains("name=\"description\""));
        assert_eq!(parse_index("<html></html>"), None);
    }

    #[test]
    fn headers_type_and_cache_the_modules() {
        let text = headers("/", &hashed());
        assert_eq!(
            header_content_type(&text, "/pkg/app_bg-89abcdef.wasm").as_deref(),
            Some("application/wasm")
        );
        assert!(text.contains(
            "/pkg/app-0123abcd.js\n  Cache-Control: public, max-age=31536000, immutable"
        ));
        assert!(text.contains("/*\n  X-Content-Type-Options: nosniff\n\n"));
        assert!(text.contains("/index.html\n  Cache-Control: no-cache"));
        assert!(!text.contains('!'));
        assert_eq!(header_content_type(&text, "/index.html"), None);
        let nested = headers("/app/", &hashed());
        assert!(header_content_type(&nested, "/app/pkg/app_bg-89abcdef.wasm").is_some());

        let files = hosting_files("/app/", &hashed());
        let names: Vec<&str> = files.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["nginx.conf", "apache.htaccess", "Caddyfile"]);
        assert!(
            files[0]
                .1
                .contains("location ~ ^/app/pkg/app(_bg)?-[0-9a-f]{8}\\.(js|wasm)$ {")
        );
        assert!(files[1].1.contains("AddType application/wasm .wasm"));
        assert!(files[2].1.contains("@hashed path_regexp ^/app/pkg/"));
    }

    #[test]
    fn the_manifest_lists_the_icons() {
        let written: serde_json::Value =
            serde_json::from_str(&manifest("Notes", "Take notes", "#000000", true)).unwrap();
        assert_eq!(written["icons"].as_array().unwrap().len(), 3);
        assert_eq!(written["icons"][2]["purpose"], "maskable");
        assert_eq!(written["description"], "Take notes");
        let bare: serde_json::Value =
            serde_json::from_str(&manifest("Notes", "", "#000000", false)).unwrap();
        assert!(bare["icons"].as_array().unwrap().is_empty());
        assert!(bare.get("description").is_none());
    }

    #[test]
    fn icons_are_resized_flattened_and_padded() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("icon.png");
        // A transparent 1024 icon with an opaque blue centre.
        let mut image = Rgba::new(1024, 1024);
        for y in 256..768 {
            for x in 256..768 {
                image.set(x, y, [0, 0, 255, 255]);
            }
        }
        std::fs::write(&source, encode(&image).unwrap()).unwrap();
        let icons = icons(&source, [255, 0, 0]).unwrap();
        let names: Vec<&str> = icons.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "icon-32.png",
                "icon-192.png",
                "icon-512.png",
                "icon-180.png",
                "icon-maskable-512.png"
            ]
        );
        let get = |name: &str| decode(&icons.iter().find(|(n, _)| n == name).unwrap().1).unwrap();
        let favicon = get("icon-32.png");
        assert_eq!((favicon.width, favicon.height), (32, 32));
        assert_eq!(favicon.get(0, 0)[3], 0, "keeps transparency");
        let touch = get("icon-180.png");
        assert_eq!(touch.get(0, 0), [255, 0, 0, 255], "flattened");
        let maskable = get("icon-maskable-512.png");
        assert_eq!(maskable.get(0, 0), [255, 0, 0, 255]);
        assert_eq!(maskable.get(256, 256), [0, 0, 255, 255]);
    }

    #[test]
    fn wasm_opt_gets_the_features_rustc_compiled_for() {
        let cfg = "target_arch=\"wasm32\"\ntarget_feature=\"bulk-memory\"\ntarget_feature=\"multivalue\"\ntarget_feature=\"mutable-globals\"\ntarget_feature=\"nontrapping-fptoint\"\ntarget_feature=\"reference-types\"\ntarget_feature=\"sign-ext\"\ntarget_feature=\"wide-arithmetic\"\n";
        let help = " --enable-sign-ext   sign extension\n --enable-bulk-memory\n --enable-mutable-globals\n --enable-nontrapping-float-to-int,-enable-x\n --enable-reference-types\n --enable-multivalue\n --enable-simd\n";
        let (flags, skipped) = wasm_opt_flags(cfg, help);
        assert_eq!(
            flags,
            [
                "--enable-bulk-memory",
                "--enable-multivalue",
                "--enable-mutable-globals",
                "--enable-nontrapping-float-to-int",
                "--enable-reference-types",
                "--enable-sign-ext"
            ]
        );
        assert_eq!(skipped, ["wide-arithmetic"]);
    }

    /// A minimal TrueType file: a table directory with `cmap`, `head` and
    /// `name`, and a `name` table holding `family`.
    fn tiny_font(family: &str) -> Vec<u8> {
        let utf16: Vec<u8> = family.encode_utf16().flat_map(u16::to_be_bytes).collect();
        let mut name = Vec::new();
        name.extend(0u16.to_be_bytes()); // format
        name.extend(1u16.to_be_bytes()); // count
        name.extend(18u16.to_be_bytes()); // string offset
        for value in [3u16, 1, 0x409, 1, utf16.len() as u16, 0] {
            name.extend(value.to_be_bytes());
        }
        name.extend(&utf16);
        let tables: [(&[u8; 4], Vec<u8>); 5] = [
            (b"cmap", vec![0; 4]),
            (b"head", vec![0; 54]),
            (b"hhea", vec![0; 36]),
            (b"maxp", vec![0; 6]),
            (b"name", name),
        ];
        let mut font = Vec::new();
        font.extend([0, 1, 0, 0]);
        font.extend(5u16.to_be_bytes());
        font.extend(64u16.to_be_bytes()); // searchRange: 4 * 16
        font.extend(2u16.to_be_bytes()); // entrySelector
        font.extend(16u16.to_be_bytes()); // rangeShift: 5 * 16 - 64
        let mut offset = 12 + 16 * tables.len();
        let mut data: Vec<u8> = Vec::new();
        for (tag, table) in &tables {
            font.extend(*tag);
            font.extend(0u32.to_be_bytes());
            font.extend((offset as u32).to_be_bytes());
            font.extend((table.len() as u32).to_be_bytes());
            offset += table.len();
            data.extend(table);
        }
        font.extend(data);
        font
    }

    #[test]
    fn embedded_fonts_are_found_by_family() {
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        wasm.extend([0, 1, 0, 0, 0, 9, 9, 9]); // a false start
        wasm.extend(tiny_font("Iced-Icons"));
        wasm.extend(b"padding");
        wasm.extend(tiny_font("Fira Sans"));
        assert_eq!(fonts(&wasm), ["Iced-Icons", "Fira Sans"]);
        assert!(icon_only("Iced-Icons"));
        assert!(!icon_only("Fira Sans"));
        assert!(fonts(b"\0asm\x01\0\0\0 no fonts here").is_empty());
        // A truncated font is not one.
        let font = tiny_font("Cut");
        assert!(fonts(&font[..font.len() - 4]).is_empty());
    }

    #[test]
    fn iced_fonts_are_recognised() {
        let fonts_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../graphics/fonts");
        let mut wasm = b"\0asm\x01\0\0\0".to_vec();
        for font in ["Iced-Icons.ttf", "FiraSans-Regular.ttf"] {
            wasm.extend(std::fs::read(fonts_dir.join(font)).unwrap());
            wasm.extend([0u8; 3]);
        }
        assert_eq!(fonts(&wasm), ["Iced-Icons", "Fira Sans"]);
    }

    fn leb(mut value: u64, out: &mut Vec<u8>) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    /// A module whose data section holds `font` split around its runs of
    /// zeros, as wasm-opt's memory packing leaves it.
    fn packed_module(font: &[u8], base: u64) -> Vec<u8> {
        let mut segments: Vec<(u64, &[u8])> = Vec::new();
        let mut start = None;
        for (index, byte) in font.iter().enumerate() {
            match (start, *byte) {
                (None, b) if b != 0 => start = Some(index),
                (Some(from), 0) if font[index..].iter().take(8).all(|b| *b == 0) => {
                    segments.push((base + from as u64, &font[from..index]));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(from) = start {
            segments.push((base + from as u64, &font[from..]));
        }
        assert!(segments.len() > 1, "the font has zero runs to drop");
        let mut section = Vec::new();
        leb(segments.len() as u64 + 1, &mut section);
        for (offset, bytes) in &segments {
            section.push(0); // active, memory 0
            section.push(0x41); // i32.const
            leb(*offset, &mut section); // small enough to stay positive as sleb
            section.push(0x0b);
            leb(bytes.len() as u64, &mut section);
            section.extend(*bytes);
        }
        section.push(1); // a passive segment
        leb(4, &mut section);
        section.extend(b"pass");
        let mut module = b"\0asm\x01\0\0\0".to_vec();
        module.push(0); // a custom section first
        leb(3, &mut module);
        module.extend([2, b'h', b'i']);
        module.push(11);
        leb(section.len() as u64, &mut module);
        module.extend(section);
        module
    }

    #[test]
    fn fonts_survive_memory_packing() {
        let font = tiny_font("Packed Sans");
        let module = packed_module(&font, 1024);
        assert!(
            fonts(&module).is_empty(),
            "split, the raw bytes hold no font"
        );
        let images = data_image(&module).unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[1], b"pass");
        assert_eq!(wasm_fonts(&module), ["Packed Sans"]);
        // Not a module: scanned as it is.
        assert_eq!(wasm_fonts(&font), ["Packed Sans"]);
        assert!(data_image(b"\0asm\x01\0\0\0\x0b\x05\x01\x00\x41").is_none());
    }

    #[test]
    fn font_files_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("assets/fonts")).unwrap();
        std::fs::write(dir.path().join("assets/fonts/Inter.TTF"), "x").unwrap();
        std::fs::write(dir.path().join("index.html"), "x").unwrap();
        assert_eq!(font_files(dir.path()), ["assets/fonts/Inter.TTF"]);
    }
}
