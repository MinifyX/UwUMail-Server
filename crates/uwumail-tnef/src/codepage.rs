//! Windows code pages (TNEF's OEM code page, RTF's `\ansicpg`, `PR_INTERNET_CPID`) as text.

use encoding_rs::Encoding;

/// The encoding of a Windows code page number; Windows-1252 for those not known.
pub fn encoding(code_page: u32) -> &'static Encoding {
    let label: &[u8] = match code_page {
        437 | 850 | 1252 | 20127 | 28591 => b"windows-1252",
        866 => b"ibm866",
        874 => b"windows-874",
        932 => b"shift_jis",
        936 => b"gbk",
        949 => b"euc-kr",
        950 => b"big5",
        1200 => b"utf-16le",
        1201 => b"utf-16be",
        1250 => b"windows-1250",
        1251 => b"windows-1251",
        1253 => b"windows-1253",
        1254 => b"windows-1254",
        1255 => b"windows-1255",
        1256 => b"windows-1256",
        1257 => b"windows-1257",
        1258 => b"windows-1258",
        10000 => b"macintosh",
        10007 => b"x-mac-cyrillic",
        20866 => b"koi8-r",
        21866 => b"koi8-u",
        28592 => b"iso-8859-2",
        28593 => b"iso-8859-3",
        28594 => b"iso-8859-4",
        28595 => b"iso-8859-5",
        28596 => b"iso-8859-6",
        28597 => b"iso-8859-7",
        28598 => b"iso-8859-8",
        28599 => b"windows-1254",
        28603 => b"iso-8859-13",
        28605 => b"iso-8859-15",
        50220 | 50221 | 50222 => b"iso-2022-jp",
        51932 => b"euc-jp",
        51936 => b"gbk",
        51949 => b"euc-kr",
        54936 => b"gb18030",
        65001 => b"utf-8",
        _ => b"windows-1252",
    };
    Encoding::for_label(label).unwrap_or(encoding_rs::WINDOWS_1252)
}

/// Bytes in a code page as text; what does not decode becomes U+FFFD.
pub fn decode(code_page: u32, bytes: &[u8]) -> String {
    encoding(code_page).decode_without_bom_handling(bytes).0.into_owned()
}

/// A code page's name as MIME knows it, for HTML that says which one it is written in.
pub fn from_label(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(label.trim().as_bytes())
}

/// UTF-16LE (PT_UNICODE) as text, without its terminating NULs.
pub fn utf16le(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
    let text = String::from_utf16_lossy(&units);
    text.trim_end_matches('\0').to_owned()
}

/// A string of a code page without its terminating NULs.
pub fn string8(code_page: u32, bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    decode(code_page, &bytes[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_pages() {
        assert_eq!(decode(1252, b"Gr\xfc\xdfe \x80"), "Grüße €");
        assert_eq!(decode(1251, b"\xcf\xf0\xe8"), "При");
        assert_eq!(decode(65001, "ü".as_bytes()), "ü");
        assert_eq!(decode(4242, b"\xe4"), "ä");
        assert_eq!(utf16le(&[0x41, 0, 0xfc, 0, 0, 0]), "Aü");
        assert_eq!(utf16le(&[0x41]), "");
        assert_eq!(string8(1252, b"abc\0junk"), "abc");
    }
}
