//! The media type of an attachment: the one Outlook recorded when it is well-formed, else one
//! guessed from the file name ending, else from the first bytes.

/// Whether `value` is a well-formed `type/subtype`, lower-cased.
fn well_formed(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    let (kind, sub) = value.split_once('/')?;
    let token = |s: &str| {
        !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    };
    (token(kind) && token(sub)).then_some(value)
}

fn by_ending(name: &str) -> Option<&'static str> {
    let ending = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ending.as_str() {
        "pdf" => "application/pdf",
        "doc" | "dot" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "docm" => "application/vnd.ms-word.document.macroenabled.12",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsm" => "application/vnd.ms-excel.sheet.macroenabled.12",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "odp" => "application/vnd.oasis.opendocument.presentation",
        "rtf" => "application/rtf",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "htm" | "html" => "text/html",
        "xml" => "application/xml",
        "json" => "application/json",
        "ics" => "text/calendar",
        "vcf" => "text/vcard",
        "eml" => "message/rfc822",
        "msg" => "application/vnd.ms-outlook",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "emf" => "image/emf",
        "wmf" => "image/wmf",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "rar" => "application/vnd.rar",
        "gz" => "application/gzip",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "exe" | "dll" => "application/x-msdownload",
        "js" => "text/javascript",
        _ => return None,
    })
}

fn by_content(data: &[u8]) -> Option<&'static str> {
    let starts = |magic: &[u8]| data.starts_with(magic);
    Some(if starts(b"%PDF-") {
        "application/pdf"
    } else if starts(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if starts(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        "image/gif"
    } else if starts(b"PK\x03\x04") {
        "application/zip"
    } else if starts(b"{\\rtf") {
        "application/rtf"
    } else {
        return None;
    })
}

/// The media type of an attachment.
pub fn guess(recorded: Option<&str>, name: Option<&str>, data: &[u8]) -> String {
    if let Some(kind) = recorded.and_then(well_formed).filter(|k| k != "application/octet-stream") {
        return kind;
    }
    name.and_then(by_ending).or_else(|| by_content(data)).unwrap_or("application/octet-stream").to_owned()
}

/// A file name made harmless: no directories, no control characters, not too long.
pub fn clean_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base.chars().filter(|c| !c.is_control()).take(255).collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    (!cleaned.is_empty()).then(|| cleaned.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses() {
        assert_eq!(guess(Some("Image/PNG"), Some("x.bin"), b""), "image/png");
        assert_eq!(guess(Some("not a type"), Some("Bericht.PDF"), b""), "application/pdf");
        assert!(guess(Some("application/octet-stream"), Some("a.docx"), b"").starts_with("application/vnd.openxml"));
        assert_eq!(guess(None, None, b"\x89PNG\r\n\x1a\n...."), "image/png");
        assert_eq!(guess(None, Some("noending"), b"??"), "application/octet-stream");
        assert_eq!(clean_name("C:\\Users\\x\\..\\Grüße.txt\0"), Some("Grüße.txt".into()));
        assert_eq!(clean_name("../../etc/passwd"), Some("passwd".into()));
        assert_eq!(clean_name(" .. "), None);
    }
}
