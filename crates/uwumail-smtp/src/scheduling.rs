//! Calendar scheduling (iTIP, RFC 5546) between people: invitations, answers and cancellations.
//!
//! When someone changes an event they organize or are invited to, [`Smtp::schedule_change`] works
//! out whom to tell (see `uwumail_store::itip::plan`) and tells them:
//!
//! - people of this server get the change straight into their own calendars: an invitation lands
//!   in their default calendar waiting for an answer, an answer lands in the organizer's copy;
//! - everyone else gets mail (iMIP, RFC 6047) from the person, through the normal outbound queue.
//!
//! Mail with a scheduling message that arrives from elsewhere is read by [`incoming`]: invitations
//! go into the recipient's calendar waiting for an answer (the mail arrives as well), answers and
//! cancellations update what is there. Only what the sender may say counts: an answer only for the
//! attendee whose address sent it, a cancellation only from the event's organizer, both only when
//! SPF or DKIM vouch for the sender's address, and an invitation only for the recipient's own
//! addresses. The server never answers an invitation by itself, so two servers cannot keep each
//! other busy.

use mail_builder::MessageBuilder;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::mime::{BodyPart, MimePart};
use mail_parser::{MessageParser, MimeHeaders, PartType};
use uwumail_store::itip::{self, Component, Role};
use uwumail_store::{Account, CalendarEventWrite, DavKind, NewDavCollection, StoreError};

use crate::config::Language;
use crate::scheduling_texts::{self, Kind};
use crate::submission::{Submission, SubmissionRecipient};
use crate::{Context, Smtp, now, random_id};

/// Scheduling parts larger than this are not read.
const MAX_ITIP_BYTES: usize = 1024 * 1024;

/// What became of one message to one person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Written into the calendar of someone on this server.
    Calendar,
    /// Sent by mail.
    Mail,
    /// Nothing to do, like an answer to an event the organizer no longer has.
    Skipped,
    Failed(String),
}

/// What a change sent, per recipient address, for logs and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScheduleReport {
    pub sent: Vec<(String, String, Delivery)>,
}

/// The addresses an account is known by in events, lower case.
pub(crate) async fn own_addresses(ctx: &Context, account: &Account) -> Vec<String> {
    let mut own: Vec<String> = ctx.store.addresses(&account.login).await.unwrap_or_default();
    own.push(account.login.clone());
    let mut own: Vec<String> = own.into_iter().map(|a| a.to_lowercase()).collect();
    own.sort();
    own.dedup();
    own
}

/// The language a person chose, or the server's.
async fn language_of(ctx: &Context, account_id: i64) -> Language {
    let preferences = ctx.store.preferences(account_id).await.unwrap_or_default();
    Language::preferred(preferences.get("language").and_then(|v| v.as_str()), ctx.tone().language)
}

impl Smtp {
    /// The addresses an account is known by in events, lower case: its login and every address
    /// it receives mail at (the CalDAV calendar-user-address-set).
    pub async fn calendar_addresses(&self, account: &Account) -> Vec<String> {
        own_addresses(&self.inner, account).await
    }

    /// Tells attendees or the organizer about a change `account` made to a calendar object in one
    /// of its own calendars: `old` and `new` are the object before and after, `None` when it was
    /// not there or is gone (RFC 6638 implicit scheduling). Failures are logged; the change itself
    /// is stored already.
    pub async fn schedule_change(&self, account: &Account, old: Option<&str>, new: Option<&str>) -> ScheduleReport {
        let ctx = &self.inner;
        let mut report = ScheduleReport::default();
        let old = old.and_then(Component::parse);
        let new = new.and_then(Component::parse);
        let own = own_addresses(ctx, account).await;
        let plan = itip::plan(old.as_ref(), new.as_ref(), &own);
        if plan.is_empty() {
            return report;
        }
        let now = now();
        let Some(reference) = new.as_ref().or(old.as_ref()) else { return report };
        let language = language_of(ctx, account.id).await;

        if let Some(me) = &plan.reply_as {
            let Some(organizer) = plan.organizer.clone() else { return report };
            let partstat = if new.is_none() { Some("DECLINED") } else { None };
            let message = itip::reply(reference, me, partstat, now);
            let answer = itip::partstats(&message, me).first().map(|(_, status)| status.clone()).unwrap_or_default();
            let kind = Kind::of_answer(&answer);
            let delivery = self.deliver(account, me, &organizer, &message, kind, language).await;
            report.sent.push((organizer, "REPLY".into(), delivery));
            return report;
        }

        let Some(organizer) = plan.organizer.clone() else { return report };
        if let Some(new) = &new {
            let message = itip::request(new, now);
            let before: Vec<String> =
                old.as_ref().map(|old| itip::attendees(old).into_iter().map(|a| a.address).collect()).unwrap_or_default();
            for attendee in &plan.requests {
                let kind = if before.contains(attendee) { Kind::Update } else { Kind::Invitation };
                let delivery = self.deliver(account, &organizer, attendee, &message, kind, language).await;
                report.sent.push((attendee.clone(), "REQUEST".into(), delivery));
            }
        }
        for attendee in &plan.cancels {
            let message = itip::cancel(reference, std::slice::from_ref(attendee), new.is_none(), now);
            let delivery = self.deliver(account, &organizer, attendee, &message, Kind::Cancellation, language).await;
            report.sent.push((attendee.clone(), "CANCEL".into(), delivery));
        }
        report
    }

    /// Hands one message to one person: into their calendar when they are on this server and use
    /// calendars, by mail otherwise.
    async fn deliver(
        &self,
        sender: &Account,
        from: &str,
        to: &str,
        message: &Component,
        kind: Kind,
        language: Language,
    ) -> Delivery {
        let ctx = &self.inner;
        let local = match ctx.store.resolve_recipient(to).await {
            Ok(Some(id)) => ctx.store.delivery_target(id).await.ok().flatten(),
            _ => None,
        };
        if let Some(target) = local
            && let Ok(Some(target)) = ctx.store.account_by_id(target).await
            && target.protocols.caldav
            && target.deleted_at.is_none()
        {
            return match apply(ctx, &target, message, Some(from), true).await {
                Ok(true) => Delivery::Calendar,
                Ok(false) => Delivery::Skipped,
                Err(err) => {
                    tracing::warn!(%err, to, "a scheduling message could not be put into the calendar");
                    Delivery::Failed(err.to_string())
                }
            };
        }
        match self.send_imip(sender, from, to, message, kind, language).await {
            Ok(()) => Delivery::Mail,
            Err(err) => {
                tracing::warn!(%err, login = %sender.login, to, "a scheduling mail could not be sent");
                Delivery::Failed(err)
            }
        }
    }

    /// Sends a scheduling message by mail (RFC 6047): a text for people, the message for their
    /// calendar app, and the same as an attachment for apps that only look there.
    async fn send_imip(
        &self,
        sender: &Account,
        from: &str,
        to: &str,
        message: &Component,
        kind: Kind,
        language: Language,
    ) -> Result<(), String> {
        let method = itip::method(message).unwrap_or_else(|| "REQUEST".into());
        let summary = itip::summary(message);
        let name = if sender.display_name.trim().is_empty() { from } else { sender.display_name.trim() };
        let texts = scheduling_texts::scheduling(language, kind, name, &summary.title, &summary.when, &summary.location);
        let ics = message.to_ics();
        let domain = from.rsplit_once('@').map(|(_, d)| d.to_owned()).unwrap_or_default();
        let calendar = |content_type: &str| {
            ContentType::new(content_type.to_owned()).attribute("method", method.clone()).attribute("charset", "utf-8")
        };
        let alternative = MimePart::new(
            ContentType::new("multipart/alternative"),
            vec![
                MimePart::new(
                    ContentType::new("text/plain").attribute("charset", "utf-8"),
                    BodyPart::Text(texts.body.into()),
                ),
                MimePart::new(calendar("text/calendar"), BodyPart::Text(ics.clone().into())),
            ],
        );
        let attachment =
            MimePart::new(calendar("application/ics"), BodyPart::Text(ics.into())).attachment("invite.ics".to_owned());
        let raw = MessageBuilder::new()
            .from((name.to_owned(), from.to_owned()))
            .to(to.to_owned())
            .subject(texts.subject)
            .date(Date::now())
            .message_id(format!("{}.itip@{domain}", random_id()))
            .body(MimePart::new(ContentType::new("multipart/mixed"), vec![alternative, attachment]))
            .write_to_vec()
            .map_err(|err| err.to_string())?;
        let submission = Submission {
            account: sender.clone(),
            mail_from: from.to_owned(),
            recipients: vec![SubmissionRecipient::new(to)],
            raw,
            env_id: None,
            trace: None,
        };
        self.submit(submission).await.map(|_| ()).map_err(|err| err.to_string())
    }
}

/// Where a scheduling message came from, as far as it can be trusted.
#[derive(Debug, Clone, Copy)]
pub struct Sender<'a> {
    /// The From address, when SPF or DKIM vouch for it (or it is one of our own people's).
    pub verified_from: Option<&'a str>,
    /// Sent by someone logged in on this server, whose events may have organizers of this server.
    pub local: bool,
}

/// Reads the scheduling message of a mail that reached `account_id`, if it has one, and applies it
/// to their calendars. The mail itself is delivered as usual either way.
pub(crate) async fn incoming(ctx: &Context, account_id: i64, raw: &[u8], sender: Sender<'_>) {
    let Some(message) = find_itip(raw) else { return };
    let Some(calendar) = Component::parse(&message) else { return };
    let Ok(Some(account)) = ctx.store.account_by_id(account_id).await else { return };
    if !account.protocols.caldav || account.deleted_at.is_some() {
        return;
    }
    let method = itip::method(&calendar).unwrap_or_default();
    // Organizers of this server tell their attendees directly; a message from outside that claims
    // one of them is not believed.
    if !sender.local
        && let Some(organizer) = itip::organizer(&calendar)
        && ctx.store.resolve_recipient(&organizer).await.ok().flatten().is_some()
        && method != "REPLY"
    {
        tracing::info!(login = %account.login, %method, "ignoring a scheduling message from outside for an organizer of ours");
        return;
    }
    match apply(ctx, &account, &calendar, sender.verified_from, false).await {
        Ok(true) => tracing::info!(login = %account.login, %method, "took a scheduling message into the calendar"),
        Ok(false) => tracing::debug!(login = %account.login, %method, "a scheduling message changed nothing"),
        Err(err) => tracing::warn!(login = %account.login, %method, %err, "a scheduling message could not be applied"),
    }
}

/// The first iCalendar part of a mail that carries a method.
fn find_itip(raw: &[u8]) -> Option<String> {
    let message = MessageParser::new().parse(raw)?;
    message.parts.iter().find_map(|part| {
        let content_type = part.content_type()?;
        let full = match content_type.subtype() {
            Some(subtype) => format!("{}/{}", content_type.ctype(), subtype),
            None => content_type.ctype().to_owned(),
        }
        .to_ascii_lowercase();
        if full != "text/calendar" && full != "application/ics" {
            return None;
        }
        let text = match &part.body {
            PartType::Text(text) => text.to_string(),
            PartType::Binary(bytes) | PartType::InlineBinary(bytes) => String::from_utf8(bytes.to_vec()).ok()?,
            _ => return None,
        };
        (text.len() <= MAX_ITIP_BYTES && text.to_ascii_uppercase().contains("METHOD:")).then_some(text)
    })
}

/// Applies a scheduling message to the calendars of `account`. `from` is who sent it, when that is
/// known for sure; `trusted` says it comes from this server's own scheduling, which only ever sends
/// what the organizer or attendee really did. Returns whether a calendar changed.
async fn apply(
    ctx: &Context,
    account: &Account,
    message: &Component,
    from: Option<&str>,
    trusted: bool,
) -> Result<bool, StoreError> {
    let Some(uid) = itip::uid(message) else { return Ok(false) };
    let own = own_addresses(ctx, account).await;
    let from = from.map(str::to_lowercase);
    let existing = ctx.store.own_calendar_event_by_uid(account.id, &uid).await?;
    let current = existing.as_ref().and_then(|record| Component::parse(&record.content));
    match itip::method(message).as_deref() {
        Some("REQUEST") => {
            // Only for our own addresses, from the event's organizer.
            if !matches!(itip::role(message, &own), Role::Attendee(_)) {
                return Ok(false);
            }
            let organizer = itip::organizer(message);
            if let Some(current) = &current {
                if itip::organizer(current) != organizer {
                    return Ok(false);
                }
                if itip::sequence(message) < itip::sequence(current) {
                    return Ok(false);
                }
            }
            // From outside, an update of an existing event needs the organizer's own word.
            if !trusted && current.is_some() && from.is_none_or(|from| Some(from) != organizer) {
                return Ok(false);
            }
            let copy = itip::attendee_copy(message, current.as_ref(), &own);
            write(ctx, account, existing.as_ref(), &copy, false).await?;
            Ok(true)
        }
        Some("REPLY") => {
            let (Some(record), Some(mut current)) = (existing, current) else { return Ok(false) };
            if !matches!(itip::role(&current, &own), Role::Organizer(_)) {
                return Ok(false);
            }
            // Only the attendee whose address sent it answers, and only for themselves.
            let Some(from) = from else { return Ok(false) };
            if !itip::attendees(&current).iter().any(|a| a.address == from) {
                return Ok(false);
            }
            if !itip::apply_reply(&mut current, message, &from) {
                return Ok(false);
            }
            write(ctx, account, Some(&record), &current, true).await?;
            Ok(true)
        }
        Some("CANCEL") => {
            let (Some(record), Some(mut current)) = (existing, current) else { return Ok(false) };
            let organizer = itip::organizer(&current);
            if organizer.is_none() || itip::organizer(message) != organizer {
                return Ok(false);
            }
            if !trusted && from != organizer {
                return Ok(false);
            }
            if !itip::apply_cancel(&mut current, message) {
                return Ok(false);
            }
            write(ctx, account, Some(&record), &current, false).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Stores what scheduling made of an event: over the existing one, or new in the default calendar.
async fn write(
    ctx: &Context,
    account: &Account,
    existing: Option<&uwumail_store::CalendarEventRecord>,
    calendar: &Component,
    keep_schedule_tag: bool,
) -> Result<(), StoreError> {
    let content = calendar.to_ics();
    let checked = uwumail_store::ical::check_calendar(&content, &[])
        .map_err(|refused| StoreError::Invalid(format!("the scheduled event is not valid: {refused:?}")))?;
    if checked.component != "VEVENT" {
        return Err(StoreError::Invalid("only events are scheduled".into()));
    }
    let calendar_id = match existing {
        Some(record) => record.calendar_id,
        None => default_calendar(ctx, account.id).await?,
    };
    let write = CalendarEventWrite {
        id: existing.map(|record| record.id),
        calendar_id,
        content,
        uid: checked.uid,
        starts_at: checked.starts_at,
        ends_at: checked.ends_at,
        if_etag: existing.map(|record| record.etag.clone()),
        keep_schedule_tag,
    };
    ctx.store.put_calendar_event(account.id, write).await.map(|_| ())
}

/// The calendar invitations go into: the default one, made if there is none yet.
async fn default_calendar(ctx: &Context, account_id: i64) -> Result<i64, StoreError> {
    let name = ctx.tone().language.collection_names().0;
    let calendars =
        ctx.store.dav_collections(account_id, DavKind::Calendar, NewDavCollection::default_calendar(name)).await?;
    let holds_events =
        |c: &&uwumail_store::DavCollection| c.components.is_empty() || c.components.iter().any(|k| k == "VEVENT");
    calendars
        .iter()
        .filter(holds_events)
        .find(|c| c.is_default)
        .or_else(|| calendars.iter().find(holds_events))
        .map(|c| c.id)
        .ok_or_else(|| StoreError::NotFound("a calendar for events".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calendar_part_is_found() {
        let mail = "From: gast@example.com\r\nTo: mini@example.org\r\nSubject: x\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=b\r\n\r\n--b\r\nContent-Type: text/plain\r\n\r\nHallo\r\n--b\r\n\
Content-Type: text/calendar; method=REPLY; charset=utf-8\r\n\r\nBEGIN:VCALENDAR\r\nMETHOD:REPLY\r\nEND:VCALENDAR\r\n--b--\r\n";
        assert!(find_itip(mail.as_bytes()).unwrap().contains("METHOD:REPLY"));
        assert!(find_itip(b"From: a@example.com\r\n\r\nno calendar here\r\n").is_none());
    }
}
