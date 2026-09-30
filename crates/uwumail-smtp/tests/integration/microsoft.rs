//! Refusals by Microsoft's mail servers become issues for the admins, and the sender's bounce
//! says what happened (docs/microsoft.md).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lettre::AsyncTransport;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::flow::{PASSWORD, mail, start};

const BLOCKED: &str = "550 5.7.1 Unfortunately, messages from [203.0.113.5] weren't sent. Please contact your \
                       Internet service provider since part of their network is on our block list (S3150). \
                       [AM0PR01MB1234.eurprd01.prod.exchangelabs.com]";

/// A mail server that greets like Exchange Online and refuses every recipient while `refuse` is set.
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
async fn a_microsoft_block_becomes_an_issue_and_the_bounce_explains_it() {
    let refuse = Arc::new(AtomicBool::new(true));
    let outlook = fake_outlook(refuse.clone()).await;
    let a = start("a.test", &["mini"], &[("outlook.test", outlook)]).await;
    let store = a.smtp.store();

    a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["ami@outlook.test"], "Hallo")).await.unwrap();
    let inbox = a.wait_for_inbox("mini@a.test", 1).await;
    let raw = a.raw(&inbox[0]).await;
    assert!(raw.contains("S3150"), "the reply is quoted: {raw}");
    assert!(raw.contains("Microsoft (Outlook, Hotmail, Microsoft 365) blockiert gerade Mail"), "{raw}");

    let issues = store.open_microsoft_issues().await.unwrap();
    assert_eq!(issues.len(), 1);
    let issue = &issues[0];
    assert_eq!((issue.scope.as_str(), issue.subject.as_str()), ("ip", "203.0.113.5"));
    assert_eq!((issue.group.as_str(), issue.code.as_str(), issue.domain.as_str()), ("blockList", "S3150", "a.test"));

    // Microsoft takes mail again. Behind NAT the server does not know the address Microsoft saw,
    // so any mail that went through counts; a day after the last refusal the issue is over.
    refuse.store(false, Ordering::SeqCst);
    a.mailer("mini@a.test", PASSWORD, false).send(mail("mini@a.test", &["ami@outlook.test"], "Nochmal")).await.unwrap();
    let started = std::time::Instant::now();
    let later = issue.last_seen + uwumail_store::MICROSOFT_RESOLVE_AFTER_SECS;
    while store.resolve_microsoft_issues(later).await.unwrap() == 0 {
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "the delivery was never recorded");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(store.open_microsoft_issues().await.unwrap().is_empty());
}
