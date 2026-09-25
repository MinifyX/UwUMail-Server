//! End-to-end IMAP4rev2 (RFC 9051) and shared folders (RFC 4314 ACL, RFC 2342 NAMESPACE).

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use uwumail_imap::Imap;
use uwumail_store::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role, Store};

const PASSWORD: &str = "katzenpfote-123";
const SHARED_INBOX: &str = "\"Shared/mini@example.org/INBOX\"";

struct Server {
    imap: Imap,
    store: Store,
    mini: i64,
    leni: i64,
    _dir: tempfile::TempDir,
}

async fn account(store: &Store, address: &str) -> i64 {
    store
        .create_account(NewAccount {
            address: address.into(),
            display_name: address.split('@').next().unwrap_or_default().into(),
            password: Some(PASSWORD.into()),
            role: Role::User,
            quota_bytes: 10 * 1024 * 1024,
            protocols: None,
        })
        .await
        .unwrap()
        .id
}

async fn server() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.org").await.unwrap();
    let mini = account(&store, "mini@example.org").await;
    let leni = account(&store, "leni@example.org").await;
    Server { imap: Imap::new(store.clone(), 1024 * 1024), store, mini, leni, _dir: dir }
}

async fn deliver(store: &Store, account: i64, subject: &str) -> u32 {
    let raw = format!(
        "From: Nyu <nyu@example.net>\r\nTo: Mini <mini@example.org>\r\nSubject: {subject}\r\n\
Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain; charset=utf-8\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\r\nGr=C3=BC=C3=9Fe, {subject}\r\n\
--b\r\nContent-Type: image/png; name=nyu.png\r\nContent-Transfer-Encoding: base64\r\n\r\niVBORw0KGgo=\r\n--b--\r\n"
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

async fn inbox(store: &Store, account: i64) -> i64 {
    store.imap_mailboxes(account).await.unwrap().into_iter().find(|m| m.role == Some(MailboxRole::Inbox)).unwrap().id
}

struct Client {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
    next_tag: u32,
}

impl Client {
    async fn login(server: &Server, login: &str) -> Client {
        let (client, connection) = tokio::io::duplex(4 * 1024 * 1024);
        let imap = server.imap.clone();
        tokio::spawn(async move { imap.serve_connection(connection, "192.0.2.7:40000".parse().unwrap()).await });
        let (reader, writer) = tokio::io::split(client);
        let mut client = Client { reader: BufReader::new(reader), writer, next_tag: 1 };
        let greeting = client.line().await;
        assert!(greeting.contains("IMAP4rev2"), "{greeting}");
        let (_, done) = client.command(&format!("LOGIN {login} \"{PASSWORD}\"")).await;
        assert!(done.contains("OK [CAPABILITY"), "{done}");
        client
    }

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

    /// Sends a command and expects its tagged answer to contain `expected`.
    async fn expect(&mut self, command: &str, expected: &str) -> Vec<String> {
        let (lines, done) = self.command(command).await;
        assert!(done.contains(expected), "{command}: {done} ({lines:#?})");
        lines
    }
}

fn find<'a>(lines: &'a [String], needle: &str) -> &'a str {
    lines.iter().find(|line| line.contains(needle)).unwrap_or_else(|| panic!("no line with {needle:?} in {lines:#?}"))
}

fn lacks(lines: &[String], needle: &str) {
    assert!(!lines.iter().any(|line| line.contains(needle)), "{needle:?} in {lines:#?}");
}

#[tokio::test]
async fn imap4rev2_answers_the_rev2_way() {
    let server = server().await;
    deliver(&server.store, server.mini, "eins").await;
    deliver(&server.store, server.mini, "zwei").await;
    let mut client = Client::login(&server, "mini@example.org").await;

    let lines = client.expect("CAPABILITY", "OK").await;
    let capabilities = find(&lines, "* CAPABILITY");
    for wanted in ["IMAP4rev1", "IMAP4rev2", "ACL", "RIGHTS=kxte", "BINARY", "UNAUTHENTICATE", "SEARCHRES", "NAMESPACE"]
    {
        assert!(capabilities.split(' ').any(|word| word == wanted), "{wanted} in {capabilities}");
    }
    let lines = client.expect("ENABLE IMAP4rev2", "OK").await;
    find(&lines, "* ENABLED IMAP4rev2");

    let lines = client.expect("NAMESPACE", "OK").await;
    assert_eq!(find(&lines, "* NAMESPACE"), "* NAMESPACE ((\"\" \"/\")) ((\"Shared/\" \"/\")) NIL");

    let lines = client.expect("SELECT INBOX", "OK [READ-WRITE]").await;
    find(&lines, "* 2 EXISTS");
    find(&lines, "* LIST () \"/\" \"INBOX\"");
    lacks(&lines, "RECENT");
    lacks(&lines, "[UNSEEN");

    // Plain SEARCH answers with ESEARCH.
    let (lines, done) = client.command("SEARCH ALL").await;
    assert!(done.contains("OK"), "{done}");
    let tag = done.split(' ').next().unwrap();
    assert_eq!(find(&lines, "ESEARCH"), format!("* ESEARCH (TAG \"{tag}\") ALL 1:2"));

    // SEARCHRES: the saved result stands in for `$`.
    let lines = client.expect("UID SEARCH RETURN (SAVE) SUBJECT zwei", "OK").await;
    lacks(&lines, "ESEARCH");
    let lines = client.expect("UID FETCH $ (UID)", "OK").await;
    assert_eq!(lines, vec!["* 2 FETCH (UID 2)".to_owned()]);
    let lines = client.expect("FETCH $ (FLAGS)", "OK").await;
    find(&lines, "* 2 FETCH (FLAGS ())");

    // BINARY undoes the transfer encodings: the picture's eight bytes, the text in UTF-8.
    let lines = client.expect("FETCH 1 (BINARY.PEEK[2] BINARY.SIZE[2] BINARY.PEEK[1])", "OK").await;
    let fetched = find(&lines, "* 1 FETCH");
    assert!(fetched.contains("BINARY[2] {8}\r\n\u{fffd}PNG\r\n\u{1a}\n"), "{fetched}");
    assert!(fetched.contains("BINARY.SIZE[2] 8"), "{fetched}");
    assert!(fetched.contains("BINARY[1] {13}\r\nGrüße, eins)"), "{fetched}");

    // $Forwarded as RFC 9051 writes it.
    let lines = client.expect("STORE 1 +FLAGS ($Forwarded)", "OK").await;
    find(&lines, "$Forwarded");

    // APPEND takes a literal8.
    client.send(b"a1 APPEND INBOX ~{11+}\r\nSubject: x\n\r\n").await;
    let (_, done) = client.until_tagged("a1").await;
    assert!(done.contains("OK [APPENDUID"), "{done}");

    let lines = client.expect("STATUS INBOX (MESSAGES SIZE DELETED)", "OK").await;
    assert!(find(&lines, "* STATUS").contains("MESSAGES 3"), "{lines:?}");

    // UNAUTHENTICATE: logged out, but the connection stays.
    client.expect("UNAUTHENTICATE", "OK").await;
    let (_, done) = client.command("SELECT INBOX").await;
    assert!(done.contains("BAD"), "{done}");
    client.expect(&format!("LOGIN leni@example.org \"{PASSWORD}\""), "OK").await;
    let lines = client.expect("SELECT INBOX", "OK").await;
    find(&lines, "* 0 EXISTS");
    find(&lines, "* 0 RECENT");
}

#[tokio::test]
async fn folders_are_shared_by_acl_and_used_by_their_rights() {
    let server = server().await;
    let mini_inbox = inbox(&server.store, server.mini).await;
    deliver(&server.store, server.mini, "eins").await;
    let mut mini = Client::login(&server, "mini@example.org").await;
    let mut leni = Client::login(&server, "leni@example.org").await;

    // Nothing shared yet.
    let lines = leni.expect("LIST \"\" \"*\"", "OK").await;
    lacks(&lines, "Shared");
    leni.expect(&format!("SELECT {SHARED_INBOX}"), "NO [NONEXISTENT]").await;

    // Mini shares the inbox for reading and keeping it read.
    mini.expect("SETACL INBOX leni@example.org lrs", "OK").await;
    mini.expect("SETACL INBOX anyone lr", "NO [CANNOT]").await;
    mini.expect("SETACL INBOX nobody@example.org lr", "NO [CANNOT]").await;
    mini.expect("SETACL INBOX mini@example.org lr", "NO [CANNOT]").await;
    let lines = mini.expect("GETACL INBOX", "OK").await;
    assert_eq!(find(&lines, "* ACL"), "* ACL \"INBOX\" \"mini@example.org\" lrswipkxtea \"leni@example.org\" \"lrs\"");
    let lines = mini.expect("LISTRIGHTS INBOX leni@example.org", "OK").await;
    assert_eq!(find(&lines, "* LISTRIGHTS"), "* LISTRIGHTS \"INBOX\" \"leni@example.org\" \"\" l r s w i p k x t e a");
    let lines = mini.expect("MYRIGHTS INBOX", "OK").await;
    find(&lines, "* MYRIGHTS \"INBOX\" \"lrswipkxtea\"");

    let lines = leni.expect("LIST \"\" \"*\"", "OK").await;
    assert_eq!(find(&lines, "\"Shared\""), "* LIST (\\Noselect \\HasChildren) \"/\" \"Shared\"");
    find(&lines, "(\\Noselect \\HasChildren) \"/\" \"Shared/mini@example.org\"");
    assert_eq!(find(&lines, SHARED_INBOX), format!("* LIST (\\HasNoChildren) \"/\" {SHARED_INBOX}"));
    let lines = leni.expect(&format!("MYRIGHTS {SHARED_INBOX}"), "OK").await;
    find(&lines, "\"lrs\"");
    leni.expect(&format!("GETACL {SHARED_INBOX}"), "NO [NOPERM]").await;
    leni.expect(&format!("SETACL {SHARED_INBOX} leni@example.org lrswi"), "NO [NOPERM]").await;

    let lines = leni.expect(&format!("SELECT {SHARED_INBOX}"), "OK [READ-WRITE]").await;
    find(&lines, "* 1 EXISTS");
    find(&lines, "[PERMANENTFLAGS (\\Seen)]");
    // Reading marks it seen, for everyone: the flags are the email's.
    let lines = leni.expect("FETCH 1 (BODY[TEXT])", "OK").await;
    find(&lines, "\\Seen");
    assert_eq!(server.store.imap_status(server.mini, mini_inbox).await.unwrap().unseen, 0);
    leni.expect("STORE 1 +FLAGS (\\Flagged)", "NO [NOPERM]").await;
    leni.expect("STORE 1 FLAGS (\\Seen)", "NO [NOPERM]").await;
    leni.expect("STORE 1 -FLAGS (\\Seen \\Flagged)", "OK").await;
    leni.expect("EXPUNGE", "NO [NOPERM]").await;
    leni.expect("MOVE 1 INBOX", "NO [NOPERM]").await;
    leni.expect(&format!("APPEND {SHARED_INBOX} {{11+}}\r\nSubject: x\n"), "NO [NOPERM]").await;
    leni.expect(&format!("CREATE {}", "\"Shared/mini@example.org/INBOX/Neu\""), "NO [NOPERM]").await;
    leni.expect(&format!("DELETE {SHARED_INBOX}"), "NO [NOPERM]").await;

    // Copying out makes a new email in Leni's own mailbox, in her quota.
    let (_, done) = leni.command("COPY 1 INBOX").await;
    assert!(done.contains("OK [COPYUID"), "{done}");
    let leni_inbox = inbox(&server.store, server.leni).await;
    assert_eq!(server.store.imap_status(server.leni, leni_inbox).await.unwrap().messages, 1);

    // More rights: filing in, flagging and removing.
    mini.expect("SETACL INBOX leni@example.org +witek", "OK").await;
    let lines = mini.expect("GETACL INBOX", "OK").await;
    assert!(find(&lines, "* ACL").ends_with("\"leni@example.org\" \"lrswikte\""), "{lines:?}");
    let lines = leni.expect(&format!("SELECT {SHARED_INBOX}"), "OK [READ-WRITE]").await;
    find(&lines, "[PERMANENTFLAGS (\\Answered \\Flagged \\Draft \\Deleted \\Seen \\*)]");
    leni.expect("STORE 1 +FLAGS (\\Flagged)", "OK").await;
    leni.expect(&format!("APPEND {SHARED_INBOX} (\\Seen) {{11+}}\r\nSubject: x\n"), "OK [APPENDUID").await;
    assert_eq!(server.store.imap_status(server.mini, mini_inbox).await.unwrap().messages, 2, "the owner's mail");
    leni.expect("CREATE \"Shared/mini@example.org/INBOX/Neu\"", "OK").await;
    assert!(server.store.imap_mailboxes(server.mini).await.unwrap().iter().any(|m| m.name == "Neu"));
    // A new folder inside a shared one is shared the same way.
    let lines = leni.expect("LIST \"\" \"Shared/*\"", "OK").await;
    find(&lines, "\"Shared/mini@example.org/INBOX/Neu\"");
    find(&lines, "(\\HasChildren) \"/\" \"Shared/mini@example.org/INBOX\"");

    // IDLE in the shared folder hears of the owner's new mail.
    leni.expect(&format!("SELECT {SHARED_INBOX}"), "OK").await;
    leni.send(b"i1 IDLE\r\n").await;
    assert!(leni.line().await.starts_with('+'));
    deliver(&server.store, server.mini, "drei").await;
    assert_eq!(leni.line().await, "* 3 EXISTS");
    leni.send(b"DONE\r\n").await;
    let (_, done) = leni.until_tagged("i1").await;
    assert!(done.contains("OK"), "{done}");

    // Moving out takes the message from Mini and gives Leni a copy.
    let (_, done) = leni.command("MOVE 3 INBOX").await;
    assert!(done.contains("OK"), "{done}");
    assert_eq!(server.store.imap_status(server.mini, mini_inbox).await.unwrap().messages, 2);
    assert_eq!(server.store.imap_status(server.leni, leni_inbox).await.unwrap().messages, 2);

    // Taking the share back: gone from Leni's list, and no longer to be opened.
    mini.expect("DELETEACL INBOX leni@example.org", "OK").await;
    mini.expect("DELETEACL INBOX/Neu leni@example.org", "OK").await;
    leni.expect("UNSELECT", "OK").await;
    let lines = leni.expect("LIST \"\" \"*\"", "OK").await;
    lacks(&lines, "Shared");
    leni.expect(&format!("SELECT {SHARED_INBOX}"), "NO [NONEXISTENT]").await;
}

#[tokio::test]
async fn a_shared_subfolder_shows_under_its_owners_path() {
    let server = server().await;
    let projects = server.store.create_mailbox(server.mini, "Projekte", None, None, 0, true).await.unwrap();
    let uwu = server.store.create_mailbox(server.mini, "UwUMail", Some(projects), None, 0, true).await.unwrap();
    server.store.set_mailbox_acl(server.mini, uwu, "leni@example.org", "lr").await.unwrap();
    let mut leni = Client::login(&server, "leni@example.org").await;
    let lines = leni.expect("LIST \"\" \"Shared/*\"", "OK").await;
    find(&lines, "(\\Noselect \\HasChildren) \"/\" \"Shared/mini@example.org/Projekte\"");
    find(&lines, "(\\HasNoChildren) \"/\" \"Shared/mini@example.org/Projekte/UwUMail\"");
    // Read-only rights open read-only; the parent is only a level.
    leni.expect("SELECT \"Shared/mini@example.org/Projekte/UwUMail\"", "OK [READ-ONLY]").await;
    leni.expect("SELECT \"Shared/mini@example.org/Projekte\"", "NO [NONEXISTENT]").await;
    let lines = leni.expect("STATUS \"Shared/mini@example.org/Projekte/UwUMail\" (MESSAGES)", "OK").await;
    find(&lines, "MESSAGES 0");

    // Taking the share back while it is open ends the session's hold on it.
    leni.expect("SELECT \"Shared/mini@example.org/Projekte/UwUMail\"", "OK").await;
    server.store.set_mailbox_acl(server.mini, uwu, "leni@example.org", "").await.unwrap();
    leni.send(b"n NOOP\r\n").await;
    assert_eq!(leni.line().await, "* BYE The selected mailbox is no longer shared with you");
}
