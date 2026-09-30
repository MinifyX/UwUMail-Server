//! Refusals by Microsoft's mail servers become issues for the admins, and the sender's bounce
//! says what happened (docs/microsoft.md).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lettre::AsyncTransport;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::flow::{PASSWORD, mail, start};

const BLOCKED: &str = "550 5.7.1 Unfortunately, messages from [192.0.2.9] weren't sent. Please contact your \
                       Internet service provider since part of their network is on our block list (S3150). \
                       [AM0PR01MB1234.eurprd01.prod.exchangelabs.com]";

/// A mail server that greets like Exchange Online (but is not one of its hosts) and refuses every recipient while `refuse` is set.
async fn fake_outlook(refuse: Arc<AtomicBool>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let refuse = refuse.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let greeting = "220 AM4PEPF00027A62.mail.protection.outlook.com Microsoft ESMTP MAIL Service ready\r\n";
                write.write_all(greeting.as_bytes()).await.unwrap();
                let mut in_data = false;
                while let Ok(Some(line)) = lines.next_line().await {
                    if in_data {
                        if line == "." {
                            in_data = false;
                            write.write_all(b"250 2.6.0 Queued mail for delivery\r\n").await.unwrap();
                        }
                        continue;
                    }
                    let command = line.to_ascii_uppercase();
                    let reply = if command.starts_with("EHLO") {
                        "250-AM4PEPF00027A62.mail.protection.outlook.com\r\n250 SIZE 49283072\r\n".to_owned()
                    } else if command.starts_with("RCPT") && refuse.load(Ordering::SeqCst) {
                        format!("{BLOCKED}\r\n")
                    } else if command.starts_with("DATA") {
                        in_data = true;
                        "354 Start mail input\r\n".to_owned()
                    } else if command.starts_with("QUIT") {
                        write.write_all(b"221 2.0.0 Bye\r\n").await.unwrap();
                        break;
                    } else {
                        "250 2.1.0 OK\r\n".to_owned()
                    };
                    write.write_all(reply.as_bytes()).await.unwrap();
                }
            });
        }
    });
    address
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_only_greets_like_microsoft_raises_no_issue_but_the_bounce_explains_it() {
    // The route goes to 127.0.0.1, not to a Microsoft host: the greeting and the S3150 are only
    // what the other side says, and any mail server could say them to raise a false alarm.
    let refuse = Arc::new(AtomicBool::new(true));
    let outlook = fake_outlook(refuse.clone()).await;
    let a = start("a.test", &["mini"], &[("outlook.test", outlook)]).await;
    let store = a.smtp.store();

    a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["ami@outlook.test"], "Hallo")).await.unwrap();
    let inbox = a.wait_for_inbox("mini@a.test", 1).await;
    let raw = a.raw(&inbox[0]).await;
    assert!(raw.contains("S3150"), "the reply is quoted: {raw}");
    assert!(raw.contains("Microsoft (Outlook, Hotmail, Microsoft 365) blockiert gerade Mail"), "{raw}");
    // The bounce is written after the refusal would have been recorded.
    assert!(store.open_microsoft_issues().await.unwrap().is_empty());
}
