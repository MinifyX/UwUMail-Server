//! MAPI properties as TNEF carries them (MS-OXTNEF 2.1.3.4): the message's (`attMsgProps`), an
//! attachment's (`attAttachment`) and the rows of the recipient table (`attRecipTable`).

use crate::codepage;
use crate::reader::{Eof, Reader};

/// A property set GUID as it is stored: little-endian fields.
pub type Guid = [u8; 16];

/// PSETID_Appointment {00062002-0000-0000-C000-000000000046}.
pub const PSETID_APPOINTMENT: Guid = [0x02, 0x20, 0x06, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// PSETID_Meeting {6ED8DA90-450B-101B-98DA-00AA003F1305}.
pub const PSETID_MEETING: Guid =
    [0x90, 0xDA, 0xD8, 0x6E, 0x0B, 0x45, 0x1B, 0x10, 0x98, 0xDA, 0x00, 0xAA, 0x00, 0x3F, 0x13, 0x05];
/// PSETID_Common {00062008-0000-0000-C000-000000000046}.
pub const PSETID_COMMON: Guid = [0x08, 0x20, 0x06, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
/// IID_IMessage {00020307-0000-0000-C000-000000000046}: an attached message (PT_OBJECT).
pub const IID_IMESSAGE: Guid = [0x07, 0x03, 0x02, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];

pub const PT_SHORT: u16 = 0x0002;
pub const PT_LONG: u16 = 0x0003;
pub const PT_FLOAT: u16 = 0x0004;
pub const PT_DOUBLE: u16 = 0x0005;
pub const PT_CURRENCY: u16 = 0x0006;
pub const PT_APPTIME: u16 = 0x0007;
pub const PT_ERROR: u16 = 0x000A;
pub const PT_BOOLEAN: u16 = 0x000B;
pub const PT_OBJECT: u16 = 0x000D;
pub const PT_I8: u16 = 0x0014;
pub const PT_STRING8: u16 = 0x001E;
pub const PT_UNICODE: u16 = 0x001F;
pub const PT_SYSTIME: u16 = 0x0040;
pub const PT_CLSID: u16 = 0x0048;
pub const PT_BINARY: u16 = 0x0102;
pub const MV_FLAG: u16 = 0x1000;

/// Which property: a tag below 0x8000, or a named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropId {
    Tag(u16),
    Id(Guid, u32),
    Name(Guid, String),
}

/// A property's value. Strings are text already, in whatever code page the stream said.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Short(i16),
    Long(i32),
    Float(f32),
    Double(f64),
    Currency(i64),
    AppTime(f64),
    Error(u32),
    Bool(bool),
    I8(i64),
    /// A FILETIME: 100 ns since 1601.
    Time(u64),
    String(String),
    Binary(Vec<u8>),
    Clsid(Guid),
    /// An attached object: its interface id and bytes (a TNEF stream for IID_IMessage).
    Object(Guid, Vec<u8>),
    Multi(Vec<Value>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            Value::Multi(list) => list.first().and_then(Value::as_str),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Binary(b) | Value::Object(_, b) => Some(b),
            Value::String(s) => Some(s.as_bytes()),
            Value::Multi(list) => list.first().and_then(Value::as_bytes),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Short(v) => Some(i64::from(*v)),
            Value::Long(v) => Some(i64::from(*v)),
            Value::I8(v) | Value::Currency(v) => Some(*v),
            Value::Bool(v) => Some(i64::from(*v)),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(v) => Some(*v),
            other => other.as_i64().map(|v| v != 0),
        }
    }

    /// A time as Unix seconds.
    pub fn as_time(&self) -> Option<i64> {
        match self {
            Value::Time(ft) if *ft > 0 => Some(crate::time::filetime(*ft)),
            _ => None,
        }
    }
}

/// One property.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    pub id: PropId,
    /// The MAPI property type (PT_*), with the multi-value flag.
    pub kind: u16,
    pub value: Value,
}

/// The properties of a message, an attachment or a recipient.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Properties(pub Vec<Property>);

impl Properties {
    pub fn tag(&self, tag: u16) -> Option<&Value> {
        self.0.iter().rev().find(|p| p.id == PropId::Tag(tag)).map(|p| &p.value)
    }

    pub fn named(&self, guid: &Guid, id: u32) -> Option<&Value> {
        self.0.iter().rev().find(|p| matches!(&p.id, PropId::Id(g, i) if g == guid && *i == id)).map(|p| &p.value)
    }

    pub fn str(&self, tag: u16) -> Option<&str> {
        self.tag(tag).and_then(Value::as_str).filter(|s| !s.trim().is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Parses a block of properties. What was read before a fault is kept; the fault is reported.
pub(crate) fn parse_block(data: &[u8], code_page: u32, budget: &mut usize) -> (Properties, bool) {
    let mut reader = Reader::new(data);
    let mut props = Properties::default();
    let complete = read_props(&mut reader, code_page, budget, &mut props).is_ok();
    (props, complete)
}

/// The rows of `attRecipTable`: a count, then per row a count of properties and the properties.
pub(crate) fn parse_rows(data: &[u8], code_page: u32, budget: &mut usize) -> (Vec<Properties>, bool) {
    let mut reader = Reader::new(data);
    let mut rows = Vec::new();
    let Ok(count) = reader.u32() else { return (rows, false) };
    for _ in 0..count {
        let mut props = Properties::default();
        let ok = read_props(&mut reader, code_page, budget, &mut props).is_ok();
        rows.push(props);
        if !ok {
            return (rows, false);
        }
    }
    (rows, true)
}

fn read_props(reader: &mut Reader<'_>, code_page: u32, budget: &mut usize, out: &mut Properties) -> Result<(), Eof> {
    let count = reader.u32()?;
    for _ in 0..count {
        // Every property takes at least four bytes, so a made-up count runs out of input soon.
        if *budget == 0 {
            return Err(Eof);
        }
        *budget -= 1;
        let kind = reader.u16()?;
        let tag = reader.u16()?;
        let id = if tag >= 0x8000 {
            let guid = reader.array16()?;
            match reader.u32()? {
                0 => PropId::Id(guid, reader.u32()?),
                _ => {
                    let len = reader.u32()? as usize;
                    let name = reader.take(len)?;
                    reader.pad4(len);
                    PropId::Name(guid, codepage::utf16le(name))
                }
            }
        } else {
            PropId::Tag(tag)
        };
        let base = kind & !MV_FLAG;
        let value = if kind & MV_FLAG != 0 {
            let n = reader.u32()?;
            let mut values = Vec::new();
            for _ in 0..n {
                if *budget == 0 {
                    return Err(Eof);
                }
                *budget -= 1;
                values.push(read_value(reader, base, code_page, true)?);
            }
            Value::Multi(values)
        } else {
            read_value(reader, base, code_page, false)?
        };
        out.0.push(Property { id, kind, value });
    }
    Ok(())
}

fn is_variable(kind: u16) -> bool {
    matches!(kind, PT_STRING8 | PT_UNICODE | PT_BINARY | PT_OBJECT)
}

fn read_value(reader: &mut Reader<'_>, kind: u16, code_page: u32, in_multi: bool) -> Result<Value, Eof> {
    if is_variable(kind) && !in_multi {
        // A single variable-length value still comes with a count, which is 1.
        let n = reader.u32()?;
        let mut first = None;
        // Each value takes at least four bytes, so a made-up count runs out of input soon.
        for _ in 0..n {
            let value = read_one(reader, kind, code_page)?;
            first.get_or_insert(value);
        }
        return Ok(first.unwrap_or(Value::Null));
    }
    read_one(reader, kind, code_page)
}

fn read_one(reader: &mut Reader<'_>, kind: u16, code_page: u32) -> Result<Value, Eof> {
    Ok(match kind {
        0x0000 | 0x0001 => {
            reader.skip(4)?;
            Value::Null
        }
        PT_SHORT => {
            let v = reader.u16()? as i16;
            reader.skip(2).ok();
            Value::Short(v)
        }
        PT_LONG => Value::Long(reader.i32()?),
        PT_FLOAT => Value::Float(f32::from_bits(reader.u32()?)),
        PT_DOUBLE => Value::Double(f64::from_bits(reader.u64()?)),
        PT_APPTIME => Value::AppTime(f64::from_bits(reader.u64()?)),
        PT_CURRENCY => Value::Currency(reader.u64()? as i64),
        PT_I8 => Value::I8(reader.u64()? as i64),
        PT_ERROR => Value::Error(reader.u32()?),
        PT_BOOLEAN => Value::Bool(reader.u32()? & 0xFFFF != 0),
        PT_SYSTIME => Value::Time(reader.u64()?),
        PT_CLSID => Value::Clsid(reader.array16()?),
        PT_STRING8 | PT_UNICODE | PT_BINARY | PT_OBJECT => {
            let len = reader.u32()? as usize;
            let bytes = reader.take(len)?;
            reader.pad4(len);
            match kind {
                PT_STRING8 => Value::String(codepage::string8(code_page, bytes)),
                PT_UNICODE => Value::String(codepage::utf16le(bytes)),
                PT_OBJECT if bytes.len() >= 16 => {
                    let mut iid = [0u8; 16];
                    iid.copy_from_slice(&bytes[..16]);
                    Value::Object(iid, bytes[16..].to_vec())
                }
                _ => Value::Binary(bytes.to_vec()),
            }
        }
        // A type this reader does not know: its size is unknown, so the block ends here.
        _ => return Err(Eof),
    })
}

// Property tags this crate reads.
pub const PR_MESSAGE_CLASS: u16 = 0x001A;
pub const PR_SUBJECT: u16 = 0x0037;
pub const PR_CLIENT_SUBMIT_TIME: u16 = 0x0039;
pub const PR_SENT_REPRESENTING_NAME: u16 = 0x0042;
pub const PR_START_DATE: u16 = 0x0060;
pub const PR_END_DATE: u16 = 0x0061;
pub const PR_SENT_REPRESENTING_ADDRTYPE: u16 = 0x0064;
pub const PR_SENT_REPRESENTING_EMAIL_ADDRESS: u16 = 0x0065;
pub const PR_CONVERSATION_TOPIC: u16 = 0x0070;
pub const PR_RECIPIENT_TYPE: u16 = 0x0C15;
pub const PR_SENDER_NAME: u16 = 0x0C1A;
pub const PR_SENDER_ADDRTYPE: u16 = 0x0C1E;
pub const PR_SENDER_EMAIL_ADDRESS: u16 = 0x0C1F;
pub const PR_MESSAGE_DELIVERY_TIME: u16 = 0x0E06;
pub const PR_BODY: u16 = 0x1000;
pub const PR_RTF_COMPRESSED: u16 = 0x1009;
pub const PR_HTML: u16 = 0x1013;
pub const PR_DISPLAY_NAME: u16 = 0x3001;
pub const PR_ADDRTYPE: u16 = 0x3002;
pub const PR_EMAIL_ADDRESS: u16 = 0x3003;
pub const PR_SMTP_ADDRESS: u16 = 0x39FE;
pub const PR_INTERNET_CPID: u16 = 0x3FDE;
pub const PR_ATTACH_DATA: u16 = 0x3701;
pub const PR_ATTACH_EXTENSION: u16 = 0x3703;
pub const PR_ATTACH_FILENAME: u16 = 0x3704;
pub const PR_ATTACH_METHOD: u16 = 0x3705;
pub const PR_ATTACH_LONG_FILENAME: u16 = 0x3707;
pub const PR_ATTACH_MIME_TAG: u16 = 0x370E;
pub const PR_ATTACH_CONTENT_ID: u16 = 0x3712;
pub const PR_ATTACH_CONTENT_LOCATION: u16 = 0x3713;
pub const PR_ATTACH_FLAGS: u16 = 0x3714;
pub const PR_SENDER_SMTP_ADDRESS: u16 = 0x5D01;
pub const PR_SENT_REPRESENTING_SMTP_ADDRESS: u16 = 0x5D02;
pub const PR_ATTACHMENT_HIDDEN: u16 = 0x7FFE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_with_every_kind() {
        let mut data = Vec::new();
        data.extend(5u32.to_le_bytes());
        // PT_LONG 0x0E08 = 42
        data.extend([0x03, 0x00, 0x08, 0x0E]);
        data.extend(42u32.to_le_bytes());
        // PT_UNICODE PR_SUBJECT = "Hi"
        data.extend([0x1F, 0x00, 0x37, 0x00]);
        data.extend(1u32.to_le_bytes());
        data.extend(6u32.to_le_bytes());
        data.extend([b'H', 0, b'i', 0, 0, 0, 0, 0]);
        // named PT_BOOLEAN {appointment}/0x8215 = true
        data.extend([0x0B, 0x00, 0x00, 0x80]);
        data.extend(PSETID_APPOINTMENT);
        data.extend(0u32.to_le_bytes());
        data.extend(0x8215u32.to_le_bytes());
        data.extend(1u32.to_le_bytes());
        // named by string, PT_LONG
        data.extend([0x03, 0x00, 0x01, 0x80]);
        data.extend(PSETID_COMMON);
        data.extend(1u32.to_le_bytes());
        data.extend(4u32.to_le_bytes());
        data.extend([b'x', 0, 0, 0]);
        data.extend(7u32.to_le_bytes());
        // MV PT_LONG with two values
        data.extend([0x03, 0x10, 0x00, 0x30]);
        data.extend(2u32.to_le_bytes());
        data.extend(1u32.to_le_bytes());
        data.extend(2u32.to_le_bytes());
        let mut budget = 100;
        let (props, complete) = parse_block(&data, 1252, &mut budget);
        assert!(complete);
        assert_eq!(props.tag(0x0E08), Some(&Value::Long(42)));
        assert_eq!(props.str(PR_SUBJECT), Some("Hi"));
        assert_eq!(props.named(&PSETID_APPOINTMENT, 0x8215).and_then(Value::as_bool), Some(true));
        assert_eq!(props.tag(0x3000), Some(&Value::Multi(vec![Value::Long(1), Value::Long(2)])));
        assert!(matches!(&props.0[3].id, PropId::Name(_, name) if name == "x"));
    }

    #[test]
    fn made_up_counts_end_soon() {
        let mut data = Vec::new();
        data.extend(u32::MAX.to_le_bytes());
        data.extend([0x03, 0x10, 0x00, 0x30]);
        data.extend(u32::MAX.to_le_bytes());
        let mut budget = 1000;
        let (props, complete) = parse_block(&data, 1252, &mut budget);
        assert!(!complete);
        assert!(props.is_empty());
    }
}
