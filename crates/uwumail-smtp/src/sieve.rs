//! Sieve mail rules (RFC 5228): checking a script before it is stored, and running the active one
//! when mail arrives. The engine is `sieve-rs`; this module decides what it may do here.
//!
//! Running a script only *decides*: it answers with a [`Plan`] (where the message goes, with which
//! keywords, whether it is redirected), and delivery carries that out afterwards. A script that
//! fails in any way -- too slow, too hungry, something the engine refuses -- leaves the message in
//! the inbox, as if there were no script at all. Nothing a script says is logged, and nothing of the
//! message either.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use sieve::compiler::grammar::Capability;
use sieve::{
    Arena, Compiler, Context as SieveContext, Handler, Mailbox as SieveMailbox, MessageSource, Recipient, Reply,
    Runtime, Sieve, SieveAction, Status,
};
use uwumail_store::SIEVE_MAX_SCRIPT_SIZE;

/// The Sieve extensions scripts may `require` here -- exactly the ones delivery carries out. The
/// JMAP capability and ManageSieve's `SIEVE` list both show this.
///
/// Left out on purpose: `reject`/`ereject` (a refusal after acceptance is backscatter),
/// `vacation` (the vacation response is its own feature), `enotify`, `include`, `editheader`,
/// `duplicate` and `regex` (the engine bounds each match, but not the sum of them).
pub const EXTENSIONS: &[&str] = &[
    "body",
    "comparator-i;ascii-casemap",
    "comparator-i;ascii-numeric",
    "comparator-i;octet",
    "copy",
    "envelope",
    "fileinto",
    "imap4flags",
    "mailbox",
    "mailboxid",
    "relational",
    "subaddress",
    "variables",
];

/// How many redirects one run may make. The JMAP capability and ManageSieve show it too.
pub const MAX_REDIRECTS: usize = 1;
/// Instructions a run may execute before it is stopped.
const CPU_LIMIT: usize = 20_000;
/// Memory a run may take for its strings and variables.
const MEMORY_LIMIT: usize = 4 * 1024 * 1024;
/// The largest value of one variable.
const MAX_VARIABLE_SIZE: usize = 8 * 1024;
/// Actions one run may hand back; anything beyond is a script gone wrong.
const MAX_ACTIONS: usize = 64;
/// A redirect is left out once a message has been through this many servers.
const MAX_RECEIVED_HEADERS: usize = 25;

/// Tests the extensions above provide, next to the base ones. `sieve-rs` compiles an unknown test
/// into a runtime error; checking here turns it into a refusal while the script is handed in.
const TESTS: &[&str] = &[
    "address",
    "allof",
    "anyof",
    "body",
    "envelope",
    "exists",
    "false",
    "hasflag",
    "header",
    "mailboxexists",
    "mailboxidexists",
    "not",
    "size",
    "string",
    "true",
];

fn compiler() -> &'static Compiler {
    static COMPILER: OnceLock<Compiler> = OnceLock::new();
    COMPILER.get_or_init(|| {
        Compiler::new()
            .with_max_script_size(SIEVE_MAX_SCRIPT_SIZE)
            .with_max_string_size(SIEVE_MAX_SCRIPT_SIZE)
            .with_max_nested_blocks(15)
            .with_max_nested_tests(15)
    })
}

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        let refused: Vec<Capability> =
            Capability::all().iter().filter(|capability| !allowed(capability)).cloned().collect();
        Runtime::new()
            .without_capabilities(refused)
            .with_cpu_limit(CPU_LIMIT)
            .with_memory_limit(MEMORY_LIMIT)
            .with_max_variable_size(MAX_VARIABLE_SIZE)
            .with_max_redirects(MAX_REDIRECTS)
            .with_max_out_messages(MAX_REDIRECTS)
            .with_max_received_headers(MAX_RECEIVED_HEADERS)
            .with_max_nested_includes(0)
    })
}

fn allowed(capability: &Capability) -> bool {
    EXTENSIONS.contains(&capability.to_string().as_str())
}

// ---- checking a script ----

/// Checks a script the way it will be run: valid Sieve, only the extensions above, only tests the
/// engine knows. The error names the line, for people and for RFC 9661's `invalidSieve`.
pub fn validate(script: &[u8]) -> Result<(), String> {
    compile(script).map(|_| ())
}

fn compile(script: &[u8]) -> Result<Sieve<'static>, String> {
    let text = std::str::from_utf8(script).map_err(|_| "The script is not UTF-8.".to_owned())?;
    if text.trim().is_empty() {
        return Err("The script is empty.".into());
    }
    let scan = scan(text);
    let compiled = compiler().compile(prepare(text, &scan).as_bytes()).map_err(|err| err.to_string())?;
    if let Some((extension, line)) = scan.requires.iter().find(|(name, _)| !EXTENSIONS.contains(&name.as_str())) {
        return Err(format!("Extension {extension:?} is not supported here at line {line}."));
    }
    if let Some((test, line)) = scan.tests.iter().find(|(name, _)| !TESTS.contains(&name.to_ascii_lowercase().as_str()))
    {
        return Err(format!("Unknown test {test:?} at line {line}."));
    }
    // What `prepare` papers over must still be declared by the script itself.
    let undeclared = |capability: &str, used: Option<&(String, usize)>| match used {
        Some((_, line)) if !scan.requires.iter().any(|(name, _)| name == capability) => {
            Err(format!("Undeclared capability '{capability}' at line {line}."))
        }
        _ => Ok(()),
    };
    let tag = |name: &str| scan.tags.iter().find(|(tag, _)| tag.eq_ignore_ascii_case(name));
    let test = |name: &str| scan.tests.iter().find(|(test, _)| test.eq_ignore_ascii_case(name));
    undeclared("mailbox", tag("create").or_else(|| test("mailboxexists")))?;
    undeclared("mailboxid", tag("mailboxid").or_else(|| test("mailboxidexists")))?;
    Ok(compiled)
}

/// The script as `sieve-rs` needs to see it to run it the way RFC 5228 and its extensions say,
/// with everything added on the first line so line numbers stay what the author sees:
///
/// - `fileinto :mailboxid` asks the engine for the `mailbox` extension instead of `mailboxid`
///   (RFC 9042); a script that requires `mailboxid` gets `mailbox` as well.
/// - The engine forgets a variable at the end of the block it was first set in, where RFC 5229 keeps
///   it for the whole script. Every variable the script sets is declared empty up front -- the value
///   an unset variable has anyway -- so a value set inside `if` is still there after it.
fn prepare<'a>(text: &'a str, scan: &Scan) -> std::borrow::Cow<'a, str> {
    let required = |name: &str| scan.requires.iter().any(|(required, _)| required == name);
    let mut prefix = String::new();
    if required("mailboxid") && !required("mailbox") {
        prefix.push_str("require \"mailbox\"; ");
    }
    if required("variables") && !scan.variables.is_empty() {
        prefix.push_str("require \"variables\"; ");
        for name in &scan.variables {
            prefix.push_str(&format!("set \"{name}\" \"\"; "));
        }
    }
    if prefix.is_empty() {
        return text.into();
    }
    prefix.push_str(text);
    prefix.into()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Scan {
    /// Every capability named in a `require`, with its line.
    requires: Vec<(String, usize)>,
    /// Every test name, with its line.
    tests: Vec<(String, usize)>,
    /// Every tagged argument (`:copy`), without the colon, with its line.
    tags: Vec<(String, usize)>,
    /// The names of the variables `set` assigns, lowercase, once each.
    variables: Vec<String>,
}

/// Whether `name` is a plain variable name (RFC 5229 section 3), which is all `prepare` declares.
fn plain_variable(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 64
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Text(String),
    Other(char),
}

/// The tokens of a script with their lines, as RFC 5228 section 8.1 has them. Tolerant: whatever
/// it cannot read, the compiler has already refused.
fn tokens(text: &str) -> Vec<(Token, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let (mut pos, mut line) = (0, 1);
    while pos < bytes.len() {
        let c = bytes[pos];
        match c {
            b'\n' => {
                line += 1;
                pos += 1;
            }
            b' ' | b'\t' | b'\r' => pos += 1,
            b'#' => {
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
            }
            b'/' if bytes.get(pos + 1) == Some(&b'*') => {
                pos += 2;
                while pos < bytes.len() && !(bytes[pos] == b'*' && bytes.get(pos + 1) == Some(&b'/')) {
                    if bytes[pos] == b'\n' {
                        line += 1;
                    }
                    pos += 1;
                }
                pos += 2;
            }
            b'"' => {
                let start = line;
                let mut value = Vec::new();
                pos += 1;
                while pos < bytes.len() && bytes[pos] != b'"' {
                    if bytes[pos] == b'\\' && pos + 1 < bytes.len() {
                        pos += 1;
                    }
                    if bytes[pos] == b'\n' {
                        line += 1;
                    }
                    value.push(bytes[pos]);
                    pos += 1;
                }
                pos += 1;
                out.push((Token::Text(String::from_utf8_lossy(&value).into_owned()), start));
            }
            _ if c.is_ascii_alphabetic() || c == b'_' => {
                let start = pos;
                while pos < bytes.len() && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_') {
                    pos += 1;
                }
                let word = &text[start..pos];
                if word.eq_ignore_ascii_case("text") && bytes.get(pos) == Some(&b':') {
                    // A multi-line string runs up to a line with only a dot.
                    let start_line = line;
                    let mut value = String::new();
                    let rest = &text[pos + 1..];
                    let mut consumed = rest.len();
                    let mut offset = 0;
                    for (index, raw) in rest.split_inclusive('\n').enumerate() {
                        offset += raw.len();
                        line += usize::from(raw.ends_with('\n'));
                        if index == 0 {
                            continue;
                        }
                        let content = raw.trim_end_matches(['\r', '\n']);
                        if content == "." {
                            consumed = offset;
                            break;
                        }
                        value.push_str(content.strip_prefix('.').unwrap_or(content));
                        value.push('\n');
                    }
                    pos += 1 + consumed;
                    out.push((Token::Text(value), start_line));
                } else {
                    out.push((Token::Word(word.to_owned()), line));
                }
            }
            _ if c.is_ascii() => {
                out.push((Token::Other(c as char), line));
                pos += 1;
            }
            _ => pos += 1,
        }
    }
    out
}

fn scan(text: &str) -> Scan {
    let tokens = tokens(text);
    let mut scan = Scan::default();
    let (mut parens, mut brackets) = (0usize, 0usize);
    let mut index = 0;
    while index < tokens.len() {
        let (token, line) = &tokens[index];
        let previous = index.checked_sub(1).map(|i| &tokens[i].0);
        match token {
            Token::Other('(') => parens += 1,
            Token::Other(')') => parens = parens.saturating_sub(1),
            Token::Other('[') => brackets += 1,
            Token::Other(']') => brackets = brackets.saturating_sub(1),
            Token::Other(';' | '{' | '}') => (parens, brackets) = (0, 0),
            Token::Word(word) => {
                // A word after `:` is a tag, not a command or a test.
                let tag = matches!(previous, Some(Token::Other(':')));
                let starts_test = match previous {
                    Some(Token::Word(before)) => ["if", "elsif", "not"].iter().any(|w| before.eq_ignore_ascii_case(w)),
                    Some(Token::Other('(')) => true,
                    Some(Token::Other(',')) => parens > 0 && brackets == 0,
                    _ => false,
                };
                if tag {
                    scan.tags.push((word.clone(), *line));
                } else if word.eq_ignore_ascii_case("require") {
                    let mut next = index + 1;
                    while let Some((token, line)) = tokens.get(next) {
                        match token {
                            Token::Text(name) => scan.requires.push((name.clone(), *line)),
                            Token::Other('[' | ',') => {}
                            _ => break,
                        }
                        next += 1;
                    }
                } else if word.eq_ignore_ascii_case("set") && !starts_test {
                    // `set [modifiers] name value;`: the name is the last string but one.
                    let texts: Vec<&String> = tokens[index + 1..]
                        .iter()
                        .take_while(|(token, _)| !matches!(token, Token::Other(';' | '{' | '}')))
                        .filter_map(|(token, _)| match token {
                            Token::Text(text) => Some(text),
                            _ => None,
                        })
                        .collect();
                    if let Some(name) = texts.len().checked_sub(2).map(|at| texts[at].to_ascii_lowercase())
                        && plain_variable(&name)
                        && !scan.variables.contains(&name)
                    {
                        scan.variables.push(name);
                    }
                } else if starts_test {
                    scan.tests.push((word.clone(), *line));
                }
            }
            _ => {}
        }
        index += 1;
    }
    scan
}

// ---- running a script ----

/// Where a script files a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Inbox,
    Folder {
        /// The path, with `/` between the levels.
        name: String,
        /// A JMAP mailbox id (RFC 9042), tried before the name.
        mailbox_id: Option<String>,
        /// Create the folder when it is missing (RFC 5490).
        create: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filing {
    pub target: Target,
    /// JMAP keywords, from the IMAP flags the script set.
    pub keywords: Vec<String>,
}

/// What a script decided for one message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub filings: Vec<Filing>,
    /// Addresses to redirect to, at most [`MAX_REDIRECTS`].
    pub redirects: Vec<String>,
    /// The script discarded the message: without filings it is stored nowhere.
    pub discarded: bool,
}

impl Plan {
    /// The message stays in the inbox, as without a script.
    pub fn keep() -> Plan {
        Plan { filings: vec![Filing { target: Target::Inbox, keywords: Vec::new() }], ..Plan::default() }
    }
}

/// A folder of the account, as a script names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: i64,
    /// The full path with `/` between the levels, the way JMAP names it (`Inbox/Work`).
    pub path: String,
}

/// What a script may know about the message besides its content.
#[derive(Debug, Clone, Copy)]
pub struct Envelope<'a> {
    /// `MAIL FROM`, empty for bounces.
    pub from: &'a str,
    /// The recipient this delivery is for.
    pub to: &'a str,
}

/// The JMAP keyword for an IMAP flag, or `None` for flags that are no keyword (`\Deleted`,
/// `\Recent`) or cannot be one.
pub fn keyword(flag: &str) -> Option<String> {
    let flag = flag.trim();
    if let Some(system) = flag.strip_prefix('\\') {
        return match system.to_ascii_lowercase().as_str() {
            "seen" => Some("$seen".into()),
            "flagged" => Some("$flagged".into()),
            "answered" => Some("$answered".into()),
            "draft" => Some("$draft".into()),
            _ => None,
        };
    }
    let valid = !flag.is_empty()
        && flag.len() <= 255
        && flag.bytes().all(|b| (0x21..=0x7e).contains(&b) && !b"(){]%*\"\\".contains(&b));
    valid.then(|| flag.to_ascii_lowercase())
}

/// A JMAP mailbox id (`m12`) as the number the store knows.
pub fn mailbox_number(id: &str) -> Option<i64> {
    let digits = id.strip_prefix('m')?;
    if digits.is_empty() || digits.len() > 18 || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    digits.parse().ok()
}

/// Whether a path names the inbox: `INBOX` in any case, as in IMAP.
fn is_inbox(path: &str) -> bool {
    path.eq_ignore_ascii_case("INBOX")
}

/// Normalises a folder path as a script writes it: `INBOX/x` in any case means the inbox's `x`,
/// and a trailing separator is ignored.
pub fn normalize_path(path: &str, inbox: &str) -> String {
    let path = path.trim_end_matches('/');
    match path.split_once('/') {
        Some((first, rest)) if is_inbox(first) => format!("{inbox}/{rest}"),
        None if is_inbox(path) => inbox.to_owned(),
        _ => path.to_owned(),
    }
}

/// Finds a folder by path: the JMAP name path, or the IMAP one with `INBOX`.
pub fn find_folder<'a>(folders: &'a [Folder], inbox: &str, path: &str) -> Option<&'a Folder> {
    let path = normalize_path(path, inbox);
    folders.iter().find(|folder| folder.path == path)
}

struct Collector<'a> {
    folders: &'a [Folder],
    inbox: &'a str,
    plan: Plan,
    actions: usize,
    refused: Option<&'static str>,
}

impl<'a> Collector<'a> {
    fn keywords(flags: &[&str]) -> Vec<String> {
        let set: BTreeSet<String> =
            flags.iter().flat_map(|flags| flags.split_whitespace()).filter_map(keyword).collect();
        set.into_iter().collect()
    }

    fn exists(&self, mailbox: &SieveMailbox<'_>) -> bool {
        match mailbox {
            SieveMailbox::Name(name) => find_folder(self.folders, self.inbox, name).is_some(),
            SieveMailbox::Id(id) => mailbox_number(id).is_some_and(|n| self.folders.iter().any(|f| f.id == n)),
        }
    }
}

impl<'x> Handler<'x> for Collector<'_> {
    fn mailbox_exists(
        &mut self,
        _: &SieveContext<'x>,
        mailboxes: &[SieveMailbox<'_>],
        special_use: &[&str],
    ) -> Reply<bool> {
        Reply::Ready(special_use.is_empty() && mailboxes.iter().all(|mailbox| self.exists(mailbox)))
    }

    fn action(&mut self, _: &SieveContext<'x>, action: SieveAction<'x>) -> Reply<()> {
        self.actions += 1;
        if self.actions > MAX_ACTIONS {
            self.refused = Some("too many actions");
            return Reply::Ready(());
        }
        match action {
            SieveAction::Keep { flags, message_id: 0 } => {
                self.plan.filings.push(Filing { target: Target::Inbox, keywords: Self::keywords(flags) });
            }
            SieveAction::FileInto { folder, flags, mailbox_id, special_use: None, create, message_id: 0 } => {
                let target = if is_inbox(folder) && mailbox_id.is_none() {
                    Target::Inbox
                } else {
                    Target::Folder { name: folder.to_owned(), mailbox_id: mailbox_id.map(str::to_owned), create }
                };
                self.plan.filings.push(Filing { target, keywords: Self::keywords(flags) });
            }
            SieveAction::Discard => self.plan.discarded = true,
            SieveAction::SendMessage {
                source: MessageSource::Redirect,
                recipient: Recipient::Address(address),
                message_id: 0,
                ..
            } => self.plan.redirects.push(address.to_owned()),
            // Everything else needs an extension that is switched off, or a changed message.
            _ => self.refused = Some("an action that is not supported here"),
        }
        Reply::Ready(())
    }
}

/// Runs a script over a message. `Err` says why it failed, without anything of the script or the
/// message in it; delivery then keeps the message in the inbox.
pub fn run(script: &[u8], message: &[u8], envelope: Envelope<'_>, folders: &[Folder]) -> Result<Plan, String> {
    let compiled = compile(script)?;
    let inbox = folders.first().map_or("Inbox", |folder| folder.path.as_str());
    let mut collector = Collector { folders, inbox, plan: Plan::default(), actions: 0, refused: None };
    let mut arena = Arena::new();
    let mut instance = runtime().filter(message, &compiled, &mut arena);
    instance.set_user_address(envelope.to);
    if !envelope.from.is_empty() {
        instance.set_envelope(sieve::Envelope::From, envelope.from);
    }
    instance.set_envelope(sieve::Envelope::To, envelope.to);
    match instance.run(&mut collector) {
        Ok(Status::Finished) => {}
        // Nothing here ever answers later.
        Ok(Status::Pending) => return Err("the script waited for an answer".into()),
        Err(err) => return Err(runtime_reason(&err)),
    }
    if let Some(reason) = collector.refused {
        return Err(reason.into());
    }
    let mut plan = collector.plan;
    plan.redirects.truncate(MAX_REDIRECTS);
    Ok(plan)
}

/// Why a run failed, in words that carry nothing of the script.
fn runtime_reason(err: &sieve::runtime::RuntimeError) -> String {
    use sieve::runtime::RuntimeError as E;
    match err {
        E::CPULimitReached => "the script ran too long".into(),
        E::MemoryLimitReached => "the script used too much memory".into(),
        E::CapabilityNotAllowed(_) | E::CapabilityNotSupported(_) => "the script needs an extension that is off".into(),
        E::InvalidInstruction { line_num, .. } => format!("unknown instruction at line {line_num}"),
        E::ScriptErrorMessage(_) => "the script stopped with an error".into(),
        E::TooManyIncludes | E::ScriptNotFound(_) => "the script includes another".into(),
        E::InvalidBytecode | E::AwaitingInput => "the engine failed".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MESSAGE: &[u8] = b"From: Boss <boss@example.com>\r\n\
        To: Mini <mini@example.org>\r\n\
        Cc: team@example.org\r\n\
        List-Id: Cats <cats.example.org>\r\n\
        Subject: Quarterly *report* ready\r\n\
        \r\n\
        The numbers are in. Purr.\r\n";

    fn folders() -> Vec<Folder> {
        vec![
            Folder { id: 1, path: "Inbox".into() },
            Folder { id: 5, path: "Work".into() },
            Folder { id: 12, path: "Work/Boss".into() },
            Folder { id: 13, path: "Inbox/Lists".into() },
        ]
    }

    fn envelope() -> Envelope<'static> {
        Envelope { from: "boss@example.com", to: "mini@example.org" }
    }

    fn plan(script: &str) -> Plan {
        run(script.as_bytes(), MESSAGE, envelope(), &folders()).unwrap()
    }

    fn folder(name: &str, mailbox_id: Option<&str>, create: bool, keywords: &[&str]) -> Filing {
        Filing {
            target: Target::Folder { name: name.into(), mailbox_id: mailbox_id.map(str::to_owned), create },
            keywords: keywords.iter().map(|k| k.to_string()).collect(),
        }
    }

    fn inbox(keywords: &[&str]) -> Filing {
        Filing { target: Target::Inbox, keywords: keywords.iter().map(|k| k.to_string()).collect() }
    }

    #[test]
    fn without_actions_the_message_is_kept() {
        assert_eq!(plan("# nothing to do\n"), Plan::keep());
        assert_eq!(plan("if false { discard; }"), Plan::keep());
    }

    #[test]
    fn fileinto_moves_and_copy_keeps() {
        let moved = plan("require \"fileinto\";\nif header :contains \"subject\" \"report\" { fileinto \"Work\"; }");
        assert_eq!(moved.filings, [folder("Work", None, false, &[])]);

        let copied = plan("require [\"fileinto\", \"copy\"];\nfileinto :copy \"Work\";");
        assert_eq!(copied.filings, [folder("Work", None, false, &[]), inbox(&[])]);

        let created = plan("require [\"fileinto\", \"mailbox\"];\nfileinto :create \"New/Deep\";");
        assert_eq!(created.filings, [folder("New/Deep", None, true, &[])]);

        let by_id = plan("require [\"fileinto\", \"mailboxid\"];\nfileinto :mailboxid \"m12\" \"Work/Boss\";");
        assert_eq!(by_id.filings, [folder("Work/Boss", Some("m12"), false, &[])]);
    }

    #[test]
    fn discard_and_stop() {
        let discarded = plan("if address :is \"from\" \"boss@example.com\" { discard; stop; }\nkeep;");
        assert!(discarded.discarded);
        assert!(discarded.filings.is_empty());
        assert_eq!(plan("stop;\ndiscard;"), Plan::keep());
    }

    #[test]
    fn flags_become_keywords() {
        let flagged = plan(
            "require [\"imap4flags\", \"fileinto\"];\n\
             addflag \"\\\\Seen\";\naddflag [\"\\\\Flagged\", \"Work\"];\nfileinto \"Work\";",
        );
        assert_eq!(flagged.filings, [folder("Work", None, false, &["$flagged", "$seen", "work"])]);

        let local = plan("require [\"imap4flags\"];\nkeep :flags [\"\\\\Answered\", \"\\\\Deleted\"];");
        assert_eq!(local.filings, [inbox(&["$answered"])]);

        let removed = plan("require [\"imap4flags\"];\nsetflag \"\\\\Seen \\\\Flagged\";\nremoveflag \"\\\\Flagged\";");
        assert_eq!(removed.filings, [inbox(&["$seen"])]);

        let tested = plan(
            "require [\"imap4flags\", \"fileinto\"];\naddflag \"$important\";\n\
             if hasflag \"$important\" { fileinto \"Work\"; }",
        );
        assert_eq!(tested.filings, [folder("Work", None, false, &["$important"])]);
    }

    #[test]
    fn redirects_are_counted_and_copy_keeps() {
        let copied = plan("require \"copy\";\nredirect :copy \"phone@example.net\";");
        assert_eq!(copied.redirects, ["phone@example.net"]);
        assert_eq!(copied.filings, [inbox(&[])]);

        let moved = plan("redirect \"phone@example.net\";");
        assert!(moved.filings.is_empty(), "a redirect without :copy cancels the keep");

        let many = plan("redirect \"one@example.net\";\nredirect \"two@example.net\";");
        assert_eq!(many.redirects, ["one@example.net"], "only one redirect per message");

        let to_self = plan("redirect \"mini@example.org\";");
        assert!(to_self.redirects.is_empty(), "never to oneself");
        assert_eq!(to_self.filings, [inbox(&[])]);
    }

    #[test]
    fn tests_see_envelope_body_and_mailboxes() {
        let script = "require [\"envelope\", \"fileinto\", \"body\", \"mailbox\", \"variables\", \"relational\", \
                      \"comparator-i;ascii-numeric\", \"subaddress\"];\n\
                      if envelope :domain :is \"from\" \"example.com\" { fileinto \"Work\"; }\n\
                      if body :contains \"Purr\" { fileinto \"Work/Boss\"; }\n\
                      if mailboxexists \"INBOX/Lists\" { fileinto \"INBOX/Lists\"; }\n\
                      if mailboxexists \"Nope\" { fileinto \"Nope\"; }\n\
                      if header :matches \"list-id\" \"*<*>\" { set \"list\" \"${2}\"; }\n\
                      if string :is \"${list}\" \"cats.example.org\" { fileinto \"Lists\"; }\n\
                      if header :count \"ge\" :comparator \"i;ascii-numeric\" \"to\" \"1\" { keep; }\n\
                      if envelope :user \"to\" \"mini\" { keep; }";
        let filed = plan(script);
        let targets: Vec<String> = filed
            .filings
            .iter()
            .map(|filing| match &filing.target {
                Target::Inbox => "INBOX".into(),
                Target::Folder { name, .. } => name.clone(),
            })
            .collect();
        assert_eq!(targets, ["Work", "Work/Boss", "INBOX/Lists", "Lists", "INBOX"]);
    }

    /// The layout SPEC section 3 has the webmail and the apps write.
    #[test]
    fn the_rules_the_apps_write_run_as_meant() {
        let script = r#"# Mail rules managed by UwUMail. Edit them in UwUMail; edits made elsewhere switch UwUMail to text mode.
# uwumail-rules: {"v":1,"rules":[{"id":"r1","name":"Boss","enabled":true,"match":"all","conditions":[{"field":"from","op":"contains","value":"boss@example.com"}],"actions":[{"type":"markRead"},{"type":"forward","address":"me@example.org","keepCopy":true},{"type":"move","mailboxId":"m12","mailboxName":"Work/Boss"}],"stop":true}]}
require ["fileinto", "imap4flags", "mailboxid", "copy"];

# Boss
if allof (header :contains "from" "boss@example.com") {
    addflag "\\Seen";
    redirect :copy "me@example.org";
    fileinto :mailboxid "m12" "Work/Boss";
    stop;
}

# Lists
if anyof (header :matches "list-id" "*cats.example.org*", not header :is "subject" "x \\* y") {
    addflag "\\Flagged";
    fileinto :mailboxid "m13" "Inbox/Lists";
}

# Everything
if true {
    fileinto :mailboxid "m99" "Gone";
}
"#;
        validate(script.as_bytes()).unwrap();
        let filed = plan(script);
        assert_eq!(filed.redirects, ["me@example.org"]);
        assert_eq!(filed.filings, [folder("Work/Boss", Some("m12"), false, &["$seen"])]);

        let other = b"From: someone@example.net\r\nList-Id: <cats.example.org>\r\nSubject: hi\r\n\r\nhi\r\n";
        let filed = run(script.as_bytes(), other, envelope(), &folders()).unwrap();
        assert_eq!(
            filed.filings,
            [
                folder("Inbox/Lists", Some("m13"), false, &["$flagged"]),
                folder("Gone", Some("m99"), false, &["$flagged"])
            ]
        );
        assert!(filed.redirects.is_empty());
    }

    /// The apps escape `*`, `?` and `\` in values for `:matches` (startsWith and endsWith).
    #[test]
    fn escaped_wildcards_match_themselves() {
        let starts = r#"require "fileinto";
if header :matches "subject" "Quarterly \\*report\\**" { fileinto "Starts"; }
if header :matches "subject" "Quarterly \\*x*" { fileinto "Wrong"; }
if header :matches "subject" "*ready" { fileinto "Ends"; }
if header :matches "subject" "Quarterly ?report*" { fileinto "Wildcard"; }
if not header :matches "subject" "\\?*" { fileinto "NoQuestion"; }
"#;
        let targets: Vec<String> = plan(starts)
            .filings
            .into_iter()
            .map(|filing| match filing.target {
                Target::Folder { name, .. } => name,
                Target::Inbox => "INBOX".into(),
            })
            .collect();
        assert_eq!(targets, ["Starts", "Ends", "Wildcard", "NoQuestion"]);
    }

    #[test]
    fn invalid_scripts_are_refused_with_a_line() {
        let problem = |script: &str| validate(script.as_bytes()).unwrap_err();
        assert!(problem("#comment\nInvalidSieveCommand\n").contains("line 2"), "{}", problem("#c\nInvalid\n"));
        assert!(problem("require \"vacation\";\nvacation \"away\";").contains("vacation"));
        assert!(problem("require [\"fileinto\", \"regex\"];").contains("regex"));
        assert!(problem("require [\"fileinto\",\n\"reject\"];").contains("line 2"));
        assert!(problem("require \"include\";").contains("include"));
        assert!(problem("fileinto \"x\";").contains("fileinto"), "fileinto needs its require");
        assert!(problem("if frobnicate \"x\" { keep; }").contains("frobnicate"));
        assert!(problem("if anyof (true,\n  wibble) { keep; }").contains("line 2"));
        assert!(problem("if header :contains \"subject\" \"x {").contains("line"));
        assert!(problem("").contains("empty"));
        assert!(validate(&[0xff, 0xfe]).is_err(), "not UTF-8");
        // Words inside strings, comments and tags are neither tests nor requires.
        validate(b"# require \"vacation\"\n/* if wibble */\nif header :is \"x-require\" \"if wibble\" { keep; }")
            .unwrap();
        validate(b"require \"variables\";\nset :lower \"x\" text:\nrequire \"vacation\"\n..\n.\n;\nkeep;").unwrap();
    }

    #[test]
    fn the_engines_quirks_are_papered_over() {
        // A variable first set inside a block lives on after it (RFC 5229).
        let set = plan(
            "require [\"variables\", \"fileinto\"];\n\
             if header :matches \"list-id\" \"*<*>\" { set :lower \"List\" \"${2}\"; }\n\
             fileinto \"Lists/${list}\";",
        );
        assert_eq!(set.filings, [folder("Lists/cats.example.org", None, false, &[])]);

        // :mailboxid needs only "mailboxid", and :create or mailboxexists still need "mailbox".
        validate(b"require [\"fileinto\", \"mailboxid\"];\nif mailboxidexists \"m12\" { fileinto :mailboxid \"m12\" \"x\"; }")
            .unwrap();
        let problem = |script: &str| validate(script.as_bytes()).unwrap_err();
        assert!(
            problem("require [\"fileinto\", \"mailboxid\"];\n\nfileinto :create \"x\";")
                .contains("'mailbox' at line 3")
        );
        assert!(problem("require \"mailboxid\";\nif mailboxexists \"x\" { keep; }").contains("'mailbox'"));
        assert!(
            problem("require [\"fileinto\", \"mailbox\"];\nfileinto :mailboxid \"m1\" \"x\";").contains("'mailboxid'")
        );
        // Line numbers are the author's, even with something added in front.
        assert!(problem("require [\"variables\", \"mailboxid\"];\nset \"a\" \"b\";\nwibble;").contains("line 3"));
    }

    #[test]
    fn runaway_scripts_fail_and_are_kept() {
        // Sieve has no loops; a long enough test list is the most work a script within the size limit
        // can ask for.
        let script = format!("if allof ({}true) {{ discard; }}", "true,".repeat(12_000));
        assert!(script.len() < SIEVE_MAX_SCRIPT_SIZE);
        let failed = run(script.as_bytes(), MESSAGE, envelope(), &folders());
        assert!(failed.unwrap_err().contains("too long"));
    }

    #[test]
    fn keywords_and_paths() {
        assert_eq!(keyword("\\SEEN").as_deref(), Some("$seen"));
        assert_eq!(keyword("\\Recent"), None);
        assert_eq!(keyword("Project-X").as_deref(), Some("project-x"));
        assert_eq!(keyword("bad(flag"), None);
        assert_eq!(mailbox_number("m12"), Some(12));
        assert_eq!(mailbox_number("m012"), None);
        assert_eq!(mailbox_number("12"), None);
        assert_eq!(normalize_path("inbox/Lists/", "Inbox"), "Inbox/Lists");
        assert_eq!(normalize_path("INBOX", "Posteingang"), "Posteingang");
        let folders = folders();
        assert_eq!(find_folder(&folders, "Inbox", "INBOX/Lists").map(|f| f.id), Some(13));
        assert_eq!(find_folder(&folders, "Inbox", "Work/Boss").map(|f| f.id), Some(12));
        assert!(find_folder(&folders, "Inbox", "work").is_none(), "names are exact");
    }
}
