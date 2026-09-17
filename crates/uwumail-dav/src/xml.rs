//! WebDAV XML: request bodies read into a small tree with resolved namespaces, and multistatus
//! answers written with fixed prefixes.

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, XmlVersion};

pub const DAV: &str = "DAV:";
pub const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
pub const CALSERVER: &str = "http://calendarserver.org/ns/";
pub const APPLE: &str = "http://apple.com/ns/ical/";

/// Request bodies bigger than this are refused; multiget lists of real clients stay far below.
pub const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_ELEMENTS: usize = 100_000;

/// An element of a request: its namespace and local name, attributes, children and text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Element {
    pub ns: String,
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    pub fn is(&self, ns: &str, name: &str) -> bool {
        self.ns == ns && self.name == name
    }

    pub fn child(&self, ns: &str, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.is(ns, name))
    }

    pub fn children_named<'a>(&'a self, ns: &'a str, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |child| child.is(ns, name))
    }

    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    /// The text of this element and all below it, as a client may split it with comments.
    pub fn all_text(&self) -> String {
        let mut text = self.text.clone();
        for child in &self.children {
            text.push_str(&child.all_text());
        }
        text
    }
}

/// Reads a request body. `Ok(None)` for an empty body.
pub fn parse(body: &[u8]) -> Result<Option<Element>, String> {
    let text = std::str::from_utf8(body).map_err(|_| "the body is not UTF-8".to_owned())?;
    if text.trim().is_empty() {
        return Ok(None);
    }
    let mut reader = NsReader::from_str(text);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Element> = Vec::new();
    let mut root = None;
    let mut elements = 0;
    loop {
        let (resolved, event) = reader.read_resolved_event().map_err(|err| format!("broken XML: {err}"))?;
        let namespace = match resolved {
            ResolveResult::Bound(ns) => ns.as_ref().to_owned(),
            _ => String::new(),
        };
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(start) | Event::Empty(start) if root.is_none() => {
                elements += 1;
                if elements > MAX_ELEMENTS || stack.len() >= MAX_DEPTH {
                    return Err("the XML is too big or too deep".into());
                }
                let name = start.local_name().as_ref().to_owned();
                let attributes = start
                    .attributes()
                    .filter_map(Result::ok)
                    .map(|attribute| {
                        let key = attribute.key.local_name().as_ref().to_owned();
                        let value = attribute
                            .normalized_value(XmlVersion::Implicit1_0)
                            .map(|v| v.into_owned())
                            .unwrap_or_default();
                        (key, value)
                    })
                    .collect();
                let element = Element { ns: namespace, name, attributes, ..Element::default() };
                if empty {
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(element),
                        None => root = Some(element),
                    }
                } else {
                    stack.push(element);
                }
            }
            Event::End(_) => {
                let Some(element) = stack.pop() else {
                    return Err("broken XML: an end without a start".into());
                };
                match stack.last_mut() {
                    Some(parent) => parent.children.push(element),
                    None => root = Some(element),
                }
            }
            Event::Text(text) => {
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(&text.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::CData(data) => {
                if let Some(current) = stack.last_mut() {
                    current.text.push_str(&data.xml_content(XmlVersion::Implicit1_0));
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(current) = stack.last_mut() {
                    let resolved = match reference.resolve_char_ref() {
                        Ok(Some(c)) => c,
                        Ok(None) => match &*reference {
                            "amp" => '&',
                            "lt" => '<',
                            "gt" => '>',
                            "quot" => '"',
                            "apos" => '\'',
                            other => return Err(format!("unknown XML entity &{other};")),
                        },
                        Err(err) => return Err(format!("broken XML entity: {err}")),
                    };
                    current.text.push(resolved);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("broken XML: an element is not closed".into());
    }
    root.map(Some).ok_or_else(|| "the body has no XML element".into())
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// The prefix answers use for a namespace; other namespaces get their own declaration.
pub fn prefix(ns: &str) -> Option<&'static str> {
    Some(match ns {
        DAV => "d",
        CALDAV => "c",
        CARDDAV => "card",
        CALSERVER => "cs",
        APPLE => "ical",
        _ => return None,
    })
}

/// An empty element for a property name, like `<d:displayname/>`.
pub fn empty_element(ns: &str, name: &str) -> String {
    match prefix(ns) {
        Some(prefix) => format!("<{prefix}:{name}/>"),
        None => format!("<x:{name} xmlns:x=\"{}\"/>", escape(ns)),
    }
}

/// An element for a property with inner XML.
pub fn element(ns: &str, name: &str, inner: &str) -> String {
    match prefix(ns) {
        Some(prefix) => format!("<{prefix}:{name}>{inner}</{prefix}:{name}>"),
        None => format!("<x:{name} xmlns:x=\"{}\">{inner}</x:{name}>", escape(ns)),
    }
}

pub const MULTISTATUS_START: &str = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<d:multistatus xmlns:d=\"DAV:\" \
xmlns:c=\"urn:ietf:params:xml:ns:caldav\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\" \
xmlns:cs=\"http://calendarserver.org/ns/\" xmlns:ical=\"http://apple.com/ns/ical/\">";
pub const MULTISTATUS_END: &str = "</d:multistatus>\n";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_resolve_whatever_the_prefix() {
        let body = br#"<?xml version="1.0" encoding="utf-8"?>
<A:propfind xmlns:A="DAV:" xmlns:B="urn:ietf:params:xml:ns:caldav">
  <A:prop><A:displayname/><B:calendar-home-set/><x xmlns="http://example.org/">a &amp; b</x></A:prop>
</A:propfind>"#;
        let root = parse(body).unwrap().unwrap();
        assert!(root.is(DAV, "propfind"));
        let prop = root.child(DAV, "prop").unwrap();
        assert!(prop.child(DAV, "displayname").is_some());
        assert!(prop.child(CALDAV, "calendar-home-set").is_some());
        assert_eq!(prop.child("http://example.org/", "x").unwrap().text, "a & b");
    }

    #[test]
    fn broken_or_empty_bodies() {
        assert_eq!(parse(b"  ").unwrap(), None);
        assert!(parse(b"<d:propfind xmlns:d=\"DAV:\">").is_err());
        let deep = "<a>".repeat(100) + &"</a>".repeat(100);
        assert!(parse(deep.as_bytes()).is_err());
    }

    #[test]
    fn writing() {
        assert_eq!(empty_element(DAV, "getetag"), "<d:getetag/>");
        assert_eq!(element(APPLE, "calendar-color", "#FF4D8D"), "<ical:calendar-color>#FF4D8D</ical:calendar-color>");
        assert_eq!(empty_element("urn:x", "y"), "<x:y xmlns:x=\"urn:x\"/>");
        assert_eq!(escape("<a & \"b\">"), "&lt;a &amp; &quot;b&quot;&gt;");
    }
}
