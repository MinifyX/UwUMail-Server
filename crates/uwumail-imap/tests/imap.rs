//! End-to-end IMAP sessions, as a mail app would talk to the server.

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use uwumail_imap::Imap;
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};

const PASSWORD: &str = "katzenpfote-123";

struct Server {
    imap: Imap,
    store: Store,
    account: i64,
    _dir: tempfile::TempDir,
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    let account = store
        .create_account(NewAccount {
            address: "mini@example.org".into(),
            display_name: "Mini".into(),
            password: Some(PASSWORD.into()),
            role: Role::User,
            quota_bytes: 10 * 1024 * 1024,
            protocols: None,
        })
        .await
        .unwrap()
        .id;
    Server { imap: Imap::new(store.clone(), 1024 * 1024), store, account, _dir: dir }
}

async fn deliver(store: &Store, account: i64, subject: &str) -> u32 {
    let raw = format!(
        "From: Nyu <nyu@example.net>\r\nTo: Mini <mini@example.org>\r\nSubject: {subject}\r\nMessage-ID: <{}@example.net>\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHallo Mini, {subject}\r\n\
--b\r\nContent-Type: image/png; name=nyu.png\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--b--\r\n",
        subject.replace(' ', "-")
    );
    let request = IngestRequest {
        account_id: account,
        raw: raw.into_bytes(),
        mailboxes: vec![MailboxTarget::Role(MailboxRole::Inbox)],
        keywords: vec![],
        received_at: None,
    };
    store.ingest(request).await.unwrap().uid as u32
}

struct Client {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
    next_tag: u32,
}

impl Client {
    async fn connect(server: &Server) -> Client {
        let (client, connection) = tokio::io::duplex(4 * 1024 * 1024);
        let imap = server.imap.clone();
        tokio::spawn(async move { imap.serve_connection(connection, "192.0.2.7:40000".parse().unwrap()).await });
        let (reader, writer) = tokio::io::split(client);
        let mut client = Client { reader: BufReader::new(reader), writer, next_tag: 1 };
        let greeting = client.line().await;
        assert!(greeting.starts_with("* OK [CAPABILITY IMAP4rev1"), "{greeting}");
        // The apps tell a UwUMail server by these words; changing them loses its accounts' pictures.
        assert!(greeting.trim_end().ends_with("] UwUMail IMAP ready"), "{greeting}");
        client
    }

    async fn login(server: &Server) -> Client {
        let mut client = Client::connect(server).await;
        let (_, done) = client.command(&format!("LOGIN mini@example.org \"{PASSWORD}\"")).await;
        assert!(done.contains("OK [CAPABILITY"), "{done}");
        client
    }

    /// One response line, with the data of any literals it carries inline.
    async fn line(&mut self) -> String {
        let mut text = Vec::new();
        loop {
            let mut line = Vec::new();
            let read = tokio::time::timeout(Duration::from_secs(10), self.reader.read_until(b'\n', &mut line))
                .await
                .expect("the server answered in time")
                .unwrap();
            assert!(read > 0, "the server hung up");
            text.extend_from_slice(&line);
            let trimmed = String::from_utf8_lossy(&line).trim_end().to_owned();
            let Some(size) =
                trimmed.strip_suffix('}').and_then(|s| s.rsplit_once('{')).and_then(|(_, n)| n.parse().ok())
            else {
                break;
            };
            let mut literal = vec![0u8; size];
            self.reader.read_exact(&mut literal).await.unwrap();
            text.extend_from_slice(&literal);
        }
        String::from_utf8_lossy(&text).trim_end().to_owned()
    }

    async fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    /// Sends a command and returns the untagged answers and the tagged one.
    async fn command(&mut self, command: &str) -> (Vec<String>, String) {
        let tag = format!("t{}", self.next_tag);
        self.next_tag += 1;
        self.send(format!("{tag} {command}\r\n").as_bytes()).await;
        self.until_tagged(&tag).await
    }

    async fn until_tagged(&mut self, tag: &str) -> (Vec<String>, String) {
        let mut untagged = Vec::new();
        loop {
            let line = self.line().await;
            if line.starts_with(&format!("{tag} ")) {
                return (untagged, line);
            }
            untagged.push(line);
        }
    }
}

fn find<'a>(lines: &'a [String], needle: &str) -> &'a str {
    lines.iter().find(|line| line.contains(needle)).unwrap_or_else(|| panic!("no line with {needle:?} in {lines:#?}"))
}

fn number_after(text: &str, prefix: &str) -> u64 {
    let start = text.find(prefix).unwrap_or_else(|| panic!("{prefix} not in {text}")) + prefix.len();
    text[start..].chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap()
}

#[tokio::test]
async fn apps_log_in_list_select_and_fetch() {
    let server = server().await;
    let mut client = Client::connect(&server).await;
    let (_, denied) = client.command("LOGIN mini@example.org falsch").await;
    assert!(denied.contains("NO [AUTHENTICATIONFAILED]"), "{denied}");
    let (_, early) = client.command("SELECT INBOX").await;
    assert!(early.contains("BAD"), "{early}");
    // AUTHENTICATE PLAIN with the initial response right away (SASL-IR).
    let plain = base64_plain("mini@example.org", PASSWORD);
    let (_, done) = client.command(&format!("AUTHENTICATE PLAIN {plain}")).await;
    assert!(done.contains("OK"), "{done}");

    let (lines, _) = client.command("LIST \"\" \"*\"").await;
    assert!(find(&lines, "\"INBOX\"").contains("\\HasNoChildren"));
    assert!(find(&lines, "\"Sent\"").contains("\\Sent"));
    assert!(find(&lines, "\"Junk\"").contains("\\Junk"));

    let first = deliver(&server.store, server.account, "Katzenfutter").await;
    let (lines, done) = client.command("SELECT INBOX").await;
    assert!(done.contains("OK [READ-WRITE]"), "{done}");
    find(&lines, "* 1 EXISTS");
    find(&lines, "[UIDVALIDITY ");
    find(&lines, "[HIGHESTMODSEQ ");
    find(&lines, "[UNSEEN 1]");

    let (lines, _) = client
        .command(&format!(
            "UID FETCH {first} (FLAGS RFC822.SIZE ENVELOPE BODYSTRUCTURE BODY.PEEK[HEADER.FIELDS (SUBJECT)])"
        ))
        .await;
    let fetched = find(&lines, "FETCH");
    assert!(fetched.starts_with(&format!("* 1 FETCH (UID {first} FLAGS () RFC822.SIZE ")), "{fetched}");
    assert!(fetched.contains("\"Katzenfutter\" ((\"Nyu\" NIL \"nyu\" \"example.net\"))"), "{fetched}");
    assert!(fetched.contains("(\"TEXT\" \"PLAIN\" (\"CHARSET\" \"utf-8\")"), "{fetched}");
    assert!(fetched.contains("\"MIXED\" (\"BOUNDARY\" \"b\")"), "{fetched}");
    assert!(fetched.contains("BODY[HEADER.FIELDS (SUBJECT)] {25}\r\nSubject: Katzenfutter\r\n\r\n"), "{fetched}");

    // Reading a part without PEEK marks the message as seen.
    let (lines, _) = client.command("FETCH 1 BODY[1]").await;
    let fetched = find(&lines, "FETCH");
    assert!(fetched.contains("BODY[1] {24}\r\nHallo Mini, Katzenfutter"), "{fetched}");
    assert!(fetched.contains("FLAGS (\\Seen)"), "{fetched}");
    let (lines, _) = client.command("FETCH 1 BODY.PEEK[2]<0.4>").await;
    assert!(find(&lines, "FETCH").contains("BODY[2]<0> {4}\r\niVBO"));

    let (_, done) = client.command("LOGOUT").await;
    assert!(done.contains("OK"));
}

#[tokio::test]
async fn append_store_search_move_and_expunge() {
    let server = server().await;
    let mut client = Client::login(&server).await;

    // A synchronizing literal waits for the server's go-ahead.
    let message = "From: mini@example.org\r\nTo: leni@example.org\r\nSubject: Entwurf\r\n\r\nNoch nicht fertig\r\n";
    client.send(format!("a1 APPEND Drafts (\\Draft) {{{}}}\r\n", message.len()).as_bytes()).await;
    assert!(client.line().await.starts_with("+ "));
    client.send(format!("{message}\r\n").as_bytes()).await;
    let (_, done) = client.until_tagged("a1").await;
    assert!(done.contains("OK [APPENDUID "), "{done}");
    // LITERAL+ needs no go-ahead.
    let second = "Subject: Zweiter Entwurf\r\n\r\nText\r\n";
    client.send(format!("a2 APPEND Drafts {{{}+}}\r\n{second}\r\n", second.len()).as_bytes()).await;
    let (_, done) = client.until_tagged("a2").await;
    assert!(done.contains("OK [APPENDUID "), "{done}");

    let (lines, _) = client.command("SELECT Drafts").await;
    find(&lines, "* 2 EXISTS");
    let (lines, done) = client.command("STORE 1 +FLAGS (\\Flagged $Forwarded)").await;
    assert!(done.contains("OK"), "{done}");
    assert!(find(&lines, "* 1 FETCH").contains("FLAGS (\\Draft \\Flagged $forwarded)"));
    let (lines, _) = client.command("SEARCH FLAGGED").await;
    assert_eq!(find(&lines, "* SEARCH"), "* SEARCH 1");
    let (lines, _) = client.command("UID SEARCH RETURN (COUNT ALL) SUBJECT entwurf").await;
    assert!(find(&lines, "ESEARCH").ends_with(") UID COUNT 2 ALL 1:2"));
    let (lines, _) = client.command("SEARCH TEXT fertig").await;
    assert_eq!(find(&lines, "* SEARCH"), "* SEARCH 1");

    let (_, done) = client.command("CREATE Projekte/UwUMail").await;
    assert!(done.contains("OK"), "{done}");
    let (lines, _) = client.command("LIST \"\" \"Projekte*\"").await;
    assert!(find(&lines, "\"Projekte\"").contains("\\HasChildren"));
    find(&lines, "\"Projekte/UwUMail\"");

    let (lines, done) = client.command("UID MOVE 1 Projekte/UwUMail").await;
    assert!(find(&lines, "[COPYUID ").ends_with("1 1] Moved"), "{lines:?}");
    find(&lines, "* 1 EXPUNGE");
    assert!(done.contains("OK"), "{done}");
    let (lines, _) = client.command("STATUS Projekte/UwUMail (MESSAGES UIDNEXT UNSEEN SIZE)").await;
    assert!(find(&lines, "* STATUS").contains("(MESSAGES 1 UIDNEXT 2 UNSEEN 1 SIZE "), "{lines:?}");

    let (_, done) = client.command("STORE 1 +FLAGS.SILENT (\\Deleted)").await;
    assert!(done.contains("OK"), "{done}");
    let (lines, done) = client.command("UID EXPUNGE 1:*").await;
    find(&lines, "* 1 EXPUNGE");
    assert!(done.contains("OK"), "{done}");
    let (lines, _) = client.command("STATUS Drafts (MESSAGES)").await;
    assert!(find(&lines, "* STATUS").contains("(MESSAGES 0)"));

    let (_, done) = client.command("RENAME Projekte/UwUMail Archiv/UwUMail").await;
    assert!(done.contains("OK"), "{done}");
    let (_, done) = client.command("DELETE Archiv/UwUMail").await;
    assert!(done.contains("OK"), "{done}");
    let (_, done) = client.command("SELECT Archiv/UwUMail").await;
    assert!(done.contains("NO [NONEXISTENT]"), "{done}");
    let (lines, _) = client.command("GETQUOTAROOT INBOX").await;
    find(&lines, "* QUOTAROOT \"INBOX\" \"\"");
    assert!(find(&lines, "* QUOTA ").contains("(STORAGE "));
}

#[tokio::test]
async fn changes_arrive_while_idling_and_through_qresync() {
    let server = server().await;
    let mut client = Client::login(&server).await;
    let (lines, _) = client.command("ENABLE QRESYNC").await;
    assert_eq!(find(&lines, "ENABLED"), "* ENABLED QRESYNC");
    let first = deliver(&server.store, server.account, "eins").await;
    let second = deliver(&server.store, server.account, "zwei").await;
    let (lines, _) = client.command("SELECT INBOX").await;
    let validity = number_after(find(&lines, "UIDVALIDITY"), "UIDVALIDITY ");
    let modseq = number_after(find(&lines, "HIGHESTMODSEQ"), "HIGHESTMODSEQ ");

    // Another app flags one message and deletes the other while this one is idle.
    client.send(b"i1 IDLE\r\n").await;
    assert!(client.line().await.starts_with("+ "));
    let mut other = Client::login(&server).await;
    other.command("SELECT INBOX").await;
    other.command(&format!("UID STORE {first} +FLAGS (\\Flagged)")).await;
    let line = client.line().await;
    assert!(line.starts_with("* 1 FETCH (FLAGS (\\Flagged) UID 1 MODSEQ ("), "{line}");
    other.command(&format!("UID STORE {second} +FLAGS (\\Deleted)")).await;
    other.command("EXPUNGE").await;
    let mut seen = Vec::new();
    while !seen.iter().any(|line: &String| line.starts_with("* VANISHED")) {
        seen.push(client.line().await);
    }
    assert!(seen.contains(&format!("* VANISHED {second}")), "{seen:?}");
    let third = deliver(&server.store, server.account, "drei").await;
    let mut seen = Vec::new();
    while !seen.iter().any(|line: &String| line.contains("EXISTS")) {
        seen.push(client.line().await);
    }
    assert!(seen.contains(&"* 2 EXISTS".to_owned()), "{seen:?}");
    client.send(b"DONE\r\n").await;
    let (_, done) = client.until_tagged("i1").await;
    assert!(done.contains("OK"), "{done}");

    // Coming back later with the old state.
    let mut later = Client::login(&server).await;
    later.command("ENABLE QRESYNC").await;
    let (lines, _) = later.command(&format!("SELECT INBOX (QRESYNC ({validity} {modseq} 1:{second}))")).await;
    find(&lines, &format!("* VANISHED (EARLIER) {second}"));
    assert!(find(&lines, &format!("UID {first} ")).contains("FLAGS (\\Flagged)"));
    let (lines, _) = later.command(&format!("UID FETCH 1:* (FLAGS) (CHANGEDSINCE {modseq} VANISHED)")).await;
    find(&lines, &format!("* VANISHED (EARLIER) {second}"));
    find(&lines, &format!("UID {third} "));
}

#[tokio::test]
async fn mailbox_names_are_modified_utf7_until_the_client_takes_utf8() {
    let server = server().await;
    let mut client = Client::login(&server).await;
    let (_, done) = client.command("CREATE \"Entw&APw-rfe/Gr&APwA3w-e\"").await;
    assert!(done.contains("OK"), "{done}");
    let (lines, _) = client.command("LIST \"\" \"Entw*\"").await;
    find(&lines, "\"Entw&APw-rfe/Gr&APwA3w-e\"");
    client.command("ENABLE UTF8=ACCEPT").await;
    let (lines, _) = client.command("LIST \"\" \"Entw*\"").await;
    find(&lines, "\"Entwürfe/Grüße\"");
    let (_, done) = client.command("SELECT \"Entwürfe/Grüße\"").await;
    assert!(done.contains("OK"), "{done}");
}

/// security-audit-0.8.0 A-1: a LIST pattern built to make a backtracking matcher run for ever, against
/// a long name of the account's own, is answered in time.
#[tokio::test]
async fn a_hostile_list_pattern_is_answered_in_time() {
    let server = server().await;
    let mut client = Client::login(&server).await;
    let (_, done) = client.command(&format!("CREATE \"{}\"", "a".repeat(255))).await;
    assert!(done.contains("OK"), "{done}");
    let (lines, done) = client.command(&format!("LIST \"\" \"{}b\"", "*a".repeat(127))).await;
    assert!(done.contains("OK"), "{done}");
    assert!(lines.is_empty(), "{lines:?}");
}

#[tokio::test]
async fn broken_and_oversized_input_is_refused() {
    let server = server().await;
    let mut client = Client::connect(&server).await;
    let (_, bad) = client.command("FROBNICATE").await;
    assert!(bad.contains("BAD"), "{bad}");
    client.send(b"t9 LOGIN {99999999}\r\n").await;
    let answer = client.line().await;
    assert!(answer.starts_with("t9 BAD"), "{answer}");
    let (_, still) = client.command("CAPABILITY").await;
    assert!(still.contains("OK"), "the session goes on: {still}");
}

fn base64_plain(login: &str, password: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(format!("\0{login}\0{password}"))
}

/// Before a login, a literal may not be bigger than a command.
///
/// `APPEND` is allowed a whole message, and the reader sets that much aside the moment the size is
/// announced — before the bytes arrive and before anyone has said who they are. A stranger could
/// send one short line per connection and make the server hold a message's worth of memory for as
/// long as the login takes to time out.
#[tokio::test]
async fn before_a_login_a_literal_may_not_be_bigger_than_a_command() {
    let server = server().await;
    let mut stranger = Client::connect(&server).await;
    // Under this server's append limit (1 MiB), over what a command may be.
    stranger.send(b"x1 APPEND INBOX {900000}\r\n").await;
    let answer = stranger.line().await;
    assert!(answer.starts_with("x1 NO") || answer.starts_with("x1 BAD"), "a stranger was allowed it: {answer}");
    assert!(!answer.starts_with("+ "), "the server offered to take the data: {answer}");

    // Logged in, the same size is welcome: this is a real APPEND of a real message.
    let mut member = Client::login(&server).await;
    member.send(b"x2 APPEND INBOX {900000}\r\n").await;
    let ready = member.line().await;
    assert!(ready.starts_with("+ "), "a member was refused their own message: {ready}");
}
