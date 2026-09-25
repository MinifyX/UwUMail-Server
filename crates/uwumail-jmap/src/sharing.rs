//! Folders others share with the account (docs/sharing.md): each person who shares something is
//! an account of their own in the session (`isPersonal: false`, id `a<their account>`), and the
//! mail methods work there within the rights they gave (RFC 4314 letters in the store, shown as
//! RFC 8621 `myRights` and RFC 9670 `shareWith`). People on the server are Principals
//! (`urn:ietf:params:jmap:principals`, id `p<account>`).
//!
//! A call for a shared account runs with the owner's account in [`Ctx::account`], so every method
//! reads and writes the owner's data, and [`Ctx::shared`] saying what the caller may see and do.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value, json};
use uwumail_store::{Account, ShareLevel, Store, has_rights, normalize_rights};

use crate::error::{MethodError, MethodResult};
use crate::ids;
use crate::methods::Ctx;

pub const PRINCIPALS: &str = "urn:ietf:params:jmap:principals";
pub const PRINCIPALS_OWNER: &str = "urn:ietf:params:jmap:principals:owner";

/// The methods that work on someone else's account.
const SHARED_METHODS: &[&str] = &[
    "Mailbox/get",
    "Mailbox/changes",
    "Mailbox/query",
    "Mailbox/set",
    "Email/get",
    "Email/changes",
    "Email/query",
    "Email/set",
    "Email/import",
    "Email/parse",
    "Thread/get",
    "Thread/changes",
    "SearchSnippet/get",
];

/// What the logged-in account may see and do in someone else's account.
pub struct SharedView {
    /// The logged-in account, while [`Ctx::account`] is the owner's.
    pub me: Account,
    /// Rights per mailbox of the owner that is shared with `me`.
    pub rights: HashMap<i64, String>,
}

impl SharedView {
    pub fn rights_of(&self, mailbox: i64) -> &str {
        self.rights.get(&mailbox).map_or("", String::as_str)
    }

    pub fn may(&self, mailbox: i64, needed: &str) -> bool {
        self.rights.get(&mailbox).is_some_and(|rights| has_rights(rights, needed))
    }

    /// Mailboxes shown in the account.
    pub fn visible(&self, mailbox: i64) -> bool {
        self.rights.get(&mailbox).is_some_and(|rights| rights.contains('l') || rights.contains('r'))
    }

    /// Mailboxes whose messages may be read.
    pub fn readable(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.rights.iter().filter(|(_, r)| r.contains('r')).map(|(id, _)| *id).collect();
        ids.sort_unstable();
        ids
    }

    /// Whether an email in these mailboxes may be read.
    pub fn may_read_email(&self, mailbox_ids: &[i64]) -> bool {
        mailbox_ids.iter().any(|mailbox| self.may(*mailbox, "r"))
    }

    /// Whether a keyword may be set or cleared in a mailbox: `$seen` needs `s`, `$deleted` `t`,
    /// every other keyword `w`.
    pub fn may_change_keyword(&self, mailbox_ids: &[i64], keyword: &str) -> bool {
        let needed = match keyword {
            "$seen" => "s",
            "$deleted" => "t",
            _ => "w",
        };
        mailbox_ids.iter().any(|mailbox| self.may(*mailbox, needed))
    }
}

/// Moves a call for someone else's account into that account. Returns whether it did; the
/// caller runs the method and then calls [`leave`].
pub async fn enter(ctx: &mut Ctx<'_>, name: &str, args: &Value) -> MethodResult<bool> {
    if ctx.shared.is_some() {
        return Ok(false);
    }
    let Some(requested) = args.get("accountId").and_then(Value::as_str) else {
        return Ok(false);
    };
    if requested == ctx.account_id() {
        return Ok(false);
    }
    let Some(owner) = ids::parse('a', requested) else {
        return Ok(false);
    };
    let store = &ctx.jmap.store;
    let shared: Vec<_> =
        store.mailboxes_shared_with(ctx.account.id).await?.into_iter().filter(|m| m.owner_id == owner).collect();
    if shared.is_empty() {
        // Not shared with us: the ordinary check answers accountNotFound.
        return Ok(false);
    }
    if !SHARED_METHODS.contains(&name) {
        return Err(MethodError::new("accountNotSupportedByMethod", format!("{name} works on your own account only")));
    }
    let Some(owner_account) = store.account_by_id(owner).await? else {
        return Ok(false);
    };
    let rights = shared.into_iter().map(|m| (m.mailbox.id, m.rights)).collect();
    let me = std::mem::replace(&mut ctx.account, owner_account);
    ctx.shared = Some(SharedView { me, rights });
    Ok(true)
}

/// Back to the logged-in account after a call [`enter`] moved.
pub fn leave(ctx: &mut Ctx<'_>) {
    if let Some(view) = ctx.shared.take() {
        ctx.account = view.me;
    }
}

pub fn principal_id(account_id: i64) -> String {
    format!("p{account_id}")
}

/// RFC 8621 MailboxRights for a set of rights, with RFC 9670's `mayAdmin`.
pub fn rights_json(rights: &str, is_inbox: bool, may_submit: bool) -> Value {
    json!({
        "mayReadItems": rights.contains('r'),
        "mayAddItems": rights.contains('i'),
        "mayRemoveItems": has_rights(rights, "te"),
        "maySetSeen": rights.contains('s'),
        "maySetKeywords": rights.contains('w'),
        "mayCreateChild": rights.contains('k'),
        "mayRename": rights.contains('x') && !is_inbox,
        "mayDelete": rights.contains('x') && !is_inbox,
        "maySubmit": may_submit,
        "mayAdmin": rights.contains('a'),
    })
}

/// Rights from a `shareWith` value: a MailboxRights object (missing means false), or one of the
/// levels `"read"`, `"write"` and `"all"` as a shortcut. Empty means not shared.
pub fn rights_from_json(value: &Value) -> Result<String, String> {
    match value {
        Value::Null => Ok(String::new()),
        Value::String(level) => ShareLevel::parse(level)
            .map(|level| level.rights().to_owned())
            .ok_or_else(|| format!("unknown level {level:?}, expected read, write or all")),
        Value::Object(map) => {
            let mut letters = String::new();
            for (key, value) in map {
                let Some(on) = value.as_bool() else {
                    return Err(format!("{key} must be true or false"));
                };
                if !on {
                    continue;
                }
                letters.push_str(match key.as_str() {
                    "mayReadItems" => "lr",
                    "mayAddItems" => "i",
                    "mayRemoveItems" => "te",
                    "maySetSeen" => "s",
                    "maySetKeywords" => "w",
                    "mayCreateChild" => "k",
                    "mayRename" | "mayDelete" => "x",
                    "mayAdmin" => "a",
                    // Sending from someone else's account is not part of sharing folders.
                    "maySubmit" => "",
                    other => return Err(format!("unknown right {other}")),
                });
            }
            if !letters.is_empty() {
                letters.push('l');
            }
            normalize_rights(&letters).map_err(|err| err.to_string())
        }
        _ => Err("a right set must be an object, a level or null".into()),
    }
}

/// `shareWith` of every mailbox of an owner: principal id to rights.
pub async fn share_with(store: &Store, owner: i64) -> MethodResult<HashMap<i64, Map<String, Value>>> {
    let mut by_mailbox: HashMap<i64, Map<String, Value>> = HashMap::new();
    for entry in store.shares_by_owner(owner).await? {
        by_mailbox
            .entry(entry.mailbox_id)
            .or_default()
            .insert(principal_id(entry.grantee_id), rights_json(&entry.rights, false, false));
    }
    Ok(by_mailbox)
}

/// Leaves only what the caller may see in a /changes answer for a shared account: what went out
/// of sight counts as destroyed.
pub async fn filter_changes(
    ctx: &Ctx<'_>,
    kind: &str,
    created: &mut Vec<i64>,
    updated: &mut Vec<i64>,
    destroyed: &mut Vec<i64>,
) -> MethodResult<()> {
    let Some(view) = ctx.shared.as_ref() else {
        return Ok(());
    };
    let seen: HashSet<i64> = match kind {
        "Mailbox" => created.iter().chain(updated.iter()).copied().filter(|id| view.visible(*id)).collect(),
        "Email" => {
            let ids: Vec<i64> = created.iter().chain(updated.iter()).copied().collect();
            ctx.jmap
                .store
                .emails_by_ids(ctx.account.id, ids)
                .await?
                .into_iter()
                .filter(|record| view.may_read_email(&record.mailbox_ids))
                .map(|record| record.id)
                .collect()
        }
        "Thread" => {
            let ids: Vec<i64> = created.iter().chain(updated.iter()).copied().collect();
            let threads = ctx.jmap.store.thread_emails(ctx.account.id, ids).await?;
            let mut visible = HashSet::new();
            for (thread, emails) in threads {
                let records = ctx.jmap.store.emails_by_ids(ctx.account.id, emails).await?;
                if records.iter().any(|record| view.may_read_email(&record.mailbox_ids)) {
                    visible.insert(thread);
                }
            }
            visible
        }
        _ => HashSet::new(),
    };
    for list in [&mut *created, &mut *updated] {
        let (keep, gone): (Vec<i64>, Vec<i64>) = list.iter().partition(|id| seen.contains(id));
        *list = keep;
        destroyed.extend(gone);
    }
    destroyed.sort_unstable();
    destroyed.dedup();
    Ok(())
}

/// The accounts others share with `me`: owner, login, and whether the rights are only to read.
pub async fn shared_accounts(store: &Store, me: i64) -> Vec<(i64, String, bool)> {
    let Ok(shared) = store.mailboxes_shared_with(me).await else {
        return Vec::new();
    };
    let mut accounts: Vec<(i64, String, bool)> = Vec::new();
    for entry in shared {
        let read_only = !entry.rights.chars().any(|right| "switexk".contains(right));
        match accounts.iter_mut().find(|(owner, _, _)| *owner == entry.owner_id) {
            Some(account) => account.2 &= read_only,
            None => accounts.push((entry.owner_id, entry.owner_login, read_only)),
        }
    }
    accounts
}

/// What the session state adds for the shared accounts, so it changes when they do.
pub fn state_suffix(accounts: &[(i64, String, bool)]) -> String {
    accounts.iter().map(|(owner, _, read_only)| format!("-s{owner}{}", if *read_only { "r" } else { "w" })).collect()
}

/// Adds the shared accounts and the principals capability to a session document.
pub fn add_to_session(document: &mut Value, me: &Account, accounts: &[(i64, String, bool)]) {
    let own = ids::account(me.id);
    document["capabilities"][PRINCIPALS] = json!({});
    document["accounts"][&own]["accountCapabilities"][PRINCIPALS] =
        json!({ "currentUserPrincipalId": principal_id(me.id) });
    document["accounts"][&own]["accountCapabilities"][PRINCIPALS_OWNER] =
        json!({ "accountIdForPrincipal": own, "principalId": principal_id(me.id) });
    document["primaryAccounts"][PRINCIPALS] = json!(own);
    let mail = document["accounts"][&own]["accountCapabilities"][crate::session::MAIL].clone();
    for (owner, login, read_only) in accounts {
        let mut capabilities = mail.clone();
        capabilities["mayCreateTopLevelMailbox"] = json!(false);
        document["accounts"][ids::account(*owner)] = json!({
            "name": login,
            "isPersonal": false,
            "isReadOnly": read_only,
            "accountCapabilities": {
                crate::session::MAIL: capabilities,
                PRINCIPALS_OWNER: { "accountIdForPrincipal": ids::account(*owner), "principalId": principal_id(*owner) }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rights_round_trip() {
        let all = rights_json(uwumail_store::ALL_RIGHTS, false, false);
        // Everything but `p`, which nothing here needs.
        assert_eq!(rights_from_json(&all).unwrap(), "lrswikxtea");
        assert_eq!(rights_from_json(&json!({ "mayReadItems": true })).unwrap(), "lr");
        assert_eq!(rights_from_json(&json!({ "mayReadItems": false })).unwrap(), "");
        assert_eq!(rights_from_json(&json!("write")).unwrap(), ShareLevel::Write.rights());
        assert!(rights_from_json(&json!({ "mayFly": true })).is_err());
        assert!(rights_from_json(&json!("some")).is_err());
        assert_eq!(rights_json("lr", false, false)["mayReadItems"], json!(true));
        assert_eq!(rights_json("lr", false, false)["maySetSeen"], json!(false));
    }
}
