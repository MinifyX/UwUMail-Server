//! The addresses a person wrote to and heard from lately, for address suggestions while writing
//! (JMAP `AddressSuggestion/query`, docs/jmap-suggest.md).

use rusqlite::params;

use crate::address::EmailAddress;
use crate::{Result, Store};

/// One address as it appeared in a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressUse {
    pub address: EmailAddress,
    /// The person sent this message (to the address); otherwise they received it (from it).
    pub sent: bool,
    /// When the message arrived or was sent.
    pub at: i64,
}

impl Store {
    /// Addresses of the latest `max_messages` messages that mention `text` in an address or name:
    /// recipients of mail in the Sent mailbox, senders of all other mail. Junk and Trash are left
    /// out. An empty `text` takes the latest messages.
    pub async fn address_history(&self, account_id: i64, text: &str, max_messages: usize) -> Result<Vec<AddressUse>> {
        let needle = text.trim().to_lowercase();
        self.read(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT e.received_at, e.from_addr, e.to_addr, e.cc_addr, e.bcc_addr,
                        EXISTS (SELECT 1 FROM email_mailboxes em JOIN mailboxes m ON m.id = em.mailbox_id
                                WHERE em.email_id = e.id AND m.role = 'sent')
                 FROM emails e
                 WHERE e.account_id = ?1
                   AND NOT EXISTS (SELECT 1 FROM email_mailboxes em JOIN mailboxes m ON m.id = em.mailbox_id
                                   WHERE em.email_id = e.id AND m.role IN ('junk', 'trash'))
                   AND (?2 = '' OR instr(lower(e.from_addr || e.to_addr || e.cc_addr || e.bcc_addr), ?2) > 0)
                 ORDER BY e.received_at DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![account_id, needle, max_messages as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            })?;
            let parse = |json: &str| serde_json::from_str::<Vec<EmailAddress>>(json).unwrap_or_default();
            let mut uses = Vec::new();
            for row in rows {
                let (at, from, to, cc, bcc, sent) = row?;
                let addresses = if sent {
                    [parse(&to), parse(&cc), parse(&bcc)].concat()
                } else {
                    parse(&from)
                };
                uses.extend(addresses.into_iter().map(|address| AddressUse { address, sent, at }));
            }
            Ok(uses)
        })
        .await
    }
}
