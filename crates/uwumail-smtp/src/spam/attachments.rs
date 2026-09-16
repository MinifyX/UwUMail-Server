//! Attachments that are a common way in for malware and fake login pages: programs, Office files with
//! macros and web pages, also inside zip archives, and names dressed up as something harmless. The
//! endings are the ones the UwUMail apps ask about before opening a file.

use std::io::Cursor;

use mail_parser::{Message, MimeHeaders};

use super::Hit;

/// Files that run code when opened: programs, scripts, installers, shortcuts, disk images and app
/// packages.
const PROGRAMS: &[&str] = &[
    "exe",
    "com",
    "bat",
    "cmd",
    "msi",
    "msix",
    "msixbundle",
    "appx",
    "appxbundle",
    "appref-ms",
    "application",
    "msp",
    "mst",
    "scr",
    "pif",
    "cpl",
    "lnk",
    "url",
    "reg",
    "inf",
    "ins",
    "isp",
    "hta",
    "chm",
    "hlp",
    "msc",
    "scf",
    "settingcontent-ms",
    "library-ms",
    "diagcab",
    "gadget",
    "js",
    "jse",
    "vbs",
    "vbe",
    "wsf",
    "wsh",
    "wsc",
    "sct",
    "ps1",
    "ps1xml",
    "ps2",
    "psc1",
    "psd1",
    "psm1",
    "jar",
    "jnlp",
    "app",
    "dmg",
    "pkg",
    "command",
    "sh",
    "run",
    "appimage",
    "deb",
    "rpm",
    "iso",
    "img",
    "vhd",
    "vhdx",
    "apk",
    "apks",
    "apkm",
    "xapk",
    "aab",
];

/// Office files that can carry macros or pull in outside data.
const MACROS: &[&str] =
    &["docm", "dotm", "xlsm", "xltm", "xlam", "xll", "pptm", "potm", "ppam", "sldm", "one", "iqy", "slk"];

/// Web pages as attachments, a favourite way to bring a fake login page past link checks.
const WEB_PAGES: &[&str] = &["html", "htm", "xhtml", "shtml", "mht", "mhtml"];

/// Endings a disguised program pretends to have.
const HARMLESS_LOOKING: &[&str] = &[
    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "txt", "rtf", "jpg", "jpeg", "png", "gif", "mp3", "mp4",
];

/// Zip archives larger than this are not opened, and neither are ones with more entries.
const MAX_ZIP: usize = 25 * 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 10_000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Program,
    Macro,
    WebPage,
    Other,
}

/// Characters that reverse how text is shown, e.g. to show `rechnung\u{202E}fdp.exe` as "rechnungexe.pdf".
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A file name as it can be shown in a log: no direction tricks, no control characters, not too long.
fn shown(name: &str) -> String {
    name.chars().filter(|c| !is_bidi_control(*c)).map(|c| if c.is_control() { ' ' } else { c }).take(100).collect()
}

/// The ending Windows would go by: it ignores trailing dots and spaces, so "tool.exe. " is tool.exe.
fn ending(name: &str) -> Option<String> {
    let name: String = name.chars().filter(|c| !is_bidi_control(*c)).collect();
    let name = name.rsplit(['/', '\u{5c}']).next().unwrap_or_default().trim_end_matches(['.', ' ']);
    name.rsplit_once('.').map(|(_, ending)| ending.to_ascii_lowercase())
}

fn kind(name: &str) -> Kind {
    match ending(name) {
        Some(ending) if PROGRAMS.contains(&ending.as_str()) => Kind::Program,
        Some(ending) if MACROS.contains(&ending.as_str()) => Kind::Macro,
        Some(ending) if WEB_PAGES.contains(&ending.as_str()) => Kind::WebPage,
        _ => Kind::Other,
    }
}

/// A name dressed up as something else: direction marks that turn the ending around, a harmless-looking
/// ending right before the real one ("rechnung.pdf.exe"), or a long gap of spaces that pushes the real
/// ending out of view ("rechnung.pdf          .exe").
fn is_disguised(name: &str) -> bool {
    if name.chars().any(is_bidi_control) {
        return true;
    }
    let trimmed = name.trim_end_matches(['.', ' ']);
    let Some((stem, _)) = trimmed.rsplit_once('.') else { return false };
    if stem.ends_with("   ") {
        return true;
    }
    stem.trim_end()
        .rsplit_once('.')
        .is_some_and(|(_, inner)| HARMLESS_LOOKING.contains(&inner.to_ascii_lowercase().as_str()))
}

/// The first program or macro file inside a zip archive, read from its directory without unpacking.
/// Encrypted archives still show their file names, so they are covered too.
fn program_in_zip(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_ZIP {
        return None;
    }
    let archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    if archive.len() > MAX_ZIP_ENTRIES {
        return None;
    }
    let found = archive.file_names().find(|name| matches!(kind(name), Kind::Program | Kind::Macro))?;
    Some(shown(found))
}

fn once(hits: &mut Vec<Hit>, rule: &'static str, points: f32, detail: String) {
    if !hits.iter().any(|hit| hit.rule == rule) {
        hits.push(Hit { rule, points, detail: Some(detail) });
    }
}

pub(crate) fn judge(message: &Message<'_>) -> Vec<Hit> {
    let mut hits = Vec::new();
    for part in message.attachments() {
        let Some(name) = part.attachment_name() else { continue };
        let kind = kind(name);
        match kind {
            Kind::Program => once(&mut hits, "EXECUTABLE_ATTACHMENT", 3.0, shown(name)),
            Kind::Macro => once(&mut hits, "MACRO_ATTACHMENT", 2.0, shown(name)),
            Kind::WebPage => once(&mut hits, "HTML_ATTACHMENT", 1.5, shown(name)),
            Kind::Other => {}
        }
        if matches!(kind, Kind::Program | Kind::Macro) && is_disguised(name) {
            once(&mut hits, "DISGUISED_ATTACHMENT", 2.0, shown(name));
        }
        let zipped = ending(name).as_deref() == Some("zip")
            || part.content_type().is_some_and(|ct| ct.subtype().is_some_and(|sub| sub.contains("zip")));
        if zipped && let Some(inside) = program_in_zip(part.contents()) {
            once(&mut hits, "ARCHIVE_WITH_PROGRAM", 3.0, format!("{}: {inside}", shown(name)));
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use base64::Engine;
    use mail_parser::MessageParser;

    use super::*;

    /// A message with these attachments, each given as (file name, content).
    fn message_with(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut raw = String::from(
            "From: a@example.com\r\nSubject: Rechnung\r\nMIME-Version: 1.0\r\n\
             Content-Type: multipart/mixed; boundary=\"b\"\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nAnbei.\r\n",
        );
        for (name, content) in files {
            let encoded = base64::engine::general_purpose::STANDARD.encode(content);
            raw.push_str(&format!(
                "--b\r\nContent-Type: application/octet-stream\r\n\
                 Content-Disposition: attachment; filename=\"{name}\"\r\n\
                 Content-Transfer-Encoding: base64\r\n\r\n{encoded}\r\n"
            ));
        }
        raw.push_str("--b--\r\n");
        raw.into_bytes()
    }

    fn rules(raw: &[u8]) -> Vec<&'static str> {
        let message = MessageParser::default().parse(raw).unwrap();
        judge(&message).into_iter().map(|hit| hit.rule).collect()
    }

    #[test]
    fn programs_macros_and_web_pages_count_once_each() {
        let raw = message_with(&[
            ("setup.exe", b"MZ"),
            ("tool.EXE. ", b"MZ"),
            ("umsatz.xlsm", b"PK"),
            ("login.html", b"<form>"),
        ]);
        assert_eq!(rules(&raw), ["EXECUTABLE_ATTACHMENT", "MACRO_ATTACHMENT", "HTML_ATTACHMENT"]);
        assert!(rules(&message_with(&[("rechnung.pdf", b"%PDF"), ("foto.jpg", b"jpg")])).is_empty());
    }

    #[test]
    fn dressed_up_names_are_noticed() {
        for name in ["rechnung.pdf.exe", "rechnung.pdf          .exe", "rechnung\u{202E}fdp.exe"] {
            assert_eq!(
                rules(&message_with(&[(name, b"MZ")])),
                ["EXECUTABLE_ATTACHMENT", "DISGUISED_ATTACHMENT"],
                "{name}"
            );
        }
        assert!(!is_disguised("version.2.exe"));
    }

    #[test]
    fn programs_inside_zip_archives_are_found_without_unpacking() {
        let zipped = |names: &[&str]| {
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for name in names {
                writer.start_file(*name, options).unwrap();
                writer.write_all(b"data").unwrap();
            }
            writer.finish().unwrap().into_inner()
        };
        let evil = zipped(&["liesmich.txt", "ordner/rechnung.js"]);
        assert_eq!(rules(&message_with(&[("rechnung.zip", &evil)])), ["ARCHIVE_WITH_PROGRAM"]);
        let fine = zipped(&["fotos/urlaub.jpg", "liste.xlsx"]);
        assert!(rules(&message_with(&[("fotos.zip", &fine)])).is_empty());
        // Not a zip at all: nothing to find, nothing to fail.
        assert!(rules(&message_with(&[("kaputt.zip", b"PK not really")])).is_empty());
    }
}
