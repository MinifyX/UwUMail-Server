//! The properties of principals, homes, collections and entries, and the filters of calendar
//! queries.

use uwumail_store::{Account, DAV_RESOURCE_MAX_BYTES, DavCollection, DavKind, DavResourceInfo};

use crate::objects;
use crate::xml::{self, APPLE, CALDAV, CALSERVER, CARDDAV, DAV, Element};
use crate::{collection_href, home_href, kind_segment, principal_href};

pub enum Requested {
    AllProp,
    PropName,
    Props(Vec<(String, String)>),
}

impl Requested {
    /// Whether calendar-data or address-data is asked for, which needs the entries' content.
    pub fn wants_data(&self) -> bool {
        match self {
            Requested::Props(props) => props.iter().any(|(ns, name)| {
                (ns == CALDAV && name == "calendar-data") || (ns == CARDDAV && name == "address-data")
            }),
            _ => false,
        }
    }
}

pub enum Target {
    Root,
    Principals,
    Principal,
    Home(DavKind),
    Collection(DavCollection),
    Resource(DavCollection, DavResourceInfo, Option<String>),
}

pub fn sync_token(collection_id: i64, change: i64) -> String {
    format!("urn:uwumail:dav:sync:{collection_id}:{change}")
}

pub fn parse_sync_token(token: &str) -> Option<(i64, i64)> {
    let (id, change) = token.strip_prefix("urn:uwumail:dav:sync:")?.split_once(':')?;
    Some((id.parse().ok()?, change.parse().ok()?))
}

pub fn content_type(kind: DavKind, component: &str) -> String {
    match kind {
        DavKind::Calendar => format!("text/calendar; charset=utf-8; component={}", component.to_ascii_lowercase()),
        DavKind::Addressbook => "text/vcard; charset=utf-8".into(),
    }
}

/// `Tue, 15 Nov 1994 08:12:31 GMT`
fn http_date(unix: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = unix.div_euclid(86_400);
    let seconds = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{}, {day:02} {} {year} {:02}:{:02}:{:02} GMT",
        DAYS[days.rem_euclid(7) as usize],
        MONTHS[month as usize - 1],
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn href(path: &str) -> String {
    format!("<d:href>{}</d:href>", xml::escape(path))
}

const PRIVILEGES: &str = "<d:privilege><d:all/></d:privilege><d:privilege><d:read/></d:privilege>\
<d:privilege><d:write/></d:privilege><d:privilege><d:write-properties/></d:privilege>\
<d:privilege><d:write-content/></d:privilege><d:privilege><d:bind/></d:privilege>\
<d:privilege><d:unbind/></d:privilege><d:privilege><d:read-current-user-privilege-set/></d:privilege>";

impl Target {
    fn href(&self, login: &str) -> String {
        match self {
            Target::Root => "/dav/".into(),
            Target::Principals => "/dav/principals/".into(),
            Target::Principal => principal_href(login),
            Target::Home(kind) => home_href(*kind, login),
            Target::Collection(collection) => collection_href(collection.kind, login, &collection.slug),
            Target::Resource(collection, info, _) => {
                format!("{}{}", collection_href(collection.kind, login, &collection.slug), info.name)
            }
        }
    }

    /// Properties a client gets for `allprop` and sees for `propname`.
    fn default_props(&self) -> Vec<(&'static str, &'static str)> {
        let mut props = vec![(DAV, "resourcetype"), (DAV, "displayname")];
        match self {
            Target::Collection(_) => props.extend([(CALSERVER, "getctag"), (DAV, "sync-token")]),
            Target::Resource(..) => props.extend([
                (DAV, "getetag"),
                (DAV, "getcontenttype"),
                (DAV, "getcontentlength"),
                (DAV, "getlastmodified"),
            ]),
            _ => {}
        }
        props
    }

    /// The value of a property, as inner XML, or `None` when it has none.
    fn value(&self, ns: &str, name: &str, account: &Account, login: &str) -> Option<String> {
        let collection = match self {
            Target::Collection(collection) | Target::Resource(collection, ..) => Some(collection),
            _ => None,
        };
        Some(match (ns, name, self) {
            (DAV, "resourcetype", Target::Root | Target::Principals | Target::Home(_)) => "<d:collection/>".into(),
            (DAV, "resourcetype", Target::Principal) => "<d:collection/><d:principal/>".into(),
            (DAV, "resourcetype", Target::Collection(c)) => match c.kind {
                DavKind::Calendar => "<d:collection/><c:calendar/>".into(),
                DavKind::Addressbook => "<d:collection/><card:addressbook/>".into(),
            },
            (DAV, "resourcetype", Target::Resource(..)) => String::new(),
            (DAV, "displayname", Target::Principal) => {
                xml::escape(if account.display_name.is_empty() { &account.login } else { &account.display_name })
            }
            (DAV, "displayname", Target::Home(kind)) => kind_segment(*kind).into(),
            (DAV, "displayname", Target::Collection(c)) => {
                xml::escape(if c.display_name.is_empty() { &c.slug } else { &c.display_name })
            }
            (DAV, "current-user-principal", _) => href(&principal_href(login)),
            (DAV, "principal-URL", Target::Principal) => href(&principal_href(login)),
            (DAV, "principal-collection-set", _) => href("/dav/principals/"),
            (DAV, "owner", Target::Home(_) | Target::Collection(_) | Target::Resource(..)) => {
                href(&principal_href(login))
            }
            (DAV, "current-user-privilege-set", _) => PRIVILEGES.into(),
            (CALDAV, "calendar-home-set", Target::Principal | Target::Root) => {
                href(&home_href(DavKind::Calendar, login))
            }
            (CARDDAV, "addressbook-home-set", Target::Principal | Target::Root) => {
                href(&home_href(DavKind::Addressbook, login))
            }
            (CALDAV, "calendar-user-address-set", Target::Principal) => href(&format!("mailto:{login}")),
            (DAV, "supported-report-set", Target::Collection(c)) => {
                let reports: &[(&str, &str)] = match c.kind {
                    DavKind::Calendar => {
                        &[(DAV, "sync-collection"), (CALDAV, "calendar-multiget"), (CALDAV, "calendar-query")]
                    }
                    DavKind::Addressbook => {
                        &[(DAV, "sync-collection"), (CARDDAV, "addressbook-multiget"), (CARDDAV, "addressbook-query")]
                    }
                };
                reports
                    .iter()
                    .map(|(ns, report)| {
                        format!(
                            "<d:supported-report><d:report>{}</d:report></d:supported-report>",
                            xml::empty_element(ns, report)
                        )
                    })
                    .collect()
            }
            (CALSERVER, "getctag", Target::Collection(c)) => c.change.to_string(),
            (DAV, "sync-token", Target::Collection(c)) => xml::escape(&sync_token(c.id, c.change)),
            (DAV, "getetag", Target::Collection(c)) => xml::escape(&format!("\"c{}\"", c.change)),
            (CALDAV, "supported-calendar-component-set", Target::Collection(c)) if c.kind == DavKind::Calendar => {
                c.components.iter().map(|component| format!("<c:comp name=\"{}\"/>", xml::escape(component))).collect()
            }
            (CALDAV, "calendar-description", Target::Collection(c)) if c.kind == DavKind::Calendar => {
                xml::escape(&c.description)
            }
            (CARDDAV, "addressbook-description", Target::Collection(c)) if c.kind == DavKind::Addressbook => {
                xml::escape(&c.description)
            }
            (APPLE, "calendar-color", Target::Collection(c)) if c.kind == DavKind::Calendar => {
                xml::escape(c.color.as_deref()?)
            }
            (APPLE, "calendar-order", Target::Collection(c)) if c.kind == DavKind::Calendar => c.sort_order.to_string(),
            (CALDAV, "calendar-timezone", Target::Collection(c)) if c.kind == DavKind::Calendar => {
                xml::escape(c.timezone.as_deref()?)
            }
            (CALDAV, "supported-calendar-data", Target::Collection(c)) if c.kind == DavKind::Calendar => {
                "<c:calendar-data content-type=\"text/calendar\" version=\"2.0\"/>".into()
            }
            (CARDDAV, "supported-address-data", Target::Collection(c)) if c.kind == DavKind::Addressbook => {
                "<card:address-data-type content-type=\"text/vcard\" version=\"3.0\"/>\
<card:address-data-type content-type=\"text/vcard\" version=\"4.0\"/>"
                    .into()
            }
            (CALDAV, "max-resource-size", Target::Collection(c)) if c.kind == DavKind::Calendar => {
                DAV_RESOURCE_MAX_BYTES.to_string()
            }
            (CARDDAV, "max-resource-size", Target::Collection(c)) if c.kind == DavKind::Addressbook => {
                DAV_RESOURCE_MAX_BYTES.to_string()
            }
            (DAV, "getetag", Target::Resource(_, info, _)) => xml::escape(&info.etag),
            (DAV, "getcontenttype", Target::Resource(c, info, _)) => content_type(c.kind, &info.component),
            (DAV, "getcontentlength", Target::Resource(_, info, _)) => info.size.to_string(),
            (DAV, "getlastmodified", Target::Resource(_, info, _)) => http_date(info.modified_at),
            (CALDAV, "calendar-data", Target::Resource(c, _, Some(content))) if c.kind == DavKind::Calendar => {
                xml::escape(content)
            }
            (CARDDAV, "address-data", Target::Resource(c, _, Some(content))) if c.kind == DavKind::Addressbook => {
                xml::escape(content)
            }
            _ => {
                let _ = collection;
                return None;
            }
        })
    }
}

/// One `<d:response>` with the found properties and the missing ones.
pub fn response(target: &Target, requested: &Requested, account: &Account, login: &str) -> String {
    let mut found = String::new();
    let mut missing = String::new();
    match requested {
        Requested::PropName => {
            for (ns, name) in target.default_props() {
                found.push_str(&xml::empty_element(ns, name));
            }
        }
        Requested::AllProp => {
            for (ns, name) in target.default_props() {
                if let Some(value) = target.value(ns, name, account, login) {
                    found.push_str(&xml::element(ns, name, &value));
                }
            }
        }
        Requested::Props(props) => {
            for (ns, name) in props {
                match target.value(ns, name, account, login) {
                    Some(value) if value.is_empty() => found.push_str(&xml::empty_element(ns, name)),
                    Some(value) => found.push_str(&xml::element(ns, name, &value)),
                    None => missing.push_str(&xml::empty_element(ns, name)),
                }
            }
        }
    }
    let mut out = format!("<d:response>{}", href(&target.href(login)));
    if !found.is_empty() || missing.is_empty() {
        out.push_str(&format!("<d:propstat><d:prop>{found}</d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat>"));
    }
    if !missing.is_empty() {
        out.push_str(&format!(
            "<d:propstat><d:prop>{missing}</d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat>"
        ));
    }
    out.push_str("</d:response>");
    out
}

pub fn not_found(path: &str) -> String {
    format!("<d:response>{}<d:status>HTTP/1.1 404 Not Found</d:status></d:response>", href(path))
}

/// The parts of a calendar-query filter this server checks: the component and a time range.
/// Everything else matches, which only means the client gets a little more than it asked for.
pub struct QueryFilter {
    component: Option<String>,
    start: Option<i64>,
    end: Option<i64>,
}

/// `20260917T080000Z` as a Unix time.
fn utc_time(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    if date.len() != 8 || time.len() != 6 {
        return None;
    }
    let number = |s: &str| s.parse::<i64>().ok();
    let (year, month, day) = (number(&date[..4])?, number(&date[4..6])?, number(&date[6..])?);
    let (hour, minute, second) = (number(&time[..2])?, number(&time[2..4])?, number(&time[4..])?);
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

impl QueryFilter {
    pub fn from_report(root: &Element) -> QueryFilter {
        let mut filter = QueryFilter { component: None, start: None, end: None };
        let Some(outer) = root.child(CALDAV, "filter").and_then(|f| f.child(CALDAV, "comp-filter")) else {
            return filter;
        };
        if let Some(inner) = outer.child(CALDAV, "comp-filter") {
            filter.component = inner.attribute("name").map(str::to_ascii_uppercase);
            if let Some(range) = inner.child(CALDAV, "time-range") {
                filter.start = range.attribute("start").and_then(utc_time);
                filter.end = range.attribute("end").and_then(utc_time);
            }
        }
        filter
    }

    pub fn matches(&self, info: &DavResourceInfo) -> bool {
        if let Some(component) = &self.component
            && info.component != "VCARD"
            && &info.component != component
        {
            return false;
        }
        if self.start.is_none() && self.end.is_none() {
            return true;
        }
        objects::overlaps(info.starts_at, info.ends_at, self.start, self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_dates_and_times() {
        assert_eq!(parse_sync_token(&sync_token(7, 42)), Some((7, 42)));
        assert_eq!(parse_sync_token("http://example.org/sync/1"), None);
        assert_eq!(http_date(784_887_151), "Tue, 15 Nov 1994 08:12:31 GMT");
        assert_eq!(utc_time("19941115T081231Z"), Some(784_887_151));
        assert_eq!(utc_time("19941115T081231"), None);
    }
}
