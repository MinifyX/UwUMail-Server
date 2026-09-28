//! Downloading a whole calendar or address book from another CalDAV or CardDAV server: the
//! entries it lists, then their data in portions with multiget, or one by one where the server
//! has no multiget.

use uwumail_store::DavKind;

use super::{Remote, RemoteCollection, RemoteError};
use crate::xml::{self, CALDAV, CARDDAV, DAV};

/// Entries asked for per multiget.
const BATCH: usize = 50;
/// The most one multiget answer may carry.
const MAX_BATCH_BYTES: usize = 32 * 1024 * 1024;
/// The most one collection may bring altogether.
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
/// The most all collections of one move may bring together. Each is kept until all are fetched,
/// so a provider listing many collections of 60 MiB each could fill the memory
/// (security-audit-0.16.0 PROTOCOLS-12).
pub const MAX_IMPORT_BYTES: usize = 128 * 1024 * 1024;
/// Entries of one collection; a collection here holds no more.
const MAX_ENTRIES: usize = uwumail_store::IMPORT_MAX_OBJECTS;

/// The data of every entry of a remote collection, as the server has it: iCalendar objects or
/// vCards, to be cut and checked like a file. `budget` is what the whole move may still bring
/// (start with [`MAX_IMPORT_BYTES`]); what this collection brings is taken off it, and a
/// collection that would take more fails with [`RemoteError::TooLarge`].
pub async fn fetch_collection(
    remote: &mut Remote<'_>,
    collection: &RemoteCollection,
    budget: &mut usize,
) -> Result<Vec<String>, RemoteError> {
    let limit = MAX_TOTAL_BYTES.min(*budget);
    let url = collection.url.as_str();
    let listed = remote.propfind(url, 1, "<d:getetag/><d:resourcetype/>").await?.ok_or(RemoteError::NotFound)?;
    let hrefs: Vec<String> = listed
        .entries
        .iter()
        .filter(|entry| entry.href.path() != collection.url.path() && entry.prop(DAV, "getetag").is_some())
        .filter(|entry| entry.prop(DAV, "resourcetype").is_none_or(|types| types.children.is_empty()))
        .map(|entry| entry.href.path().to_owned())
        .take(MAX_ENTRIES)
        .collect();
    let (ns, data, report, prefix) = match collection.kind {
        DavKind::Calendar => (CALDAV, "calendar-data", "calendar-multiget", "c"),
        DavKind::Addressbook => (CARDDAV, "address-data", "addressbook-multiget", "card"),
    };
    let mut texts = Vec::with_capacity(hrefs.len());
    let mut total = 0usize;
    let mut multiget = true;
    for batch in hrefs.chunks(BATCH) {
        let mut got = None;
        if multiget {
            let body = format!(
                "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<{prefix}:{report} xmlns:d=\"DAV:\" \
xmlns:c=\"urn:ietf:params:xml:ns:caldav\" xmlns:card=\"urn:ietf:params:xml:ns:carddav\">\
<d:prop><d:getetag/><{prefix}:{data}/></d:prop>{}</{prefix}:{report}>",
                batch.iter().map(|href| format!("<d:href>{}</d:href>", xml::escape(href))).collect::<String>()
            );
            match remote.report(url, body, MAX_BATCH_BYTES).await? {
                Some(answer) => {
                    got = Some(
                        answer
                            .entries
                            .iter()
                            .filter(|entry| !entry.missing)
                            .filter_map(|entry| entry.prop(ns, data).map(|data| data.all_text()))
                            .collect::<Vec<_>>(),
                    )
                }
                None => multiget = false,
            }
        }
        let batch_texts = match got {
            Some(texts) => texts,
            None => {
                let mut one_by_one = Vec::with_capacity(batch.len());
                for href in batch {
                    let Ok(entry) = collection.url.join(href) else { continue };
                    let accept = if collection.kind == DavKind::Calendar { "text/calendar" } else { "text/vcard" };
                    let reply = remote.get(entry.as_str(), accept, uwumail_store::DAV_RESOURCE_MAX_BYTES * 2).await?;
                    if reply.answer.status == 200 {
                        one_by_one.push(String::from_utf8_lossy(&reply.answer.body).into_owned());
                    }
                }
                one_by_one
            }
        };
        for text in batch_texts {
            total += text.len();
            if total > limit {
                return Err(RemoteError::TooLarge);
            }
            texts.push(text);
        }
    }
    *budget -= total;
    Ok(texts)
}
