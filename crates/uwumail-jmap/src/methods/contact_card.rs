//! ContactCard/get, /set and /query (RFC 9610, section 3) on the vCards of the account's CardDAV
//! address books. See docs/jmap-contacts.md.
//!
//! Cards are read from and written back to vCard each time; the JSON clients see is what calcard
//! makes of the stored card.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use uwumail_store::{ContactCardRecord, ContactCardWrite, DAV_RESOURCE_MAX_BYTES, DavCollection, StoreError};

use super::address_book::{address_books, check_enabled};
use super::{Ctx, SetResponse, check_set_size, get_ids, if_in_state, pick, query_response};
use crate::error::{MethodError, MethodResult, SetError};
use crate::jscal::{apply_patch, format_utc, matches_terms, now, overlapping_paths, parse_utc, pointer_tokens};
use crate::jscontact;
use crate::{MAX_OBJECTS_IN_GET, ids};

/// Always part of a card returned by /get, whatever `properties` says.
const ALWAYS: &[&str] = &["id", "addressBookIds"];
const MAX_QUERY_LIMIT: usize = 5000;
/// How long one query may spend reading cards before it gives up.
const QUERY_TIME_LIMIT: Duration = Duration::from_secs(5);
/// Writes retried when CardDAV changed the card between reading and writing it.
const WRITE_ATTEMPTS: usize = 3;

/// A stored card, read.
struct Loaded {
    record: ContactCardRecord,
    card: Map<String, Value>,
}

async fn load(ctx: &Ctx<'_>, ids: Option<Vec<i64>>) -> MethodResult<Vec<Loaded>> {
    let all = ids.is_none();
    let records = ctx.jmap.store.contact_cards(ctx.account.id, ids).await?;
    if all && records.len() > MAX_OBJECTS_IN_GET {
        return Err(MethodError::new("requestTooLarge", "too many cards to fetch at once; ask for ids"));
    }
    run_blocking(move || {
        records
            .into_iter()
            .filter_map(|record| Some(Loaded { card: jscontact::from_vcard(&record.content)?, record }))
            .collect()
    })
    .await
}

async fn run_blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> MethodResult<T> {
    tokio::task::spawn_blocking(f).await.map_err(|_| MethodError::server_fail("the contact work failed"))
}

/// The card as JMAP shows it: with its id and address book.
fn decorate(card: &mut Map<String, Value>, id: i64, address_book_id: i64) {
    card.insert("id".into(), json!(ids::contact_card(id)));
    card.insert("addressBookIds".into(), json!({ ids::address_book(address_book_id): true }));
}

/// The requested properties of a card; `None` asks for all but the vCard conversion hints.
fn output(mut card: Map<String, Value>, properties: &Option<Vec<String>>) -> Value {
    match properties {
        None => {
            card.remove("vCard");
            Value::Object(card)
        }
        Some(list) => pick(card, list),
    }
}

// ------------------------------------------------------------------------------------------------
// ContactCard/get

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let state = ctx.state().await?;
    let properties = match args.get("properties") {
        None | Some(Value::Null) => None,
        Some(_) => {
            let mut list = super::properties(args, "properties", &[])?;
            for always in ALWAYS {
                if !list.iter().any(|p| p == always) {
                    list.push((*always).to_owned());
                }
            }
            Some(list)
        }
    };
    let (loaded, requested) = match get_ids(args)? {
        None => (load(ctx, None).await?, None),
        Some(requested) => {
            let wanted: BTreeSet<i64> = requested.iter().filter_map(|id| ctx.parse_id('k', id)).collect();
            let loaded =
                if wanted.is_empty() { Vec::new() } else { load(ctx, Some(wanted.into_iter().collect())).await? };
            (loaded, Some(requested))
        }
    };
    let mut list = Vec::new();
    let mut not_found = Vec::new();
    let show = |loaded: &Loaded| {
        let mut card = loaded.card.clone();
        decorate(&mut card, loaded.record.id, loaded.record.address_book_id);
        output(card, &properties)
    };
    match requested {
        None => list.extend(loaded.iter().map(show)),
        Some(requested) => {
            for id in requested {
                match ctx.parse_id('k', &id).and_then(|n| loaded.iter().find(|l| l.record.id == n)) {
                    Some(found) => list.push(show(found)),
                    None => not_found.push(id),
                }
            }
        }
    }
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

// ------------------------------------------------------------------------------------------------
// ContactCard/set

fn invalid(error: jscontact::Invalid) -> SetError {
    let properties: Vec<&str> = error.properties.iter().map(String::as_str).collect();
    SetError::invalid_properties(&properties, error.description)
}

/// The one address book of `addressBookIds`, which has to be one of the account's.
fn address_book_of(ctx: &Ctx<'_>, value: &Value, books: &[DavCollection]) -> Result<i64, SetError> {
    let error =
        || SetError::invalid_properties(&["addressBookIds"], "a card belongs to exactly one of your address books");
    let Value::Object(map) = value else { return Err(error()) };
    let mut chosen = map.iter().filter(|(_, v)| v.as_bool() == Some(true));
    let (Some((id, _)), None) = (chosen.next(), chosen.next()) else { return Err(error()) };
    if map.values().any(|v| v.as_bool() != Some(true)) {
        return Err(error());
    }
    let id = ctx.parse_id('b', id).ok_or_else(error)?;
    books.iter().any(|b| b.id == id).then_some(id).ok_or_else(error)
}

/// A new uid: a random UUID as a URN, as vCard 4 has them.
fn new_uid() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the operating system RNG failed");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = hex::encode(bytes);
    format!("urn:uuid:{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// Checks a card and turns it into a vCard the way a CardDAV PUT would take it. Returns the vCard
/// and its uid.
fn convert(card: &Map<String, Value>) -> Result<String, SetError> {
    jscontact::validate(card).map_err(invalid)?;
    let content = jscontact::to_vcard(card).map_err(|message| SetError::new("invalidProperties", message))?;
    if content.len() > DAV_RESOURCE_MAX_BYTES {
        return Err(SetError::new("tooLarge", format!("a card may take up to {DAV_RESOURCE_MAX_BYTES} bytes")));
    }
    // The same check a CardDAV PUT goes through, and the uid has to come through unchanged.
    let uid = card.get("uid").and_then(Value::as_str).unwrap_or_default();
    let stored_uid = calcard::vcard::VCard::parse(&content).ok().and_then(|v| v.uid().map(str::to_owned));
    if stored_uid.as_deref() != Some(uid) || jscontact::from_vcard(&content).is_none() {
        return Err(SetError::new("invalidProperties", "the card cannot be stored as vCard"));
    }
    Ok(content)
}

/// Everything a write needs from around it.
struct Writer<'a> {
    books: Vec<DavCollection>,
    ctx: &'a Ctx<'a>,
}

impl Writer<'_> {
    /// Checks a card, turns it into a vCard and stores it.
    async fn store(
        &self,
        card: &Map<String, Value>,
        id: Option<i64>,
        address_book_id: i64,
        if_etag: Option<String>,
    ) -> Result<Result<i64, SetError>, ()> {
        let checked = card.clone();
        let content = match run_blocking(move || convert(&checked)).await {
            Ok(Ok(content)) => content,
            Ok(Err(err)) => return Ok(Err(err)),
            Err(_) => return Ok(Err(SetError::new("serverFail", "the card could not be converted"))),
        };
        let uid = card.get("uid").and_then(Value::as_str).unwrap_or_default().to_owned();
        let write = ContactCardWrite { id, address_book_id, content, uid, if_etag };
        match self.ctx.jmap.store.put_contact_card(self.ctx.account.id, write).await {
            Ok((id, _)) => Ok(Ok(id)),
            // Changed over CardDAV meanwhile: read it again.
            Err(StoreError::Conflict(_)) => Err(()),
            Err(StoreError::QuotaExceeded) => Ok(Err(SetError::new("overQuota", "the address book is full"))),
            Err(StoreError::NotFound(_)) if id.is_some() => Ok(Err(SetError::not_found())),
            Err(StoreError::NotFound(_)) => {
                Ok(Err(SetError::invalid_properties(&["addressBookIds"], "no such address book")))
            }
            Err(err) => Ok(Err(SetError::from(err))),
        }
    }

    async fn create(&self, object: &Value) -> Result<(i64, Map<String, Value>), SetError> {
        let Value::Object(object) = object else {
            return Err(SetError::new("invalidProperties", "a card must be an object"));
        };
        // A null is the same as leaving the property out, at the top.
        let mut card: Map<String, Value> =
            object.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| (k.clone(), v.clone())).collect();
        if card.contains_key("id") {
            return Err(SetError::invalid_properties(&["id"], "set by the server"));
        }
        let mut server_set: Map<String, Value> = Map::new();
        // Without a choice, the card goes into the default address book.
        let address_book_id =
            match card.remove("addressBookIds") {
                Some(value) => address_book_of(self.ctx, &value, &self.books)?,
                None => {
                    let book =
                        self.books.iter().find(|b| b.is_default).or(self.books.first()).ok_or_else(|| {
                            SetError::invalid_properties(&["addressBookIds"], "there is no address book")
                        })?;
                    server_set.insert("addressBookIds".into(), json!({ ids::address_book(book.id): true }));
                    book.id
                }
            };
        let stamp = json!(format_utc(now()));
        for (property, value) in
            [("@type", json!("Card")), ("version", json!("1.0")), ("uid", json!(new_uid())), ("created", stamp.clone())]
        {
            if !card.contains_key(property) {
                card.insert(property.into(), value.clone());
                server_set.insert(property.into(), value);
            }
        }
        card.insert("updated".into(), stamp.clone());
        server_set.insert("updated".into(), stamp);
        let id = match self.store(&card, None, address_book_id, None).await {
            Ok(result) => result?,
            Err(()) => return Err(SetError::new("serverFail", "the card could not be stored")),
        };
        server_set.insert("id".into(), json!(ids::contact_card(id)));
        Ok((id, server_set))
    }

    /// Applies a patch to a stored card. Returns what the server set.
    async fn update(&self, id: i64, patch: &Map<String, Value>) -> Result<Value, SetError> {
        let paths: Vec<&str> = patch.keys().map(String::as_str).collect();
        if overlapping_paths(&paths) {
            return Err(SetError::new("invalidPatch", "one path of the patch lies inside another"));
        }
        for _ in 0..WRITE_ATTEMPTS {
            let loaded = self.load_one(id).await?;
            let mut card = loaded.card.clone();
            let mut books: BTreeSet<i64> = BTreeSet::from([loaded.record.address_book_id]);
            for (path, value) in patch {
                let tokens = pointer_tokens(path)
                    .ok_or_else(|| SetError::new("invalidPatch", format!("{path} is not a pointer")))?;
                let top = tokens[0].as_str();
                match top {
                    "addressBookIds" if tokens.len() == 1 => {
                        books = BTreeSet::from([address_book_of(self.ctx, value, &self.books)?]);
                    }
                    "addressBookIds" if tokens.len() == 2 => {
                        let book = self
                            .ctx
                            .parse_id('b', &tokens[1])
                            .ok_or_else(|| SetError::invalid_properties(&["addressBookIds"], "no such address book"))?;
                        match value {
                            Value::Bool(true) => {
                                books.insert(book);
                            }
                            Value::Null | Value::Bool(false) => {
                                books.remove(&book);
                            }
                            _ => return Err(SetError::invalid_properties(&["addressBookIds"], "values must be true")),
                        }
                    }
                    "uid" | "@type" if tokens.len() == 1 && loaded.card.get(top) == Some(value) => {}
                    "id" | "uid" | "@type" | "addressBookIds" => {
                        return Err(SetError::invalid_properties(&[top], "cannot be changed"));
                    }
                    _ => apply_patch(&mut card, path, value.clone())
                        .map_err(|message| SetError::new("invalidPatch", message))?,
                }
            }
            let [address_book_id] = books.iter().copied().collect::<Vec<_>>()[..] else {
                return Err(SetError::invalid_properties(
                    &["addressBookIds"],
                    "a card belongs to exactly one address book",
                ));
            };
            if !self.books.iter().any(|b| b.id == address_book_id) {
                return Err(SetError::invalid_properties(&["addressBookIds"], "no such address book"));
            }
            let stamp = json!(format_utc(now()));
            card.insert("updated".into(), stamp.clone());
            let etag = Some(loaded.record.etag.clone());
            match self.store(&card, Some(id), address_book_id, etag).await {
                Ok(result) => {
                    result?;
                    return Ok(json!({ "updated": stamp }));
                }
                Err(()) => continue,
            }
        }
        Err(SetError::new("serverFail", "the card keeps changing; try again"))
    }

    async fn load_one(&self, id: i64) -> Result<Loaded, SetError> {
        let mut loaded = load(self.ctx, Some(vec![id]))
            .await
            .map_err(|_| SetError::new("serverFail", "the card could not be read"))?;
        loaded.pop().ok_or_else(SetError::not_found)
    }
}

pub async fn set(ctx: &mut Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    check_set_size(args)?;
    let books = address_books(ctx).await?;
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let mut response = SetResponse::default();
    let mut created_ids: Vec<(String, String)> = Vec::new();

    {
        let writer = Writer { books, ctx };
        for (creation_id, object) in args.get("create").and_then(Value::as_object).into_iter().flatten() {
            match writer.create(object).await {
                Ok((id, server_set)) => {
                    created_ids.push((creation_id.clone(), ids::contact_card(id)));
                    response.created.insert(creation_id.clone(), Value::Object(server_set));
                }
                Err(err) => {
                    response.not_created.insert(creation_id.clone(), err.to_json());
                }
            }
        }
    }
    // Later updates in the same call may name the new cards by their creation ids.
    ctx.created_ids.extend(created_ids);
    let books = address_books(ctx).await?;
    let writer = Writer { books, ctx };

    for (id, patch) in args.get("update").and_then(Value::as_object).into_iter().flatten() {
        let result = async {
            let patch =
                patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))?;
            let card_id = ctx.parse_id('k', id).ok_or_else(SetError::not_found)?;
            writer.update(card_id, patch).await
        }
        .await;
        match result {
            Ok(server_set) => {
                response.updated.insert(id.clone(), server_set);
            }
            Err(err) => {
                response.not_updated.insert(id.clone(), err.to_json());
            }
        }
    }

    for id in args.get("destroy").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        let result = match ctx.parse_id('k', id) {
            Some(card_id) => {
                ctx.jmap.store.destroy_contact_card(ctx.account.id, card_id, None).await.map_err(SetError::from)
            }
            None => Err(SetError::not_found()),
        };
        match result {
            Ok(()) => response.destroyed.push(id.to_owned()),
            Err(err) => {
                response.not_destroyed.insert(id.to_owned(), err.to_json());
            }
        }
    }

    let new_state = ctx.state().await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

// ------------------------------------------------------------------------------------------------
// ContactCard/query

/// Conditions that search a text (RFC 9610, section 3.3.1).
const TEXT_CONDITIONS: &[&str] = &[
    "text",
    "name",
    "name/given",
    "name/surname",
    "name/surname2",
    "nickname",
    "organization",
    "email",
    "phone",
    "onlineService",
    "address",
    "note",
];
const DATE_CONDITIONS: &[&str] = &["createdBefore", "createdAfter", "updatedBefore", "updatedAfter"];
const SORTS: &[&str] = &["created", "updated", "name/given", "name/surname", "name/surname2"];

#[derive(Debug, Clone)]
enum Filter {
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Vec<Filter>),
    Condition(Condition),
}

#[derive(Debug, Clone, Default)]
struct Condition {
    /// `None` inside: an address book that is not the account's, which nothing is in.
    address_book: Option<Option<i64>>,
    uid: Option<String>,
    has_member: Option<String>,
    kind: Option<String>,
    /// `(property, is before, the time)`.
    dates: Vec<(&'static str, bool, i64)>,
    /// `(condition, terms)`.
    texts: Vec<(&'static str, Vec<String>)>,
}

fn parse_filter(ctx: &Ctx<'_>, value: &Value) -> MethodResult<Filter> {
    let Value::Object(map) = value else {
        return Err(MethodError::new("unsupportedFilter", "a filter is an object"));
    };
    if let Some(operator) = map.get("operator") {
        let conditions = map
            .get("conditions")
            .and_then(Value::as_array)
            .ok_or_else(|| MethodError::new("unsupportedFilter", "an operator needs conditions"))?
            .iter()
            .map(|c| parse_filter(ctx, c))
            .collect::<MethodResult<Vec<_>>>()?;
        return match operator.as_str() {
            Some("AND") => Ok(Filter::And(conditions)),
            Some("OR") => Ok(Filter::Or(conditions)),
            Some("NOT") => Ok(Filter::Not(conditions)),
            _ => Err(MethodError::new("unsupportedFilter", "operator must be AND, OR or NOT")),
        };
    }
    let mut condition = Condition::default();
    for (key, value) in map {
        let text =
            value.as_str().ok_or_else(|| MethodError::new("unsupportedFilter", format!("{key} must be a text")))?;
        if let Some(name) = TEXT_CONDITIONS.iter().find(|c| **c == key) {
            condition.texts.push((name, crate::jscal::search_terms(text)));
            continue;
        }
        if let Some(name) = DATE_CONDITIONS.iter().find(|c| **c == key) {
            let time = parse_utc(text)
                .ok_or_else(|| MethodError::new("unsupportedFilter", format!("{key} must be a UTC date and time")))?;
            let property = if name.starts_with("created") { "created" } else { "updated" };
            condition.dates.push((property, name.ends_with("Before"), time));
            continue;
        }
        match key.as_str() {
            "inAddressBook" => condition.address_book = Some(ctx.parse_id('b', text)),
            "uid" => condition.uid = Some(text.to_owned()),
            "hasMember" => condition.has_member = Some(text.to_owned()),
            "kind" => condition.kind = Some(text.to_owned()),
            _ => {
                return Err(MethodError::new(
                    "unsupportedFilter",
                    format!("{key} is not a filter of ContactCard/query"),
                ));
            }
        }
    }
    Ok(Filter::Condition(condition))
}

fn condition_matches(condition: &Condition, loaded: &Loaded) -> bool {
    let card = &loaded.card;
    if let Some(book) = condition.address_book
        && book != Some(loaded.record.address_book_id)
    {
        return false;
    }
    if condition.uid.as_ref().is_some_and(|uid| card.get("uid").and_then(Value::as_str) != Some(uid.as_str())) {
        return false;
    }
    if let Some(kind) = &condition.kind
        && card.get("kind").and_then(Value::as_str).unwrap_or("individual") != kind
    {
        return false;
    }
    if let Some(member) = &condition.has_member
        && card.get("members").and_then(|m| m.get(member)) != Some(&Value::Bool(true))
    {
        return false;
    }
    for (property, before, time) in &condition.dates {
        let Some(stamp) = card.get(*property).and_then(Value::as_str).and_then(parse_utc) else { return false };
        if (*before && stamp >= *time) || (!*before && stamp < *time) {
            return false;
        }
    }
    condition.texts.iter().all(|(name, terms)| {
        let haystack = if *name == "text" { jscontact::all_text(card) } else { jscontact::field_text(card, name) };
        matches_terms(&haystack, terms)
    })
}

fn matches(filter: &Filter, loaded: &Loaded) -> bool {
    match filter {
        Filter::And(all) => all.iter().all(|f| matches(f, loaded)),
        Filter::Or(any) => any.iter().any(|f| matches(f, loaded)),
        Filter::Not(none) => !none.iter().any(|f| matches(f, loaded)),
        Filter::Condition(condition) => condition_matches(condition, loaded),
    }
}

/// The value a card is sorted by for one sort property.
fn sort_key(card: &Map<String, Value>, property: &str) -> String {
    match property {
        "created" | "updated" => card.get(property).and_then(Value::as_str).unwrap_or_default().to_owned(),
        name => jscontact::sort_text(card, name.trim_start_matches("name/")),
    }
}

pub async fn query(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    check_enabled(ctx)?;
    let state = ctx.state().await?;
    let filter = match args.get("filter") {
        None | Some(Value::Null) => None,
        Some(value) => Some(parse_filter(ctx, value)?),
    };
    let mut sort: Vec<(String, bool)> = Vec::new();
    if let Some(list) = args.get("sort").filter(|s| !s.is_null()) {
        for comparator in list.as_array().ok_or_else(|| MethodError::invalid_arguments("sort must be a list"))? {
            let property = comparator.get("property").and_then(Value::as_str).unwrap_or_default();
            if !SORTS.contains(&property) {
                return Err(MethodError::new("unsupportedSort", format!("cannot sort by {property}")));
            }
            sort.push((property.to_owned(), comparator.get("isAscending").and_then(Value::as_bool).unwrap_or(true)));
        }
    }
    let records = ctx.jmap.store.contact_cards(ctx.account.id, None).await?;
    let deadline = Instant::now() + QUERY_TIME_LIMIT;
    let mut hits = run_blocking(move || -> MethodResult<Vec<(i64, Vec<String>)>> {
        let mut hits = Vec::new();
        for record in records {
            if Instant::now() > deadline {
                return Err(MethodError::new("serverUnavailable", "searching the cards takes too long"));
            }
            let Some(card) = jscontact::from_vcard(&record.content) else { continue };
            let loaded = Loaded { record, card };
            if filter.as_ref().is_none_or(|filter| matches(filter, &loaded)) {
                let keys: Vec<String> = sort.iter().map(|(property, _)| sort_key(&loaded.card, property)).collect();
                hits.push((loaded.record.id, keys));
            }
        }
        hits.sort_by(|a, b| {
            for (index, (_, ascending)) in sort.iter().enumerate() {
                let order = a.1[index].cmp(&b.1[index]);
                let order = if *ascending { order } else { order.reverse() };
                if order.is_ne() {
                    return order;
                }
            }
            a.0.cmp(&b.0)
        });
        Ok(hits)
    })
    .await??;
    let ids = hits.drain(..).map(|(id, _)| ids::contact_card(id)).collect();
    query_response(ctx, args, state, ids, MAX_QUERY_LIMIT)
}
