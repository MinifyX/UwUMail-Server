//! Our own extension `urn:uwumail:jmap:birthdays` (docs/birthdays.md): finding the birthdays
//! other calendars kept as events and moving them into the contacts, where the birthdays calendar
//! shows them. `Birthdays/scan` finds and matches, `Birthdays/import` moves and deletes the event
//! it came from, both or neither.

use serde_json::{Map, Value, json};
use uwumail_store::StoreError;
use uwumail_store::birthday_import::{BirthdayCandidate, BirthdayTarget, CardChoice};
use uwumail_store::birthdays::PartialDate;

use super::Ctx;
use crate::error::{MethodError, MethodResult, SetError};
use crate::ids;

/// Events one `Birthdays/import` moves at most.
pub const MAX_IMPORT: usize = 500;

fn check_enabled(ctx: &Ctx<'_>) -> MethodResult<()> {
    super::calendar::check_enabled(ctx)?;
    super::address_book::check_enabled(ctx)?;
    if ctx.shared.is_some() {
        return Err(MethodError::new("accountNotSupportedByMethod", "birthdays are moved in one's own account"));
    }
    Ok(())
}

fn date_json(date: &PartialDate) -> Value {
    json!({ "month": date.month, "day": date.day, "year": date.year })
}

fn choice_json(choice: &CardChoice) -> Value {
    json!({
        "contactId": ids::contact_card(choice.card_id),
        "addressBookId": ids::address_book(choice.address_book_id),
        "name": choice.name,
        "birthday": choice.birthday.as_ref().map(date_json),
    })
}

fn candidate_json(candidate: &BirthdayCandidate) -> Value {
    json!({
        "eventId": ids::calendar_event(candidate.event_id),
        "calendarId": ids::calendar(candidate.calendar_id),
        "title": candidate.title,
        "name": candidate.found.name,
        "birthday": date_json(&candidate.found.date),
        "marked": candidate.found.marked,
        "mayDeleteEvent": candidate.deletable,
        "match": candidate.state.as_str(),
        "contacts": candidate.choices.iter().map(choice_json).collect::<Vec<_>>(),
    })
}

/// `Birthdays/scan {accountId}` → `{accountId, candidates, truncated}`.
pub async fn scan(ctx: &Ctx<'_>, _args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let (candidates, truncated) = ctx.jmap.store.scan_birthday_events(ctx.account.id).await?;
    Ok(json!({
        "accountId": ctx.account_id(),
        "candidates": candidates.iter().map(candidate_json).collect::<Vec<_>>(),
        "truncated": truncated,
    }))
}

/// One entry of `Birthdays/import`: what to do with the event's birthday.
fn target_of(ctx: &Ctx<'_>, entry: &Map<String, Value>) -> Result<BirthdayTarget, SetError> {
    let overwrite = match entry.get("overwrite") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(overwrite)) => *overwrite,
        Some(_) => return Err(SetError::invalid_properties(&["overwrite"], "must be true or false")),
    };
    match (entry.get("contactId"), entry.get("newContact")) {
        (Some(Value::String(id)), None | Some(Value::Null)) => {
            let card_id =
                ctx.parse_id('k', id).ok_or_else(|| SetError::invalid_properties(&["contactId"], "no such contact"))?;
            Ok(BirthdayTarget::Card { card_id, overwrite })
        }
        (None | Some(Value::Null), Some(Value::Object(new))) => {
            let name = new.get("name").and_then(Value::as_str).map(str::trim).unwrap_or_default();
            if name.is_empty() {
                return Err(SetError::invalid_properties(&["newContact/name"], "a new contact needs a name"));
            }
            let address_book_id = match new.get("addressBookId") {
                None | Some(Value::Null) => None,
                Some(Value::String(id)) => Some(ctx.parse_id('b', id).ok_or_else(|| {
                    SetError::invalid_properties(&["newContact/addressBookId"], "no such address book")
                })?),
                Some(_) => return Err(SetError::invalid_properties(&["newContact/addressBookId"], "must be an id")),
            };
            Ok(BirthdayTarget::NewCard { name: name.to_owned(), address_book_id })
        }
        _ => Err(SetError::invalid_properties(
            &["contactId", "newContact"],
            "give exactly one of contactId and newContact",
        )),
    }
}

/// `Birthdays/import {accountId, entries: {eventId: {contactId | newContact: {name, addressBookId},
/// overwrite, deleteEvent}}}` → `{accountId, imported: {eventId: {contactId, created, eventDeleted}},
/// notImported: {eventId: SetError}}`.
pub async fn import(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let entries = match args.get("entries") {
        Some(Value::Object(entries)) if entries.len() <= MAX_IMPORT => entries,
        Some(Value::Object(_)) => {
            return Err(MethodError::new("requestTooLarge", format!("at most {MAX_IMPORT} entries at once")));
        }
        _ => return Err(MethodError::invalid_arguments("entries must be an object of event ids")),
    };
    // The contacts' address book is made first, as for ContactCard/set.
    super::address_book::address_books(ctx).await?;
    let mut imported = Map::new();
    let mut not_imported = Map::new();
    for (event, entry) in entries {
        let result = async {
            let entry = entry.as_object().ok_or_else(|| SetError::new("invalidArguments", "an entry is an object"))?;
            let event_id = ctx.parse_id('v', event).ok_or_else(SetError::not_found)?;
            let target = target_of(ctx, entry)?;
            let delete_event = entry.get("deleteEvent").and_then(Value::as_bool).unwrap_or(true);
            match ctx.jmap.store.move_birthday(ctx.account.id, event_id, target, delete_event).await {
                Ok(moved) => Ok(json!({
                    "contactId": ids::contact_card(moved.card_id),
                    "created": moved.created,
                    "eventDeleted": moved.event_deleted,
                })),
                Err(StoreError::Conflict(_)) => {
                    Err(SetError::new("stateMismatch", "the event or the contact changed meanwhile; scan again"))
                }
                Err(err) => Err(SetError::from(err)),
            }
        }
        .await;
        match result {
            Ok(done) => {
                imported.insert(event.clone(), done);
            }
            Err(err) => {
                not_imported.insert(event.clone(), err.to_json());
            }
        }
    }
    Ok(json!({ "accountId": ctx.account_id(), "imported": imported, "notImported": not_imported }))
}
