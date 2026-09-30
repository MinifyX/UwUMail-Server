//! BIMI (Brand Indicators for Message Identification) for hosted domains (docs/bimi.md).
//!
//! - [`tiny_ps`] turns an uploaded SVG into SVG Tiny PS, the profile BIMI requires: a square
//!   `viewBox`, a `<title>`, `version="1.2"` and `baseProfile="tiny-ps"`, and nothing that runs,
//!   links elsewhere or carries pixels. What the profile cannot draw (clip paths, masks, filters,
//!   embedded pictures) is refused with its name rather than dropped, so the logo never looks
//!   different from what the admin uploaded without them knowing.
//! - [`certificate`] reads a Verified Mark or Common Mark Certificate (PEM).
//! - [`record`] writes the `default._bimi` TXT record, [`dmarc_fit`] says whether the domain's
//!   DMARC policy is strict enough for receivers to show the logo.

use std::collections::HashMap;
use std::fmt::Write as _;

use base64::Engine as _;
use quick_xml::events::Event;
use serde::Serialize;

/// Uploads larger than this are not read at all.
pub const MAX_SVG_INPUT: usize = 256 * 1024;
/// The BIMI group asks for logos of at most 32 KB.
pub const MAX_SVG_BYTES: usize = 32 * 1024;
/// A certificate with its chain is a few kilobytes.
pub const MAX_PEM_INPUT: usize = 64 * 1024;
const MAX_CERTIFICATES: usize = 8;
const MAX_DEPTH: usize = 64;
const MAX_TITLE_CHARS: usize = 100;
/// Marks the square put behind a logo, so doing it again replaces it.
const BACKGROUND_ID: &str = "bimi-background";
const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";

/// The selector receivers look up.
pub fn record_name(domain: &str) -> String {
    format!("default._bimi.{domain}")
}

/// The TXT record: where the logo is, and the certificate when there is one.
pub fn record(logo_url: &str, certificate_url: Option<&str>) -> String {
    match certificate_url {
        Some(certificate) => format!("v=BIMI1; l={logo_url}; a={certificate}"),
        None => format!("v=BIMI1; l={logo_url}"),
    }
}

/// The tags of a published BIMI record, lower-case names.
pub fn record_tags(text: &str) -> HashMap<String, String> {
    text.split(';')
        .filter_map(|tag| tag.split_once('='))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect()
}

/// Why an SVG cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvgError {
    /// Not an SVG document, or not one that can be read.
    NotSvg,
    /// Bigger than [`MAX_SVG_BYTES`] even after cleaning; the size it has.
    TooLarge(usize),
    /// Uses something SVG Tiny PS cannot draw; its name.
    Unsupported(String),
    /// Refers to something outside the file.
    External(String),
    /// Carries a pixel image.
    Raster,
    /// Says nowhere how big it is.
    NoSize,
    /// No title, or one too long.
    Title,
    /// The background is not a `#rrggbb` colour.
    Background,
}

impl SvgError {
    /// The code the portal knows, and the detail it may show.
    pub fn code(&self) -> (&'static str, String) {
        match self {
            SvgError::NotSvg => ("bimiNotSvg", "this is not an SVG file".into()),
            SvgError::TooLarge(size) => ("bimiSvgTooLarge", format!("{size} bytes")),
            SvgError::Unsupported(what) => ("bimiSvgUnsupported", what.clone()),
            SvgError::External(what) => ("bimiSvgExternal", what.clone()),
            SvgError::Raster => ("bimiSvgRaster", "an embedded pixel image".into()),
            SvgError::NoSize => ("bimiSvgNoSize", "no viewBox, width or height".into()),
            SvgError::Title => ("bimiTitle", format!("a title of 1 to {MAX_TITLE_CHARS} characters")),
            SvgError::Background => ("bimiBackground", "a colour like #ffffff".into()),
        }
    }
}

/// How a logo is cleaned.
pub struct SvgOptions<'a> {
    /// The title; empty keeps the one the file has.
    pub title: &'a str,
    /// A `#rrggbb` colour for a solid square behind the logo; `None` keeps the file's own.
    pub background: Option<&'a str>,
}

#[derive(Debug)]
struct Element {
    name: String,
    attributes: Vec<(String, String)>,
    children: Vec<Node>,
}

#[derive(Debug)]
enum Node {
    Element(Element),
    Text(String),
}

/// Elements SVG Tiny PS has and a logo may use.
const ALLOWED: [&str; 21] = [
    "svg",
    "g",
    "defs",
    "desc",
    "title",
    "path",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "text",
    "tspan",
    "textArea",
    "tbreak",
    "linearGradient",
    "radialGradient",
    "stop",
    "solidColor",
    "use",
];

/// Things that do not draw anything: left out without a word.
const DROPPED: [&str; 16] = [
    "script",
    "metadata",
    "animate",
    "animateColor",
    "animateMotion",
    "animateTransform",
    "set",
    "discard",
    "handler",
    "listener",
    "prefetch",
    "audio",
    "video",
    "animation",
    "sodipodi:namedview",
    "style",
];

/// Attributes SVG Tiny 1.2 knows for drawing; everything else is left out.
const ATTRIBUTES: [&str; 50] = [
    "id",
    "transform",
    "d",
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "width",
    "height",
    "points",
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-opacity",
    "opacity",
    "color",
    "display",
    "visibility",
    "gradientUnits",
    "gradientTransform",
    "offset",
    "stop-color",
    "stop-opacity",
    "solid-color",
    "solid-opacity",
    "font-family",
    "font-size",
    "font-style",
    "font-weight",
    "font-variant",
    "text-anchor",
    "display-align",
    "line-increment",
    "text-align",
    "vector-effect",
    "xml:space",
];

/// Properties that change how something looks and that Tiny PS cannot do.
const REFUSED_PROPERTIES: [&str; 3] = ["clip-path", "mask", "filter"];

fn parse(input: &str) -> Result<Element, SvgError> {
    let mut reader = quick_xml::Reader::from_str(input);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let start = |tag: &quick_xml::events::BytesStart<'_>| -> Result<Element, SvgError> {
        let mut attributes = Vec::new();
        for attribute in tag.attributes() {
            let attribute = attribute.map_err(|_| SvgError::NotSvg)?;
            let value = attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_err(|_| SvgError::NotSvg)?;
            attributes.push((attribute.key.as_ref().to_owned(), value.into_owned()));
        }
        Ok(Element { name: tag.name().as_ref().to_owned(), attributes, children: Vec::new() })
    };
    let text = |stack: &mut Vec<Element>, text: String| {
        if let Some(parent) = stack.last_mut() {
            match parent.children.last_mut() {
                Some(Node::Text(before)) => before.push_str(&text),
                _ => parent.children.push(Node::Text(text)),
            }
        }
    };
    loop {
        match reader.read_event().map_err(|_| SvgError::NotSvg)? {
            Event::Start(tag) => {
                if stack.len() >= MAX_DEPTH || root.is_some() {
                    return Err(SvgError::NotSvg);
                }
                stack.push(start(&tag)?);
            }
            Event::Empty(tag) => {
                let element = start(&tag)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(Node::Element(element)),
                    None if root.is_none() => root = Some(element),
                    None => return Err(SvgError::NotSvg),
                }
            }
            Event::End(_) => {
                let element = stack.pop().ok_or(SvgError::NotSvg)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(Node::Element(element)),
                    None => root = Some(element),
                }
            }
            Event::Text(content) => {
                text(&mut stack, content.xml_content(quick_xml::XmlVersion::Implicit1_0).into_owned())
            }
            Event::CData(content) => {
                text(&mut stack, content.into_inner().into_owned());
            }
            Event::GeneralRef(reference) => {
                let resolved = match reference.resolve_char_ref().map_err(|_| SvgError::NotSvg)? {
                    Some(character) => character.to_string(),
                    None => match reference.as_ref() {
                        "lt" => "<".into(),
                        "gt" => ">".into(),
                        "amp" => "&".into(),
                        "apos" => "'".into(),
                        "quot" => "\"".into(),
                        // Entities of a DTD are not expanded; a file that needs them is not taken.
                        _ => return Err(SvgError::NotSvg),
                    },
                };
                text(&mut stack, resolved);
            }
            Event::Eof => break,
            // Declarations, comments, processing instructions and a DTD are left out.
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(SvgError::NotSvg);
    }
    let root = root.ok_or(SvgError::NotSvg)?;
    if local(&root.name) != "svg" {
        return Err(SvgError::NotSvg);
    }
    Ok(root)
}

/// An element's name without an `svg:` prefix; other prefixes belong to editors.
fn local(name: &str) -> &str {
    name.strip_prefix("svg:").unwrap_or(name)
}

/// One CSS rule of a `<style>` element: the selector's kind ranks it, like CSS does.
struct CssRule {
    /// 0 for an element name, 1 for a class, 2 for an id.
    rank: u8,
    selector: String,
    declarations: Vec<(String, String)>,
}

fn declarations(text: &str) -> Vec<(String, String)> {
    text.split(';')
        .filter_map(|declaration| declaration.split_once(':'))
        .map(|(property, value)| {
            let value = value.trim().trim_end_matches("!important").trim();
            (property.trim().to_ascii_lowercase(), value.to_owned())
        })
        .filter(|(property, value)| !property.is_empty() && !value.is_empty())
        .collect()
}

/// The rules of the file's style sheets. Selectors other than a single element, class or id are
/// left out; the preview shows what that changes.
fn css_rules(root: &Element) -> Vec<CssRule> {
    fn sheets(element: &Element, found: &mut String) {
        for child in &element.children {
            match child {
                Node::Element(child) if local(&child.name) == "style" => {
                    for text in &child.children {
                        if let Node::Text(text) = text {
                            found.push_str(text);
                            found.push('\n');
                        }
                    }
                }
                Node::Element(child) => sheets(child, found),
                Node::Text(_) => {}
            }
        }
    }
    let mut sheet = String::new();
    sheets(root, &mut sheet);
    // Comments first, then rule by rule.
    let mut clean = String::new();
    let mut rest = sheet.as_str();
    while let Some(start) = rest.find("/*") {
        clean.push_str(&rest[..start]);
        rest = rest[start + 2..].split_once("*/").map_or("", |(_, after)| after);
    }
    clean.push_str(rest);
    let mut rules = Vec::new();
    for block in clean.split('}') {
        let Some((selectors, body)) = block.split_once('{') else { continue };
        if selectors.trim_start().starts_with('@') {
            continue;
        }
        let declarations = declarations(body);
        for selector in selectors.split(',') {
            let selector = selector.trim();
            let simple =
                |name: &str| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || "-_".contains(c));
            let (rank, name) = if let Some(class) = selector.strip_prefix('.') {
                (1, class)
            } else if let Some(id) = selector.strip_prefix('#') {
                (2, id)
            } else {
                (0, selector)
            };
            if simple(name) {
                rules.push(CssRule { rank, selector: name.to_owned(), declarations: declarations.clone() });
            }
        }
    }
    rules.sort_by_key(|rule| rule.rank);
    rules
}

/// A reference inside an attribute value: `url(#id)` is fine, anything else points outside.
fn check_urls(value: &str) -> Result<(), SvgError> {
    // ASCII lower case keeps every byte where it was, so positions fit both.
    let lower = value.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find("url(") {
        let start = from + found + 4;
        let inside = &value[start..];
        let trimmed = inside.trim_start().trim_start_matches(['"', '\'']);
        if trimmed.to_ascii_lowercase().starts_with("data:image") {
            return Err(SvgError::Raster);
        }
        if !trimmed.starts_with('#') {
            let end = trimmed.find([')', '"', '\'']).unwrap_or(trimmed.len());
            return Err(SvgError::External(trimmed[..end].chars().take(80).collect()));
        }
        from = start;
    }
    Ok(())
}

struct Cleaner<'a> {
    rules: &'a [CssRule],
    uses_xlink: bool,
    /// A new square goes behind the logo, so the one from an earlier upload goes.
    replace_background: bool,
}

impl Cleaner<'_> {
    /// The element's look as attributes: its own, then the style sheet's rules, then its `style`.
    fn attributes(&mut self, element: &Element) -> Result<Vec<(String, String)>, SvgError> {
        let name = local(&element.name);
        let mut merged: Vec<(String, String)> = Vec::new();
        let set = |merged: &mut Vec<(String, String)>, key: &str, value: &str| match merged
            .iter_mut()
            .find(|(known, _)| known == key)
        {
            Some(entry) => entry.1 = value.to_owned(),
            None => merged.push((key.to_owned(), value.to_owned())),
        };
        let mut classes: Vec<&str> = Vec::new();
        let mut id = None;
        let mut style = None;
        for (key, value) in &element.attributes {
            let lower = key.to_ascii_lowercase();
            match key.as_str() {
                "class" => classes = value.split_whitespace().collect(),
                "style" => style = Some(value.as_str()),
                "href" | "xlink:href" => {
                    let value = value.trim();
                    if value.to_ascii_lowercase().starts_with("data:image") {
                        return Err(SvgError::Raster);
                    }
                    if !value.starts_with('#') {
                        return Err(SvgError::External(value.chars().take(80).collect()));
                    }
                    self.uses_xlink = true;
                    set(&mut merged, "xlink:href", value);
                }
                _ if lower.starts_with("on") => {}
                _ => {
                    if key == "id" {
                        id = Some(value.as_str());
                    }
                    set(&mut merged, key, value);
                }
            }
        }
        for rule in self.rules {
            let matches = match rule.rank {
                0 => rule.selector == name,
                1 => classes.contains(&rule.selector.as_str()),
                _ => id == Some(rule.selector.as_str()),
            };
            if matches {
                for (property, value) in &rule.declarations {
                    set(&mut merged, property, value);
                }
            }
        }
        if let Some(style) = style {
            for (property, value) in declarations(style) {
                set(&mut merged, &property, &value);
            }
        }
        let mut kept = Vec::new();
        for (key, value) in merged {
            if REFUSED_PROPERTIES.contains(&key.as_str()) {
                if value.trim() != "none" {
                    return Err(SvgError::Unsupported(key));
                }
                continue;
            }
            if key != "xlink:href" && !ATTRIBUTES.contains(&key.as_str()) {
                continue;
            }
            check_urls(&value)?;
            if value.to_ascii_lowercase().contains("javascript:") {
                continue;
            }
            kept.push((key, value));
        }
        Ok(kept)
    }

    /// The cleaned children of an element.
    fn children(&mut self, element: &Element, keep_text: bool) -> Result<Vec<Node>, SvgError> {
        let mut children = Vec::new();
        for child in &element.children {
            match child {
                Node::Text(text) if keep_text => children.push(Node::Text(text.clone())),
                Node::Text(_) => {}
                Node::Element(child) => {
                    let name = local(&child.name);
                    if name.contains(':') || DROPPED.contains(&name) {
                        continue;
                    }
                    // A link is only a wrapper for what it contains.
                    if name == "a" {
                        children.extend(self.children(child, keep_text)?);
                        continue;
                    }
                    if name == "image" {
                        return Err(SvgError::Raster);
                    }
                    if name == "title" || name == "svg" || !ALLOWED.contains(&name) {
                        if name == "title" {
                            continue;
                        }
                        return Err(SvgError::Unsupported(name.to_owned()));
                    }
                    if name == "use" && !child.attributes.iter().any(|(key, _)| key.ends_with("href")) {
                        continue;
                    }
                    let attributes = self.attributes(child)?;
                    // Our own square from an earlier upload makes way for the new one.
                    if self.replace_background
                        && attributes.iter().any(|(key, value)| key == "id" && value == BACKGROUND_ID)
                    {
                        continue;
                    }
                    let keep_text = matches!(name, "text" | "tspan" | "textArea" | "desc");
                    let grandchildren = self.children(child, keep_text)?;
                    children.push(Node::Element(Element {
                        name: name.to_owned(),
                        attributes,
                        children: grandchildren,
                    }));
                }
            }
        }
        Ok(children)
    }
}

/// Numbers of a `viewBox`, or a size from `width` and `height`.
fn view_box(root: &Element) -> Result<[f64; 4], SvgError> {
    let attribute = |key: &str| root.attributes.iter().find(|(known, _)| known == key).map(|(_, value)| value.trim());
    if let Some(value) = attribute("viewBox") {
        let numbers: Vec<f64> = value
            .split([' ', ',', '\t', '\n', '\r'])
            .filter(|part| !part.is_empty())
            .filter_map(|part| part.parse().ok())
            .collect();
        if let [x, y, width, height] = numbers[..]
            && width > 0.0
            && height > 0.0
            && width.is_finite()
            && height.is_finite()
        {
            return Ok([x, y, width, height]);
        }
    }
    let length = |key: &str| {
        attribute(key)
            .map(|value| value.trim_end_matches("px"))
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| *value > 0.0 && value.is_finite())
    };
    match (length("width"), length("height")) {
        (Some(width), Some(height)) => Ok([0.0, 0.0, width, height]),
        _ => Err(SvgError::NoSize),
    }
}

fn number(value: f64) -> String {
    let rounded = (value * 1000.0).round() / 1000.0;
    if rounded == rounded.trunc() { format!("{}", rounded as i64) } else { format!("{rounded}") }
}

fn escape(text: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\n' | '\r' | '\t' if attribute => out.push(' '),
            // Control characters are not allowed in XML 1.0.
            c if (c as u32) < 0x20 && !matches!(c, '\n' | '\r' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

fn write_element(out: &mut String, element: &Element) {
    out.push('<');
    out.push_str(&element.name);
    for (key, value) in &element.attributes {
        let _ = write!(out, " {key}=\"{}\"", escape(value, true));
    }
    if element.children.is_empty() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    for child in &element.children {
        match child {
            Node::Element(child) => write_element(out, child),
            Node::Text(text) => out.push_str(&escape(text, false)),
        }
    }
    let _ = write!(out, "</{}>", element.name);
}

fn valid_colour(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// The title a file has, if any.
fn existing_title(root: &Element) -> Option<String> {
    root.children.iter().find_map(|child| match child {
        Node::Element(element) if local(&element.name) == "title" => Some(
            element
                .children
                .iter()
                .filter_map(|node| if let Node::Text(text) = node { Some(text.as_str()) } else { None })
                .collect::<String>(),
        ),
        _ => None,
    })
}

/// Turns an SVG into SVG Tiny PS as BIMI wants it, or says why it cannot be done.
pub fn tiny_ps(input: &str, options: &SvgOptions<'_>) -> Result<String, SvgError> {
    if input.len() > MAX_SVG_INPUT {
        return Err(SvgError::TooLarge(input.len()));
    }
    let root = parse(input.trim_start_matches('\u{feff}'))?;
    // Only SVG itself; another default namespace makes it some other kind of document.
    if root.attributes.iter().any(|(key, value)| key == "xmlns" && value != SVG_NS) {
        return Err(SvgError::NotSvg);
    }
    let title = match options.title.trim() {
        "" => existing_title(&root)
            .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default(),
        title => title.to_owned(),
    };
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(SvgError::Title);
    }
    if options.background.is_some_and(|colour| !valid_colour(colour)) {
        return Err(SvgError::Background);
    }
    // Square, the same size both ways, centred on what was drawn.
    let [x, y, width, height] = view_box(&root)?;
    let size = width.max(height);
    let (x, y) = (x - (size - width) / 2.0, y - (size - height) / 2.0);

    let rules = css_rules(&root);
    let mut cleaner = Cleaner { rules: &rules, uses_xlink: false, replace_background: options.background.is_some() };
    // What the root says about the look is passed on to a group around everything.
    let mut inherited = cleaner.attributes(&root)?;
    inherited
        .retain(|(key, _)| !matches!(key.as_str(), "id" | "x" | "y" | "width" | "height" | "xml:space" | "transform"));
    let drawn = cleaner.children(&root, false)?;

    let mut children = vec![Node::Element(Element {
        name: "title".into(),
        attributes: Vec::new(),
        children: vec![Node::Text(title)],
    })];
    if let Some(colour) = options.background {
        children.push(Node::Element(Element {
            name: "rect".into(),
            attributes: vec![
                ("id".into(), BACKGROUND_ID.into()),
                ("x".into(), number(x)),
                ("y".into(), number(y)),
                ("width".into(), number(size)),
                ("height".into(), number(size)),
                ("fill".into(), colour.to_ascii_lowercase()),
            ],
            children: Vec::new(),
        }));
    }
    if inherited.is_empty() {
        children.extend(drawn);
    } else {
        children.push(Node::Element(Element { name: "g".into(), attributes: inherited, children: drawn }));
    }
    let mut attributes = vec![("xmlns".to_owned(), SVG_NS.to_owned())];
    if cleaner.uses_xlink {
        attributes.push(("xmlns:xlink".into(), XLINK_NS.into()));
    }
    attributes.extend([
        ("version".to_owned(), "1.2".to_owned()),
        ("baseProfile".to_owned(), "tiny-ps".to_owned()),
        ("viewBox".to_owned(), format!("{} {} {} {}", number(x), number(y), number(size), number(size))),
    ]);
    let svg = Element { name: "svg".into(), attributes, children };
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    write_element(&mut out, &svg);
    out.push('\n');
    if out.len() > MAX_SVG_BYTES {
        return Err(SvgError::TooLarge(out.len()));
    }
    Ok(out)
}

/// What kind of mark certificate it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkKind {
    /// Verified Mark Certificate: a registered trademark.
    Vmc,
    /// Common Mark Certificate: a logo used for a year or more.
    Cmc,
    Unknown,
}

/// A mark certificate as the portal shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkCertificate {
    pub kind: MarkKind,
    pub subject: String,
    pub issuer: String,
    pub not_before: i64,
    pub not_after: i64,
    pub names: Vec<String>,
    pub has_logotype: bool,
}

impl MarkCertificate {
    pub fn covers(&self, domain: &str) -> bool {
        let domain = domain.to_ascii_lowercase();
        self.names.iter().any(|name| {
            let name = name.to_ascii_lowercase();
            name == domain || name == record_name(&domain)
        })
    }
}

/// Why a certificate cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateError {
    /// Not a PEM certificate, or one that cannot be read.
    Invalid,
    /// A certificate, but not one for BIMI.
    NotBimi,
}

impl CertificateError {
    pub fn code(&self) -> (&'static str, String) {
        match self {
            CertificateError::Invalid => ("bimiCertificateInvalid", "a PEM certificate is needed".into()),
            CertificateError::NotBimi => {
                ("bimiCertificateNotBimi", "the certificate is not a Verified Mark or Common Mark Certificate".into())
            }
        }
    }
}

/// id-kp-BrandIndicatorforMessageIdentification (RFC 9344 style mark certificates).
const BIMI_KEY_PURPOSE: &str = "1.3.6.1.5.5.7.3.31";
/// The logotype extension (RFC 3709), where the certificate carries the logo itself.
const LOGOTYPE: &str = "1.3.6.1.5.5.7.1.12";
/// Certificate policies of the Mark Certificate requirements.
const VMC_POLICY: &str = "1.3.6.1.4.1.53087.1.1";
const CMC_POLICY: &str = "1.3.6.1.4.1.53087.4.1";
/// The mark type in the subject: "Registered Mark", "Government Mark", "Prior Use Mark",
/// "Modified Registered Mark".
const MARK_TYPE: &str = "1.3.6.1.4.1.53087.1.13";

/// Reads a mark certificate with its chain. Returns the PEM to keep (certificates only, leaf
/// first as given) and what the leaf says.
pub fn certificate(pem: &str) -> Result<(String, MarkCertificate), CertificateError> {
    use x509_parser::extensions::{GeneralName, ParsedExtension};
    use x509_parser::pem::Pem;

    if pem.len() > MAX_PEM_INPUT {
        return Err(CertificateError::Invalid);
    }
    let mut blocks = Vec::new();
    for block in Pem::iter_from_buffer(pem.as_bytes()) {
        let block = block.map_err(|_| CertificateError::Invalid)?;
        if block.label == "CERTIFICATE" {
            block.parse_x509().map_err(|_| CertificateError::Invalid)?;
            blocks.push(block.contents);
        }
        if blocks.len() > MAX_CERTIFICATES {
            return Err(CertificateError::Invalid);
        }
    }
    let leaf = blocks.first().ok_or(CertificateError::Invalid)?;
    let (_, cert) = x509_parser::parse_x509_certificate(leaf).map_err(|_| CertificateError::Invalid)?;
    let for_bimi = cert
        .extended_key_usage()
        .ok()
        .flatten()
        .is_some_and(|usage| usage.value.other.iter().any(|oid| oid.to_id_string() == BIMI_KEY_PURPOSE));
    if !for_bimi {
        return Err(CertificateError::NotBimi);
    }
    let mut names = Vec::new();
    let mut policies = Vec::new();
    let mut has_logotype = false;
    for extension in cert.iter_extensions() {
        if extension.oid.to_id_string() == LOGOTYPE {
            has_logotype = true;
        }
        match extension.parsed_extension() {
            ParsedExtension::SubjectAlternativeName(alternative) => {
                for name in &alternative.general_names {
                    if let GeneralName::DNSName(dns) = name {
                        names.push(dns.to_string());
                    }
                }
            }
            ParsedExtension::CertificatePolicies(found) => {
                policies.extend(found.iter().map(|policy| policy.policy_id.to_id_string()));
            }
            _ => {}
        }
    }
    let mark_type = cert
        .subject()
        .iter_attributes()
        .find(|attribute| attribute.attr_type().to_id_string() == MARK_TYPE)
        .and_then(|attribute| attribute.as_str().ok())
        .map(str::to_ascii_lowercase);
    let kind = match mark_type.as_deref() {
        Some("prior use mark" | "modified registered mark") => MarkKind::Cmc,
        Some("registered mark" | "government mark") => MarkKind::Vmc,
        _ if policies.iter().any(|policy| policy == CMC_POLICY) => MarkKind::Cmc,
        _ if policies.iter().any(|policy| policy == VMC_POLICY) => MarkKind::Vmc,
        _ => MarkKind::Unknown,
    };
    let summary = MarkCertificate {
        kind,
        subject: cert.subject().to_string(),
        issuer: cert.issuer().to_string(),
        not_before: cert.validity().not_before.timestamp(),
        not_after: cert.validity().not_after.timestamp(),
        names,
        has_logotype,
    };
    let engine = base64::engine::general_purpose::STANDARD;
    let mut kept = String::new();
    for der in &blocks {
        kept.push_str("-----BEGIN CERTIFICATE-----\n");
        let encoded = engine.encode(der);
        for line in encoded.as_bytes().chunks(64) {
            kept.push_str(std::str::from_utf8(line).unwrap_or_default());
            kept.push('\n');
        }
        kept.push_str("-----END CERTIFICATE-----\n");
    }
    Ok((kept, summary))
}

/// How the domain's DMARC policy suits BIMI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DmarcFitness {
    /// `p=quarantine` or `p=reject` for all mail.
    Ok,
    /// A record, but too lenient: `p=none`, `pct` below 100 or `sp=none`.
    Weak,
    Missing,
    /// Not checked yet.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DmarcFit {
    pub status: DmarcFitness,
    pub policy: Option<String>,
    pub pct: Option<u32>,
    pub subdomain_policy: Option<String>,
    pub record: Option<String>,
}

/// Whether a DMARC record lets receivers show a BIMI logo: they want `p=quarantine` or
/// `p=reject` applied to all mail (`pct=100`, the default), and no `sp=none`.
pub fn dmarc_fit(record: Option<&str>) -> DmarcFit {
    let Some(record) = record else {
        return DmarcFit {
            status: DmarcFitness::Missing,
            policy: None,
            pct: None,
            subdomain_policy: None,
            record: None,
        };
    };
    let tags = record_tags(record);
    let policy = tags.get("p").map(|value| value.to_ascii_lowercase());
    let pct = tags.get("pct").and_then(|value| value.parse::<u32>().ok());
    let subdomain_policy = tags.get("sp").map(|value| value.to_ascii_lowercase());
    let enforced = matches!(policy.as_deref(), Some("quarantine" | "reject"))
        && pct.is_none_or(|pct| pct >= 100)
        && subdomain_policy.as_deref() != Some("none");
    DmarcFit {
        status: if enforced { DmarcFitness::Ok } else { DmarcFitness::Weak },
        policy,
        pct,
        subdomain_policy,
        record: Some(record.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(svg: &str) -> Result<String, SvgError> {
        tiny_ps(svg, &SvgOptions { title: "Example", background: Some("#FFFFFF") })
    }

    #[test]
    fn a_plain_logo_becomes_tiny_ps() {
        let svg = r##"<?xml version="1.0"?>
            <!-- made with an editor -->
            <svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" x="5" y="5" onload="alert(1)">
              <metadata>nothing</metadata>
              <script>alert(1)</script>
              <circle cx="50" cy="50" r="40" fill="#ff66aa" onclick="alert(2)"/>
            </svg>"##;
        let out = clean(svg).unwrap();
        assert!(
            out.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"")
        );
        assert!(out.contains(r#"version="1.2" baseProfile="tiny-ps" viewBox="0 -50 200 200""#), "{out}");
        assert!(out.contains("<title>Example</title>"));
        assert!(
            out.contains(r##"<rect id="bimi-background" x="0" y="-50" width="200" height="200" fill="#ffffff"/>"##)
        );
        assert!(out.contains(r##"<circle cx="50" cy="50" r="40" fill="#ff66aa"/>"##), "{out}");
        for gone in ["script", "alert", "metadata", "onload", "onclick", "x=\"5\"", "width=\"200\" height=\"100\""] {
            assert!(!out.contains(gone), "{gone} is still in {out}");
        }
        // Cleaning its own output gives the same again, with a new background or keeping the one it has.
        assert_eq!(tiny_ps(&out, &SvgOptions { title: "", background: Some("#ffffff") }).unwrap(), out);
        assert_eq!(tiny_ps(&out, &SvgOptions { title: "", background: None }).unwrap(), out);
        let retitled = tiny_ps(&out, &SvgOptions { title: "Other & more", background: None }).unwrap();
        assert!(retitled.contains("<title>Other &amp; more</title>") && retitled.contains("bimi-background"));
    }

    #[test]
    fn styles_become_attributes() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape" viewBox="0 0 10 10">
            <defs><style>/* colours */ .cls-1{fill:#123456;} .cls-2, #dot { fill: red !important } path { stroke: blue } g rect { fill: green }</style></defs>
            <inkscape:grid/>
            <path class="cls-1" d="M0 0L10 10" inkscape:label="line" style="stroke-width:2;mix-blend-mode:multiply"/>
            <rect id="dot" class="cls-1" width="1" height="1"/>
            <text x="1" y="9" font-size="3">Hi &amp; hello &#x263A;</text>
          </svg>"##;
        let out = tiny_ps(svg, &SvgOptions { title: "Dots", background: None }).unwrap();
        assert!(out.contains(r##"<path d="M0 0L10 10" stroke="blue" fill="#123456" stroke-width="2"/>"##), "{out}");
        // An id beats a class.
        assert!(out.contains(r##"<rect id="dot" width="1" height="1" fill="red"/>"##), "{out}");
        assert!(out.contains("<text x=\"1\" y=\"9\" font-size=\"3\">Hi &amp; hello \u{263A}</text>"), "{out}");
        assert!(!out.contains("inkscape") && !out.contains("style") && !out.contains("class"), "{out}");
        assert!(!out.contains("bimi-background"));
    }

    #[test]
    fn what_tiny_ps_cannot_draw_is_refused_by_name() {
        let wrap =
            |inner: &str| format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10">{inner}</svg>"#);
        assert_eq!(
            clean(&wrap(r#"<clipPath id="c"><rect width="1" height="1"/></clipPath>"#)),
            Err(SvgError::Unsupported("clipPath".into()))
        );
        assert_eq!(
            clean(&wrap(r#"<rect width="1" height="1" filter="url(#f)"/>"#)),
            Err(SvgError::Unsupported("filter".into()))
        );
        assert_eq!(
            clean(&wrap(r#"<rect width="1" height="1" style="mask:url(#m)"/>"#)),
            Err(SvgError::Unsupported("mask".into()))
        );
        assert_eq!(clean(&wrap(r#"<foreignObject/>"#)), Err(SvgError::Unsupported("foreignObject".into())));
        assert_eq!(clean(&wrap(r#"<image href="data:image/png;base64,AAAA"/>"#)), Err(SvgError::Raster));
        assert_eq!(clean(&wrap(r#"<use xlink:href="data:image/png;base64,AAAA"/>"#)), Err(SvgError::Raster));
        assert_eq!(
            clean(&wrap(r#"<use href="https://logo.example/x.svg#a"/>"#)),
            Err(SvgError::External("https://logo.example/x.svg#a".into()))
        );
        assert_eq!(
            clean(&wrap(r#"<rect width="1" height="1" fill="url(https://logo.example/p.svg#g)"/>"#)),
            Err(SvgError::External("https://logo.example/p.svg#g".into()))
        );
        // Links to something inside the file are fine.
        let local = clean(&wrap(
            r##"<defs><linearGradient id="g"><stop offset="0" stop-color="#fff"/></linearGradient></defs><rect width="1" height="1" fill="url(#g)"/><use href="#g"/>"##,
        ))
        .unwrap();
        assert!(
            local.contains(r#"xmlns:xlink="http://www.w3.org/1999/xlink""#)
                && local.contains(r##"<use xlink:href="#g"/>"##),
            "{local}"
        );
    }

    #[test]
    fn broken_or_foreign_files_are_refused() {
        assert_eq!(clean("not xml at all"), Err(SvgError::NotSvg));
        assert_eq!(clean("<html><body/></html>"), Err(SvgError::NotSvg));
        assert_eq!(clean(r#"<svg xmlns="http://www.w3.org/2000/svg"><g></svg>"#), Err(SvgError::NotSvg));
        assert_eq!(
            clean(
                r#"<!DOCTYPE svg [<!ENTITY a "aaaa">]><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><text>&a;</text></svg>"#
            ),
            Err(SvgError::NotSvg)
        );
        assert_eq!(clean(r#"<svg xmlns="http://www.w3.org/2000/svg"><rect/></svg>"#), Err(SvgError::NoSize));
        assert_eq!(
            tiny_ps(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"/>"#,
                &SvgOptions { title: "", background: None }
            ),
            Err(SvgError::Title)
        );
        assert_eq!(
            tiny_ps(
                r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"/>"#,
                &SvgOptions { title: "A", background: Some("red") }
            ),
            Err(SvgError::Background)
        );
        let deep = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1">{}{}</svg>"#,
            "<g>".repeat(100),
            "</g>".repeat(100)
        );
        assert_eq!(clean(&deep), Err(SvgError::NotSvg));
        let big = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1">{}</svg>"#,
            r#"<rect width="1" height="1" fill="red"/>"#.repeat(1200)
        );
        assert!(matches!(clean(&big), Err(SvgError::TooLarge(_))));
        assert_eq!(SvgError::Unsupported("mask".into()).code().0, "bimiSvgUnsupported");
    }

    #[test]
    fn records_and_dmarc() {
        assert_eq!(record_name("example.org"), "default._bimi.example.org");
        assert_eq!(
            record("https://mail.example.org/bimi/example.org.svg", None),
            "v=BIMI1; l=https://mail.example.org/bimi/example.org.svg"
        );
        let full = record("https://h.example/l.svg", Some("https://h.example/c.pem"));
        let tags = record_tags(&full);
        assert_eq!((tags["v"].as_str(), tags["a"].as_str()), ("BIMI1", "https://h.example/c.pem"));

        assert_eq!(dmarc_fit(None).status, DmarcFitness::Missing);
        assert_eq!(dmarc_fit(Some("v=DMARC1; p=reject")).status, DmarcFitness::Ok);
        assert_eq!(
            dmarc_fit(Some("v=DMARC1; p=quarantine; pct=100; rua=mailto:d@example.org")).status,
            DmarcFitness::Ok
        );
        let weak = dmarc_fit(Some("v=DMARC1; p=quarantine; pct=50"));
        assert_eq!((weak.status, weak.pct), (DmarcFitness::Weak, Some(50)));
        assert_eq!(dmarc_fit(Some("v=DMARC1; p=none")).status, DmarcFitness::Weak);
        assert_eq!(dmarc_fit(Some("v=DMARC1; p=reject; sp=none")).status, DmarcFitness::Weak);
    }

    fn mark_certificate(purpose: &[u64], mark_type: Option<&str>) -> String {
        use rcgen::{CertificateParams, CustomExtension, DnType, ExtendedKeyUsagePurpose, KeyPair};
        let mut params = CertificateParams::new(vec!["example.org".to_owned()]).unwrap();
        params.distinguished_name.push(DnType::OrganizationName, "Example Club");
        if let Some(mark_type) = mark_type {
            params.distinguished_name.push(DnType::CustomDnType(vec![1, 3, 6, 1, 4, 1, 53087, 1, 13]), mark_type);
        }
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::Other(purpose.to_vec())];
        // An empty logotype extension is enough to be noticed.
        params.custom_extensions =
            vec![CustomExtension::from_oid_content(&[1, 3, 6, 1, 5, 5, 7, 1, 12], vec![0x30, 0x00])];
        let key = KeyPair::generate().unwrap();
        params.self_signed(&key).unwrap().pem()
    }

    #[test]
    fn mark_certificates_are_read() {
        let pem = mark_certificate(&[1, 3, 6, 1, 5, 5, 7, 3, 31], Some("Prior Use Mark"));
        let (kept, summary) = certificate(&format!("junk before\n{pem}")).unwrap();
        assert!(kept.starts_with("-----BEGIN CERTIFICATE-----\n") && !kept.contains("junk"));
        assert_eq!(summary.kind, MarkKind::Cmc);
        assert_eq!(summary.names, ["example.org"]);
        assert!(summary.has_logotype && summary.covers("example.org") && !summary.covers("example.net"));
        assert!(summary.subject.contains("Example Club"));
        assert!(summary.not_after > summary.not_before);
        // Reading what was kept gives the same.
        assert_eq!(certificate(&kept).unwrap().1, summary);

        let registered = mark_certificate(&[1, 3, 6, 1, 5, 5, 7, 3, 31], Some("Registered Mark"));
        assert_eq!(certificate(&registered).unwrap().1.kind, MarkKind::Vmc);
        // A web server's certificate is not a mark certificate.
        let web = mark_certificate(&[1, 3, 6, 1, 5, 5, 7, 3, 1], None);
        assert_eq!(certificate(&web), Err(CertificateError::NotBimi));
        assert_eq!(
            certificate("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"),
            Err(CertificateError::Invalid)
        );
        assert_eq!(certificate("nothing"), Err(CertificateError::Invalid));
    }
}
