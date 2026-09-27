//! Identities that come with a membership: people who may send as a group or a shared mailbox
//! find its address among their sending identities, and lose it again with the right.

use rusqlite::{Connection, params};

use crate::db::{next_modseq, record_change};
use crate::extras::owns;
use crate::{Result, Store};

/// Accounts whose identities changed in a write, with their new change number, to announce after
/// the write is committed.
#[derive(Debug, Default)]
pub(crate) struct Granted(Vec<(i64, i64)>);

impl Granted {
    /// Another account whose state moved on in the same write.
    pub(crate) fn push(&mut self, account_id: i64, modseq: i64) {
        self.0.push((account_id, modseq));
    }
}

/// Adds an identity for `email` to an account that has identities already. One that has none yet
/// gets it with its defaults, the first time they are read.
pub(crate) fn grant_identity(
    conn: &Connection,
    account_id: i64,
    email: &str,
    name: &str,
    granted: &mut Granted,
) -> Result<()> {
    let (count, has): (i64, bool) = conn.query_row(
        "SELECT count(*), coalesce(sum(email = ?2), 0) > 0 FROM identities WHERE account_id = ?1",
        params![account_id, email],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if count == 0 || has {
        return Ok(());
    }
    let name = if name.trim().is_empty() {
        conn.query_row("SELECT display_name FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))?
    } else {
        name.trim().to_owned()
    };
    let modseq = next_modseq(conn, account_id)?;
    conn.execute(
        "INSERT INTO identities (account_id, name, email, created_modseq, updated_modseq) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![account_id, name, email, modseq],
    )?;
    record_change(conn, account_id, modseq, "Identity", conn.last_insert_rowid(), "created")?;
    granted.push(account_id, modseq);
    Ok(())
}

/// Removes the identities for `email` from an account that may no longer send as it.
pub(crate) fn revoke_identities(conn: &Connection, account_id: i64, email: &str, granted: &mut Granted) -> Result<()> {
    if owns(conn, account_id, email)? {
        return Ok(());
    }
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM identities WHERE account_id = ?1 AND email = ?2")?
        .query_map(params![account_id, email], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    if ids.is_empty() {
        return Ok(());
    }
    let modseq = next_modseq(conn, account_id)?;
    for id in ids {
        conn.execute("DELETE FROM identities WHERE id = ?1", [id])?;
        record_change(conn, account_id, modseq, "Identity", id, "destroyed")?;
    }
    granted.push(account_id, modseq);
    Ok(())
}

/// The addresses an account may send as because it belongs to a group or a shared mailbox, with
/// the name its identity starts with.
pub(crate) fn granted_addresses(conn: &Connection, account_id: i64) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT g.local_part || '@' || d.name, g.name FROM groups g
         JOIN group_members m ON m.group_id = g.id JOIN domains d ON d.id = g.domain_id
         WHERE m.account_id = ?1 AND g.members_may_send_as = 1
         UNION ALL
         SELECT a.local_part || '@' || d.name, acc.display_name FROM shared_mailbox_members s
         JOIN accounts acc ON acc.id = s.account_id AND acc.deleted_at IS NULL
         JOIN addresses a ON a.account_id = s.account_id JOIN domains d ON d.id = a.domain_id
         WHERE s.member_id = ?1 AND s.may_send = 1",
    )?;
    let rows = stmt.query_map([account_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

impl Store {
    pub(crate) fn notify_granted(&self, granted: Granted) {
        for (account, modseq) in granted.0 {
            self.notify_change(account, modseq);
        }
    }
}
