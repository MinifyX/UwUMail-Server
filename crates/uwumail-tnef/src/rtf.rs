//! Compressed RTF (MS-OXRTFCP) and what is in it: HTML or text that Outlook wrapped in RTF
//! (MS-OXRTFEX), or real RTF, turned into text and simple HTML.
//!
//! The interpreter reads RTF in one pass with a stack of its own (never recursion), bounded in
//! depth, and never writes more than it is allowed to.

use crate::codepage;
use crate::html::escape;

/// The dictionary compressed RTF starts with (MS-OXRTFCP 2.1.2.1), 207 bytes.
const PREBUF: &[u8] = b"{\\rtf1\\ansi\\mac\\deff0\\deftab720{\\fonttbl;}{\\f0\\fnil \\froman \\fswiss \\fmodern \\fscript \\fdecor MS Sans SerifSymbolArialTimes New RomanCourier{\\colortbl\\red0\\green0\\blue0\r\n\\par \\pard\\plain\\f0\\fs20\\b\\i\\u\\tab\\tx";
const LZFU: u32 = 0x7546_5A4C;
const MELA: u32 = 0x414C_454D;

/// Why compressed RTF could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtfError {
    /// Shorter than its header, or neither `LZFu` nor `MELA`.
    Malformed,
    /// It would unpack to more than allowed.
    TooLarge,
}

/// Unpacks compressed RTF (`PR_RTF_COMPRESSED`) to at most `max` bytes. The CRC is not checked:
/// a wrong one only costs a garbled body, never more.
pub fn decompress(data: &[u8], max: usize) -> Result<Vec<u8>, RtfError> {
    if data.len() < 16 {
        return Err(RtfError::Malformed);
    }
    let word = |i: usize| u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
    let comp_size = word(0) as usize;
    let raw_size = word(4) as usize;
    let magic = word(8);
    let end = comp_size.saturating_add(4).min(data.len()).max(16);
    let body = &data[16..end];
    if magic == MELA {
        let n = raw_size.min(body.len());
        if n > max {
            return Err(RtfError::TooLarge);
        }
        return Ok(body[..n].to_vec());
    }
    if magic != LZFU {
        return Err(RtfError::Malformed);
    }
    let mut dict = [0u8; 4096];
    dict[..PREBUF.len()].copy_from_slice(PREBUF);
    let mut write = PREBUF.len();
    let mut out = Vec::with_capacity(raw_size.min(max).min(body.len().saturating_mul(8)));
    let mut input = body.iter().copied();
    'outer: while let Some(control) = input.next() {
        for bit in 0..8 {
            if control & (1 << bit) == 0 {
                let Some(byte) = input.next() else { break 'outer };
                out.push(byte);
                dict[write] = byte;
                write = (write + 1) % 4096;
            } else {
                let (Some(hi), Some(lo)) = (input.next(), input.next()) else { break 'outer };
                let reference = (usize::from(hi) << 8) | usize::from(lo);
                let offset = reference >> 4;
                let len = (reference & 0xF) + 2;
                if offset == write {
                    break 'outer;
                }
                for k in 0..len {
                    let byte = dict[(offset + k) % 4096];
                    out.push(byte);
                    dict[write] = byte;
                    write = (write + 1) % 4096;
                }
            }
            if out.len() > max {
                return Err(RtfError::TooLarge);
            }
        }
    }
    Ok(out)
}

/// What an RTF body is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// HTML that Outlook wrapped in RTF (`\fromhtml1`), as it was.
    Html(String),
    /// Plain text that Outlook wrapped in RTF (`\fromtext`).
    Text(String),
    /// Real RTF: its text, and simple HTML keeping paragraphs, bold, italic, underline,
    /// strike-through and links.
    Rtf { text: String, html: String },
}

/// How deep RTF groups may nest; deeper ones are read as part of the group at this depth.
const MAX_DEPTH: usize = 512;
/// Text of one field instruction (`HYPERLINK "…"`) kept at most.
const MAX_FIELD: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Html,
    Text,
    Rtf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Format {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

#[derive(Debug, Clone, Copy)]
struct Group {
    /// A destination whose text is not shown.
    skip: bool,
    htmltag: bool,
    htmlrtf: bool,
    fldinst: bool,
    /// This group opened a link (`\fldrslt` of a HYPERLINK).
    link: bool,
    uc: u32,
    format: Format,
}

impl Default for Group {
    fn default() -> Self {
        Group { skip: false, htmltag: false, htmlrtf: false, fldinst: false, link: false, uc: 1, format: Format::default() }
    }
}

const SKIPPED: &[&str] = &[
    "fonttbl",
    "colortbl",
    "stylesheet",
    "info",
    "pict",
    "object",
    "header",
    "headerl",
    "headerr",
    "headerf",
    "footer",
    "footerl",
    "footerr",
    "footerf",
    "footnote",
    "listtable",
    "listoverridetable",
    "rsidtbl",
    "generator",
    "xmlnstbl",
    "themedata",
    "colorschememapping",
    "latentstyles",
    "datastore",
    "mmathPr",
    "pgdsctbl",
    "filetbl",
    "revtbl",
    "bkmkstart",
    "bkmkend",
    "mhtmltag",
    "fldtype",
    "datafield",
    "xe",
    "tc",
    "txe",
    "private",
];

struct Out {
    mode: Mode,
    max: usize,
    code_page: u32,
    /// Bytes of the code page waiting to become text.
    pending: Vec<u8>,
    high_surrogate: Option<u16>,
    /// HTML (Html mode) or plain text (Text and Rtf modes).
    text: String,
    /// Rtf mode: finished paragraphs, and the one being written.
    html: String,
    para: String,
    open: Format,
    in_link: bool,
    field: String,
    full: bool,
}

impl Out {
    fn flush(&mut self, format: Format) {
        if self.pending.is_empty() {
            return;
        }
        let bytes = std::mem::take(&mut self.pending);
        let text = codepage::decode(self.code_page, &bytes);
        self.write(&text, format);
    }

    fn byte(&mut self, byte: u8, format: Format) {
        if self.pending.len() >= 4096 {
            self.flush(format);
        }
        self.pending.push(byte);
    }

    fn write(&mut self, text: &str, format: Format) {
        if self.full {
            return;
        }
        let mut room = self.max.saturating_sub(self.text.len() + self.html.len() + self.para.len());
        if self.mode == Mode::Rtf {
            // The text goes into the HTML as well.
            room /= 2;
        }
        let text = if text.len() > room {
            self.full = true;
            let mut cut = room;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            &text[..cut]
        } else {
            text
        };
        self.text.push_str(text);
        if self.mode == Mode::Rtf {
            self.set_format(format);
            self.para.push_str(&escape(text));
        }
    }

    fn set_format(&mut self, format: Format) {
        if format == self.open {
            return;
        }
        self.close_format();
        let tags = [(format.bold, "b"), (format.italic, "i"), (format.underline, "u"), (format.strike, "s")];
        for (on, tag) in tags {
            if on {
                self.para.push('<');
                self.para.push_str(tag);
                self.para.push('>');
            }
        }
        self.open = format;
    }

    fn close_format(&mut self) {
        let f = self.open;
        let tags = [(f.strike, "s"), (f.underline, "u"), (f.italic, "i"), (f.bold, "b")];
        for (on, tag) in tags {
            if on {
                self.para.push_str("</");
                self.para.push_str(tag);
                self.para.push('>');
            }
        }
        self.open = Format::default();
    }

    fn paragraph(&mut self) {
        match self.mode {
            Mode::Html => self.write_raw("\r\n"),
            Mode::Text => self.write_raw("\n"),
            Mode::Rtf => {
                if self.full {
                    return;
                }
                self.text.push('\n');
                self.close_format();
                let was_link = self.in_link;
                if was_link {
                    self.para.push_str("</a>");
                }
                if self.para.is_empty() {
                    self.html.push_str("<div><br></div>\n");
                } else {
                    self.html.push_str("<div>");
                    self.html.push_str(&self.para);
                    self.html.push_str("</div>\n");
                }
                self.para.clear();
                if was_link {
                    self.in_link = false;
                }
            }
        }
    }

    fn line(&mut self) {
        match self.mode {
            Mode::Rtf => {
                self.text.push('\n');
                self.close_format();
                self.para.push_str("<br>");
            }
            _ => self.paragraph(),
        }
    }

    fn write_raw(&mut self, text: &str) {
        if self.text.len() + text.len() > self.max {
            self.full = true;
            return;
        }
        self.text.push_str(text);
    }

    fn start_link(&mut self, url: &str) {
        if self.mode != Mode::Rtf || self.in_link {
            return;
        }
        self.close_format();
        self.para.push_str("<a href=\"");
        self.para.push_str(&escape(url));
        self.para.push_str("\">");
        self.in_link = true;
    }

    fn end_link(&mut self) {
        if self.in_link {
            self.close_format();
            self.para.push_str("</a>");
            self.in_link = false;
        }
    }
}

/// The URL of a `HYPERLINK "…"` field instruction, when it is one that may be linked.
fn hyperlink(instruction: &str) -> Option<String> {
    let rest = instruction.trim_start().strip_prefix("HYPERLINK")?.trim_start();
    let url = if let Some(quoted) = rest.strip_prefix('"') {
        quoted.split('"').next()?
    } else {
        rest.split_whitespace().next()?
    };
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:"))
        .then(|| url.to_owned())
}

/// Reads an RTF document; its output is at most about `max` bytes.
pub fn convert(rtf: &[u8], max: usize) -> Content {
    let head = &rtf[..rtf.len().min(4096)];
    let has = |needle: &[u8]| head.windows(needle.len()).any(|w| w == needle);
    let mode = if has(b"\\fromhtml") {
        Mode::Html
    } else if has(b"\\fromtext") {
        Mode::Text
    } else {
        Mode::Rtf
    };
    let mut out = Out {
        mode,
        max,
        code_page: 1252,
        pending: Vec::new(),
        high_surrogate: None,
        text: String::new(),
        html: String::new(),
        para: String::new(),
        open: Format::default(),
        in_link: false,
        field: String::new(),
        full: false,
    };
    let mut stack: Vec<Group> = Vec::new();
    let mut group = Group::default();
    // Groups deeper than MAX_DEPTH are counted, not kept.
    let mut excess = 0usize;
    // Characters still to skip after `\uN`.
    let mut skip_chars = 0u32;
    let mut star = false;
    let mut i = 0usize;

    // Whether text in the current state is shown.
    let visible = |g: &Group| -> bool {
        if g.skip || g.fldinst {
            return false;
        }
        match mode {
            Mode::Html => g.htmltag || !g.htmlrtf,
            _ => true,
        }
    };

    while i < rtf.len() && !out.full {
        let c = rtf[i];
        match c {
            b'{' => {
                i += 1;
                out.flush(group.format);
                if stack.len() >= MAX_DEPTH {
                    excess += 1;
                } else {
                    stack.push(group);
                    group.link = false;
                }
                star = false;
            }
            b'}' => {
                i += 1;
                if !group.fldinst {
                    out.flush(group.format);
                } else {
                    out.pending.clear();
                }
                if excess > 0 {
                    excess -= 1;
                } else if let Some(parent) = stack.pop() {
                    if group.link && !parent.link {
                        out.end_link();
                    }
                    group = parent;
                }
                star = false;
            }
            b'\\' => {
                i += 1;
                let Some(&next) = rtf.get(i) else { break };
                if next.is_ascii_alphabetic() {
                    let start = i;
                    while i < rtf.len() && rtf[i].is_ascii_alphabetic() && i - start < 32 {
                        i += 1;
                    }
                    let word = std::str::from_utf8(&rtf[start..i]).unwrap_or("");
                    let mut param: Option<i32> = None;
                    let negative = rtf.get(i) == Some(&b'-');
                    if negative {
                        i += 1;
                    }
                    let digits_start = i;
                    let mut value: i64 = 0;
                    while i < rtf.len() && rtf[i].is_ascii_digit() && i - digits_start < 10 {
                        value = value * 10 + i64::from(rtf[i] - b'0');
                        i += 1;
                    }
                    if i > digits_start {
                        let v = if negative { -value } else { value };
                        param = Some(v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32);
                    }
                    if rtf.get(i) == Some(&b' ') {
                        i += 1;
                    }
                    let was_star = std::mem::take(&mut star);
                    // A control word counts as one of the characters `\uN` skips.
                    if skip_chars > 0 {
                        skip_chars -= 1;
                        if word != "bin" {
                            continue;
                        }
                    }
                    let on = param.is_none_or(|p| p != 0);
                    match word {
                        "htmltag" => group.htmltag = true,
                        "fldinst" => {
                            out.flush(group.format);
                            group.fldinst = true;
                            out.field.clear();
                        }
                        "fldrslt" => {
                            if let Some(url) = hyperlink(&out.field) {
                                out.flush(group.format);
                                out.start_link(&url);
                                group.link = true;
                            }
                            out.field.clear();
                        }
                        w if SKIPPED.contains(&w) => group.skip = true,
                        _ if was_star => group.skip = true,
                        "htmlrtf" => {
                            out.flush(group.format);
                            group.htmlrtf = on;
                        }
                        "ansicpg" => {
                            if let Some(p) = param.filter(|p| *p > 0) {
                                out.code_page = p as u32;
                            }
                        }
                        "ansi" => out.code_page = 1252,
                        "mac" => out.code_page = 10000,
                        "pc" => out.code_page = 437,
                        "pca" => out.code_page = 850,
                        "uc" => group.uc = param.unwrap_or(1).clamp(0, 10) as u32,
                        "u" => {
                            if let Some(p) = param {
                                let unit = if p < 0 { (p + 65536) as u16 } else { p as u16 };
                                if visible(&group) {
                                    out.flush(group.format);
                                    emit_unit(&mut out, unit, group.format);
                                }
                                skip_chars = group.uc;
                            }
                        }
                        "bin" => {
                            let n = param.unwrap_or(0).max(0) as usize;
                            i = i.saturating_add(n).min(rtf.len());
                        }
                        "par" | "sect" | "page" | "row" => {
                            if visible(&group) {
                                out.flush(group.format);
                                out.paragraph();
                            }
                        }
                        "line" => {
                            if visible(&group) {
                                out.flush(group.format);
                                out.line();
                            }
                        }
                        "tab" | "cell" => emit_char(&mut out, &group, visible(&group), "\t"),
                        "emdash" => emit_char(&mut out, &group, visible(&group), "\u{2014}"),
                        "endash" => emit_char(&mut out, &group, visible(&group), "\u{2013}"),
                        "bullet" => emit_char(&mut out, &group, visible(&group), "\u{2022}"),
                        "lquote" => emit_char(&mut out, &group, visible(&group), "\u{2018}"),
                        "rquote" => emit_char(&mut out, &group, visible(&group), "\u{2019}"),
                        "ldblquote" => emit_char(&mut out, &group, visible(&group), "\u{201C}"),
                        "rdblquote" => emit_char(&mut out, &group, visible(&group), "\u{201D}"),
                        "plain" => {
                            out.flush(group.format);
                            group.format = Format::default();
                        }
                        "b" => {
                            out.flush(group.format);
                            group.format.bold = on;
                        }
                        "i" => {
                            out.flush(group.format);
                            group.format.italic = on;
                        }
                        "ul" => {
                            out.flush(group.format);
                            group.format.underline = on;
                        }
                        "ulnone" => {
                            out.flush(group.format);
                            group.format.underline = false;
                        }
                        "strike" => {
                            out.flush(group.format);
                            group.format.strike = on;
                        }
                        _ => {}
                    }
                } else {
                    i += 1;
                    match next {
                        b'*' => star = true,
                        b'\'' => {
                            let hex = rtf.get(i..i + 2).and_then(|h| std::str::from_utf8(h).ok());
                            let byte = hex.and_then(|h| u8::from_str_radix(h, 16).ok());
                            if byte.is_some() {
                                i += 2;
                            }
                            if skip_chars > 0 {
                                skip_chars -= 1;
                            } else if let Some(byte) = byte {
                                text_byte(&mut out, &group, visible(&group), byte);
                            }
                        }
                        b'\\' | b'{' | b'}' => {
                            if skip_chars > 0 {
                                skip_chars -= 1;
                            } else {
                                text_byte(&mut out, &group, visible(&group), next);
                            }
                        }
                        b'~' => emit_char(&mut out, &group, visible(&group), "\u{00A0}"),
                        b'_' => emit_char(&mut out, &group, visible(&group), "\u{2011}"),
                        b'\r' | b'\n' => {
                            if visible(&group) {
                                out.flush(group.format);
                                out.paragraph();
                            }
                        }
                        _ => {}
                    }
                }
            }
            b'\r' | b'\n' => i += 1,
            _ => {
                i += 1;
                if skip_chars > 0 {
                    skip_chars -= 1;
                    continue;
                }
                text_byte(&mut out, &group, visible(&group), c);
            }
        }
    }
    out.flush(group.format);
    match mode {
        Mode::Html => Content::Html(out.text),
        Mode::Text => Content::Text(out.text),
        Mode::Rtf => {
            out.end_link();
            if !out.para.is_empty() {
                out.close_format();
                out.html.push_str("<div>");
                let para = std::mem::take(&mut out.para);
                out.html.push_str(&para);
                out.html.push_str("</div>\n");
            }
            Content::Rtf { text: out.text.trim_end().to_owned(), html: out.html }
        }
    }
}

fn text_byte(out: &mut Out, group: &Group, visible: bool, byte: u8) {
    if group.fldinst && !group.skip {
        if out.field.len() < MAX_FIELD {
            out.field.push(char::from(byte));
        }
        return;
    }
    if visible {
        if out.high_surrogate.take().is_some() {
            out.write("\u{FFFD}", group.format);
        }
        out.byte(byte, group.format);
    }
}

fn emit_char(out: &mut Out, group: &Group, visible: bool, text: &str) {
    if visible {
        out.flush(group.format);
        out.write(text, group.format);
    }
}

fn emit_unit(out: &mut Out, unit: u16, format: Format) {
    match unit {
        0xD800..=0xDBFF => {
            if out.high_surrogate.replace(unit).is_some() {
                out.write("\u{FFFD}", format);
            }
        }
        0xDC00..=0xDFFF => {
            let text = match out.high_surrogate.take() {
                Some(high) => String::from_utf16_lossy(&[high, unit]),
                None => "\u{FFFD}".to_owned(),
            };
            out.write(&text, format);
        }
        _ => {
            if out.high_surrogate.take().is_some() {
                out.write("\u{FFFD}", format);
            }
            let text = char::from_u32(u32::from(unit)).unwrap_or('\u{FFFD}').to_string();
            out.write(&text, format);
        }
    }
}

#[cfg(any(test, feature = "builder"))]
/// Packs RTF the way Outlook does (`LZFu`), with a simple search for repeats. For tests.
pub fn compress(rtf: &[u8]) -> Vec<u8> {
    let mut dict = [0u8; 4096];
    dict[..PREBUF.len()].copy_from_slice(PREBUF);
    let mut write = PREBUF.len();
    // How much of the dictionary holds something: all of it once it has wrapped around.
    let mut filled = PREBUF.len();
    let mut body = Vec::new();
    let mut pos = 0usize;
    let mut done = false;
    while !done {
        let mut control = 0u8;
        let mut chunk = Vec::new();
        for bit in 0..8 {
            if pos >= rtf.len() {
                // The end: a reference to where the next byte would be written.
                control |= 1 << bit;
                let reference = (write << 4) as u16;
                chunk.extend(reference.to_be_bytes());
                done = true;
                break;
            }
            // The longest earlier run (2–17 bytes) that does not overlap what it writes.
            let mut best = (0usize, 0usize);
            let max_len = (rtf.len() - pos).min(17);
            if max_len >= 2 {
                for back in 1..filled.min(4095) {
                    let offset = (write + 4096 - back) % 4096;
                    let limit = max_len.min(back);
                    let mut len = 0;
                    while len < limit && dict[(offset + len) % 4096] == rtf[pos + len] {
                        len += 1;
                    }
                    if len > best.1 && offset != write {
                        best = (offset, len);
                    }
                }
            }
            let emitted = if best.1 >= 2 {
                control |= 1 << bit;
                let reference = ((best.0 << 4) | (best.1 - 2)) as u16;
                chunk.extend(reference.to_be_bytes());
                best.1
            } else {
                chunk.push(rtf[pos]);
                1
            };
            for k in 0..emitted {
                dict[write] = rtf[pos + k];
                write = (write + 1) % 4096;
                filled = (filled + 1).min(4096);
            }
            pos += emitted;
        }
        body.push(control);
        body.extend(chunk);
    }
    let mut out = Vec::with_capacity(body.len() + 16);
    out.extend(((body.len() + 12) as u32).to_le_bytes());
    out.extend((rtf.len() as u32).to_le_bytes());
    out.extend(LZFU.to_le_bytes());
    out.extend(crc32(&body).to_le_bytes());
    out.extend(body);
    out
}

/// The CRC of MS-OXRTFCP (CRC-32 without the final inversion, starting at 0).
#[cfg(any(test, feature = "builder"))]
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dictionary_has_the_size_the_spec_gives() {
        assert_eq!(PREBUF.len(), 207);
    }

    /// The example of MS-OXRTFCP 3.1.1 (compressing "{\rtf1\ansi\ansicpg1252\pard hello world}\r\n").
    #[test]
    fn the_specification_example() {
        let packed: [u8; 0x2D] = [
            0x2d, 0x00, 0x00, 0x00, 0x2b, 0x00, 0x00, 0x00, 0x4c, 0x5a, 0x46, 0x75, 0xf1, 0xc5, 0xc7, 0xa7, 0x03,
            0x00, 0x0a, 0x00, 0x72, 0x63, 0x70, 0x67, 0x31, 0x32, 0x35, 0x42, 0x32, 0x0a, 0xf3, 0x20, 0x68, 0x65,
            0x6c, 0x09, 0x00, 0x20, 0x62, 0x77, 0x05, 0xb0, 0x6c, 0x64, 0x7d,
        ];
        let mut packed = packed.to_vec();
        packed.extend([0x0a, 0x80, 0x0f, 0xa0]);
        assert_eq!(decompress(&packed, 1 << 20).unwrap(), b"{\\rtf1\\ansi\\ansicpg1252\\pard hello world}\r\n");
    }

    #[test]
    fn compress_round_trips() {
        let rtf = b"{\\rtf1\\ansi\\ansicpg1252\\fromhtml1 {\\*\\htmltag64 <p>}hello hello hello{\\*\\htmltag72 </p>}}";
        let packed = compress(rtf);
        assert_eq!(decompress(&packed, 1 << 20).unwrap(), rtf);
        let long: Vec<u8> = (0..20_000u32).map(|i| b"abcdefgh {}\\"[(i as usize * 7 + i as usize / 13) % 12]).collect();
        assert_eq!(decompress(&compress(&long), 1 << 20).unwrap(), long);
        assert_eq!(decompress(&compress(&long), 100), Err(RtfError::TooLarge));
        assert_eq!(decompress(b"short", 100), Err(RtfError::Malformed));
    }

    #[test]
    fn uncompressed_rtf() {
        let mut data = Vec::new();
        data.extend(17u32.to_le_bytes());
        data.extend(5u32.to_le_bytes());
        data.extend(MELA.to_le_bytes());
        data.extend(0u32.to_le_bytes());
        data.extend(b"{\\rtf}");
        assert_eq!(decompress(&data, 100).unwrap(), b"{\\rtf");
    }

    #[test]
    fn encapsulated_html() {
        let rtf = br#"{\rtf1\ansi\ansicpg1252\fromhtml1 \deff0{\fonttbl{\f0\fswiss Arial;}}
{\*\htmltag19 <html>}{\*\htmltag34 <head>}{\*\htmltag41 <style>}{\*\htmltag241 p \{ color: red \}}{\*\htmltag49 </style>}
{\*\htmltag50 <body>}
{\*\htmltag64 <p>}\htmlrtf {\htmlrtf0 Gr\'fc\'dfe \u8364?\htmlrtf\par}\htmlrtf0
{\*\mhtmltag84 <a href="cid:orig">}{\*\htmltag84 <a href="https://example.com/a?b=1&amp;c">}Link{\*\htmltag92 </a>}
{\*\htmltag72 </p>}\htmlrtf {\b hidden}\htmlrtf0 {\*\htmltag58 </body>}{\*\htmltag27 </html>}}"#;
        let Content::Html(html) = convert(rtf, 1 << 20) else { panic!("html") };
        assert!(html.contains("<style>p { color: red }</style>"), "{html}");
        assert!(html.contains("<p>Grüße €"), "{html}");
        assert!(html.contains(r#"<a href="https://example.com/a?b=1&amp;c">Link</a>"#), "{html}");
        assert!(!html.contains("cid:orig"));
        assert!(!html.contains("Arial"));
        assert!(!html.contains("hidden"));
    }

    #[test]
    fn real_rtf() {
        let rtf = br#"{\rtf1\ansi\ansicpg1251\deff0{\fonttbl{\f0 Arial;}}{\colortbl;\red255\green0\blue0;}
{\*\generator Riched20;}\pard Hallo {\b fett} und {\i kursiv\i0  normal}\par
\'cf\'f0\'e8\u1074?\line Zeile\par
{\field{\*\fldinst{HYPERLINK "https://example.com/x?a=1&b=2"}}{\fldrslt{Beispiel}}}\par
{\field{\*\fldinst{HYPERLINK "javascript:alert(1)"}}{\fldrslt{Bad}}}<script>\par}"#;
        let Content::Rtf { text, html } = convert(rtf, 1 << 20) else { panic!("rtf") };
        assert_eq!(text, "Hallo fett und kursiv normal\nПрив\nZeile\nBeispiel\nBad<script>");
        assert!(html.contains("<div>Hallo <b>fett</b> und <i>kursiv</i> normal</div>"), "{html}");
        assert!(html.contains("Прив<br>Zeile"), "{html}");
        assert!(html.contains(r#"<a href="https://example.com/x?a=1&amp;b=2">Beispiel</a>"#), "{html}");
        assert!(!html.contains("javascript"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn encapsulated_text() {
        let Content::Text(text) = convert(br"{\rtf1\ansi\fromtext {\fonttbl{\f0 x;}}Hallo\par Welt}", 1000) else {
            panic!("text")
        };
        assert_eq!(text, "Hallo\nWelt");
    }

    #[test]
    fn output_is_bounded_and_nesting_is_not_recursion() {
        let mut deep = b"{\\rtf1 ".to_vec();
        deep.extend(std::iter::repeat_n(b'{', 100_000));
        deep.extend(b"x");
        deep.extend(std::iter::repeat_n(b'}', 100_000));
        let Content::Rtf { text, .. } = convert(&deep, 1000) else { panic!() };
        assert_eq!(text, "x");
        let big = [b"{\\rtf1 ".as_slice(), &vec![b'a'; 10_000]].concat();
        let Content::Rtf { text, html } = convert(&big, 1000) else { panic!() };
        assert!(text.len() + html.len() <= 1100);
        assert!(text.len() > 400, "what fits is kept");
        // Surrogate pairs by \u, and stray halves.
        let Content::Rtf { text, .. } = convert(br"{\rtf1 \u-10179?\u-8704?\u-10179?x}", 100) else { panic!() };
        assert_eq!(text, "\u{1F600}\u{FFFD}x");
    }
}
