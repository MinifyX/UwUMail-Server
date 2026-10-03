//! Signatures per domain (docs/signatures.md).
//!
//! A person writes one signature for all their addresses of a domain, or one for every domain
//! (`*`); a single address may have its own (the identity's override). An admin may give a domain
//! a company signature: a template for people without their own, or a footer the server appends
//! to every message sent from the domain.
//!
//! What a sending identity uses, first match wins: its own signature, the person's signature for
//! its domain, the person's signature for every domain, the company template. JMAP's
//! `Identity/get` hands out that effective signature with its placeholders filled, so every mail
//! program keeps working without knowing about domains.

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

use crate::db::{next_modseq, record_change};
use crate::extras::IDENTITY_SIGNATURE_MAX_BYTES;
use crate::{Result, Store, StoreError, normalize_domain, now};

/// The key of a person's signature for every domain.
pub const SIGNATURE_ALL_DOMAINS: &str = "*";
/// The most signatures one change may set or remove.
pub const MAX_SIGNATURE_CHANGES: usize = 500;

/// A signature as text and as HTML. Either may be empty.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SignatureText {
    pub text: String,
    pub html: String,
}

impl SignatureText {
    pub fn new(text: impl Into<String>, html: impl Into<String>) -> SignatureText {
        SignatureText { text: text.into(), html: html.into() }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty() && self.html.trim().is_empty()
    }

    /// Fails when the text or the HTML is over [`IDENTITY_SIGNATURE_MAX_BYTES`].
    pub fn check_size(&self) -> Result<()> {
        if self.text.len() > IDENTITY_SIGNATURE_MAX_BYTES || self.html.len() > IDENTITY_SIGNATURE_MAX_BYTES {
            return Err(StoreError::Invalid(format!(
                "a signature may take at most {IDENTITY_SIGNATURE_MAX_BYTES} bytes"
            )));
        }
        Ok(())
    }

    /// Both parts with the placeholders filled for one sender (see [`fill_placeholders`]).
    pub fn filled(&self, name: &str, email: &str) -> SignatureText {
        SignatureText {
            text: fill_placeholders(&self.text, name, email, false),
            html: fill_placeholders(&self.html, name, email, true),
        }
    }
}

/// How a domain's company signature is used.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CompanySignatureMode {
    /// Not used.
    #[default]
    Off,
    /// Offered to people without a signature of their own; they may change it.
    Template,
    /// Appended by the server to every message sent from the domain.
    Footer,
}

impl CompanySignatureMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CompanySignatureMode::Off => "off",
            CompanySignatureMode::Template => "template",
            CompanySignatureMode::Footer => "footer",
        }
    }

    pub fn parse(value: &str) -> Option<CompanySignatureMode> {
        match value {
            "off" => Some(CompanySignatureMode::Off),
            "template" => Some(CompanySignatureMode::Template),
            "footer" => Some(CompanySignatureMode::Footer),
            _ => None,
        }
    }
}

/// A domain's company signature, set by an admin.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CompanySignature {
    pub mode: CompanySignatureMode,
    pub text: String,
    pub html: String,
}

impl CompanySignature {
    fn signature(&self) -> SignatureText {
        SignatureText::new(self.text.clone(), self.html.clone())
    }
}

/// Where an identity's effective signature comes from.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SignatureSource {
    /// Its own.
    Identity,
    /// The person's signature for its domain.
    Domain,
    /// The person's signature for every domain.
    AllDomains,
    /// The company template of its domain.
    Company,
    /// None at all.
    #[default]
    None,
}

/// One of a person's domains in the overview.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DomainSignatureInfo {
    pub domain: String,
    /// How many sending addresses the person has on it.
    pub address_count: usize,
    /// The person's own signature for the domain.
    pub signature: Option<SignatureText>,
    /// The company signature, when the admin uses one.
    pub company: Option<CompanySignature>,
    /// What addresses of the domain without their own signature use.
    pub source: SignatureSource,
}

/// One sending address in the overview.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IdentitySignatureInfo {
    pub id: i64,
    pub name: String,
    pub email: String,
    pub domain: String,
    /// Its own signature, when it has one (placeholders as written).
    pub signature: Option<SignatureText>,
    /// What it sends with, placeholders filled.
    pub effective: SignatureText,
    pub source: SignatureSource,
}

/// Everything the signature settings show.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SignatureOverview {
    pub state: String,
    /// The person's signature for every domain.
    pub all_domains: Option<SignatureText>,
    pub domains: Vec<DomainSignatureInfo>,
    pub identities: Vec<IdentitySignatureInfo>,
}

/// A change to a person's signatures; `None` removes one.
#[derive(Debug, Clone, Default)]
pub struct SignatureChanges {
    /// By domain, or [`SIGNATURE_ALL_DOMAINS`].
    pub domains: Vec<(String, Option<SignatureText>)>,
    /// By identity id: its own signature, or back to the domain's.
    pub identities: Vec<(i64, Option<SignatureText>)>,
}

/// The placeholders a signature may use, German and English names alike.
pub const PLACEHOLDERS: &[&str] = &["name", "adresse", "address", "email", "domain"];

/// `text` with `{name}`, `{adresse}` (`{address}`, `{email}`) and `{domain}` filled for one sender.
/// Unknown braces stay as they are. For HTML the values are escaped: a name is whatever its owner
/// typed and must never become markup.
pub fn fill_placeholders(text: &str, name: &str, email: &str, html: bool) -> String {
    if !text.contains('{') {
        return text.to_owned();
    }
    let domain = email.rsplit_once('@').map(|(_, domain)| domain).unwrap_or_default();
    let mut out = String::with_capacity(text.len() + 32);
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let value = after.find('}').filter(|close| *close <= 16).and_then(|close| {
            let key = after[..close].trim().to_ascii_lowercase();
            let value = match key.as_str() {
                "name" => name,
                "adresse" | "address" | "email" | "e-mail" => email,
                "domain" => domain,
                _ => return None,
            };
            Some((value, close))
        });
        match value {
            Some((value, close)) => {
                if html {
                    out.push_str(&escape_html(value));
                } else {
                    out.push_str(value);
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Escapes text for HTML content and attribute values.
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// The lower-case domain of an address, empty without one.
pub(crate) fn domain_of(email: &str) -> String {
    email.rsplit_once('@').map(|(_, domain)| domain.to_ascii_lowercase()).unwrap_or_default()
}

/// What resolving an account's signatures needs, read once.
pub(crate) struct SignatureContext {
    pub(crate) display_name: String,
    user: HashMap<String, SignatureText>,
    company: HashMap<String, CompanySignature>,
}

impl SignatureContext {
    pub(crate) fn load(conn: &Connection, account_id: i64) -> Result<SignatureContext> {
        let display_name: String = conn
            .query_row("SELECT display_name FROM accounts WHERE id = ?1", [account_id], |row| row.get(0))
            .optional()?
            .unwrap_or_default();
        let mut user = HashMap::new();
        let mut stmt =
            conn.prepare("SELECT domain, text_signature, html_signature FROM user_signatures WHERE account_id = ?1")?;
        for row in stmt.query_map([account_id], |row| {
            Ok((row.get::<_, String>(0)?, SignatureText::new(row.get::<_, String>(1)?, row.get::<_, String>(2)?)))
        })? {
            let (domain, signature) = row?;
            user.insert(domain, signature);
        }
        let mut company = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT d.name, s.mode, s.text_signature, s.html_signature FROM domain_signatures s
             JOIN domains d ON d.id = s.domain_id
             WHERE s.mode <> 'off' AND d.name IN (
                 SELECT lower(substr(email, instr(email, '@') + 1)) FROM identities WHERE account_id = ?1)",
        )?;
        for row in stmt.query_map([account_id], company_from_row)? {
            let (domain, signature) = row?;
            company.insert(domain, signature);
        }
        Ok(SignatureContext { display_name, user, company })
    }

    pub(crate) fn company(&self, domain: &str) -> Option<&CompanySignature> {
        self.company.get(domain)
    }

    /// The signature addresses of `domain` use without one of their own, as written.
    pub(crate) fn for_domain(&self, domain: &str) -> (SignatureText, SignatureSource) {
        if let Some(own) = self.user.get(domain) {
            return (own.clone(), SignatureSource::Domain);
        }
        if let Some(all) = self.user.get(SIGNATURE_ALL_DOMAINS) {
            return (all.clone(), SignatureSource::AllDomains);
        }
        match self.company.get(domain) {
            Some(company) if company.mode == CompanySignatureMode::Template => {
                (company.signature(), SignatureSource::Company)
            }
            _ => (SignatureText::default(), SignatureSource::None),
        }
    }

    /// An identity's signature as written, and where it comes from.
    pub(crate) fn effective(&self, email: &str, own: Option<&SignatureText>) -> (SignatureText, SignatureSource) {
        match own {
            Some(own) => (own.clone(), SignatureSource::Identity),
            None => self.for_domain(&domain_of(email)),
        }
    }

    /// The name `{name}` stands for: the identity's, else the account's.
    pub(crate) fn name_for<'a>(&'a self, identity_name: &'a str) -> &'a str {
        if identity_name.trim().is_empty() { &self.display_name } else { identity_name }
    }
}

fn company_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, CompanySignature)> {
    let mode: String = row.get(1)?;
    Ok((
        row.get(0)?,
        CompanySignature {
            mode: CompanySignatureMode::parse(&mode).unwrap_or_default(),
            text: row.get(2)?,
            html: row.get(3)?,
        },
    ))
}

/// Records an `Identity` change, once each, for every identity of the account on one of `domains`
/// (all of them when it holds [`SIGNATURE_ALL_DOMAINS`]), so mail programs fetch their new
/// effective signature. Identities in `skip` are recorded by the caller.
fn touch_identities(conn: &Connection, account_id: i64, modseq: i64, domains: &[&str], skip: &[i64]) -> Result<()> {
    if domains.is_empty() {
        return Ok(());
    }
    let all = domains.contains(&SIGNATURE_ALL_DOMAINS);
    let identities: Vec<(i64, String)> = conn
        .prepare("SELECT id, lower(substr(email, instr(email, '@') + 1)) FROM identities WHERE account_id = ?1")?
        .query_map(params![account_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (id, domain) in identities {
        if skip.contains(&id) || !(all || domains.contains(&domain.as_str())) {
            continue;
        }
        conn.execute("UPDATE identities SET updated_modseq = ?1 WHERE id = ?2", params![modseq, id])?;
        record_change(conn, account_id, modseq, "Identity", id, "updated")?;
    }
    Ok(())
}

fn state(conn: &Connection, account_id: i64) -> Result<String> {
    Ok(conn
        .query_row("SELECT modseq FROM accounts WHERE id = ?1", [account_id], |row| row.get::<_, i64>(0))?
        .to_string())
}

impl Store {
    /// Everything the signature settings of an account show.
    pub async fn signature_overview(&self, account_id: i64) -> Result<SignatureOverview> {
        // Makes sure the default identities exist.
        let identities = self.identities(account_id).await?;
        self.read(move |conn| {
            let context = SignatureContext::load(conn, account_id)?;
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for identity in &identities {
                let domain = domain_of(&identity.email);
                if !domain.is_empty() {
                    *counts.entry(domain).or_default() += 1;
                }
            }
            let domains = counts
                .into_iter()
                .map(|(domain, address_count)| {
                    let (_, source) = context.for_domain(&domain);
                    DomainSignatureInfo {
                        signature: context.user.get(&domain).cloned(),
                        company: context.company(&domain).cloned(),
                        source,
                        address_count,
                        domain,
                    }
                })
                .collect();
            let identities = identities
                .into_iter()
                .map(|identity| IdentitySignatureInfo {
                    domain: domain_of(&identity.email),
                    effective: SignatureText::new(identity.text_signature, identity.html_signature),
                    signature: identity.signature_override,
                    source: identity.signature_source,
                    id: identity.id,
                    name: identity.name,
                    email: identity.email,
                })
                .collect();
            Ok(SignatureOverview {
                state: state(conn, account_id)?,
                all_domains: context.user.get(SIGNATURE_ALL_DOMAINS).cloned(),
                domains,
                identities,
            })
        })
        .await
    }

    /// Sets or removes signatures of an account: per domain, for every domain, per address. All
    /// or nothing: one bad entry and nothing changes.
    pub async fn set_signatures(&self, account_id: i64, changes: SignatureChanges) -> Result<()> {
        if changes.domains.len() + changes.identities.len() > MAX_SIGNATURE_CHANGES {
            return Err(StoreError::Invalid(format!("at most {MAX_SIGNATURE_CHANGES} signatures at once")));
        }
        let mut domains = Vec::with_capacity(changes.domains.len());
        for (domain, signature) in changes.domains {
            if let Some(signature) = &signature {
                signature.check_size()?;
            }
            let key = if domain.trim() == SIGNATURE_ALL_DOMAINS {
                SIGNATURE_ALL_DOMAINS.to_owned()
            } else {
                normalize_domain(&domain)?
            };
            // `*`, ` *` and `Example.ORG`, `example.org` name the same entry: the last one counts,
            // and each is written once (security review 0.22 SIG-2).
            domains.retain(|(known, _): &(String, Option<SignatureText>)| *known != key);
            domains.push((key, signature));
        }
        let mut identities: Vec<(i64, Option<SignatureText>)> = Vec::with_capacity(changes.identities.len());
        for (id, signature) in changes.identities {
            if let Some(signature) = &signature {
                signature.check_size()?;
            }
            identities.retain(|(known, _)| *known != id);
            identities.push((id, signature));
        }
        self.identities(account_id).await?;
        let modseq = self
            .write(move |tx| {
                let own_domains: Vec<String> = tx
                    .prepare(
                        "SELECT DISTINCT lower(substr(email, instr(email, '@') + 1)) FROM identities WHERE account_id = ?1",
                    )?
                    .query_map([account_id], |row| row.get(0))?
                    .collect::<Result<_, _>>()?;
                for (domain, _) in &domains {
                    if domain != SIGNATURE_ALL_DOMAINS && !own_domains.contains(domain) {
                        return Err(StoreError::Invalid(format!("{domain} is not a domain of your addresses")));
                    }
                }
                for (id, _) in &identities {
                    let exists: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM identities WHERE id = ?1 AND account_id = ?2)",
                        params![id, account_id],
                        |row| row.get(0),
                    )?;
                    if !exists {
                        return Err(StoreError::NotFound(format!("identity {id}")));
                    }
                }
                let modseq = next_modseq(tx, account_id)?;
                for (domain, signature) in &domains {
                    match signature {
                        Some(signature) => tx.execute(
                            "INSERT INTO user_signatures (account_id, domain, text_signature, html_signature, updated_at)
                             VALUES (?1, ?2, ?3, ?4, ?5)
                             ON CONFLICT (account_id, domain) DO UPDATE SET text_signature = excluded.text_signature,
                                 html_signature = excluded.html_signature, updated_at = excluded.updated_at",
                            params![account_id, domain, signature.text, signature.html, now()],
                        )?,
                        None => tx.execute(
                            "DELETE FROM user_signatures WHERE account_id = ?1 AND domain = ?2",
                            params![account_id, domain],
                        )?,
                    };
                }
                // Every identity the change reaches is recorded once, however many entries name it.
                let scopes: Vec<&str> = domains.iter().map(|(domain, _)| domain.as_str()).collect();
                let explicit: Vec<i64> = identities.iter().map(|(id, _)| *id).collect();
                touch_identities(tx, account_id, modseq, &scopes, &explicit)?;
                for (id, signature) in &identities {
                    let (text, html, on) = match signature {
                        Some(signature) => (signature.text.as_str(), signature.html.as_str(), true),
                        None => ("", "", false),
                    };
                    tx.execute(
                        "UPDATE identities SET text_signature = ?1, html_signature = ?2, signature_override = ?3,
                             updated_modseq = ?4 WHERE id = ?5",
                        params![text, html, on, modseq, id],
                    )?;
                    record_change(tx, account_id, modseq, "Identity", *id, "updated")?;
                }
                Ok(modseq)
            })
            .await?;
        self.notify_change(account_id, modseq);
        Ok(())
    }

    /// The company signature of a domain; off and empty when it has none.
    pub async fn domain_signature(&self, domain: &str) -> Result<CompanySignature> {
        let domain = normalize_domain(domain)?;
        self.read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT d.name, s.mode, s.text_signature, s.html_signature FROM domain_signatures s
                     JOIN domains d ON d.id = s.domain_id WHERE d.name = ?1",
                    [domain],
                    company_from_row,
                )
                .optional()?
                .map(|(_, signature)| signature)
                .unwrap_or_default())
        })
        .await
    }

    /// The footer the server appends to mail from `domain`, when its admin wants one.
    pub async fn company_footer(&self, domain: &str) -> Result<Option<SignatureText>> {
        let Ok(domain) = normalize_domain(domain) else { return Ok(None) };
        let signature = self.domain_signature(&domain).await?;
        let footer = signature.signature();
        Ok((signature.mode == CompanySignatureMode::Footer && !footer.is_empty()).then_some(footer))
    }

    /// Sets a domain's company signature. Everyone with an address on the domain gets an
    /// `Identity` change, as their effective signature may have changed with it.
    pub async fn set_domain_signature(&self, domain: &str, signature: CompanySignature) -> Result<()> {
        let domain = normalize_domain(domain)?;
        SignatureText::new(signature.text.clone(), signature.html.clone()).check_size()?;
        let changed = self
            .write(move |tx| {
                let domain_id: i64 = tx
                    .query_row("SELECT id FROM domains WHERE name = ?1", [&domain], |row| row.get(0))
                    .optional()?
                    .ok_or_else(|| StoreError::NotFound(format!("domain {domain}")))?;
                tx.execute(
                    "INSERT INTO domain_signatures (domain_id, mode, text_signature, html_signature, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (domain_id) DO UPDATE SET mode = excluded.mode, text_signature = excluded.text_signature,
                         html_signature = excluded.html_signature, updated_at = excluded.updated_at",
                    params![domain_id, signature.mode.as_str(), signature.text, signature.html, now()],
                )?;
                let accounts: Vec<i64> = tx
                    .prepare(
                        "SELECT DISTINCT account_id FROM identities
                         WHERE lower(substr(email, instr(email, '@') + 1)) = ?1",
                    )?
                    .query_map([&domain], |row| row.get(0))?
                    .collect::<Result<_, _>>()?;
                let mut changed = Vec::with_capacity(accounts.len());
                for account_id in accounts {
                    let modseq = next_modseq(tx, account_id)?;
                    touch_identities(tx, account_id, modseq, &[domain.as_str()], &[])?;
                    changed.push((account_id, modseq));
                }
                Ok(changed)
            })
            .await?;
        for (account_id, modseq) in changed {
            self.notify_change(account_id, modseq);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::store;
    use crate::{IdentityUpdate, NewAccount, Role};

    #[test]
    fn placeholders_are_filled_and_escaped_in_html() {
        let name = "Mini <b>&</b> \"Co\"";
        let text = fill_placeholders(
            "{name}\n{adresse} | {address} | {domain} | {E-Mail} | {unknown} | {",
            name,
            "mini@example.org",
            false,
        );
        assert_eq!(
            text,
            "Mini <b>&</b> \"Co\"\nmini@example.org | mini@example.org | example.org | mini@example.org | {unknown} | {"
        );
        let html = fill_placeholders(
            "<p>{name}</p><a href=\"mailto:{adresse}\">{ Domain }</a>",
            name,
            "mini@example.org",
            true,
        );
        assert_eq!(
            html,
            "<p>Mini &lt;b&gt;&amp;&lt;/b&gt; &quot;Co&quot;</p><a href=\"mailto:mini@example.org\">example.org</a>"
        );
        // Hostile input: no panic on odd braces or multi-byte text, no endless loop.
        assert_eq!(fill_placeholders("ü{{name}}ö{", "Ä", "a@b.example", false), "ü{Ä}ö{");
        assert_eq!(fill_placeholders("{name", "x", "a@b.example", true), "{name");
        assert_eq!(fill_placeholders(&"{".repeat(1000), "x", "a@b.example", true).len(), 1000);
    }

    async fn setup() -> (Store, tempfile::TempDir, i64) {
        let (store, dir) = store().await;
        store.create_domain("example.org").await.unwrap();
        store.create_domain("example.net").await.unwrap();
        let account = store
            .create_account(NewAccount {
                address: "mini@example.org".into(),
                display_name: "Mini".into(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        store.add_alias("info@example.org", "mini@example.org").await.unwrap();
        store.add_alias("mini@example.net", "mini@example.org").await.unwrap();
        (store, dir, account.id)
    }

    fn by_email<'a>(overview: &'a SignatureOverview, email: &str) -> &'a IdentitySignatureInfo {
        overview.identities.iter().find(|i| i.email == email).unwrap()
    }

    #[tokio::test]
    async fn the_identity_beats_the_domain_beats_all_domains_beats_the_template() {
        let (store, _dir, mini) = setup().await;
        let overview = store.signature_overview(mini).await.unwrap();
        assert_eq!(overview.identities.len(), 3);
        let org = overview.domains.iter().find(|d| d.domain == "example.org").unwrap();
        assert_eq!(org.address_count, 2);
        assert_eq!(org.source, SignatureSource::None);

        store
            .set_domain_signature(
                "example.org",
                CompanySignature {
                    mode: CompanySignatureMode::Template,
                    text: "Firma {name}".into(),
                    html: String::new(),
                },
            )
            .await
            .unwrap();
        let overview = store.signature_overview(mini).await.unwrap();
        assert_eq!(by_email(&overview, "info@example.org").effective.text, "Firma Mini");
        assert_eq!(by_email(&overview, "info@example.org").source, SignatureSource::Company);
        assert_eq!(by_email(&overview, "mini@example.net").source, SignatureSource::None);

        let all = SignatureText::new("Alle {adresse}", "<p>Alle {name}</p>");
        store
            .set_signatures(
                mini,
                SignatureChanges { domains: vec![("*".into(), Some(all.clone()))], ..Default::default() },
            )
            .await
            .unwrap();
        let overview = store.signature_overview(mini).await.unwrap();
        assert_eq!(overview.all_domains, Some(all));
        assert_eq!(by_email(&overview, "mini@example.net").effective.text, "Alle mini@example.net");
        assert_eq!(by_email(&overview, "info@example.org").source, SignatureSource::AllDomains);

        let domain = SignatureText::new("Org {domain}", "");
        store
            .set_signatures(
                mini,
                SignatureChanges { domains: vec![("Example.ORG".into(), Some(domain))], ..Default::default() },
            )
            .await
            .unwrap();
        let overview = store.signature_overview(mini).await.unwrap();
        assert_eq!(by_email(&overview, "info@example.org").effective.text, "Org example.org");
        assert_eq!(by_email(&overview, "mini@example.net").source, SignatureSource::AllDomains);

        let info = by_email(&overview, "info@example.org").id;
        store
            .set_signatures(
                mini,
                SignatureChanges { identities: vec![(info, Some(SignatureText::new("", "")))], ..Default::default() },
            )
            .await
            .unwrap();
        let overview = store.signature_overview(mini).await.unwrap();
        assert_eq!(by_email(&overview, "info@example.org").source, SignatureSource::Identity);
        assert_eq!(by_email(&overview, "info@example.org").effective.text, "", "explicitly none");
        assert_eq!(by_email(&overview, "mini@example.org").effective.text, "Org example.org");

        // Back to the domain's, and the domain's removed falls back to all domains.
        store
            .set_signatures(
                mini,
                SignatureChanges { identities: vec![(info, None)], domains: vec![("example.org".into(), None)] },
            )
            .await
            .unwrap();
        let overview = store.signature_overview(mini).await.unwrap();
        assert_eq!(by_email(&overview, "info@example.org").source, SignatureSource::AllDomains);
        // JMAP's view is the effective one.
        let identities = store.identities(mini).await.unwrap();
        let info = identities.iter().find(|i| i.email == "info@example.org").unwrap();
        assert_eq!(info.text_signature, "Alle info@example.org");
        assert_eq!(info.html_signature, "<p>Alle Mini</p>");
    }

    #[tokio::test]
    async fn a_footer_is_no_template_and_changes_are_validated() {
        let (store, _dir, mini) = setup().await;
        let footer =
            CompanySignature { mode: CompanySignatureMode::Footer, text: "Footer".into(), html: String::new() };
        store.set_domain_signature("example.org", footer.clone()).await.unwrap();
        assert_eq!(store.domain_signature("example.org").await.unwrap(), footer);
        assert_eq!(store.company_footer("EXAMPLE.org").await.unwrap(), Some(SignatureText::new("Footer", "")));
        assert_eq!(store.company_footer("example.net").await.unwrap(), None);
        let overview = store.signature_overview(mini).await.unwrap();
        assert_eq!(by_email(&overview, "info@example.org").source, SignatureSource::None);
        assert!(store.set_domain_signature("unknown.example", footer).await.is_err());

        // Not a domain of hers, too big, someone else's identity, too many: all refused.
        let one = |domain: &str, text: String| SignatureChanges {
            domains: vec![(domain.into(), Some(SignatureText::new(text, "")))],
            ..Default::default()
        };
        assert!(matches!(
            store.set_signatures(mini, one("example.com", "x".into())).await,
            Err(StoreError::Invalid(_))
        ));
        let huge = "x".repeat(IDENTITY_SIGNATURE_MAX_BYTES + 1);
        assert!(matches!(store.set_signatures(mini, one("example.org", huge)).await, Err(StoreError::Invalid(_))));
        let other = store
            .create_account(NewAccount {
                address: "leni@example.net".into(),
                display_name: "Leni".into(),
                password: None,
                role: Role::User,
                quota_bytes: 0,
                protocols: None,
            })
            .await
            .unwrap();
        let leni_identity = store.identities(other.id).await.unwrap()[0].id;
        let theirs = SignatureChanges {
            identities: vec![(leni_identity, Some(SignatureText::default()))],
            ..Default::default()
        };
        assert!(matches!(store.set_signatures(mini, theirs).await, Err(StoreError::NotFound(_))));
        let many = SignatureChanges {
            domains: (0..=MAX_SIGNATURE_CHANGES).map(|_| ("example.org".to_owned(), None)).collect(),
            ..Default::default()
        };
        assert!(store.set_signatures(mini, many).await.is_err());
        assert!(store.signature_overview(mini).await.unwrap().domains.iter().all(|d| d.signature.is_none()));
    }

    #[tokio::test]
    async fn writing_back_the_effective_signature_keeps_following_the_domain() {
        let (store, _dir, mini) = setup().await;
        store
            .set_signatures(
                mini,
                SignatureChanges {
                    domains: vec![("example.org".into(), Some(SignatureText::new("Org {name}", "")))],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let identity =
            store.identities(mini).await.unwrap().into_iter().find(|i| i.email == "info@example.org").unwrap();
        // A mail program saving all identities sends the same text back: no override.
        let same = IdentityUpdate { text_signature: Some(identity.text_signature.clone()), ..Default::default() };
        store.update_identity(mini, identity.id, same).await.unwrap();
        let after = store.identities(mini).await.unwrap().into_iter().find(|i| i.id == identity.id).unwrap();
        assert_eq!(after.signature_source, SignatureSource::Domain);
        // A different one becomes its own; the HTML part follows from what it had.
        let own = IdentityUpdate { text_signature: Some("Own".into()), ..Default::default() };
        store.update_identity(mini, identity.id, own).await.unwrap();
        let after = store.identities(mini).await.unwrap().into_iter().find(|i| i.id == identity.id).unwrap();
        assert_eq!(after.signature_source, SignatureSource::Identity);
        assert_eq!(after.text_signature, "Own");
        assert_eq!(after.signature_override, Some(SignatureText::new("Own", "")));
    }

    #[tokio::test]
    async fn duplicate_entries_count_once_and_identities_are_touched_once() {
        let (store, _dir, mini) = setup().await;
        // `*`, ` *`, `*\t` and differently cased domains name the same entry: the last one wins.
        let text = |t: &str| Some(SignatureText::new(t, ""));
        let mut domains: Vec<(String, Option<SignatureText>)> =
            (0..200).flat_map(|_| [("*".to_owned(), text("a")), (" *".to_owned(), text("b"))]).collect();
        domains.push(("*\t".into(), text("Alle")));
        domains.push(("Example.ORG".into(), text("x")));
        domains.push(("example.org ".into(), text("Org")));
        let info = store.identities(mini).await.unwrap().into_iter().find(|i| i.email == "info@example.org").unwrap();
        let identities = vec![(info.id, text("first")), (info.id, None)];
        let before = store.changes(mini, "Identity", 0, 1000).await.unwrap().new_state;
        store.set_signatures(mini, SignatureChanges { domains, identities }).await.unwrap();
        let overview = store.signature_overview(mini).await.unwrap();
        assert_eq!(overview.all_domains, Some(SignatureText::new("Alle", "")));
        assert_eq!(by_email(&overview, "mini@example.org").effective.text, "Org");
        assert_eq!(by_email(&overview, "info@example.org").source, SignatureSource::Domain, "the last entry wins");
        let changes = store.changes(mini, "Identity", before, 1000).await.unwrap();
        assert_eq!(changes.updated.len(), 3, "every identity is reported");

        // However many entries name an identity, it is written once (security review 0.22 SIG-2).
        let deltas = store
            .write(move |tx| {
                let mut deltas = Vec::new();
                for (round, scopes) in [
                    &["*"][..],
                    &["*", "example.org", "example.net", "*"],
                    &["example.org"],
                    &["example.org", "example.org"],
                ]
                .into_iter()
                .enumerate()
                {
                    let start = tx.total_changes();
                    touch_identities(tx, mini, 1_000_000 + round as i64, scopes, &[])?;
                    deltas.push(tx.total_changes() - start);
                }
                Ok(deltas)
            })
            .await
            .unwrap();
        assert_eq!(deltas[0], deltas[1]);
        assert_eq!(deltas[2], deltas[3]);
        assert!(deltas[2] < deltas[0] && deltas[2] > 0);
    }

    #[tokio::test]
    async fn an_account_has_a_bounded_number_of_identities() {
        let (store, _dir, mini) = setup().await;
        store
            .write(move |tx| {
                for i in 0..crate::extras::MAX_IDENTITIES_PER_ACCOUNT {
                    tx.execute(
                        "INSERT INTO identities (account_id, name, email, created_modseq, updated_modseq)
                         VALUES (?1, 'x', ?2, 1, 1)",
                        params![mini, format!("mini+{i}@example.org")],
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
        let refused = store.create_identity(mini, "Noch eine", "mini+more@example.org").await;
        assert!(matches!(refused, Err(StoreError::Rule { code: "overQuota", .. })), "{refused:?}");
    }
}
