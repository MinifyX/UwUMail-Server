//! Which cards with a photo name which address (migration 0055), so a sender's picture comes from
//! the reader's own address books without reading every vCard they have for every message in a list.
//! Kept in the same transaction as every write of a card, over CardDAV and JMAP alike; a card that
//! goes takes its rows with it (foreign key).

use calcard::common::Data;
use calcard::vcard::{VCard, VCardProperty, VCardValue};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::dav::{DavCollection, DavKind};
use crate::{Result, Store};

/// Addresses of one card that are looked at; real cards have a handful.
const MAX_EMAILS_PER_CARD: usize = 64;
/// The longest `https:` photo address that is kept, as for remote pictures.
const MAX_PHOTO_URL: usize = 4096;
const BACKFILL_MARKER: &str = "contact_photos.backfill";

/// The photo of a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContactPhoto {
    /// Carried in the card (`data:` or vCard 3 base64): the bytes, not yet known to be a picture.
    Inline(Vec<u8>),
    /// An `https:` address the photo is at.
    Remote(String),
}

/// Whether `haystack` names `needle`, which is uppercase ASCII, in any case.
fn mentions(haystack: &str, needle: &str) -> bool {
    haystack.as_bytes().windows(needle.len()).any(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn photo_of(value: &VCardValue) -> Option<ContactPhoto> {
    match value {
        VCardValue::Binary(data) if !data.data.is_empty() => Some(ContactPhoto::Inline(data.data.clone())),
        VCardValue::Text(text) => {
            let text = text.trim();
            if text.len() > 5 && text[..5].eq_ignore_ascii_case("data:") {
                return Data::try_parse(text.as_bytes())
                    .filter(|data| !data.data.is_empty())
                    .map(|data| ContactPhoto::Inline(data.data));
            }
            (text.len() <= MAX_PHOTO_URL && text.len() > 8 && text[..8].eq_ignore_ascii_case("https://"))
                .then(|| ContactPhoto::Remote(text.to_owned()))
        }
        _ => None,
    }
}

/// The photo of a vCard: the first `PHOTO` that is carried in the card or at an `https:` address.
pub fn contact_photo(content: &str) -> Option<ContactPhoto> {
    if !mentions(content, "PHOTO") {
        return None;
    }
    let card = VCard::parse(content).ok()?;
    card.entries
        .iter()
        .filter(|entry| entry.name == VCardProperty::Photo)
        .find_map(|entry| entry.values.iter().find_map(photo_of))
}

/// The addresses of a card that has a photo, lowercase; empty for a card without one.
fn photo_emails(content: &str) -> Vec<String> {
    if !mentions(content, "PHOTO") {
        return Vec::new();
    }
    let Ok(card) = VCard::parse(content) else { return Vec::new() };
    let has_photo = card
        .entries
        .iter()
        .filter(|entry| entry.name == VCardProperty::Photo)
        .any(|entry| entry.values.iter().any(|value| photo_of(value).is_some()));
    if !has_photo {
        return Vec::new();
    }
    let mut emails: Vec<String> = card
        .entries
        .iter()
        .filter(|entry| entry.name == VCardProperty::Email)
        .flat_map(|entry| entry.values.iter())
        .filter_map(|value| match value {
            VCardValue::Text(text) => {
                let text = text.trim();
                let text = text.strip_prefix("mailto:").unwrap_or(text);
                Some(text.trim().to_lowercase())
            }
            _ => None,
        })
        .filter(|email| email.len() <= 320 && email.contains('@') && !email.contains(char::is_whitespace))
        .take(MAX_EMAILS_PER_CARD)
        .collect();
    emails.sort_unstable();
    emails.dedup();
    emails
}

fn insert(conn: &Connection, resource_id: i64, collection_id: i64, account_id: i64, content: &str) -> Result<()> {
    for email in photo_emails(content) {
        conn.execute(
            "INSERT OR IGNORE INTO contact_photos (resource_id, collection_id, account_id, email)
             VALUES (?1, ?2, ?3, ?4)",
            params![resource_id, collection_id, account_id, email],
        )?;
    }
    Ok(())
}

/// Brings the rows of one card up to date after it was written into `collection`.
pub(crate) fn index_card(
    tx: &Transaction<'_>,
    resource_id: i64,
    collection: &DavCollection,
    content: &str,
) -> Result<()> {
    if collection.kind != DavKind::Addressbook {
        return Ok(());
    }
    tx.execute("DELETE FROM contact_photos WHERE resource_id = ?1", [resource_id])?;
    insert(tx, resource_id, collection.id, collection.account_id, content)
}

/// Fills the table for the cards that were there before it existed, once after migration 0055.
pub(crate) fn backfill(conn: &mut Connection) -> Result<()> {
    if crate::db::get_setting(conn, BACKFILL_MARKER)?.is_none() {
        return Ok(());
    }
    let tx = conn.transaction()?;
    let mut indexed = 0usize;
    {
        let mut cards = tx.prepare(
            "SELECT r.id, r.collection_id, c.account_id, r.content FROM dav_resources r
             JOIN dav_collections c ON c.id = r.collection_id
             WHERE c.kind = 'addressbook' AND r.component = 'VCARD'",
        )?;
        let mut rows = cards.query([])?;
        while let Some(row) = rows.next()? {
            let content: String = row.get(3)?;
            insert(&tx, row.get(0)?, row.get(1)?, row.get(2)?, &content)?;
            indexed += 1;
        }
    }
    crate::db::delete_setting(&tx, BACKFILL_MARKER)?;
    tx.commit()?;
    if indexed > 0 {
        tracing::info!(cards = indexed, "indexed the photos of existing contacts");
    }
    Ok(())
}

impl Store {
    /// The first card with a photo for `email` that `account_id` sees: in its default address book
    /// first, then in its other ones, then in those shared with it. The card's content, to take the
    /// photo from.
    pub async fn contact_card_with_photo(&self, account_id: i64, email: &str) -> Result<Option<String>> {
        let email = email.trim().to_lowercase();
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT r.content FROM contact_photos p
                     JOIN dav_resources r ON r.id = p.resource_id
                     JOIN dav_collections c ON c.id = p.collection_id
                     JOIN accounts o ON o.id = c.account_id AND o.deleted_at IS NULL
                     WHERE p.email = ?2
                       AND (p.account_id = ?1
                            OR p.collection_id IN (SELECT collection_id FROM dav_shares WHERE account_id = ?1))
                     ORDER BY p.account_id = ?1 DESC, c.is_default DESC, c.sort_order, c.id, r.id
                     LIMIT 1",
                    params![account_id, email],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn photos_and_addresses_are_read_from_cards() {
        let v4 = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:a\r\nFN:Ami\r\nEMAIL:Ami@Example.org\r\n\
                  EMAIL;TYPE=work:mailto:ami@work.example\r\nPHOTO:data:image/png;base64,iVBORw0KGgo=\r\nEND:VCARD\r\n";
        assert_eq!(photo_emails(v4), ["ami@example.org", "ami@work.example"]);
        assert!(matches!(contact_photo(v4), Some(ContactPhoto::Inline(bytes)) if bytes.starts_with(b"\x89PNG")));

        let v3 = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:b\r\nFN:Nyu\r\nEMAIL:nyu@example.org\r\n\
                  PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQ\r\nEND:VCARD\r\n";
        assert_eq!(photo_emails(v3), ["nyu@example.org"]);
        assert!(matches!(contact_photo(v3), Some(ContactPhoto::Inline(bytes)) if bytes.starts_with(&[0xff, 0xd8])));

        let linked = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c\r\nFN:Mini\r\nEMAIL:mini@example.org\r\n\
                      PHOTO:https://pictures.example.org/mini.jpg\r\nEND:VCARD\r\n";
        assert_eq!(contact_photo(linked), Some(ContactPhoto::Remote("https://pictures.example.org/mini.jpg".into())));

        let plain_http = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:d\r\nFN:X\r\nEMAIL:x@example.org\r\n\
                          PHOTO:http://pictures.example.org/x.jpg\r\nEND:VCARD\r\n";
        assert!(photo_emails(plain_http).is_empty(), "never fetched, so not indexed");
        let without = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:e\r\nFN:Y\r\nEMAIL:y@example.org\r\nEND:VCARD\r\n";
        assert!(photo_emails(without).is_empty());
    }
}
