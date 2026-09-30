use uwumail_tnef::builder::{self, Pattern, Props};
use uwumail_tnef::mapi::{self, PSETID_APPOINTMENT, PSETID_MEETING};
use uwumail_tnef::{IcsOptions, MeetingKind, PartStat, Person, decode};

use crate::fixtures::{self, START};

fn person(name: &str, email: &str) -> Person {
    Person { name: Some(name.into()), email: Some(email.into()) }
}

fn unfolded(ics: &str) -> Vec<String> {
    ics.replace("\r\n ", "").split("\r\n").map(str::to_owned).collect()
}

fn ical(tnef: &[u8], options: &IcsOptions) -> Vec<String> {
    let message = decode(tnef).unwrap();
    let meeting = message.meeting().expect("a meeting");
    let ics = meeting.to_ical(options).expect("an iCalendar object");
    assert!(ics.split("\r\n").all(|line| line.len() <= 75), "folded:\n{ics}");
    unfolded(&ics)
}

fn has(lines: &[String], line: &str) {
    assert!(lines.iter().any(|l| l == line), "missing {line:?} in\n{}", lines.join("\n"));
}

#[test]
fn a_recurring_request() {
    let message = decode(&fixtures::request()).unwrap();
    let meeting = message.meeting().unwrap();
    assert_eq!(meeting.kind, MeetingKind::Request);
    assert_eq!(meeting.uid.as_deref(), Some("umzug-2026@example.com"));
    assert_eq!(meeting.sequence, 3);
    assert_eq!(meeting.start, Some(START));
    let lines = ical(&fixtures::request(), &IcsOptions { now: 0, ..Default::default() });
    for line in [
        "METHOD:REQUEST",
        "BEGIN:VTIMEZONE",
        "TZID:W. Europe Standard Time",
        "RRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU",
        "UID:umzug-2026@example.com",
        "SEQUENCE:3",
        "DTSTAMP:20261026T090000Z",
        "DTSTART;TZID=W. Europe Standard Time:20261027T100000",
        "DTEND;TZID=W. Europe Standard Time:20261027T110000",
        "RRULE:FREQ=WEEKLY;BYDAY=TU,TH;COUNT=10",
        "EXDATE;TZID=W. Europe Standard Time:20261103T100000",
        "SUMMARY:Umzugsplanung",
        "LOCATION:Raum 1\\, Etage 2",
        "DESCRIPTION:Wir planen den Umzug.\\nBitte Kisten mitbringen\\; danke\\, Mini",
        "ORGANIZER;CN=\"Mini Organizer\":mailto:mini@example.com",
        "ATTENDEE;CN=\"Nyu\";ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:nyu@example.com",
        "ATTENDEE;CN=\"Ami\";ROLE=OPT-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:ami@example.com",
        "ATTENDEE;CN=\"Raum 1\";ROLE=NON-PARTICIPANT;CUTYPE=RESOURCE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:raum1@example.com",
        "STATUS:CONFIRMED",
        "TRANSP:OPAQUE",
        "END:VCALENDAR",
    ] {
        has(&lines, line);
    }
    assert_eq!(lines.iter().filter(|l| l.starts_with("ATTENDEE")).count(), 3, "not the organizer");
}

#[test]
fn an_answer_names_who_answers() {
    let props = Props::new()
        .unicode(mapi::PR_SUBJECT, "Mit Vorbehalt angenommen: Umzugsplanung")
        .unicode(mapi::PR_SENT_REPRESENTING_NAME, "Nyu")
        .unicode(mapi::PR_SENT_REPRESENTING_SMTP_ADDRESS, "nyu@example.com")
        .named_time(&PSETID_APPOINTMENT, 0x820D, START)
        .named_time(&PSETID_APPOINTMENT, 0x820E, START + 3600)
        .named_binary(&PSETID_MEETING, 0x0003, &builder::global_object_id("umzug-2026@example.com", None));
    let tnef = fixtures::simple("IPM.Schedule.Meeting.Resp.Tent", props);
    let message = decode(&tnef).unwrap();
    assert_eq!(message.meeting().unwrap().kind, MeetingKind::Reply(PartStat::Tentative));
    let options = IcsOptions { now: START, to: vec![person("Mini", "mini@example.com")], ..Default::default() };
    let lines = ical(&tnef, &options);
    has(&lines, "METHOD:REPLY");
    has(&lines, "DTSTART:20261027T090000Z");
    has(&lines, "ORGANIZER;CN=\"Mini\":mailto:mini@example.com");
    has(&lines, "ATTENDEE;CN=\"Nyu\";PARTSTAT=TENTATIVE:mailto:nyu@example.com");
    has(&lines, "SUMMARY:Mit Vorbehalt angenommen: Umzugsplanung");
    // Without a sender in the TNEF, the mail's From answers.
    let props = Props::new().named_time(&PSETID_APPOINTMENT, 0x820D, START).named_binary(
        &PSETID_MEETING,
        0x0003,
        &builder::global_object_id("x@example.com", None),
    );
    let tnef = fixtures::simple("IPM.Schedule.Meeting.Resp.Neg", props);
    let options = IcsOptions {
        from: Some(person("Ami", "ami@example.com")),
        to: vec![person("Mini", "mini@example.com")],
        ..Default::default()
    };
    has(&ical(&tnef, &options), "ATTENDEE;CN=\"Ami\";PARTSTAT=DECLINED:mailto:ami@example.com");
    // Without anyone to answer to, there is no answer.
    let message = decode(&tnef).unwrap();
    assert!(message.meeting().unwrap().to_ical(&IcsOptions::default()).is_none());
}

#[test]
fn a_cancellation_takes_attendees_from_the_mail() {
    let tnef = fixtures::simple("IPM.Schedule.Meeting.Canceled", fixtures::organizer_props("Abgesagt: Umzugsplanung"));
    let options = IcsOptions {
        now: 0,
        from: Some(person("Someone Else", "else@example.com")),
        to: vec![person("Nyu", "nyu@example.com"), person("Mini", "mini@example.com")],
        cc: vec![person("Ami", "ami@example.com")],
    };
    let lines = ical(&tnef, &options);
    has(&lines, "METHOD:CANCEL");
    has(&lines, "STATUS:CANCELLED");
    has(&lines, "ORGANIZER;CN=\"Mini Organizer\":mailto:mini@example.com");
    has(&lines, "ATTENDEE;CN=\"Nyu\";ROLE=REQ-PARTICIPANT:mailto:nyu@example.com");
    has(&lines, "ATTENDEE;CN=\"Ami\";ROLE=OPT-PARTICIPANT:mailto:ami@example.com");
    assert!(!lines.iter().any(|l| l.contains("ATTENDEE") && l.contains("mini@")));
    assert!(!lines.iter().any(|l| l.starts_with("RRULE:FREQ=WEEKLY")));
}

#[test]
fn addresses_from_the_mail_cannot_add_lines() {
    let tnef = fixtures::simple("IPM.Schedule.Meeting.Canceled", fixtures::organizer_props("Abgesagt"));
    let options = IcsOptions {
        now: 0,
        to: vec![
            person("Nyu", "nyu@example.com\r\nATTENDEE:mailto:extra@example.com"),
            person("Ami", "ami@example.com"),
        ],
        ..Default::default()
    };
    let lines = ical(&tnef, &options);
    has(&lines, "ATTENDEE;CN=\"Ami\";ROLE=REQ-PARTICIPANT:mailto:ami@example.com");
    assert!(!lines.iter().any(|l| l.contains("extra@") || l.contains("nyu@")), "{}", lines.join("\n"));
}

#[test]
fn all_day_and_single_instances() {
    // Midnight in Berlin is 23:00 UTC the day before in winter.
    let midnight = START - 10 * 3600;
    let props = fixtures::organizer_props("Umzugstag")
        .named_time(&PSETID_APPOINTMENT, 0x820D, midnight)
        .named_time(&PSETID_APPOINTMENT, 0x820E, midnight + 86_400)
        .named_bool(&PSETID_APPOINTMENT, 0x8215, true);
    let lines = ical(&fixtures::simple("IPM.Schedule.Meeting.Request", props), &IcsOptions::default());
    has(&lines, "DTSTART;VALUE=DATE:20261027");
    has(&lines, "DTEND;VALUE=DATE:20261028");
    assert!(!lines.iter().any(|l| l == "BEGIN:VTIMEZONE"));

    let props = fixtures::organizer_props("Umzugsplanung")
        .named_binary(
            &PSETID_MEETING,
            0x0003,
            &builder::global_object_id("umzug-2026@example.com", Some((2026, 10, 29))),
        )
        .named_time(&PSETID_APPOINTMENT, 0x8228, START + 2 * 86_400)
        .named_bool(&PSETID_APPOINTMENT, 0x8223, true);
    let lines = ical(&fixtures::simple("IPM.Schedule.Meeting.Request", props), &IcsOptions::default());
    has(&lines, "RECURRENCE-ID;TZID=W. Europe Standard Time:20261029T100000");
    has(&lines, "UID:umzug-2026@example.com");
}

#[test]
fn monthly_and_yearly_patterns() {
    let pattern = |frequency, pattern_type, period, specific: Vec<u32>, end_type| {
        builder::recurrence(&Pattern {
            frequency,
            pattern_type,
            period,
            specific,
            end_type,
            occurrences: 0,
            first_weekday: 0,
            deleted: vec![],
            modified: vec![],
            start_date: builder::minutes_1601(2026, 10, 27),
            end_date: builder::minutes_1601(2027, 10, 27),
            start_offset: 600,
            end_offset: 660,
        })
    };
    let render = |blob: Vec<u8>| {
        let props = fixtures::organizer_props("Serie").named_binary(&PSETID_APPOINTMENT, 0x8216, &blob);
        ical(&fixtures::simple("IPM.Appointment", props), &IcsOptions::default())
    };
    let lines = render(pattern(0x200C, 3, 1, vec![0b10, 5], 0x2023));
    has(&lines, "METHOD:PUBLISH");
    has(&lines, "RRULE:FREQ=MONTHLY;BYDAY=MO;BYSETPOS=-1;WKST=SU");
    let lines = render(pattern(0x200D, 2, 12, vec![27], 0x2021));
    has(&lines, "RRULE:FREQ=YEARLY;BYMONTHDAY=27;BYMONTH=10;UNTIL=20271027T080000Z;WKST=SU");
    let lines = render(pattern(0x200A, 0, 2 * 1440, vec![], 0x2023));
    has(&lines, "RRULE:FREQ=DAILY;INTERVAL=2;WKST=SU");
    let lines = render(pattern(0x200C, 4, 3, vec![31], 0x2023));
    has(&lines, "RRULE:FREQ=MONTHLY;INTERVAL=3;BYMONTHDAY=-1;WKST=SU");
}

#[test]
fn what_is_no_meeting() {
    let message = decode(&fixtures::note()).unwrap();
    assert!(message.meeting().is_none());
    // A request without a GlobalObjectId has no UID, and so no iCalendar object.
    let props = Props::new().named_time(&PSETID_APPOINTMENT, 0x820D, START);
    let message = decode(&fixtures::simple("IPM.Schedule.Meeting.Request", props)).unwrap();
    assert!(message.meeting().unwrap().to_ical(&IcsOptions::default()).is_none());
}
