//! A ManageSieve session (RFC 5804) against the listener, the way a mail app or `sieve-connect`
//! talks to it: STARTTLS first, then AUTHENTICATE PLAIN, then the script commands.

use std::sync::Arc;

use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use uwumail_imap::{Imap, ManageSieve};
use uwumail_store::{NewAccount, Role, Store};

const PASSWORD: &str = "katzenpfote-123";

struct Client<S> {
    stream: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    /// Reads up to and including the OK, NO or BYE line; literals are read whole.
    async fn response(&mut self) -> String {
        let mut response = String::new();
        loop {
            let mut line = String::new();
            self.stream.read_line(&mut line).await.unwrap();
            assert!(!line.is_empty(), "the server hung up after {response:?}");
            response.push_str(&line);
            if let Some(size) = line.trim_end().strip_suffix('}').and_then(|l| l.rsplit_once('{')).map(|(_, n)| n) {
                let mut literal = vec![0; size.parse().unwrap()];
                self.stream.read_exact(&mut literal).await.unwrap();
                response.push_str(&String::from_utf8(literal).unwrap());
                continue;
            }
            let upper = line.to_ascii_uppercase();
            if upper.starts_with("OK") || upper.starts_with("NO") || upper.starts_with("BYE") {
                return response;
            }
        }
    }

    async fn command(&mut self, command: &str) -> String {
        self.stream.get_mut().write_all(format!("{command}\r\n").as_bytes()).await.unwrap();
        self.response().await
    }

    /// A command whose last argument is a script, sent as a non-synchronizing literal.
    async fn with_script(&mut self, command: &str, script: &str) -> String {
        self.command(&format!("{command} {{{}+}}\r\n{script}", script.len())).await
    }
}

async fn setup() -> (Store, std::net::SocketAddr, Vec<u8>, watch::Sender<bool>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).await.unwrap();
    store.create_domain("example.com").await.unwrap();
    for user in ["mini", "nyu"] {
        store
            .create_account(NewAccount {
                address: format!("{user}@example.com"),
                display_name: String::new(),
                password: Some(PASSWORD.into()),
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
    }
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let key = rustls_pki_types::PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into());
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.cert.der().clone()], key)
        .unwrap();
    let imap = Imap::new(store.clone(), 1024 * 1024);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (shutdown, rx) = watch::channel(false);
    tokio::spawn(ManageSieve::new(&imap).serve(listener, Arc::new(tls), rx));
    (store, address, certificate.cert.der().to_vec(), shutdown, dir)
}

/// Connects, reads the greeting and switches to TLS.
async fn secure(
    address: std::net::SocketAddr,
    certificate: &[u8],
) -> (Client<tokio_rustls::client::TlsStream<TcpStream>>, String, String) {
    let mut plain = Client { stream: BufReader::new(TcpStream::connect(address).await.unwrap()) };
    let greeting = plain.response().await;
    let refused = plain.command(&format!("AUTHENTICATE \"PLAIN\" \"{}\"", plain_login("mini@example.com"))).await;
    assert!(refused.starts_with("NO (ENCRYPT-NEEDED)"), "no password before TLS: {refused}");
    assert!(plain.command("STARTTLS").await.starts_with("OK"));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls_pki_types::CertificateDer::from(certificate.to_vec())).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let tls = connector.connect(name, plain.stream.into_inner()).await.unwrap();
    let mut client = Client { stream: BufReader::new(tls) };
    let capabilities = client.response().await;
    (client, greeting, capabilities)
}

fn plain_login(login: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(format!("\0{login}\0{PASSWORD}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_managesieve_session_manages_the_scripts_delivery_runs() {
    let (store, address, certificate, _shutdown, _dir) = setup().await;
    let (mut client, greeting, capabilities) = secure(address, &certificate).await;
    assert!(greeting.contains("\"STARTTLS\""), "{greeting}");
    assert!(greeting.contains("\"SASL\" \"\""), "no mechanisms before TLS: {greeting}");
    assert!(greeting.contains("\"VERSION\" \"1.0\""), "{greeting}");
    assert!(greeting.contains("\"SIEVE\" \"body "), "{greeting}");
    assert!(capabilities.contains("\"SASL\" \"PLAIN\""), "{capabilities}");
    assert!(!capabilities.contains("STARTTLS"), "{capabilities}");

    assert!(client.command("LISTSCRIPTS").await.starts_with("NO"), "not before logging in");
    let login = client.command(&format!("AUTHENTICATE \"PLAIN\" \"{}\"", plain_login("mini@example.com"))).await;
    assert!(login.starts_with("OK"), "{login}");
    assert!(client.command("CAPABILITY").await.contains("\"OWNER\" \"mini@example.com\""));
    assert!(client.command("AUTHENTICATE \"PLAIN\" \"eA==\"").await.starts_with("NO"), "no second login");

    assert!(client.command("HAVESPACE \"rules\" 100").await.starts_with("OK"));
    assert!(client.command("HAVESPACE \"rules\" 999999").await.starts_with("NO (QUOTA/MAXSIZE)"));

    let broken = client.with_script("PUTSCRIPT \"broken\"", "#comment\r\nInvalidSieveCommand\r\n").await;
    assert!(broken.starts_with("NO") && broken.contains("line 2"), "{broken}");
    let checked = client.with_script("CHECKSCRIPT", "require \"vacation\";\r\n").await;
    assert!(checked.starts_with("NO") && checked.contains("vacation"), "{checked}");
    let script = "require [\"fileinto\"];\r\nif header :contains \"subject\" \"x\" { fileinto \"Archive\"; }\r\n";
    assert!(client.with_script("CHECKSCRIPT", script).await.starts_with("OK"));
    assert!(client.with_script("PUTSCRIPT \"rules\"", script).await.starts_with("OK"));
    assert_eq!(client.command("LISTSCRIPTS").await, "\"rules\"\r\nOK \"Listscripts completed\"\r\n");

    assert!(client.command("SETACTIVE \"nope\"").await.starts_with("NO (NONEXISTENT)"));
    assert!(client.command("SETACTIVE \"rules\"").await.starts_with("OK"));
    assert!(client.command("LISTSCRIPTS").await.starts_with("\"rules\" ACTIVE\r\n"));
    let mini = store.account("mini@example.com").await.unwrap().unwrap().id;
    assert_eq!(store.active_sieve_script(mini).await.unwrap().unwrap().1, script, "delivery runs this one");

    let fetched = client.command("GETSCRIPT \"rules\"").await;
    assert_eq!(fetched, format!("{{{}}}\r\n{script}\r\nOK \"Getscript completed\"\r\n", script.len()));

    assert!(client.command("DELETESCRIPT \"rules\"").await.starts_with("NO (ACTIVE)"));
    assert!(client.command("RENAMESCRIPT \"rules\" \"Regeln\"").await.starts_with("OK"));
    assert!(client.command("LISTSCRIPTS").await.starts_with("\"Regeln\" ACTIVE"), "a renamed script stays active");
    assert!(client.command("SETACTIVE \"\"").await.starts_with("OK"));
    assert!(store.active_sieve_script(mini).await.unwrap().is_none());
    assert!(client.command("DELETESCRIPT \"rules\"").await.starts_with("NO (NONEXISTENT)"));
    assert!(client.command("DELETESCRIPT \"Regeln\"").await.starts_with("OK"));
    assert_eq!(client.command("NOOP \"sync-42\"").await, "OK (TAG \"sync-42\") \"Done\"\r\n");

    assert!(client.command("UNAUTHENTICATE").await.starts_with("OK"));
    assert!(client.command("LISTSCRIPTS").await.starts_with("NO"));
    assert!(client.command("LOGOUT").await.starts_with("OK"));
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_passwords_are_counted_and_scripts_are_per_account() {
    let (store, address, certificate, _shutdown, _dir) = setup().await;
    let nyu = store.account("nyu@example.com").await.unwrap().unwrap().id;
    store.put_sieve_script(nyu, "nyus", b"keep;").await.unwrap();

    let (mut client, _, _) = secure(address, &certificate).await;
    let wrong = base64::engine::general_purpose::STANDARD.encode("\0mini@example.com\0nope");
    // Without an initial response the server asks with an empty challenge.
    client.stream.get_mut().write_all(b"AUTHENTICATE \"PLAIN\"\r\n").await.unwrap();
    let mut challenge = String::new();
    client.stream.read_line(&mut challenge).await.unwrap();
    assert_eq!(challenge, "\"\"\r\n");
    assert!(client.command(&format!("\"{wrong}\"")).await.starts_with("NO"));
    assert!(client.command(&format!("AUTHENTICATE \"PLAIN\" \"{wrong}\"")).await.starts_with("NO"));
    let third = client.command(&format!("AUTHENTICATE \"PLAIN\" \"{wrong}\"")).await;
    assert!(third.starts_with("BYE"), "{third}");

    let (mut client, _, _) = secure(address, &certificate).await;
    assert!(
        client
            .command(&format!("AUTHENTICATE \"PLAIN\" \"{}\"", plain_login("mini@example.com")))
            .await
            .starts_with("OK")
    );
    assert_eq!(client.command("LISTSCRIPTS").await, "OK \"Listscripts completed\"\r\n", "Nyu's script is hers");
    assert!(client.command("GETSCRIPT \"nyus\"").await.starts_with("NO (NONEXISTENT)"));
    assert!(client.command("SETACTIVE \"nyus\"").await.starts_with("NO (NONEXISTENT)"));
    assert!(store.active_sieve_script(nyu).await.unwrap().is_none());
}
