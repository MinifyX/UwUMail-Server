//! A small TNEF writer, for tests of programs that read winmail.dat. It writes what Outlook
//! writes for the parts this crate reads, not every TNEF feature.

use crate::mapi::{self, Guid};

/// MAPI properties, written as TNEF has them.
#[derive(Debug, Clone, Default)]
pub struct Props {
    count: u32,
    bytes: Vec<u8>,
}

fn pad(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

fn utf16(text: &str) -> Vec<u8> {
    let mut out: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    out.extend([0, 0]);
    out
}

/// Unix seconds as a FILETIME.
pub fn filetime(unix: i64) -> u64 {
    ((unix + 11_644_473_600) * 10_000_000) as u64
}

impl Props {
    pub fn new() -> Props {
        Props::default()
    }

    fn head(&mut self, kind: u16, id: u16) {
        self.count += 1;
        self.bytes.extend(kind.to_le_bytes());
        self.bytes.extend(id.to_le_bytes());
    }

    fn named_head(&mut self, kind: u16, guid: &Guid, lid: u32) {
        self.head(kind, 0x8000);
        self.bytes.extend(guid);
        self.bytes.extend(0u32.to_le_bytes());
        self.bytes.extend(lid.to_le_bytes());
    }

    fn variable(&mut self, data: &[u8]) {
        self.bytes.extend(1u32.to_le_bytes());
        self.bytes.extend((data.len() as u32).to_le_bytes());
        self.bytes.extend(data);
        pad(&mut self.bytes);
    }

    pub fn long(mut self, tag: u16, value: i32) -> Props {
        self.head(mapi::PT_LONG, tag);
        self.bytes.extend(value.to_le_bytes());
        self
    }

    pub fn bool(mut self, tag: u16, value: bool) -> Props {
        self.head(mapi::PT_BOOLEAN, tag);
        self.bytes.extend(u32::from(value).to_le_bytes());
        self
    }

    pub fn time(mut self, tag: u16, unix: i64) -> Props {
        self.head(mapi::PT_SYSTIME, tag);
        self.bytes.extend(filetime(unix).to_le_bytes());
        self
    }

    pub fn unicode(mut self, tag: u16, value: &str) -> Props {
        self.head(mapi::PT_UNICODE, tag);
        self.variable(&utf16(value));
        self
    }

    /// A string in the stream's code page.
    pub fn string8(mut self, tag: u16, value: &[u8]) -> Props {
        self.head(mapi::PT_STRING8, tag);
        let mut data = value.to_vec();
        data.push(0);
        self.variable(&data);
        self
    }

    pub fn binary(mut self, tag: u16, value: &[u8]) -> Props {
        self.head(mapi::PT_BINARY, tag);
        self.variable(value);
        self
    }

    /// An attached object, such as a message (`mapi::IID_IMESSAGE` and a TNEF stream).
    pub fn object(mut self, tag: u16, iid: &Guid, value: &[u8]) -> Props {
        self.head(mapi::PT_OBJECT, tag);
        let mut data = iid.to_vec();
        data.extend(value);
        self.variable(&data);
        self
    }

    pub fn named_long(mut self, guid: &Guid, lid: u32, value: i32) -> Props {
        self.named_head(mapi::PT_LONG, guid, lid);
        self.bytes.extend(value.to_le_bytes());
        self
    }

    pub fn named_bool(mut self, guid: &Guid, lid: u32, value: bool) -> Props {
        self.named_head(mapi::PT_BOOLEAN, guid, lid);
        self.bytes.extend(u32::from(value).to_le_bytes());
        self
    }

    pub fn named_time(mut self, guid: &Guid, lid: u32, unix: i64) -> Props {
        self.named_head(mapi::PT_SYSTIME, guid, lid);
        self.bytes.extend(filetime(unix).to_le_bytes());
        self
    }

    pub fn named_unicode(mut self, guid: &Guid, lid: u32, value: &str) -> Props {
        self.named_head(mapi::PT_UNICODE, guid, lid);
        self.variable(&utf16(value));
        self
    }

    pub fn named_binary(mut self, guid: &Guid, lid: u32, value: &[u8]) -> Props {
        self.named_head(mapi::PT_BINARY, guid, lid);
        self.variable(value);
        self
    }

    /// The block as `attMsgProps` or `attAttachment` hold it.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.count.to_le_bytes().to_vec();
        out.extend(&self.bytes);
        out
    }
}

/// A TNEF stream being written.
#[derive(Debug, Clone)]
pub struct Tnef {
    bytes: Vec<u8>,
}

impl Default for Tnef {
    fn default() -> Self {
        Tnef::new()
    }
}

impl Tnef {
    pub fn new() -> Tnef {
        let mut bytes = crate::SIGNATURE.to_le_bytes().to_vec();
        bytes.extend(0x0001u16.to_le_bytes());
        let mut tnef = Tnef { bytes };
        tnef.attribute(1, 0x0008_9006, &0x0001_0000u32.to_le_bytes());
        tnef
    }

    /// Any attribute: level 1 (message) or 2 (attachment), the id with its type, the data.
    pub fn attribute(&mut self, level: u8, id: u32, data: &[u8]) -> &mut Tnef {
        self.bytes.push(level);
        self.bytes.extend(id.to_le_bytes());
        self.bytes.extend((data.len() as u32).to_le_bytes());
        self.bytes.extend(data);
        let sum = data.iter().fold(0u16, |sum, b| sum.wrapping_add(u16::from(*b)));
        self.bytes.extend(sum.to_le_bytes());
        self
    }

    pub fn code_page(&mut self, code_page: u32) -> &mut Tnef {
        let mut data = code_page.to_le_bytes().to_vec();
        data.extend(0u32.to_le_bytes());
        self.attribute(1, 0x0006_9007, &data)
    }

    pub fn message_class(&mut self, class: &str) -> &mut Tnef {
        let mut data = class.as_bytes().to_vec();
        data.push(0);
        self.attribute(1, 0x0007_8008, &data)
    }

    pub fn message_props(&mut self, props: &Props) -> &mut Tnef {
        self.attribute(1, 0x0006_9003, &props.to_bytes())
    }

    /// The recipient table: one row of properties per recipient.
    pub fn recipients(&mut self, rows: &[Props]) -> &mut Tnef {
        let mut data = (rows.len() as u32).to_le_bytes().to_vec();
        for row in rows {
            data.extend(row.to_bytes());
        }
        self.attribute(1, 0x0006_9004, &data)
    }

    /// An attachment: its short title, its data and its properties (long name, MIME tag, …).
    pub fn attachment(&mut self, title: &str, data: &[u8], props: &Props) -> &mut Tnef {
        let mut rend = 1u16.to_le_bytes().to_vec();
        rend.extend([0xFF; 4]);
        rend.extend([0; 8]);
        self.attribute(2, 0x0006_9002, &rend);
        let mut name = title.as_bytes().to_vec();
        name.push(0);
        self.attribute(2, 0x0001_8010, &name);
        if !data.is_empty() {
            self.attribute(2, 0x0006_800F, data);
        }
        self.attribute(2, 0x0006_9005, &props.to_bytes())
    }

    pub fn build(&self) -> Vec<u8> {
        self.bytes.clone()
    }
}

/// Compressed RTF, as `PR_RTF_COMPRESSED` holds it.
pub fn compressed_rtf(rtf: &[u8]) -> Vec<u8> {
    crate::rtf::compress(rtf)
}

/// A GlobalObjectId that carries `uid` the way Outlook stores the UIDs of invitations from
/// elsewhere (`vCal-Uid`); `instance` is the year, month and day of one instance of a series.
pub fn global_object_id(uid: &str, instance: Option<(u16, u8, u8)>) -> Vec<u8> {
    let mut goid = vec![0x04, 0, 0, 0, 0x82, 0, 0xE0, 0, 0x74, 0xC5, 0xB7, 0x10, 0x1A, 0x82, 0xE0, 0x08];
    match instance {
        Some((year, month, day)) => {
            goid.extend(year.to_be_bytes());
            goid.extend([month, day]);
        }
        None => goid.extend([0; 4]),
    }
    goid.extend([0; 16]);
    let mut data = b"vCal-Uid\x01\x00\x00\x00".to_vec();
    data.extend(uid.as_bytes());
    data.push(0);
    goid.extend((data.len() as u32).to_le_bytes());
    goid.extend(data);
    goid
}

/// A SYSTEMTIME of a time-zone rule: month, week (5 = last), weekday (0 = Sunday), hour.
fn rule_time(out: &mut Vec<u8>, month: u16, week: u16, weekday: u16, hour: u16) {
    for v in [0, month, weekday, week, hour, 0, 0, 0] {
        out.extend(v.to_le_bytes());
    }
}

/// A TZDEFINITION with one rule, as `PidLidAppointmentTimeZoneDefinitionStartDisplay` holds it.
/// Biases in minutes (UTC = local + bias); `switches` are (month, week, weekday, hour) of the
/// change to standard and to daylight time.
pub fn time_zone_definition(
    name: &str,
    bias: i32,
    daylight_bias: i32,
    switches: Option<((u16, u16, u16, u16), (u16, u16, u16, u16))>,
) -> Vec<u8> {
    let key: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = vec![0x02, 0x01];
    out.extend(((6 + key.len()) as u16).to_le_bytes());
    out.extend(0x0002u16.to_le_bytes());
    out.extend((name.encode_utf16().count() as u16).to_le_bytes());
    out.extend(&key);
    out.extend(1u16.to_le_bytes());
    out.extend([0x02, 0x01]);
    out.extend(0x003Eu16.to_le_bytes());
    out.extend(0x0002u16.to_le_bytes());
    out.extend(2026u16.to_le_bytes());
    out.extend([0; 14]);
    out.extend(bias.to_le_bytes());
    out.extend(0i32.to_le_bytes());
    out.extend(daylight_bias.to_le_bytes());
    match switches {
        Some((standard, daylight)) => {
            rule_time(&mut out, standard.0, standard.1, standard.2, standard.3);
            rule_time(&mut out, daylight.0, daylight.1, daylight.2, daylight.3);
        }
        None => out.extend([0; 32]),
    }
    out
}

/// What a recurrence pattern says, for [`recurrence`].
#[derive(Debug, Clone)]
pub struct Pattern {
    /// 0x200A daily, 0x200B weekly, 0x200C monthly, 0x200D yearly.
    pub frequency: u16,
    /// 0 day, 1 week, 2 month (day), 3 month (nth weekday), 4 month end.
    pub pattern_type: u16,
    /// Minutes (daily), weeks or months.
    pub period: u32,
    /// Weekday mask, day of month or (mask, nth) by `pattern_type`.
    pub specific: Vec<u32>,
    /// 0x2021 until a date, 0x2022 after a count, 0x2023 never.
    pub end_type: u32,
    pub occurrences: u32,
    pub first_weekday: u32,
    /// Local midnights as minutes since 1601.
    pub deleted: Vec<u32>,
    pub modified: Vec<u32>,
    pub start_date: u32,
    pub end_date: u32,
    /// Minutes after midnight.
    pub start_offset: u32,
    pub end_offset: u32,
}

/// Local midnight of a date as minutes since 1601, as recurrence patterns count days.
pub fn minutes_1601(year: i64, month: u32, day: u32) -> u32 {
    ((crate::time::days_from_civil(year, month, day) * 1440) + 11_644_473_600 / 60) as u32
}

/// An AppointmentRecurrencePattern (`PidLidAppointmentRecur`) without exceptions.
pub fn recurrence(p: &Pattern) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(0x3004u16.to_le_bytes());
    out.extend(0x3004u16.to_le_bytes());
    out.extend(p.frequency.to_le_bytes());
    out.extend(p.pattern_type.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(p.period.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    for v in &p.specific {
        out.extend(v.to_le_bytes());
    }
    out.extend(p.end_type.to_le_bytes());
    out.extend(p.occurrences.to_le_bytes());
    out.extend(p.first_weekday.to_le_bytes());
    out.extend((p.deleted.len() as u32).to_le_bytes());
    for d in &p.deleted {
        out.extend(d.to_le_bytes());
    }
    out.extend((p.modified.len() as u32).to_le_bytes());
    for d in &p.modified {
        out.extend(d.to_le_bytes());
    }
    out.extend(p.start_date.to_le_bytes());
    out.extend(p.end_date.to_le_bytes());
    out.extend(0x3006u32.to_le_bytes());
    out.extend(0x3009u32.to_le_bytes());
    out.extend(p.start_offset.to_le_bytes());
    out.extend(p.end_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out
}
