//! Checks what clients store: one iCalendar object or one vCard, with one UID, and for calendars the
//! time the object covers, so calendar queries can pick events by date.
//!
//! The calendar check lives in the store, where JMAP Calendars uses it too.

use calcard::vcard::VCard;
pub use uwumail_store::ical::{Checked, Refused, check_calendar};

/// A vCard. One without a UID gets the resource name as its UID, as some older clients leave it out.
pub fn check_contact(content: &str, name: &str) -> Result<Checked, Refused> {
    let card = VCard::parse(content).map_err(|_| Refused::InvalidData("not a vCard".into()))?;
    let uid = card.uid().map(str::to_owned).unwrap_or_else(|| name.trim_end_matches(".vcf").to_owned());
    Ok(Checked { uid, component: "VCARD".into(), starts_at: None, ends_at: None })
}

/// Whether an object overlaps `[start, end)` (RFC 4791, 9.9). A missing start means unknown, a
/// missing end open-ended: both match.
pub fn overlaps(starts_at: Option<i64>, ends_at: Option<i64>, start: Option<i64>, end: Option<i64>) -> bool {
    let begins_before_end = match (starts_at, end) {
        (Some(object_start), Some(end)) => object_start < end,
        _ => true,
    };
    let ends_after_start = match (starts_at, ends_at, start) {
        // An instant, like an event without duration, counts at its start.
        (Some(object_start), Some(object_end), Some(start)) if object_start == object_end => object_end >= start,
        (_, Some(object_end), Some(start)) => object_end > start,
        _ => true,
    };
    begins_before_end && ends_after_start
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//UwUMail//Test//DE\r\nBEGIN:VEVENT\r\nUID:tierarzt@uwumail\r\n\
DTSTAMP:20260917T080000Z\r\nDTSTART:20260920T090000Z\r\nDTEND:20260920T100000Z\r\nSUMMARY:Tierarzt\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn all() -> Vec<String> {
        vec!["VEVENT".into(), "VTODO".into()]
    }

    #[test]
    fn events_have_one_uid_and_a_time_span() {
        let checked = check_calendar(EVENT, &all()).unwrap();
        assert_eq!(checked.uid, "tierarzt@uwumail");
        assert_eq!(checked.component, "VEVENT");
        assert_eq!(checked.starts_at, Some(1_789_894_800));
        assert_eq!(checked.ends_at, Some(1_789_898_400));
        assert!(matches!(check_calendar(EVENT, &["VTODO".into()]), Err(Refused::UnsupportedComponent(_))));
        assert!(matches!(check_calendar("hallo", &all()), Err(Refused::InvalidData(_))));
        let two = EVENT.replace(
            "END:VCALENDAR",
            "BEGIN:VEVENT\r\nUID:anders\r\nDTSTART:20260921T090000Z\r\nEND:VEVENT\r\nEND:VCALENDAR",
        );
        assert!(matches!(check_calendar(&two, &all()), Err(Refused::InvalidObject(_))));
    }

    #[test]
    fn endless_repeats_are_open_ended() {
        let weekly = EVENT.replace("SUMMARY:Tierarzt", "SUMMARY:Yoga\r\nRRULE:FREQ=WEEKLY");
        let checked = check_calendar(&weekly, &all()).unwrap();
        assert_eq!(checked.starts_at, Some(1_789_894_800));
        assert_eq!(checked.ends_at, None);
        let limited = EVENT.replace("SUMMARY:Tierarzt", "SUMMARY:Yoga\r\nRRULE:FREQ=WEEKLY;COUNT=3");
        assert_eq!(check_calendar(&limited, &all()).unwrap().ends_at, Some(1_789_898_400 + 2 * 7 * 86_400));
    }

    #[test]
    fn contacts_and_overlaps() {
        let card = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Nyu Katze\r\nUID:nyu-1\r\nEND:VCARD\r\n";
        assert_eq!(check_contact(card, "x.vcf").unwrap().uid, "nyu-1");
        let without = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Nyu\r\nEND:VCARD\r\n";
        assert_eq!(check_contact(without, "abc.vcf").unwrap().uid, "abc");
        assert!(overlaps(Some(10), Some(20), Some(15), Some(30)));
        assert!(!overlaps(Some(10), Some(20), Some(20), Some(30)));
        assert!(overlaps(Some(10), None, Some(1000), None));
        assert!(overlaps(None, None, Some(5), Some(6)));
    }
}
