//! Subscribed calendars: fetching a feed and making its calendar hold what the feed holds, every
//! so often, by a worker of the server (docs/calendar-import.md).

use std::time::Duration;

use tokio::sync::watch;
use uwumail_smtp::egress::{Egress, Purpose};
use uwumail_store::{CalendarSubscription, DavMirrorReport, Store, decode_text, split_ics};

use super::{HttpsTransport, Remote, RemoteError, Transport};

/// The most a feed may be. A year of a busy calendar is a few megabytes.
const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;
/// Entries of one feed.
const MAX_FEED_ENTRIES: usize = 20_000;
const TICK: Duration = Duration::from_secs(60);
/// Feeds per tick, one after the other.
const PER_TICK: usize = 20;
/// One feed, fetched and mirrored.
const RUN_LIMIT: Duration = Duration::from_secs(120);

/// What a feed answered: its text, or `None` when it did not change since the validator.
#[derive(Debug, Clone)]
pub struct FeedFetch {
    pub text: Option<String>,
    /// `etag:"…"` or `modified:…`, to ask with next time.
    pub validator: Option<String>,
}

/// The address a feed is fetched from: `webcal://` and `webcals://`, as calendar pages link
/// them, are HTTPS.
pub fn feed_url(input: &str) -> Result<String, RemoteError> {
    let input = input.trim();
    let lower = input.to_ascii_lowercase();
    let url = if let Some(rest) = lower.strip_prefix("webcals://").or_else(|| lower.strip_prefix("webcal://")) {
        format!("https://{}", &input[input.len() - rest.len()..])
    } else {
        input.to_owned()
    };
    super::checked_url(&url).map(|url| url.to_string())
}

/// Fetches a feed, asking only for news when there is a validator.
pub async fn fetch_feed(
    transport: &dyn Transport,
    url: &str,
    validator: Option<&str>,
) -> Result<FeedFetch, RemoteError> {
    let mut remote = Remote::anonymous(transport);
    let mut headers = vec![("accept", "text/calendar, */*;q=0.5".to_owned())];
    match validator.and_then(|v| v.split_once(':')) {
        Some(("etag", etag)) => headers.push(("if-none-match", etag.to_owned())),
        Some(("modified", date)) => headers.push(("if-modified-since", date.to_owned())),
        _ => {}
    }
    let reply = remote.request(axum::http::Method::GET, url, &headers, None, MAX_FEED_BYTES).await?;
    match reply.answer.status {
        304 => Ok(FeedFetch { text: None, validator: validator.map(str::to_owned) }),
        200 => {
            let header =
                |name: &str| reply.answer.headers.get(name).and_then(|value| value.to_str().ok()).map(str::to_owned);
            let validator = header("etag")
                .map(|etag| format!("etag:{etag}"))
                .or_else(|| header("last-modified").map(|date| format!("modified:{date}")));
            Ok(FeedFetch { text: Some(decode_text(&reply.answer.body)), validator })
        }
        status => Err(RemoteError::Status(status)),
    }
}

/// Fetches a subscription's feed and mirrors it. A feed that answers with anything but a
/// calendar, such as an error page, leaves the calendar as it was.
pub async fn refresh_subscription(
    store: &Store,
    transport: &dyn Transport,
    subscription: &CalendarSubscription,
) -> Result<Option<DavMirrorReport>, RemoteError> {
    let url = store
        .calendar_subscription_url(subscription.account_id, subscription.id)
        .await
        .map_err(|_| RemoteError::NotFound)?;
    let fetched = fetch_feed(transport, &url, subscription.validator.as_deref()).await?;
    let Some(text) = fetched.text else {
        let _ =
            store.note_subscription_run(subscription.id, Ok((fetched.validator, subscription.entries as usize))).await;
        return Ok(None);
    };
    let split = split_ics(&text, !subscription.keep_alarms);
    if !split.recognized {
        return Err(RemoteError::NotICalendar);
    }
    if split.objects.len() > MAX_FEED_ENTRIES {
        return Err(RemoteError::TooLarge);
    }
    let report = store
        .dav_mirror(subscription.account_id, subscription.collection_id, split.objects)
        .await
        .map_err(|_| RemoteError::NotFound)?;
    let _ = store.note_subscription_run(subscription.id, Ok((fetched.validator, report.entries))).await;
    Ok(Some(report))
}

/// Refreshes every subscription whose turn it is, until `shutdown` changes. Requests leave the way
/// fetched mailboxes do, through the VPN when that is set for them.
pub async fn run_subscriptions(store: Store, egress: Egress, mut shutdown: watch::Receiver<bool>) {
    loop {
        let due = match store.calendar_subscriptions_due(PER_TICK).await {
            Ok(due) => due,
            Err(err) => {
                tracing::warn!(%err, "reading the calendar subscriptions failed");
                Vec::new()
            }
        };
        if !due.is_empty() {
            let transport = HttpsTransport::new(&egress.dialer(Purpose::Fetch));
            for subscription in due {
                if *shutdown.borrow() {
                    return;
                }
                let run = tokio::time::timeout(RUN_LIMIT, refresh_subscription(&store, &transport, &subscription));
                let failed = match run.await {
                    Ok(Ok(report)) => {
                        if let Some(report) = report {
                            tracing::debug!(
                                subscription = subscription.id,
                                source = %subscription.source,
                                created = report.created,
                                updated = report.updated,
                                deleted = report.deleted,
                                "calendar subscription refreshed"
                            );
                        }
                        None
                    }
                    Ok(Err(err)) => Some(err),
                    Err(_) => Some(RemoteError::Timeout),
                };
                if let Some(err) = failed {
                    tracing::info!(subscription = subscription.id, source = %subscription.source, %err, "a calendar feed failed");
                    let _ = store.note_subscription_run(subscription.id, Err(err.code().to_owned())).await;
                }
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = shutdown.changed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;
    use uwumail_store::{DavKind, NewAccount, NewCalendarSubscription, NewDavCollection, Role};

    use super::super::testing::RouterTransport;
    use super::*;

    const FEED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:ferien-1\r\nDTSTAMP:20260101T000000Z\r\n\
DTSTART;VALUE=DATE:20261012\r\nSUMMARY:Herbstferien\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn webcal_is_https() {
        assert_eq!(
            feed_url(" webcal://Calendar.example.org/feed.ics").unwrap(),
            "https://calendar.example.org/feed.ics"
        );
        assert_eq!(feed_url("webcals://calendar.example.org/x").unwrap(), "https://calendar.example.org/x");
        assert!(feed_url("http://calendar.example.org/x").is_err());
        assert!(feed_url("webcal://127.0.0.1/x").is_err());
    }

    #[tokio::test]
    async fn feeds_are_mirrored_and_errors_leave_them_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).await.unwrap();
        store.create_domain("example.org").await.unwrap();
        let mini = store
            .create_account(NewAccount {
                address: "mini@example.org".into(),
                display_name: String::new(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap()
            .id;
        let (calendar, subscription) = store
            .create_calendar_subscription(
                mini,
                NewCalendarSubscription {
                    collection: NewDavCollection {
                        slug: "ferien".into(),
                        display_name: "Ferien".into(),
                        ..Default::default()
                    },
                    url: "https://feeds.example.net/ferien.ics".into(),
                    interval_secs: 3600,
                    keep_alarms: false,
                },
                NewDavCollection::default_calendar("Kalender"),
            )
            .await
            .unwrap();
        let router = Router::new()
            .route(
                "/ferien.ics",
                get(|headers: HeaderMap| async move {
                    if headers.get("if-none-match").is_some_and(|v| v == "\"v1\"") {
                        return (StatusCode::NOT_MODIFIED, [("etag", "\"v1\"")], String::new());
                    }
                    (StatusCode::OK, [("etag", "\"v1\"")], FEED.to_owned())
                }),
            )
            .route("/broken.ics", get(|| async { "<html>Wartungsarbeiten</html>" }));
        let transport = RouterTransport::new(router);

        let report = refresh_subscription(&store, &transport, &subscription).await.unwrap().unwrap();
        assert_eq!((report.created, report.entries), (1, 1));
        let subscription = store.calendar_subscription(mini, subscription.id).await.unwrap();
        assert_eq!(subscription.validator.as_deref(), Some("etag:\"v1\""));
        assert!(refresh_subscription(&store, &transport, &subscription).await.unwrap().is_none(), "not modified");

        store
            .update_calendar_subscription(
                mini,
                subscription.id,
                uwumail_store::CalendarSubscriptionUpdate {
                    url: Some("https://feeds.example.net/broken.ics".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let subscription = store.calendar_subscription(mini, subscription.id).await.unwrap();
        let err = refresh_subscription(&store, &transport, &subscription).await.unwrap_err();
        assert_eq!(err, RemoteError::NotICalendar);
        let kept = store.dav_collection(mini, DavKind::Calendar, "ferien").await.unwrap().unwrap();
        assert_eq!((kept.id, kept.resources), (calendar.id, 1), "an error page does not empty the calendar");
    }
}
