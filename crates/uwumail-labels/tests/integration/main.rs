//! Realistic German and English mail for the detectors, the rules and the classifier.

mod classifier;
mod detectors;
mod rules;

use uwumail_labels::Mail;

/// A raw message with the given headers (one per line, without the line end) and plain text.
pub fn message(headers: &[&str], text: &str) -> Mail {
    let mut raw = String::new();
    for header in headers {
        raw.push_str(header);
        raw.push_str("\r\n");
    }
    raw.push_str("MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n");
    raw.push_str(&text.replace('\n', "\r\n"));
    Mail::parse(raw.as_bytes())
}

/// A raw message with a text part and one attachment.
pub fn with_attachment(headers: &[&str], text: &str, content_type: &str, name: &str) -> Mail {
    let mut raw = String::new();
    for header in headers {
        raw.push_str(header);
        raw.push_str("\r\n");
    }
    raw.push_str("MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n");
    raw.push_str("--b1\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n");
    raw.push_str(&text.replace('\n', "\r\n"));
    raw.push_str(&format!(
        "\r\n--b1\r\nContent-Type: {content_type}; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--b1--\r\n"
    ));
    Mail::parse(raw.as_bytes())
}
