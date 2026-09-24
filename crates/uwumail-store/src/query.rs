//! Email search and sorting, translated to SQL.

use rusqlite::types::Value;

use crate::{Result, Store};

#[derive(Debug, Clone, PartialEq)]
pub enum EmailFilter {
    And(Vec<EmailFilter>),
    Or(Vec<EmailFilter>),
    Not(Vec<EmailFilter>),
    InMailbox(i64),
    InMailboxOtherThan(Vec<i64>),
    Before(i64),
    After(i64),
    MinSize(i64),
    MaxSize(i64),
    HasKeyword(String),
    NotKeyword(String),
    AllInThreadHaveKeyword(String),
    SomeInThreadHaveKeyword(String),
    NoneInThreadHaveKeyword(String),
    HasAttachment(bool),
    /// Subject, addresses and body.
    Text(String),
    From(String),
    To(String),
    Cc(String),
    Bcc(String),
    Subject(String),
    Body(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmailSortProperty {
    ReceivedAt,
    SentAt,
    Size,
    From,
    To,
    Subject,
    HasKeyword(String),
    AllInThreadHaveKeyword(String),
    SomeInThreadHaveKeyword(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmailSort {
    pub property: EmailSortProperty,
    pub ascending: bool,
}

/// FTS5 query that matches every word, quoted so user input cannot use FTS syntax.
fn fts_terms(text: &str, column: Option<&str>) -> Option<String> {
    let terms: Vec<String> = text
        .split_whitespace()
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        return None;
    }
    let joined = terms.join(" ");
    Some(match column {
        Some(column) => format!("{{{column}}} : ({joined})"),
        None => joined,
    })
}

struct Sql {
    params: Vec<Value>,
}

impl Sql {
    fn bind(&mut self, value: impl Into<Value>) -> String {
        self.params.push(value.into());
        format!("?{}", self.params.len())
    }

    fn filter(&mut self, filter: &EmailFilter) -> String {
        use EmailFilter as F;
        let keyword_exists = |sql: &mut Sql, keyword: &str, email: &str| {
            let p = sql.bind(keyword.to_lowercase());
            format!("EXISTS (SELECT 1 FROM email_keywords k WHERE k.email_id = {email} AND k.keyword = {p})")
        };
        let contains = |sql: &mut Sql, column: &str, needle: &str| {
            let p = sql.bind(needle.to_lowercase());
            format!("instr(lower(e.{column}), {p}) > 0")
        };
        match filter {
            F::And(items) | F::Or(items) | F::Not(items) if items.is_empty() => match filter {
                F::Or(_) => "0".into(),
                _ => "1".into(),
            },
            F::And(items) => format!("({})", items.iter().map(|f| self.filter(f)).collect::<Vec<_>>().join(" AND ")),
            F::Or(items) => format!("({})", items.iter().map(|f| self.filter(f)).collect::<Vec<_>>().join(" OR ")),
            F::Not(items) => {
                format!("NOT ({})", items.iter().map(|f| self.filter(f)).collect::<Vec<_>>().join(" OR "))
            }
            F::InMailbox(id) => {
                let p = self.bind(*id);
                format!("EXISTS (SELECT 1 FROM email_mailboxes m WHERE m.email_id = e.id AND m.mailbox_id = {p})")
            }
            F::InMailboxOtherThan(ids) => {
                let p = self.bind(serde_json::to_string(ids).unwrap_or_else(|_| "[]".into()));
                format!(
                    "EXISTS (SELECT 1 FROM email_mailboxes m WHERE m.email_id = e.id
                     AND m.mailbox_id NOT IN (SELECT value FROM json_each({p})))"
                )
            }
            F::Before(time) => format!("e.received_at < {}", self.bind(*time)),
            F::After(time) => format!("e.received_at >= {}", self.bind(*time)),
            F::MinSize(size) => format!("e.size >= {}", self.bind(*size)),
            F::MaxSize(size) => format!("e.size < {}", self.bind(*size)),
            F::HasKeyword(keyword) => keyword_exists(self, keyword, "e.id"),
            F::NotKeyword(keyword) => format!("NOT {}", keyword_exists(self, keyword, "e.id")),
            F::AllInThreadHaveKeyword(keyword) => {
                let inner = keyword_exists(self, keyword, "t.id");
                format!("NOT EXISTS (SELECT 1 FROM emails t WHERE t.thread_id = e.thread_id AND NOT {inner})")
            }
            F::SomeInThreadHaveKeyword(keyword) => {
                let inner = keyword_exists(self, keyword, "t.id");
                format!("EXISTS (SELECT 1 FROM emails t WHERE t.thread_id = e.thread_id AND {inner})")
            }
            F::NoneInThreadHaveKeyword(keyword) => {
                let inner = keyword_exists(self, keyword, "t.id");
                format!("NOT EXISTS (SELECT 1 FROM emails t WHERE t.thread_id = e.thread_id AND {inner})")
            }
            F::HasAttachment(has) => format!("e.has_attachment = {}", self.bind(*has)),
            F::Text(text) | F::Body(text) => {
                let column = matches!(filter, F::Body(_)).then_some("body");
                match fts_terms(text, column) {
                    Some(query) => {
                        let p = self.bind(query);
                        format!("e.id IN (SELECT rowid FROM email_fts WHERE email_fts MATCH {p})")
                    }
                    None => "1".into(),
                }
            }
            F::From(text) => contains(self, "from_addr", text),
            F::To(text) => contains(self, "to_addr", text),
            F::Cc(text) => contains(self, "cc_addr", text),
            F::Bcc(text) => contains(self, "bcc_addr", text),
            F::Subject(text) => contains(self, "subject", text),
        }
    }

    fn sort_expression(&mut self, property: &EmailSortProperty) -> String {
        use EmailSortProperty as S;
        let first_address = |column: &str| {
            format!(
                "lower(coalesce(json_extract(e.{column}, '$[0].name'), json_extract(e.{column}, '$[0].email'), ''))"
            )
        };
        match property {
            S::ReceivedAt => "e.received_at".into(),
            S::SentAt => "coalesce(e.sent_at, e.received_at)".into(),
            S::Size => "e.size".into(),
            S::From => first_address("from_addr"),
            S::To => first_address("to_addr"),
            S::Subject => "lower(e.subject)".into(),
            S::HasKeyword(keyword) => {
                let p = self.bind(keyword.to_lowercase());
                format!("EXISTS (SELECT 1 FROM email_keywords k WHERE k.email_id = e.id AND k.keyword = {p})")
            }
            S::AllInThreadHaveKeyword(keyword) => {
                let p = self.bind(keyword.to_lowercase());
                format!(
                    "NOT EXISTS (SELECT 1 FROM emails t WHERE t.thread_id = e.thread_id AND NOT EXISTS
                     (SELECT 1 FROM email_keywords k WHERE k.email_id = t.id AND k.keyword = {p}))"
                )
            }
            S::SomeInThreadHaveKeyword(keyword) => {
                let p = self.bind(keyword.to_lowercase());
                format!(
                    "EXISTS (SELECT 1 FROM emails t JOIN email_keywords k ON k.email_id = t.id
                     WHERE t.thread_id = e.thread_id AND k.keyword = {p})"
                )
            }
        }
    }
}

impl Store {
    /// Ids of matching emails in order, each with its thread id. With `collapse_threads`,
    /// only the first email of each thread (in sort order) is returned.
    pub async fn query_emails(
        &self,
        account_id: i64,
        filter: Option<EmailFilter>,
        sort: Vec<EmailSort>,
        collapse_threads: bool,
    ) -> Result<Vec<(i64, i64)>> {
        self.read(move |conn| {
            let mut sql = Sql { params: Vec::new() };
            let account = sql.bind(account_id);
            let condition = filter.as_ref().map(|f| sql.filter(f)).unwrap_or_else(|| "1".into());
            let mut sort = sort;
            if sort.is_empty() {
                sort.push(EmailSort { property: EmailSortProperty::ReceivedAt, ascending: false });
            }
            let mut columns = Vec::new();
            let mut order = Vec::new();
            for (index, item) in sort.iter().enumerate() {
                columns.push(format!("{} AS s{index}", sql.sort_expression(&item.property)));
                order.push(format!("s{index} {}", if item.ascending { "ASC" } else { "DESC" }));
            }
            let last_direction = if sort[0].ascending { "ASC" } else { "DESC" };
            order.push(format!("id {last_direction}"));
            let order = order.join(", ");
            let columns = columns.join(", ");
            let base = format!(
                "SELECT e.id AS id, e.thread_id AS thread_id, {columns} FROM emails e
                 WHERE e.account_id = {account} AND {condition}"
            );
            let statement = if collapse_threads {
                format!(
                    "SELECT id, thread_id FROM (SELECT *, row_number() OVER (PARTITION BY thread_id ORDER BY {order}) AS rank
                     FROM ({base})) WHERE rank = 1 ORDER BY {order}"
                )
            } else {
                format!("SELECT id, thread_id FROM ({base}) ORDER BY {order}")
            };
            let mut stmt = conn.prepare(&statement)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(sql.params.iter()), |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{IngestRequest, MailboxRole, MailboxTarget, NewAccount, Role};

    #[tokio::test]
    async fn filters_sorts_and_collapses() {
        let (store, _dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        let account = store
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
        let inbox = store
            .mailboxes(account)
            .await
            .unwrap()
            .into_iter()
            .find(|m| m.role == Some(MailboxRole::Inbox))
            .unwrap()
            .id;
        let mut ids = Vec::new();
        for (index, (subject, from, keywords, role, references)) in [
            ("Katzenfutter", "Nyu <nyu@x.example>", vec!["$seen"], MailboxRole::Inbox, ""),
            ("Re: Katzenfutter", "Ami <ami@x.example>", vec![], MailboxRole::Inbox, "m0@x"),
            ("Rechnung", "Shop <shop@y.example>", vec!["$flagged"], MailboxRole::Archive, ""),
        ]
        .into_iter()
        .enumerate()
        {
            let refs = if references.is_empty() { String::new() } else { format!("References: <{references}>\r\n") };
            let raw = format!(
                "From: {from}\r\nSubject: {subject}\r\nMessage-ID: <m{index}@x>\r\n{refs}\r\nThunfisch {index}\r\n"
            );
            ids.push(
                store
                    .ingest(IngestRequest {
                        account_id: account,
                        raw: raw.into_bytes(),
                        mailboxes: vec![MailboxTarget::Role(role)],
                        keywords: keywords.into_iter().map(String::from).collect(),
                        received_at: Some(100 + index as i64),
                    })
                    .await
                    .unwrap()
                    .id,
            );
        }
        let q = |filter: Option<EmailFilter>, sort: Vec<EmailSort>, collapse: bool| {
            let store = store.clone();
            async move {
                store
                    .query_emails(account, filter, sort, collapse)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|(id, _)| id)
                    .collect::<Vec<_>>()
            }
        };

        assert_eq!(q(None, vec![], false).await, vec![ids[2], ids[1], ids[0]]);
        assert_eq!(q(Some(EmailFilter::InMailbox(inbox)), vec![], false).await, vec![ids[1], ids[0]]);
        assert_eq!(q(Some(EmailFilter::InMailbox(inbox)), vec![], true).await, vec![ids[1]]);
        assert_eq!(q(Some(EmailFilter::NotKeyword("$SEEN".into())), vec![], false).await, vec![ids[2], ids[1]]);
        assert_eq!(q(Some(EmailFilter::From("SHOP".into())), vec![], false).await, vec![ids[2]]);
        assert_eq!(q(Some(EmailFilter::Text("thunfisch".into())), vec![], false).await.len(), 3);
        assert_eq!(q(Some(EmailFilter::Body("\"\" OR".into())), vec![], false).await.len(), 0);
        let not_in_inbox = EmailFilter::Not(vec![EmailFilter::InMailbox(inbox)]);
        assert_eq!(q(Some(not_in_inbox), vec![], false).await, vec![ids[2]]);
        let or = EmailFilter::Or(vec![EmailFilter::HasKeyword("$flagged".into()), EmailFilter::Subject("re:".into())]);
        assert_eq!(q(Some(or), vec![], false).await, vec![ids[2], ids[1]]);
        let by_subject = vec![EmailSort { property: EmailSortProperty::Subject, ascending: true }];
        assert_eq!(q(None, by_subject, false).await, vec![ids[0], ids[1], ids[2]]);
        let unread_thread = EmailFilter::SomeInThreadHaveKeyword("$seen".into());
        assert_eq!(q(Some(unread_thread), vec![], false).await, vec![ids[1], ids[0]]);
    }
}
