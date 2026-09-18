//! Reads IMAP commands (RFC 3501 with the extensions this server offers) from a complete command:
//! the connection has already collected every literal the command announced.

use crate::command::*;
use crate::mutf7;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// The tag, when the command got that far, so the answer can carry it.
    pub tag: Option<String>,
    pub message: String,
}

type Parsed<T> = Result<T, String>;

/// Where a literal starts in a line: `{123}` or `{123+}` right before its CRLF.
pub fn literal_announcement(line: &[u8]) -> Option<(usize, bool)> {
    let line = line.strip_suffix(b"\r\n").or_else(|| line.strip_suffix(b"\n"))?;
    let inner = line.strip_suffix(b"}")?;
    let open = inner.iter().rposition(|&b| b == b'{')?;
    let (digits, plus) = match inner[open + 1..].strip_suffix(b"+") {
        Some(digits) => (digits, true),
        None => (&inner[open + 1..], false),
    };
    if digits.is_empty() || digits.len() > 10 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let size = std::str::from_utf8(digits).ok()?.parse().ok()?;
    Some((size, plus))
}

/// How deeply a SEARCH key may nest.
///
/// `(`, `NOT` and `OR` each make [`Parser::search_key`] call itself, and nothing else stopped it:
/// a command line may be 64 KiB, which is tens of thousands of brackets, and the stack runs out
/// long before that. A stack overflow is not an error a process can catch — it takes the whole
/// server down, and this parser runs before anyone has logged in.
///
/// Real clients nest two or three levels. Thunderbird's widest saved search is nowhere near this.
const MAX_SEARCH_DEPTH: usize = 32;

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    /// Mailbox names come as UTF-8 once the client enabled UTF8=ACCEPT, as modified UTF-7 before.
    utf8: bool,
    /// How many SEARCH keys deep we are, against [`MAX_SEARCH_DEPTH`].
    depth: usize,
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/// Days since 1970-01-01 of a calendar date (proleptic Gregorian).
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The IMAP flag or keyword as the store keeps it.
pub fn keyword_of_flag(flag: &str) -> Option<String> {
    let lower = flag.to_ascii_lowercase();
    match lower.as_str() {
        "\\seen" => Some("$seen".into()),
        "\\answered" => Some("$answered".into()),
        "\\flagged" => Some("$flagged".into()),
        "\\draft" => Some("$draft".into()),
        "\\deleted" => Some("$deleted".into()),
        _ if lower.starts_with('\\') => None,
        _ => Some(lower),
    }
}

fn is_atom_char(b: u8) -> bool {
    b > 0x20 && b < 0x7f && !b"(){%*\"\\]".contains(&b)
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn at_end(&self) -> bool {
        matches!(&self.input[self.pos..], b"" | b"\r\n" | b"\n")
    }

    fn expect_end(&self) -> Parsed<()> {
        if self.at_end() { Ok(()) } else { Err("unexpected text at the end of the command".into()) }
    }

    fn byte(&mut self, expected: u8) -> Parsed<()> {
        if self.peek() == Some(expected) {
            self.pos += 1;
            Ok(())
        } else {
            Err(format!("expected '{}'", expected as char))
        }
    }

    fn sp(&mut self) -> Parsed<()> {
        self.byte(b' ').map_err(|_| "expected a space".to_owned())
    }

    fn eat(&mut self, expected: u8) -> bool {
        self.byte(expected).is_ok()
    }

    /// An atom-like word, with `]` allowed where the caller wants it.
    fn word(&mut self, allow_bracket: bool) -> Parsed<&'a str> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if is_atom_char(b) || (allow_bracket && b == b']') {
                self.pos += 1;
            } else {
                break;
            }
        }
        if start == self.pos {
            return Err("expected a word".into());
        }
        std::str::from_utf8(&self.input[start..self.pos]).map_err(|_| "a word is not ASCII".into())
    }

    fn atom(&mut self) -> Parsed<&'a str> {
        self.word(false)
    }

    /// A FETCH item name, which ends where its section starts.
    fn item_name(&mut self) -> Parsed<&'a str> {
        let start = self.pos;
        while self.peek().is_some_and(|b| is_atom_char(b) && b != b'[') {
            self.pos += 1;
        }
        if start == self.pos {
            return Err("expected a FETCH item".into());
        }
        std::str::from_utf8(&self.input[start..self.pos]).map_err(|_| "a word is not ASCII".into())
    }

    fn keyword(&mut self, expected: &str) -> Parsed<()> {
        let start = self.pos;
        match self.atom() {
            Ok(word) if word.eq_ignore_ascii_case(expected) => Ok(()),
            _ => {
                self.pos = start;
                Err(format!("expected {expected}"))
            }
        }
    }

    fn try_keyword(&mut self, expected: &str) -> bool {
        let start = self.pos;
        let len = expected.len();
        let matches =
            self.input.get(start..start + len).is_some_and(|word| word.eq_ignore_ascii_case(expected.as_bytes()))
                && !self.input.get(start + len).copied().is_some_and(is_atom_char);
        if matches {
            self.pos += len;
        }
        matches
    }

    fn number(&mut self) -> Parsed<u64> {
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        std::str::from_utf8(&self.input[start..self.pos])
            .ok()
            .filter(|digits| !digits.is_empty() && digits.len() <= 20)
            .and_then(|digits| digits.parse().ok())
            .ok_or_else(|| "expected a number".into())
    }

    fn nz_number32(&mut self) -> Parsed<u32> {
        match self.number()? {
            n @ 1..=0xffff_ffff => Ok(n as u32),
            _ => Err("expected a number from 1 to 4294967295".into()),
        }
    }

    fn quoted(&mut self) -> Parsed<Vec<u8>> {
        self.byte(b'"')?;
        let mut value = Vec::new();
        loop {
            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    return Ok(value);
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b @ (b'"' | b'\\')) => {
                            value.push(b);
                            self.pos += 1;
                        }
                        _ => return Err("only \\\" and \\\\ may be escaped in a quoted string".into()),
                    }
                }
                Some(b'\r' | b'\n') | None => return Err("a quoted string is not closed".into()),
                Some(b) => {
                    value.push(b);
                    self.pos += 1;
                }
            }
        }
    }

    fn literal(&mut self) -> Parsed<&'a [u8]> {
        self.byte(b'{')?;
        let size = self.number()? as usize;
        self.eat(b'+');
        self.byte(b'}')?;
        if !self.eat(b'\r') {
            return Err("a literal must end its line".into());
        }
        self.byte(b'\n')?;
        // checked: a client may announce up to twenty digits, and pos + size would wrap. It wraps
        // quietly in release and panics in a debug build -- and with panic = "abort" a panic here,
        // in a parser a stranger reaches before logging in, would take the whole server with it.
        let end = self.pos.checked_add(size).ok_or("the literal is longer than this server can hold")?;
        let data = self.input.get(self.pos..end).ok_or("the literal is shorter than announced")?;
        self.pos += size;
        Ok(data)
    }

    fn string(&mut self) -> Parsed<Vec<u8>> {
        match self.peek() {
            Some(b'"') => self.quoted(),
            Some(b'{') => self.literal().map(<[u8]>::to_vec),
            _ => Err("expected a string".into()),
        }
    }

    fn astring_bytes(&mut self) -> Parsed<Vec<u8>> {
        match self.peek() {
            Some(b'"' | b'{') => self.string(),
            _ => self.word(true).map(|word| word.as_bytes().to_vec()),
        }
    }

    fn astring(&mut self) -> Parsed<String> {
        self.astring_bytes().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    fn nstring(&mut self) -> Parsed<Option<String>> {
        if self.try_keyword("NIL") {
            return Ok(None);
        }
        self.string().map(|bytes| Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    fn mailbox(&mut self) -> Parsed<String> {
        let raw = self.astring_bytes()?;
        self.mailbox_name(raw)
    }

    fn mailbox_name(&self, raw: Vec<u8>) -> Parsed<String> {
        let name = if self.utf8 {
            String::from_utf8(raw).map_err(|_| "the mailbox name is not UTF-8".to_owned())?
        } else {
            let text = String::from_utf8(raw).map_err(|_| "the mailbox name is not modified UTF-7".to_owned())?;
            mutf7::decode(&text).unwrap_or(text)
        };
        if name.eq_ignore_ascii_case("INBOX") { Ok("INBOX".into()) } else { Ok(name) }
    }

    /// A LIST pattern: like a mailbox name, but `%` and `*` may appear unquoted.
    fn list_pattern(&mut self) -> Parsed<String> {
        let raw = match self.peek() {
            Some(b'"' | b'{') => self.string()?,
            _ => {
                let start = self.pos;
                while self.peek().is_some_and(|b| is_atom_char(b) || b == b'%' || b == b'*' || b == b']') {
                    self.pos += 1;
                }
                if start == self.pos {
                    return Err("expected a mailbox pattern".into());
                }
                self.input[start..self.pos].to_vec()
            }
        };
        self.mailbox_name(raw)
    }

    fn seq_num(&mut self) -> Parsed<SeqNum> {
        if self.eat(b'*') { Ok(SeqNum::Largest) } else { self.nz_number32().map(SeqNum::Value) }
    }

    fn sequence_set(&mut self) -> Parsed<SequenceSet> {
        let mut ranges = Vec::new();
        loop {
            let from = self.seq_num()?;
            let to = if self.eat(b':') { self.seq_num()? } else { from };
            ranges.push((from, to));
            if !self.eat(b',') {
                return Ok(SequenceSet(ranges));
            }
        }
    }

    fn flag(&mut self) -> Parsed<String> {
        let start = self.pos;
        self.eat(b'\\');
        if self.peek() == Some(b'*') {
            self.pos += 1;
        } else {
            self.atom()?;
        }
        Ok(String::from_utf8_lossy(&self.input[start..self.pos]).into_owned())
    }

    fn flag_list(&mut self) -> Parsed<Vec<String>> {
        self.byte(b'(')?;
        let mut flags = Vec::new();
        if self.eat(b')') {
            return Ok(flags);
        }
        loop {
            flags.push(self.flag()?);
            if self.eat(b')') {
                return Ok(flags);
            }
            self.sp()?;
        }
    }

    /// Items in parentheses separated by spaces, or a single item without them.
    fn list_of<T>(&mut self, mut item: impl FnMut(&mut Self) -> Parsed<T>) -> Parsed<Vec<T>> {
        if !self.eat(b'(') {
            return item(self).map(|value| vec![value]);
        }
        let mut values = Vec::new();
        if self.eat(b')') {
            return Ok(values);
        }
        loop {
            values.push(item(self)?);
            if self.eat(b')') {
                return Ok(values);
            }
            self.sp()?;
        }
    }

    fn date(&mut self) -> Parsed<i64> {
        let quoted = self.eat(b'"');
        let start = self.pos;
        while self.peek().is_some_and(|b| b.is_ascii_alphanumeric() || b == b'-') {
            self.pos += 1;
        }
        let text = std::str::from_utf8(&self.input[start..self.pos]).unwrap_or_default();
        if quoted {
            self.byte(b'"')?;
        }
        parse_date(text).ok_or_else(|| "expected a date like 17-Sep-2026".into())
    }

    fn status_item(&mut self) -> Parsed<StatusItem> {
        let word = self.atom()?.to_ascii_uppercase();
        Ok(match word.as_str() {
            "MESSAGES" => StatusItem::Messages,
            "RECENT" => StatusItem::Recent,
            "UIDNEXT" => StatusItem::UidNext,
            "UIDVALIDITY" => StatusItem::UidValidity,
            "UNSEEN" => StatusItem::Unseen,
            "SIZE" => StatusItem::Size,
            "DELETED" => StatusItem::Deleted,
            "HIGHESTMODSEQ" => StatusItem::HighestModSeq,
            _ => return Err(format!("unknown status item {word}")),
        })
    }

    fn command(&mut self) -> Parsed<CommandBody> {
        let name = self.atom()?.to_ascii_uppercase();
        let body = match name.as_str() {
            "CAPABILITY" => CommandBody::Capability,
            "NOOP" => CommandBody::Noop,
            "LOGOUT" => CommandBody::Logout,
            "STARTTLS" => CommandBody::StartTls,
            "NAMESPACE" => CommandBody::Namespace,
            "IDLE" => CommandBody::Idle,
            "CHECK" => CommandBody::Check,
            "CLOSE" => CommandBody::Close,
            "UNSELECT" => CommandBody::Unselect,
            "EXPUNGE" => CommandBody::Expunge { uids: None },
            "ID" => {
                self.sp()?;
                if !self.try_keyword("NIL") {
                    self.list_of(|p| p.nstring())?;
                }
                CommandBody::Id
            }
            "LOGIN" => {
                self.sp()?;
                let username = self.astring()?;
                self.sp()?;
                let password = self.astring()?;
                CommandBody::Login { username, password }
            }
            "AUTHENTICATE" => {
                self.sp()?;
                let mechanism = self.atom()?.to_ascii_uppercase();
                let initial = if self.eat(b' ') {
                    let start = self.pos;
                    while self.peek().is_some_and(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)) {
                        self.pos += 1;
                    }
                    Some(String::from_utf8_lossy(&self.input[start..self.pos]).into_owned())
                } else {
                    None
                };
                CommandBody::Authenticate { mechanism, initial }
            }
            "ENABLE" => {
                let mut capabilities = Vec::new();
                while self.eat(b' ') {
                    capabilities.push(self.atom()?.to_ascii_uppercase());
                }
                if capabilities.is_empty() {
                    return Err("ENABLE needs a capability".into());
                }
                CommandBody::Enable(capabilities)
            }
            "SELECT" | "EXAMINE" => {
                self.sp()?;
                let mailbox = self.mailbox()?;
                let (mut condstore, mut qresync) = (false, None);
                if self.eat(b' ') {
                    self.byte(b'(')?;
                    loop {
                        if self.try_keyword("CONDSTORE") {
                            condstore = true;
                        } else if self.try_keyword("QRESYNC") {
                            self.sp()?;
                            self.byte(b'(')?;
                            let uid_validity = self.nz_number32()?;
                            self.sp()?;
                            let modseq = self.number()?;
                            let mut known_uids = None;
                            if self.eat(b' ') {
                                if self.peek() == Some(b'(') {
                                    self.skip_parenthesized()?;
                                } else {
                                    known_uids = Some(self.sequence_set()?);
                                    if self.eat(b' ') {
                                        self.skip_parenthesized()?;
                                    }
                                }
                            }
                            self.byte(b')')?;
                            qresync = Some(QresyncParams { uid_validity, modseq, known_uids });
                        } else {
                            return Err("unknown SELECT parameter".into());
                        }
                        if self.eat(b')') {
                            break;
                        }
                        self.sp()?;
                    }
                }
                CommandBody::Select { mailbox, read_only: name == "EXAMINE", condstore, qresync }
            }
            "CREATE" | "DELETE" | "SUBSCRIBE" | "UNSUBSCRIBE" => {
                self.sp()?;
                let mailbox = self.mailbox()?;
                if name == "CREATE" && self.eat(b' ') {
                    // CREATE-SPECIAL-USE and friends: the options are not needed.
                    self.skip_parenthesized()?;
                }
                match name.as_str() {
                    "CREATE" => CommandBody::Create { mailbox },
                    "DELETE" => CommandBody::Delete { mailbox },
                    "SUBSCRIBE" => CommandBody::Subscribe { mailbox },
                    _ => CommandBody::Unsubscribe { mailbox },
                }
            }
            "RENAME" => {
                self.sp()?;
                let from = self.mailbox()?;
                self.sp()?;
                let to = self.mailbox()?;
                CommandBody::Rename { from, to }
            }
            "LIST" => CommandBody::List(self.list()?),
            "LSUB" => {
                self.sp()?;
                let reference = self.mailbox_or_empty()?;
                self.sp()?;
                let pattern = self.list_pattern()?;
                CommandBody::Lsub { reference, pattern }
            }
            "STATUS" => {
                self.sp()?;
                let mailbox = self.mailbox()?;
                self.sp()?;
                self.byte(b'(')?;
                let mut items = vec![self.status_item()?];
                while self.eat(b' ') {
                    items.push(self.status_item()?);
                }
                self.byte(b')')?;
                CommandBody::Status { mailbox, items }
            }
            "APPEND" => {
                self.sp()?;
                let mailbox = self.mailbox()?;
                self.sp()?;
                let mut flags = Vec::new();
                if self.peek() == Some(b'(') {
                    flags = self.flag_list()?;
                    self.sp()?;
                }
                let mut date = None;
                if self.peek() == Some(b'"') {
                    let text = self.quoted()?;
                    date = Some(
                        parse_date_time(&String::from_utf8_lossy(&text))
                            .ok_or("expected a date like \"17-Sep-2026 10:00:00 +0200\"")?,
                    );
                    self.sp()?;
                }
                let message = self.literal()?.to_vec();
                CommandBody::Append { mailbox, flags, date, message }
            }
            "SEARCH" | "FETCH" | "STORE" | "COPY" | "MOVE" => self.message_command(&name, false)?,
            "UID" => {
                self.sp()?;
                let sub = self.atom()?.to_ascii_uppercase();
                match sub.as_str() {
                    "SEARCH" | "FETCH" | "STORE" | "COPY" | "MOVE" => self.message_command(&sub, true)?,
                    "EXPUNGE" => {
                        self.sp()?;
                        CommandBody::Expunge { uids: Some(self.sequence_set()?) }
                    }
                    _ => return Err(format!("unknown command UID {sub}")),
                }
            }
            "GETQUOTA" => {
                self.sp()?;
                CommandBody::GetQuota { root: self.astring()? }
            }
            "GETQUOTAROOT" => {
                self.sp()?;
                CommandBody::GetQuotaRoot { mailbox: self.mailbox()? }
            }
            _ => return Err(format!("unknown command {name}")),
        };
        self.expect_end()?;
        Ok(body)
    }

    fn mailbox_or_empty(&mut self) -> Parsed<String> {
        let raw = self.astring_bytes()?;
        if raw.is_empty() { Ok(String::new()) } else { self.mailbox_name(raw) }
    }

    fn skip_parenthesized(&mut self) -> Parsed<()> {
        self.byte(b'(')?;
        let mut depth = 1;
        while depth > 0 {
            match self.peek() {
                Some(b'(') => depth += 1,
                Some(b')') => depth -= 1,
                Some(b'"') => {
                    self.quoted()?;
                    continue;
                }
                Some(b'{') => {
                    self.literal()?;
                    continue;
                }
                Some(b'\r' | b'\n') | None => return Err("a parenthesized list is not closed".into()),
                _ => {}
            }
            self.pos += 1;
        }
        Ok(())
    }

    fn list(&mut self) -> Parsed<ListCommand> {
        let mut list = ListCommand::default();
        self.sp()?;
        if self.eat(b'(') {
            list.extended = true;
            while !self.eat(b')') {
                self.eat(b' ');
                let option = self.atom()?.to_ascii_uppercase();
                match option.as_str() {
                    "SUBSCRIBED" => list.subscribed = true,
                    "SPECIAL-USE" => list.special_use = true,
                    "REMOTE" | "RECURSIVEMATCH" => {}
                    _ => return Err(format!("unknown LIST option {option}")),
                }
            }
            self.sp()?;
        }
        list.reference = self.mailbox_or_empty()?;
        self.sp()?;
        if self.eat(b'(') {
            list.extended = true;
            loop {
                list.patterns.push(self.list_pattern()?);
                if self.eat(b')') {
                    break;
                }
                self.sp()?;
            }
        } else {
            list.patterns.push(self.list_pattern()?);
        }
        if self.eat(b' ') {
            list.extended = true;
            self.keyword("RETURN")?;
            self.sp()?;
            self.byte(b'(')?;
            while !self.eat(b')') {
                self.eat(b' ');
                let option = self.atom()?.to_ascii_uppercase();
                match option.as_str() {
                    "SUBSCRIBED" => list.return_subscribed = true,
                    "CHILDREN" => list.return_children = true,
                    "SPECIAL-USE" => list.return_special_use = true,
                    "STATUS" => {
                        self.sp()?;
                        self.byte(b'(')?;
                        let mut items = vec![self.status_item()?];
                        while self.eat(b' ') {
                            items.push(self.status_item()?);
                        }
                        self.byte(b')')?;
                        list.return_status = Some(items);
                    }
                    _ => return Err(format!("unknown LIST return option {option}")),
                }
            }
        }
        Ok(list)
    }

    fn message_command(&mut self, name: &str, uid: bool) -> Parsed<CommandBody> {
        self.sp()?;
        if name == "SEARCH" {
            return self.search(uid);
        }
        let set = self.sequence_set()?;
        self.sp()?;
        match name {
            "FETCH" => {
                let items = if self.peek() == Some(b'(') {
                    self.list_of(|p| p.fetch_item())?
                } else {
                    let start = self.pos;
                    match self.atom()?.to_ascii_uppercase().as_str() {
                        "ALL" => {
                            vec![FetchItem::Flags, FetchItem::InternalDate, FetchItem::Rfc822Size, FetchItem::Envelope]
                        }
                        "FAST" => vec![FetchItem::Flags, FetchItem::InternalDate, FetchItem::Rfc822Size],
                        "FULL" => vec![
                            FetchItem::Flags,
                            FetchItem::InternalDate,
                            FetchItem::Rfc822Size,
                            FetchItem::Envelope,
                            FetchItem::Body,
                        ],
                        _ => {
                            self.pos = start;
                            vec![self.fetch_item()?]
                        }
                    }
                };
                let (mut changed_since, mut vanished) = (None, false);
                if self.eat(b' ') {
                    self.byte(b'(')?;
                    loop {
                        if self.try_keyword("CHANGEDSINCE") {
                            self.sp()?;
                            changed_since = Some(self.number()?);
                        } else if self.try_keyword("VANISHED") {
                            vanished = true;
                        } else {
                            return Err("unknown FETCH modifier".into());
                        }
                        if self.eat(b')') {
                            break;
                        }
                        self.sp()?;
                    }
                }
                if vanished && (!uid || changed_since.is_none()) {
                    return Err("VANISHED needs UID FETCH with CHANGEDSINCE".into());
                }
                Ok(CommandBody::Fetch { uid, set, items, changed_since, vanished })
            }
            "STORE" => {
                let mut unchanged_since = None;
                if self.eat(b'(') {
                    self.keyword("UNCHANGEDSINCE")?;
                    self.sp()?;
                    unchanged_since = Some(self.number()?);
                    self.byte(b')')?;
                    self.sp()?;
                }
                let action = if self.eat(b'+') {
                    StoreAction::Add
                } else if self.eat(b'-') {
                    StoreAction::Remove
                } else {
                    StoreAction::Replace
                };
                let item = self.atom()?.to_ascii_uppercase();
                let silent = match item.as_str() {
                    "FLAGS" => false,
                    "FLAGS.SILENT" => true,
                    _ => return Err("expected FLAGS or FLAGS.SILENT".into()),
                };
                self.sp()?;
                let flags = if self.peek() == Some(b'(') {
                    self.flag_list()?
                } else {
                    let mut flags = vec![self.flag()?];
                    while self.eat(b' ') {
                        flags.push(self.flag()?);
                    }
                    flags
                };
                Ok(CommandBody::Store { uid, set, unchanged_since, action, silent, flags })
            }
            _ => {
                let mailbox = self.mailbox()?;
                Ok(if name == "COPY" {
                    CommandBody::Copy { uid, set, mailbox }
                } else {
                    CommandBody::Move { uid, set, mailbox }
                })
            }
        }
    }

    fn fetch_item(&mut self) -> Parsed<FetchItem> {
        let word = self.item_name()?.to_ascii_uppercase();
        let (base, peek) = match word.as_str() {
            "ENVELOPE" => return Ok(FetchItem::Envelope),
            "FLAGS" => return Ok(FetchItem::Flags),
            "INTERNALDATE" => return Ok(FetchItem::InternalDate),
            "RFC822" => return Ok(FetchItem::Rfc822),
            "RFC822.HEADER" => return Ok(FetchItem::Rfc822Header),
            "RFC822.SIZE" => return Ok(FetchItem::Rfc822Size),
            "RFC822.TEXT" => return Ok(FetchItem::Rfc822Text),
            "BODYSTRUCTURE" => return Ok(FetchItem::BodyStructure),
            "UID" => return Ok(FetchItem::Uid),
            "MODSEQ" => return Ok(FetchItem::ModSeq),
            "BODY" => ("BODY", false),
            "BODY.PEEK" => ("BODY", true),
            _ => return Err(format!("unknown FETCH item {word}")),
        };
        if self.peek() != Some(b'[') {
            if peek {
                return Err("BODY.PEEK needs a section".into());
            }
            return Ok(FetchItem::Body);
        }
        debug_assert_eq!(base, "BODY");
        self.byte(b'[')?;
        let section = self.section()?;
        self.byte(b']')?;
        let mut partial = None;
        if self.eat(b'<') {
            let origin = u32::try_from(self.number()?).map_err(|_| "the partial origin is too big")?;
            self.byte(b'.')?;
            let count = self.nz_number32()?;
            self.byte(b'>')?;
            partial = Some((origin, count));
        }
        Ok(FetchItem::BodySection { section, partial, peek })
    }

    fn section(&mut self) -> Parsed<Section> {
        let mut section = Section::default();
        if self.peek() == Some(b']') {
            return Ok(section);
        }
        loop {
            if self.peek().is_some_and(|b| b.is_ascii_digit()) {
                section.part.push(self.nz_number32()?);
                if self.eat(b'.') {
                    continue;
                }
                return Ok(section);
            }
            let word = self.word(false)?.to_ascii_uppercase();
            section.text = Some(match word.as_str() {
                "HEADER" => SectionText::Header,
                "TEXT" => SectionText::Text,
                "MIME" if !section.part.is_empty() => SectionText::Mime,
                "HEADER.FIELDS" | "HEADER.FIELDS.NOT" => {
                    self.sp()?;
                    let fields = self.list_of(|p| p.astring())?;
                    if fields.is_empty() {
                        return Err("HEADER.FIELDS needs field names".into());
                    }
                    if word == "HEADER.FIELDS" {
                        SectionText::HeaderFields(fields)
                    } else {
                        SectionText::HeaderFieldsNot(fields)
                    }
                }
                _ => return Err(format!("unknown section {word}")),
            });
            return Ok(section);
        }
    }

    fn search(&mut self, uid: bool) -> Parsed<CommandBody> {
        let mut returns = None;
        if self.try_keyword("RETURN") {
            self.sp()?;
            self.byte(b'(')?;
            let mut options = Vec::new();
            while !self.eat(b')') {
                self.eat(b' ');
                options.push(match self.atom()?.to_ascii_uppercase().as_str() {
                    "MIN" => SearchReturn::Min,
                    "MAX" => SearchReturn::Max,
                    "ALL" => SearchReturn::All,
                    "COUNT" => SearchReturn::Count,
                    other => return Err(format!("unknown SEARCH return option {other}")),
                });
            }
            if options.is_empty() {
                options.push(SearchReturn::All);
            }
            returns = Some(options);
            self.sp()?;
        }
        if self.try_keyword("CHARSET") {
            self.sp()?;
            let charset = self.astring()?;
            if !["UTF-8", "US-ASCII"].iter().any(|known| known.eq_ignore_ascii_case(&charset)) {
                return Err("[BADCHARSET (UTF-8 US-ASCII)] only UTF-8 and US-ASCII".into());
            }
            self.sp()?;
        }
        let mut keys = vec![self.search_key()?];
        while self.eat(b' ') {
            keys.push(self.search_key()?);
        }
        let criteria = if keys.len() == 1 { keys.remove(0) } else { SearchKey::And(keys) };
        Ok(CommandBody::Search { uid, returns, criteria })
    }

    fn search_string(&mut self) -> Parsed<String> {
        self.sp()?;
        self.astring()
    }

    fn search_key(&mut self) -> Parsed<SearchKey> {
        self.depth += 1;
        if self.depth > MAX_SEARCH_DEPTH {
            self.depth -= 1;
            return Err("the search is nested too deeply".into());
        }
        let key = self.nested_search_key();
        self.depth -= 1;
        key
    }

    fn nested_search_key(&mut self) -> Parsed<SearchKey> {
        if self.eat(b'(') {
            let mut keys = vec![self.search_key()?];
            while self.eat(b' ') {
                keys.push(self.search_key()?);
            }
            self.byte(b')')?;
            return Ok(if keys.len() == 1 { keys.remove(0) } else { SearchKey::And(keys) });
        }
        if self.peek().is_some_and(|b| b.is_ascii_digit() || b == b'*') {
            return self.sequence_set().map(SearchKey::SequenceSet);
        }
        let word = self.atom()?.to_ascii_uppercase();
        let keyword = |name: &str| SearchKey::Keyword(name.into());
        let unkeyword = |name: &str| SearchKey::Unkeyword(name.into());
        Ok(match word.as_str() {
            "ALL" => SearchKey::All,
            "ANSWERED" => keyword("$answered"),
            "DELETED" => keyword("$deleted"),
            "DRAFT" => keyword("$draft"),
            "FLAGGED" => keyword("$flagged"),
            "SEEN" => keyword("$seen"),
            "UNANSWERED" => unkeyword("$answered"),
            "UNDELETED" => unkeyword("$deleted"),
            "UNDRAFT" => unkeyword("$draft"),
            "UNFLAGGED" => unkeyword("$flagged"),
            "UNSEEN" => unkeyword("$seen"),
            "NEW" => SearchKey::New,
            "OLD" => SearchKey::Old,
            "RECENT" => SearchKey::Recent,
            "KEYWORD" | "UNKEYWORD" => {
                self.sp()?;
                let flag = self.flag()?;
                let name = keyword_of_flag(&flag).ok_or("not a keyword")?;
                if word == "KEYWORD" { SearchKey::Keyword(name) } else { SearchKey::Unkeyword(name) }
            }
            "BCC" => SearchKey::Bcc(self.search_string()?),
            "CC" => SearchKey::Cc(self.search_string()?),
            "FROM" => SearchKey::From(self.search_string()?),
            "TO" => SearchKey::To(self.search_string()?),
            "SUBJECT" => SearchKey::Subject(self.search_string()?),
            "BODY" => SearchKey::Body(self.search_string()?),
            "TEXT" => SearchKey::Text(self.search_string()?),
            "HEADER" => {
                let field = self.search_string()?;
                SearchKey::Header(field, self.search_string()?)
            }
            "BEFORE" | "ON" | "SINCE" | "SENTBEFORE" | "SENTON" | "SENTSINCE" => {
                self.sp()?;
                let day = self.date()?;
                match word.as_str() {
                    "BEFORE" => SearchKey::Before(day),
                    "ON" => SearchKey::On(day),
                    "SINCE" => SearchKey::Since(day),
                    "SENTBEFORE" => SearchKey::SentBefore(day),
                    "SENTON" => SearchKey::SentOn(day),
                    _ => SearchKey::SentSince(day),
                }
            }
            "LARGER" | "SMALLER" | "YOUNGER" | "OLDER" => {
                self.sp()?;
                let n = self.number()?;
                match word.as_str() {
                    "LARGER" => SearchKey::Larger(n),
                    "SMALLER" => SearchKey::Smaller(n),
                    "YOUNGER" => SearchKey::Younger(n),
                    _ => SearchKey::Older(n),
                }
            }
            "UID" => {
                self.sp()?;
                SearchKey::Uid(self.sequence_set()?)
            }
            "NOT" => {
                self.sp()?;
                SearchKey::Not(Box::new(self.search_key()?))
            }
            "OR" => {
                self.sp()?;
                let left = self.search_key()?;
                self.sp()?;
                SearchKey::Or(Box::new(left), Box::new(self.search_key()?))
            }
            "MODSEQ" => {
                self.sp()?;
                if self.peek() == Some(b'"') {
                    // An entry name and type; only per-message modseqs exist here.
                    self.quoted()?;
                    self.sp()?;
                    self.atom()?;
                    self.sp()?;
                }
                SearchKey::ModSeq(self.number()?)
            }
            _ => return Err(format!("unknown search key {word}")),
        })
    }
}

/// `17-Sep-2026` as days since 1970-01-01.
pub fn parse_date(text: &str) -> Option<i64> {
    let mut parts = text.split('-');
    let day: u32 = parts.next()?.parse().ok()?;
    let month = parts.next()?.to_ascii_lowercase();
    let month = MONTHS.iter().position(|name| *name == month)? as u32 + 1;
    let year: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=31).contains(&day) || !(1..=9999).contains(&year) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// `17-Sep-2026 10:00:00 +0200` (the day may have a leading space) as a Unix time.
pub fn parse_date_time(text: &str) -> Option<i64> {
    let text = text.trim_start();
    let (date, rest) = text.split_once(' ')?;
    let (time, zone) = rest.split_once(' ')?;
    let days = parse_date(date)?;
    let mut clock = time.split(':').map(|part| part.parse::<i64>().ok());
    let (h, m, s) = (clock.next()??, clock.next()??, clock.next()??);
    if clock.next().is_some() || h > 23 || m > 59 || s > 60 || zone.len() != 5 {
        return None;
    }
    let sign = match zone.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let zone_h: i64 = zone[1..3].parse().ok()?;
    let zone_m: i64 = zone[3..5].parse().ok()?;
    Some(days * 86_400 + h * 3600 + m * 60 + s - sign * (zone_h * 3600 + zone_m * 60))
}

/// Parses one complete command. `utf8` says whether the client enabled UTF8=ACCEPT.
pub fn parse_command(input: &[u8], utf8: bool) -> Result<Command, ParseError> {
    let mut parser = Parser { input, pos: 0, utf8, depth: 0 };
    let tag = match parser.word(false) {
        Ok(tag) if !tag.contains('+') => tag.to_owned(),
        _ => return Err(ParseError { tag: None, message: "a command starts with a tag".into() }),
    };
    let body = parser.sp().and_then(|_| parser.command());
    match body {
        Ok(body) => Ok(Command { tag, body }),
        Err(message) => Err(ParseError { tag: Some(tag), message }),
    }
}

/// The client's answer to an AUTHENTICATE challenge, or `None` when it cancelled with `*`.
pub fn parse_continuation(line: &[u8]) -> Option<&str> {
    let line = std::str::from_utf8(line).ok()?.trim_end_matches(['\r', '\n']);
    (line != "*").then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> CommandBody {
        parse_command(text.as_bytes(), false).unwrap_or_else(|err| panic!("{text}: {err:?}")).body
    }

    #[test]
    fn literals_are_announced_at_the_end_of_a_line() {
        assert_eq!(literal_announcement(b"a APPEND INBOX {310}\r\n"), Some((310, false)));
        assert_eq!(literal_announcement(b"a LOGIN {4+}\r\n"), Some((4, true)));
        assert_eq!(literal_announcement(b"a LOGIN mini {x}\r\n"), None);
        assert_eq!(literal_announcement(b"a NOOP\r\n"), None);
    }

    #[test]
    fn logins_accept_atoms_quoted_strings_and_literals() {
        assert_eq!(
            parse("a1 LOGIN mini@example.de \"geheim \\\"passwort\\\"\"\r\n"),
            CommandBody::Login { username: "mini@example.de".into(), password: "geheim \"passwort\"".into() }
        );
        assert_eq!(
            parse("a2 LOGIN {4}\r\nmini {6}\r\nkatze!\r\n"),
            CommandBody::Login { username: "mini".into(), password: "katze!".into() }
        );
        let error = parse_command(b"a3 LOGIN mini\r\n", false).unwrap_err();
        assert_eq!(error.tag.as_deref(), Some("a3"));
    }

    #[test]
    fn select_with_qresync() {
        assert_eq!(
            parse("s SELECT inbox (QRESYNC (67890007 20050715194045000 41,43:211,214:541))\r\n"),
            CommandBody::Select {
                mailbox: "INBOX".into(),
                read_only: false,
                condstore: false,
                qresync: Some(QresyncParams {
                    uid_validity: 67890007,
                    modseq: 20050715194045000,
                    known_uids: Some(SequenceSet(vec![
                        (SeqNum::Value(41), SeqNum::Value(41)),
                        (SeqNum::Value(43), SeqNum::Value(211)),
                        (SeqNum::Value(214), SeqNum::Value(541)),
                    ])),
                }),
            }
        );
        assert!(matches!(
            parse("e EXAMINE \"Gesendet\" (CONDSTORE)\r\n"),
            CommandBody::Select { read_only: true, condstore: true, .. }
        ));
    }

    #[test]
    fn mailbox_names_are_decoded_from_modified_utf7_until_utf8_is_enabled() {
        assert_eq!(parse("c CREATE \"Entw&APw-rfe\"\r\n"), CommandBody::Create { mailbox: "Entwürfe".into() });
        let utf8 = parse_command("c CREATE \"Entwürfe\"\r\n".as_bytes(), true).unwrap().body;
        assert_eq!(utf8, CommandBody::Create { mailbox: "Entwürfe".into() });
    }

    #[test]
    fn fetch_items_sections_and_modifiers() {
        let body = parse(
            "f UID FETCH 1:* (UID FLAGS BODY.PEEK[HEADER.FIELDS (From Subject)]<0.2048> BODY[1.2.MIME] MODSEQ) (CHANGEDSINCE 12 VANISHED)\r\n",
        );
        let CommandBody::Fetch { uid, set, items, changed_since, vanished } = body else { panic!() };
        assert!(uid && vanished);
        assert_eq!(changed_since, Some(12));
        assert_eq!(set, SequenceSet(vec![(SeqNum::Value(1), SeqNum::Largest)]));
        assert_eq!(
            items,
            vec![
                FetchItem::Uid,
                FetchItem::Flags,
                FetchItem::BodySection {
                    section: Section {
                        part: vec![],
                        text: Some(SectionText::HeaderFields(vec!["From".into(), "Subject".into()]))
                    },
                    partial: Some((0, 2048)),
                    peek: true,
                },
                FetchItem::BodySection {
                    section: Section { part: vec![1, 2], text: Some(SectionText::Mime) },
                    partial: None,
                    peek: false
                },
                FetchItem::ModSeq,
            ]
        );
        assert_eq!(
            parse("f FETCH 2 FAST\r\n"),
            CommandBody::Fetch {
                uid: false,
                set: SequenceSet(vec![(SeqNum::Value(2), SeqNum::Value(2))]),
                items: vec![FetchItem::Flags, FetchItem::InternalDate, FetchItem::Rfc822Size],
                changed_since: None,
                vanished: false,
            }
        );
        assert!(parse_command(b"f FETCH 1 (FLAGS) (VANISHED)\r\n", false).is_err());
    }

    #[test]
    fn store_copy_move_and_expunge() {
        assert_eq!(
            parse("s UID STORE 3:5 (UNCHANGEDSINCE 7) +FLAGS.SILENT (\\Seen $Forwarded)\r\n"),
            CommandBody::Store {
                uid: true,
                set: SequenceSet(vec![(SeqNum::Value(3), SeqNum::Value(5))]),
                unchanged_since: Some(7),
                action: StoreAction::Add,
                silent: true,
                flags: vec!["\\Seen".into(), "$Forwarded".into()],
            }
        );
        assert!(matches!(parse("m UID MOVE 1,4 Archive\r\n"), CommandBody::Move { uid: true, .. }));
        assert!(matches!(parse("x UID EXPUNGE 4:*\r\n"), CommandBody::Expunge { uids: Some(_) }));
    }

    #[test]
    fn search_keys_nest() {
        assert_eq!(
            parse(
                "s UID SEARCH RETURN (MIN COUNT) CHARSET UTF-8 OR FROM nyu (UNSEEN SINCE 1-Sep-2026) NOT DELETED\r\n"
            ),
            CommandBody::Search {
                uid: true,
                returns: Some(vec![SearchReturn::Min, SearchReturn::Count]),
                criteria: SearchKey::And(vec![
                    SearchKey::Or(
                        Box::new(SearchKey::From("nyu".into())),
                        Box::new(SearchKey::And(vec![
                            SearchKey::Unkeyword("$seen".into()),
                            SearchKey::Since(days_from_civil(2026, 9, 1)),
                        ])),
                    ),
                    SearchKey::Not(Box::new(SearchKey::Keyword("$deleted".into()))),
                ]),
            }
        );
        assert!(parse_command(b"s SEARCH CHARSET KOI8-R ALL\r\n", false).unwrap_err().message.contains("BADCHARSET"));
    }

    #[test]
    fn list_variants() {
        let CommandBody::List(plain) = parse("l LIST \"\" \"*\"\r\n") else { panic!() };
        assert_eq!((plain.patterns, plain.extended), (vec!["*".to_owned()], false));
        let CommandBody::List(extended) = parse(
            "l LIST (SUBSCRIBED) \"\" (INBOX \"Arch%\") RETURN (CHILDREN SPECIAL-USE STATUS (MESSAGES UNSEEN))\r\n",
        ) else {
            panic!()
        };
        assert!(extended.subscribed && extended.return_children && extended.return_special_use);
        assert_eq!(extended.patterns, vec!["INBOX", "Arch%"]);
        assert_eq!(extended.return_status, Some(vec![StatusItem::Messages, StatusItem::Unseen]));
    }

    #[test]
    fn append_with_flags_and_date() {
        let body = parse("a APPEND Sent (\\Seen) \"17-Sep-2026 12:30:00 +0200\" {5+}\r\nHallo\r\n");
        assert_eq!(
            body,
            CommandBody::Append {
                mailbox: "Sent".into(),
                flags: vec!["\\Seen".into()],
                date: Some(days_from_civil(2026, 9, 17) * 86_400 + 10 * 3600 + 30 * 60),
                message: b"Hallo".to_vec(),
            }
        );
    }

    #[test]
    fn id_and_enable() {
        assert_eq!(parse("i ID (\"name\" \"iPhone Mail\" \"version\" NIL)\r\n"), CommandBody::Id);
        assert_eq!(parse("i ID NIL\r\n"), CommandBody::Id);
        assert_eq!(
            parse("e ENABLE CONDSTORE qresync\r\n"),
            CommandBody::Enable(vec!["CONDSTORE".into(), "QRESYNC".into()])
        );
    }

    #[test]
    fn flags_map_to_keywords() {
        assert_eq!(keyword_of_flag("\\Seen").as_deref(), Some("$seen"));
        assert_eq!(keyword_of_flag("$MDNSent").as_deref(), Some("$mdnsent"));
        assert_eq!(keyword_of_flag("\\Recent"), None);
    }

    #[test]
    fn dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(parse_date("29-Feb-2024"), Some(days_from_civil(2024, 2, 29)));
        assert_eq!(parse_date_time(" 1-Jan-2000 00:00:00 +0100"), Some(days_from_civil(2000, 1, 1) * 86_400 - 3600));
        assert_eq!(parse_date("32-Jan-2000"), None);
    }
}

#[cfg(test)]
mod depth_tests {
    use super::*;

    /// A search nested past what any client sends is an error, not a crash.
    ///
    /// `(`, `NOT` and `OR` each make the parser call itself, and a command line may be 64 KiB —
    /// tens of thousands of brackets, far past the stack. This runs before anyone has logged in,
    /// and a stack overflow cannot be caught: it takes the whole server down.
    #[test]
    fn a_search_nested_too_deeply_is_refused_rather_than_fatal() {
        let nested = |depth: usize| {
            let mut line = b"z SEARCH ".to_vec();
            line.extend(std::iter::repeat_n(b'(', depth));
            line.extend_from_slice(b"ALL");
            line.extend(std::iter::repeat_n(b')', depth));
            line.extend_from_slice(b"\r\n");
            line
        };
        assert!(parse_command(&nested(4), false).is_ok(), "what a client really sends still works");
        // The key inside the brackets counts as a level of its own, so n brackets are n + 1 deep.
        assert!(parse_command(&nested(MAX_SEARCH_DEPTH - 1), false).is_ok(), "and the whole allowance");
        assert!(parse_command(&nested(MAX_SEARCH_DEPTH), false).is_err(), "one past it is refused");
        // The sizes that used to end the process. Reaching this line at all is the test.
        for depth in [200, 5_000, 30_000] {
            assert!(parse_command(&nested(depth), false).is_err(), "depth {depth} should be refused");
        }
    }

    /// `NOT` and `OR` recurse as well, so the same guard has to cover them.
    #[test]
    fn not_and_or_are_counted_too() {
        let mut line = b"z SEARCH ".to_vec();
        line.extend(std::iter::repeat_n(b"NOT ".as_slice(), 5_000).flatten().copied());
        line.extend_from_slice(b"ALL\r\n");
        assert!(parse_command(&line, false).is_err());

        let mut line = b"z SEARCH ".to_vec();
        line.extend(std::iter::repeat_n(b"OR ALL ".as_slice(), 5_000).flatten().copied());
        line.extend_from_slice(b"ALL\r\n");
        assert!(parse_command(&line, false).is_err());
    }

    /// A literal may announce twenty digits; adding that to the position must not wrap.
    #[test]
    fn an_absurd_literal_length_does_not_wrap_the_position() {
        for line in [
            b"z LOGIN {18446744073709551615}\r\nx\r\n".as_slice(),
            b"z LOGIN {99999999999999999999}\r\nx\r\n".as_slice(),
        ] {
            assert!(parse_command(line, false).is_err(), "it has to be an error, and never a panic");
        }
    }
}
