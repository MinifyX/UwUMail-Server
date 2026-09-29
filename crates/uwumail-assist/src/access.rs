//! Who may use which provider for what, and with which model: the admin's policy and providers,
//! the person's own providers and choices, the daily quotas. Also setting providers up and asking
//! them for their models.

use std::collections::BTreeMap;
use std::net::IpAddr;

use http_body_util::BodyExt;
use hyper::Request;
use hyper::header::{ACCEPT, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use url::{Host, Url};
use uwumail_smtp::egress::{Reach, is_local_network};
use uwumail_store::{
    ASSIST_MAX_ACCESS_ENTRIES, ASSIST_MAX_LABELS, ASSIST_MAX_PERSONAL_PROVIDERS, Account, AssistFeatures, AssistPolicy,
    AssistProviderRecord, AssistProviderWrite, SecretChange, StoreError, normalize_domain,
};

use crate::chatgpt::{self, Poll, Tokens};
use crate::kinds::{self, BaseUrl, Key, KindInfo, Shape};
use crate::llm::{self, Completion, Prompt, ProviderError, Target};
use crate::{Assist, AssistError, FEATURES, MAX_INSTRUCTION_CHARS, MAX_TEXT_CHARS, Result, now};

const NAME_MAX_CHARS: usize = 60;
const MODEL_MAX_CHARS: usize = 200;
const URL_MAX_CHARS: usize = 500;
const KEY_MAX_CHARS: usize = 4096;
/// Models a provider's list is cut to.
const MAX_MODELS: usize = 500;
/// The features that use the fast model unless told otherwise.
fn uses_fast_model(feature: &str) -> bool {
    feature != "compose"
}

/// Deserializes a field that may be absent (`None`), `null` (`Some(None)`) or a value.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

/// What the capability says about the person.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capability {
    pub features: AssistFeatures,
    pub may_add_providers: bool,
    pub may_use_private_addresses: bool,
    pub max_providers: usize,
    pub max_labels: usize,
    pub max_instruction_chars: usize,
    pub max_text_chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Quota {
    pub requests_per_day: Option<i64>,
    pub tokens_per_day: Option<i64>,
}

/// A provider as a person sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub scope: &'static str,
    pub base_url: Option<String>,
    pub has_key: bool,
    pub key_hint: Option<String>,
    pub model: Option<String>,
    pub fast_model: Option<String>,
    pub features: Vec<String>,
    pub quota: Option<Quota>,
    pub experimental: bool,
    pub connected: bool,
}

/// A server provider as the admin sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminProviderView {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub base_url: Option<String>,
    pub has_key: bool,
    pub key_hint: Option<String>,
    pub model: Option<String>,
    pub fast_model: Option<String>,
    pub enabled: bool,
    pub access: String,
    pub domains: Vec<String>,
    pub people: Vec<String>,
    pub features: Vec<String>,
    pub requests_per_day: Option<i64>,
    pub tokens_per_day: Option<i64>,
    pub created_at: i64,
}

/// A new or changed provider, as the portal and JMAP send it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInput {
    pub name: Option<String>,
    pub kind: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub base_url: Option<Option<String>>,
    /// Omitted: keep; `""`: remove.
    pub api_key: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub model: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub fast_model: Option<Option<String>>,
    pub enabled: Option<bool>,
    pub access: Option<String>,
    pub domains: Option<Vec<String>>,
    pub people: Option<Vec<String>>,
    pub features: Option<Vec<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub requests_per_day: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    pub tokens_per_day: Option<Option<i64>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Choice {
    pub provider_id: i64,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Effective {
    pub provider_id: i64,
    pub provider_name: String,
    pub model: String,
    pub scope: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub default: Option<Choice>,
    pub features: BTreeMap<String, Option<Choice>>,
    pub auto_labels: bool,
    pub refine_events: bool,
    pub effective: BTreeMap<String, Option<Effective>>,
    /// The JMAP state of the person's assist objects.
    #[serde(skip)]
    pub state: String,
}

/// A change to the settings; what is left out stays.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    #[serde(default, deserialize_with = "nullable")]
    pub default: Option<Option<Choice>>,
    pub features: Option<BTreeMap<String, Option<Choice>>>,
    pub auto_labels: Option<bool>,
    pub refine_events: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodayUsage {
    pub provider_id: i64,
    pub provider_name: String,
    pub requests: i64,
    pub tokens: i64,
    pub requests_per_day: Option<i64>,
    pub tokens_per_day: Option<i64>,
}

/// A provider a person may use, with what for.
#[derive(Debug, Clone)]
pub(crate) struct Available {
    pub record: AssistProviderRecord,
    pub info: &'static KindInfo,
    pub server: bool,
    pub features: Vec<String>,
    pub usable: bool,
}

impl Available {
    fn scope(&self) -> &'static str {
        if self.server { "server" } else { "personal" }
    }

    /// The model for `feature`: the chosen one, the provider's, or the kind's suggestion.
    fn model_for(&self, feature: &str, chosen: Option<&str>) -> Option<String> {
        let chosen = chosen.map(str::trim).filter(|m| !m.is_empty()).map(str::to_owned);
        let provider = if uses_fast_model(feature) {
            self.record.fast_model.clone().or_else(|| self.record.model.clone())
        } else {
            self.record.model.clone()
        };
        let preset = if uses_fast_model(feature) { self.info.fast_model.or(self.info.model) } else { self.info.model };
        chosen.or(provider).or_else(|| preset.map(str::to_owned))
    }

    fn view(&self) -> ProviderView {
        let record = &self.record;
        let quota = (self.server && (record.requests_per_day.is_some() || record.tokens_per_day.is_some()))
            .then_some(Quota { requests_per_day: record.requests_per_day, tokens_per_day: record.tokens_per_day });
        ProviderView {
            id: record.id,
            name: record.name.clone(),
            kind: record.kind.clone(),
            scope: self.scope(),
            base_url: if self.server { None } else { record.base_url.clone() },
            has_key: record.has_secret && self.info.key != Key::Login,
            key_hint: if self.info.key == Key::Login { None } else { record.key_hint.clone() },
            model: record.model.clone(),
            fast_model: record.fast_model.clone(),
            features: self.features.clone(),
            quota,
            experimental: self.info.experimental,
            connected: self.usable,
        }
    }
}

fn admin_view(record: &AssistProviderRecord) -> AdminProviderView {
    let (domains, people) = match record.access.as_str() {
        "domains" => (record.access_list.clone(), Vec::new()),
        "people" => (Vec::new(), record.access_list.clone()),
        _ => (Vec::new(), Vec::new()),
    };
    AdminProviderView {
        id: record.id,
        name: record.name.clone(),
        kind: record.kind.clone(),
        base_url: record.base_url.clone(),
        has_key: record.has_secret,
        key_hint: record.key_hint.clone(),
        model: record.model.clone(),
        fast_model: record.fast_model.clone(),
        enabled: record.enabled,
        access: record.access.clone(),
        domains,
        people,
        features: record.features.clone(),
        requests_per_day: record.requests_per_day,
        tokens_per_day: record.tokens_per_day,
        created_at: record.created_at,
    }
}

fn domain_of(login: &str) -> String {
    login.rsplit_once('@').map(|(_, domain)| domain.to_ascii_lowercase()).unwrap_or_default()
}

fn allowed_for(record: &AssistProviderRecord, account: &Account) -> bool {
    match record.access.as_str() {
        "everyone" => true,
        "domains" => {
            let domain = domain_of(&account.login);
            record.access_list.iter().any(|entry| entry.eq_ignore_ascii_case(&domain))
        }
        "people" => record.access_list.iter().any(|entry| entry.eq_ignore_ascii_case(&account.login)),
        _ => false,
    }
}

fn key_hint(key: &str) -> Option<String> {
    let chars: Vec<char> = key.trim().chars().collect();
    (chars.len() >= 8).then(|| format!("…{}", chars[chars.len() - 4..].iter().collect::<String>()))
}

fn check_features(features: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for feature in features {
        if !FEATURES.contains(&feature.as_str()) {
            return Err(AssistError::invalid("badFeature", "features", format!("{feature:?} is not a feature")));
        }
        if !out.contains(feature) {
            out.push(feature.clone());
        }
    }
    Ok(out)
}

fn check_model(value: Option<String>, property: &'static str) -> Result<Option<String>> {
    let Some(value) = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) else { return Ok(None) };
    if value.chars().count() > MODEL_MAX_CHARS || value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(AssistError::invalid("badModel", property, "a model name is one word of at most 200 characters"));
    }
    Ok(Some(value))
}

/// Checks a provider's address. `reach` is what the provider may connect to: an address outside it is
/// refused now, and an address that resolves outside it later is refused when connecting.
pub(crate) fn check_base_url(url: &str, reach: Reach) -> Result<String> {
    let url = url.trim();
    let bad = |description: &str| AssistError::invalid("badProviderUrl", "baseUrl", description);
    if url.chars().count() > URL_MAX_CHARS {
        return Err(bad("the address is too long"));
    }
    let parsed = Url::parse(url).map_err(|_| bad("that is not a web address"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(bad("the address has to start with https:// or http://"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(bad("the address may not contain a login; the key has its own field"));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(bad("the address may not contain ? or #"));
    }
    let private = |ip: IpAddr| !uwumail_smtp::fetch::is_public(ip);
    let (is_private, local_name) = match parsed.host() {
        None => return Err(bad("the address has no host")),
        Some(Host::Ipv4(ip)) => (private(IpAddr::V4(ip)), false),
        Some(Host::Ipv6(ip)) => (private(IpAddr::V6(ip)), false),
        Some(Host::Domain(domain)) => {
            let local = domain.eq_ignore_ascii_case("localhost") || !domain.contains('.') || domain.ends_with(".local");
            (local, local)
        }
    };
    if is_private {
        let allowed = match (reach, parsed.host()) {
            (Reach::Public, _) => false,
            (Reach::Lan, Some(Host::Ipv4(ip))) => is_local_network(IpAddr::V4(ip)),
            (Reach::Lan, Some(Host::Ipv6(ip))) => is_local_network(IpAddr::V6(ip)),
            // A name is checked when it is resolved.
            (Reach::Lan, _) => local_name,
            (Reach::Any, _) => true,
        };
        if !allowed {
            return Err(AssistError::invalid(
                "privateAddress",
                "baseUrl",
                "the address points into the local network, which is not allowed here",
            ));
        }
    } else if parsed.scheme() == "http" {
        return Err(AssistError::invalid(
            "plainHttpPublic",
            "baseUrl",
            "plain http:// only works inside the local network; use https:// on the internet",
        ));
    }
    if reach == Reach::Public && parsed.scheme() == "http" {
        return Err(AssistError::invalid(
            "plainHttpPublic",
            "baseUrl",
            "plain http:// only works inside the local network; use https:// on the internet",
        ));
    }
    Ok(parsed.as_str().trim_end_matches('/').to_owned())
}

/// A provider record after applying `input` on top of `before` (or of nothing, for a new one).
fn build_write(
    before: Option<&AssistProviderRecord>,
    input: &ProviderInput,
    server: bool,
    reach: Reach,
) -> Result<(AssistProviderWrite, SecretChange)> {
    let kind_name = match (before, input.kind.as_deref()) {
        (Some(before), None) => before.kind.clone(),
        (Some(before), Some(kind)) if kind == before.kind => before.kind.clone(),
        (Some(_), Some(_)) => {
            return Err(AssistError::invalid("badProviderKind", "kind", "the kind of a provider can't be changed"));
        }
        (None, Some(kind)) => kind.to_owned(),
        (None, None) => return Err(AssistError::invalid("badProviderKind", "kind", "a provider needs a kind")),
    };
    let info = kinds::kind(&kind_name)
        .ok_or_else(|| AssistError::invalid("badProviderKind", "kind", format!("{kind_name:?} is not a kind")))?;
    if server && info.personal_only {
        return Err(AssistError::invalid(
            "badProviderKind",
            "kind",
            "a ChatGPT subscription belongs to one person and can't be shared with everyone",
        ));
    }
    let name = input.name.clone().or_else(|| before.map(|b| b.name.clone())).unwrap_or_else(|| info.name.to_owned());
    let name = name.trim().to_owned();
    if name.is_empty() || name.chars().count() > NAME_MAX_CHARS || name.chars().any(char::is_control) {
        return Err(AssistError::invalid("badProviderName", "name", "a name has 1 to 60 characters"));
    }
    let base_url = match &input.base_url {
        Some(url) => url.clone(),
        None => before.and_then(|b| b.base_url.clone()),
    }
    .filter(|url| !url.trim().is_empty());
    let base_url = match (info.base_url, base_url) {
        (BaseUrl::Fixed, _) => None,
        (_, Some(url)) => Some(check_base_url(&url, reach)?),
        (BaseUrl::Required, None) => {
            return Err(AssistError::invalid("badProviderUrl", "baseUrl", "this kind of provider needs an address"));
        }
        (BaseUrl::Optional, None) => None,
    };
    let model = match &input.model {
        Some(model) => check_model(model.clone(), "model")?,
        None => before.and_then(|b| b.model.clone()),
    };
    let fast_model = match &input.fast_model {
        Some(model) => check_model(model.clone(), "fastModel")?,
        None => before.and_then(|b| b.fast_model.clone()),
    };
    let secret = match input.api_key.as_deref().map(str::trim) {
        _ if info.key == Key::Login => SecretChange::Keep,
        None => SecretChange::Keep,
        Some("") => SecretChange::Remove,
        Some(key) if key.chars().count() > KEY_MAX_CHARS || key.chars().any(|c| c.is_control() || c == ' ') => {
            return Err(AssistError::invalid("badProviderKey", "apiKey", "that does not look like a key"));
        }
        Some(key) => SecretChange::Set(key.to_owned(), key_hint(key)),
    };
    let has_key = match &secret {
        SecretChange::Set(..) => true,
        SecretChange::Remove => false,
        SecretChange::Keep => before.is_some_and(|b| b.has_secret),
    };
    if info.key == Key::Required && !has_key {
        return Err(AssistError::invalid("badProviderKey", "apiKey", "this kind of provider needs a key"));
    }
    let features = match &input.features {
        Some(features) => check_features(features)?,
        None => before.map(|b| b.features.clone()).unwrap_or_else(|| FEATURES.iter().map(|f| f.to_string()).collect()),
    };
    let access = input.access.clone().or_else(|| before.map(|b| b.access.clone())).unwrap_or_else(|| "everyone".into());
    if !matches!(access.as_str(), "everyone" | "domains" | "people") {
        return Err(AssistError::invalid("badAccess", "access", "access is everyone, domains or people"));
    }
    let access_list = match access.as_str() {
        "domains" => {
            let list = input
                .domains
                .clone()
                .or_else(|| before.filter(|b| b.access == "domains").map(|b| b.access_list.clone()));
            let mut out = Vec::new();
            for domain in list.unwrap_or_default() {
                let domain = normalize_domain(&domain)
                    .map_err(|_| AssistError::invalid("badAccess", "domains", format!("{domain:?} is not a domain")))?;
                if !out.contains(&domain) {
                    out.push(domain);
                }
            }
            out
        }
        "people" => {
            let list =
                input.people.clone().or_else(|| before.filter(|b| b.access == "people").map(|b| b.access_list.clone()));
            let mut out: Vec<String> = Vec::new();
            for login in list.unwrap_or_default() {
                let login = login.trim().to_lowercase();
                if !login.contains('@') || login.chars().count() > 254 {
                    return Err(AssistError::invalid("badAccess", "people", format!("{login:?} is not an address")));
                }
                if !out.contains(&login) {
                    out.push(login);
                }
            }
            out
        }
        _ => Vec::new(),
    };
    if access != "everyone" && access_list.is_empty() {
        return Err(AssistError::invalid("badAccess", "access", "name at least one domain or person"));
    }
    if access_list.len() > ASSIST_MAX_ACCESS_ENTRIES {
        return Err(AssistError::invalid("badAccess", "access", "too many domains or people"));
    }
    let quota = |value: &Option<Option<i64>>, before: Option<i64>, property: &'static str| -> Result<Option<i64>> {
        let value = match value {
            Some(value) => *value,
            None => before,
        };
        match value {
            Some(n) if n < 0 => Err(AssistError::invalid("badQuota", property, "a limit is 0 or more")),
            other => Ok(other),
        }
    };
    let write = AssistProviderWrite {
        name,
        kind: kind_name,
        base_url,
        model,
        fast_model,
        enabled: input.enabled.or(before.map(|b| b.enabled)).unwrap_or(true),
        access,
        access_list,
        features,
        requests_per_day: quota(&input.requests_per_day, before.and_then(|b| b.requests_per_day), "requestsPerDay")?,
        tokens_per_day: quota(&input.tokens_per_day, before.and_then(|b| b.tokens_per_day), "tokensPerDay")?,
    };
    Ok((write, secret))
}

impl Assist {
    /// The providers `account` may see: the server's allowed for them, then their own.
    pub(crate) async fn available(&self, account: &Account, policy: &AssistPolicy) -> Result<Vec<Available>> {
        let store = self.store();
        let mut out = Vec::new();
        for record in store.assist_providers(None).await? {
            if !record.enabled || !allowed_for(&record, account) {
                continue;
            }
            let Some(info) = kinds::kind(&record.kind) else { continue };
            let features = record.features.iter().filter(|f| policy.features.get(f)).cloned().collect::<Vec<String>>();
            let usable = !info.personal_only && usable(&record, info);
            out.push(Available { record, info, server: true, features, usable });
        }
        for record in store.assist_providers(Some(account.id)).await? {
            let Some(info) = kinds::kind(&record.kind) else { continue };
            let features = if policy.allow_personal {
                FEATURES.iter().filter(|f| policy.features.get(f)).map(|f| f.to_string()).collect()
            } else {
                Vec::new()
            };
            let usable = policy.allow_personal && usable(&record, info);
            out.push(Available { record, info, server: false, features, usable });
        }
        Ok(out)
    }

    /// What `feature` will use for `account`, if anything.
    pub(crate) fn effective(
        available: &[Available],
        choices: &Map<String, Value>,
        feature: &str,
    ) -> Option<(Available, String)> {
        let fits = |a: &&Available| a.usable && a.features.iter().any(|f| f == feature);
        let chosen = |key: &str| -> Option<Choice> { serde_json::from_value(choices.get(key)?.clone()).ok() };
        for choice in [chosen(feature), chosen("default")].into_iter().flatten() {
            if let Some(found) = available.iter().filter(fits).find(|a| a.record.id == choice.provider_id)
                && let Some(model) = found.model_for(feature, choice.model.as_deref())
            {
                return Some((found.clone(), model));
            }
        }
        for server in [true, false] {
            for found in available.iter().filter(fits).filter(|a| a.server == server) {
                if let Some(model) = found.model_for(feature, None) {
                    return Some((found.clone(), model));
                }
            }
        }
        None
    }

    fn reach_of(&self, available: &Available, policy: &AssistPolicy) -> Reach {
        if available.server || self.inner.reach_anything {
            Reach::Any
        } else if policy.allow_personal_private {
            Reach::Lan
        } else {
            Reach::Public
        }
    }

    pub async fn capability(&self, account: &Account) -> Result<Capability> {
        let policy = self.store().assist_policy().await?;
        let available = self.available(account, &policy).await?;
        let prefs = self.store().assist_prefs(account.id).await?;
        let mut features = AssistFeatures::default();
        for feature in FEATURES {
            let on = policy.features.get(feature) && Self::effective(&available, &prefs.choices, feature).is_some();
            features.set(feature, on);
        }
        Ok(Capability {
            features,
            may_add_providers: policy.allow_personal,
            may_use_private_addresses: policy.allow_personal && policy.allow_personal_private,
            max_providers: ASSIST_MAX_PERSONAL_PROVIDERS,
            max_labels: ASSIST_MAX_LABELS,
            max_instruction_chars: MAX_INSTRUCTION_CHARS,
            max_text_chars: MAX_TEXT_CHARS,
        })
    }

    pub async fn providers(&self, account: &Account) -> Result<Vec<ProviderView>> {
        let policy = self.store().assist_policy().await?;
        Ok(self.available(account, &policy).await?.iter().map(Available::view).collect())
    }

    /// The JMAP state of the person's assist objects: moves with their own changes and with the
    /// admin's.
    pub async fn state(&self, account: &Account) -> Result<String> {
        let version = self.store().assist_version().await?;
        let prefs = self.store().assist_prefs(account.id).await?;
        Ok(format!("{version}-{}", prefs.modseq))
    }

    pub async fn settings(&self, account: &Account) -> Result<SettingsView> {
        let store = self.store();
        let policy = store.assist_policy().await?;
        let available = self.available(account, &policy).await?;
        let prefs = store.assist_prefs(account.id).await?;
        let refine_events = store
            .user_settings(account.id)
            .await?
            .values
            .get("assist.refineEvents")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let chosen = |key: &str| -> Option<Choice> { serde_json::from_value(prefs.choices.get(key)?.clone()).ok() };
        let mut features = BTreeMap::new();
        let mut effective = BTreeMap::new();
        for feature in FEATURES {
            features.insert(feature.to_owned(), chosen(feature));
            let found = policy
                .features
                .get(feature)
                .then(|| Self::effective(&available, &prefs.choices, feature))
                .flatten()
                .map(|(a, model)| Effective {
                    provider_id: a.record.id,
                    provider_name: a.record.name.clone(),
                    model,
                    scope: a.scope(),
                });
            effective.insert(feature.to_owned(), found);
        }
        Ok(SettingsView {
            default: chosen("default"),
            features,
            auto_labels: prefs.auto_labels,
            refine_events,
            effective,
            state: format!("{}-{}", store.assist_version().await?, prefs.modseq),
        })
    }

    pub async fn set_settings(&self, account: &Account, patch: SettingsPatch) -> Result<SettingsView> {
        let store = self.store();
        let policy = store.assist_policy().await?;
        let available = self.available(account, &policy).await?;
        let prefs = store.assist_prefs(account.id).await?;
        let mut choices = prefs.choices.clone();
        let check = |choice: &Choice, feature: Option<&str>| -> Result<()> {
            let found = available.iter().find(|a| a.record.id == choice.provider_id).ok_or_else(|| {
                AssistError::invalid("badProvider", "providerId", "that provider can't be used by you")
            })?;
            if let Some(feature) = feature
                && !found.features.iter().any(|f| f == feature)
            {
                return Err(AssistError::invalid(
                    "badProvider",
                    "providerId",
                    format!("{} may not be used for {feature}", found.record.name),
                ));
            }
            if let Some(model) = &choice.model {
                check_model(Some(model.clone()), "model")?;
            }
            Ok(())
        };
        if let Some(default) = &patch.default {
            match default {
                Some(choice) => {
                    check(choice, None)?;
                    choices.insert("default".into(), json!(choice));
                }
                None => {
                    choices.remove("default");
                }
            }
        }
        if let Some(features) = &patch.features {
            for (feature, choice) in features {
                if !FEATURES.contains(&feature.as_str()) {
                    return Err(AssistError::invalid(
                        "badFeature",
                        "features",
                        format!("{feature:?} is not a feature"),
                    ));
                }
                match choice {
                    Some(choice) => {
                        check(choice, Some(feature))?;
                        choices.insert(feature.clone(), json!(choice));
                    }
                    None => {
                        choices.remove(feature);
                    }
                }
            }
        }
        let auto_labels = patch.auto_labels.unwrap_or(prefs.auto_labels);
        if patch.default.is_some() || patch.features.is_some() || patch.auto_labels.is_some() {
            store.set_assist_prefs(account.id, choices, auto_labels).await?;
        }
        if let Some(refine) = patch.refine_events {
            store
                .update_user_settings(
                    account.id,
                    uwumail_store::SettingsChange::Patch(vec![("assist.refineEvents".into(), Some(json!(refine)))]),
                    None,
                )
                .await?;
        }
        self.settings(account).await
    }

    /// A provider of the person's own, or `Forbidden`/`NotFound`.
    async fn own_provider(&self, account: &Account, id: i64) -> Result<AssistProviderRecord> {
        match self.store().assist_provider(id).await? {
            Some(record) if record.account_id == Some(account.id) => Ok(record),
            Some(record) if record.account_id.is_none() => {
                Err(AssistError::Forbidden("server providers are changed by the admin".into()))
            }
            _ => Err(AssistError::NotFound(format!("provider {id}"))),
        }
    }

    pub async fn create_personal_provider(&self, account: &Account, input: ProviderInput) -> Result<ProviderView> {
        let policy = self.store().assist_policy().await?;
        if !policy.allow_personal {
            return Err(AssistError::Forbidden("the admin does not allow providers of your own".into()));
        }
        let reach = if policy.allow_personal_private || self.inner.reach_anything { Reach::Lan } else { Reach::Public };
        let reach = if self.inner.reach_anything { Reach::Any } else { reach };
        let (write, secret) = build_write(None, &input, false, reach)?;
        let record = self.store().create_assist_provider(Some(account.id), write, secret).await.map_err(too_many)?;
        self.personal_view(account, record.id).await
    }

    pub async fn update_personal_provider(
        &self,
        account: &Account,
        id: i64,
        input: ProviderInput,
    ) -> Result<ProviderView> {
        let before = self.own_provider(account, id).await?;
        let policy = self.store().assist_policy().await?;
        let reach = match (self.inner.reach_anything, policy.allow_personal_private) {
            (true, _) => Reach::Any,
            (false, true) => Reach::Lan,
            (false, false) => Reach::Public,
        };
        // Only what changed is checked again: an address that was fine when it was saved stays, even if
        // the admin since forbade the local network (connecting then fails, with a clear reason).
        let mut input = input;
        if input.base_url.as_ref().and_then(|url| url.as_deref()) == before.base_url.as_deref() {
            input.base_url = None;
        }
        let (write, secret) = build_write(Some(&before), &input, false, reach)?;
        self.store().update_assist_provider(id, write, secret).await?;
        self.inner.logins.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        self.personal_view(account, id).await
    }

    pub async fn delete_personal_provider(&self, account: &Account, id: i64) -> Result<()> {
        self.own_provider(account, id).await?;
        self.store().delete_assist_provider(id).await?;
        self.forget_choices(account.id, id).await
    }

    /// Drops the choices that name a provider that is gone.
    async fn forget_choices(&self, account_id: i64, provider_id: i64) -> Result<()> {
        let prefs = self.store().assist_prefs(account_id).await?;
        let mut choices = prefs.choices.clone();
        choices.retain(|_, choice| choice.get("providerId").and_then(Value::as_i64) != Some(provider_id));
        if choices.len() != prefs.choices.len() {
            self.store().set_assist_prefs(account_id, choices, prefs.auto_labels).await?;
        }
        Ok(())
    }

    async fn personal_view(&self, account: &Account, id: i64) -> Result<ProviderView> {
        let policy = self.store().assist_policy().await?;
        self.available(account, &policy)
            .await?
            .iter()
            .find(|a| a.record.id == id)
            .map(Available::view)
            .ok_or_else(|| AssistError::NotFound(format!("provider {id}")))
    }

    pub async fn admin_providers(&self) -> Result<Vec<AdminProviderView>> {
        Ok(self.store().assist_providers(None).await?.iter().map(admin_view).collect())
    }

    pub async fn create_server_provider(&self, input: ProviderInput) -> Result<AdminProviderView> {
        let (write, secret) = build_write(None, &input, true, Reach::Any)?;
        let record = self.store().create_assist_provider(None, write, secret).await.map_err(too_many)?;
        Ok(admin_view(&record))
    }

    pub async fn update_server_provider(&self, id: i64, input: ProviderInput) -> Result<AdminProviderView> {
        let before = match self.store().assist_provider(id).await? {
            Some(record) if record.account_id.is_none() => record,
            _ => return Err(AssistError::NotFound(format!("provider {id}"))),
        };
        let (write, secret) = build_write(Some(&before), &input, true, Reach::Any)?;
        Ok(admin_view(&self.store().update_assist_provider(id, write, secret).await?))
    }

    pub async fn delete_server_provider(&self, id: i64) -> Result<()> {
        match self.store().assist_provider(id).await? {
            Some(record) if record.account_id.is_none() => self.store().delete_assist_provider(id).await?,
            _ => return Err(AssistError::NotFound(format!("provider {id}"))),
        }
        Ok(())
    }

    /// Asks a provider for its models: a server provider for the admin (`account` `None`), or one the
    /// person may use.
    pub async fn models(
        &self,
        account: Option<&Account>,
        id: i64,
    ) -> Result<(Vec<(String, String)>, Option<String>, Option<String>)> {
        let policy = self.store().assist_policy().await?;
        let (record, info, reach) = match account {
            None => {
                let record = match self.store().assist_provider(id).await? {
                    Some(record) if record.account_id.is_none() => record,
                    _ => return Err(AssistError::NotFound(format!("provider {id}"))),
                };
                let info = kinds::kind(&record.kind).ok_or_else(|| AssistError::NotFound(format!("provider {id}")))?;
                (record, info, Reach::Any)
            }
            Some(account) => {
                let available = self.available(account, &policy).await?;
                let found = available
                    .into_iter()
                    .find(|a| a.record.id == id)
                    .ok_or_else(|| AssistError::NotFound(format!("provider {id}")))?;
                let reach = self.reach_of(&found, &policy);
                (found.record, found.info, reach)
            }
        };
        let model = record.model.clone().or(info.model.map(str::to_owned));
        let fast = record.fast_model.clone().or(info.fast_model.map(str::to_owned));
        if !info.known_models.is_empty() {
            let models = info.known_models.iter().map(|m| (m.to_string(), m.to_string())).collect();
            return Ok((models, model, fast));
        }
        let key = self.store().assist_provider_secret(id).await?;
        let base = kinds::endpoint(info, record.base_url.as_deref())
            .ok_or_else(|| AssistError::Unavailable("the provider has no address".into()))?;
        let client = self.inner.egress.assist_client(reach);
        // Anthropic hands out 20 at a time unless asked for more.
        let query = if info.shape == Shape::Anthropic { "?limit=1000" } else { "" };
        let mut request = Request::get(format!("{}/models{query}", base.trim_end_matches('/')))
            .header(ACCEPT, "application/json")
            .header(USER_AGENT, "UwUMail");
        match info.shape {
            Shape::Anthropic => {
                if let Some(key) = &key {
                    request = request.header("x-api-key", key.as_str());
                }
                request = request.header("anthropic-version", "2023-06-01");
            }
            _ => {
                if let Some(key) = key.as_deref().filter(|k| !k.is_empty()) {
                    request = request.header(AUTHORIZATION, format!("Bearer {key}"));
                }
            }
        }
        let request = request
            .body(http_body_util::Full::new(bytes::Bytes::new()))
            .map_err(|_| AssistError::Unavailable("the provider's address is not usable".into()))?;
        let work = async {
            let response =
                client.send(request).await.map_err(|err| provider_failed(ProviderError::from_egress(err)))?;
            let status = response.status().as_u16();
            let body = http_body_util::Limited::new(response.into_body(), llm::MAX_RESPONSE_BYTES * 4)
                .collect()
                .await
                .map_err(|_| provider_failed(ProviderError::TooLarge))?
                .to_bytes();
            if !(200..300).contains(&status) {
                return Err(provider_failed(llm::status_error(status, None, &body)));
            }
            let value: Value = serde_json::from_slice(&body)
                .map_err(|_| provider_failed(ProviderError::Garbled("not JSON".into())))?;
            Ok(value)
        };
        let value = tokio::time::timeout(std::time::Duration::from_secs(20), work)
            .await
            .map_err(|_| provider_failed(ProviderError::Timeout))??;
        let list = value.get("data").or_else(|| value.get("models")).and_then(Value::as_array);
        let mut models: Vec<(String, String)> = list
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let id = entry.get("id").or_else(|| entry.get("name")).and_then(Value::as_str)?;
                let id = id.strip_prefix("models/").unwrap_or(id);
                if id.is_empty() || id.chars().count() > MODEL_MAX_CHARS {
                    return None;
                }
                let name = entry
                    .get("display_name")
                    .or_else(|| entry.get("name"))
                    .and_then(Value::as_str)
                    .map(|name| llm::shorten(name, 100))
                    .unwrap_or_else(|| id.to_owned());
                Some((id.to_owned(), name))
            })
            .collect();
        models.sort();
        models.dedup_by(|a, b| a.0 == b.0);
        models.truncate(MAX_MODELS);
        Ok((models, model, fast))
    }

    /// Starts a ChatGPT device login for one of the person's own `chatgpt` providers.
    pub async fn chatgpt_login(&self, account: &Account, id: i64) -> Result<(String, String, u64, i64)> {
        let record = self.own_provider(account, id).await?;
        if record.kind != "chatgpt" {
            return Err(AssistError::invalid("badProviderKind", "providerId", "only a ChatGPT provider signs in"));
        }
        let client = self.chatgpt_client().await?;
        let code = chatgpt::start(&client, &self.inner.chatgpt, now())
            .await
            .map_err(|description| AssistError::ProviderFailed { description, retry_after: None, transient: false })?;
        let answer =
            (code.user_code.clone(), chatgpt::verification_uri(&self.inner.chatgpt), code.interval, code.expires_at);
        self.inner.logins.lock().unwrap_or_else(|e| e.into_inner()).insert(id, code);
        Ok(answer)
    }

    /// `pending`, `connected`, `expired` or `failed` with why.
    pub async fn chatgpt_poll(&self, account: &Account, id: i64) -> Result<(&'static str, Option<String>)> {
        let record = self.own_provider(account, id).await?;
        let code = self.inner.logins.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
        let Some(code) = code else {
            if record.has_secret {
                return Ok(("connected", None));
            }
            return Err(AssistError::invalid("chatgptNotStarted", "providerId", "no sign-in was started"));
        };
        if code.expires_at <= now() {
            self.inner.logins.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            return Ok(("expired", None));
        }
        let client = self.chatgpt_client().await?;
        match chatgpt::poll(&client, &self.inner.chatgpt, &code, now()).await {
            Poll::Pending => Ok(("pending", None)),
            Poll::Connected(tokens) => {
                self.inner.logins.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                let json = serde_json::to_string(&tokens).map_err(|err| StoreError::Internal(err.to_string()))?;
                self.store().set_assist_provider_secret(id, SecretChange::Set(json, None)).await?;
                Ok(("connected", None))
            }
            Poll::Failed(why) => {
                self.inner.logins.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                Ok(("failed", Some(why)))
            }
        }
    }

    async fn chatgpt_client(&self) -> Result<uwumail_smtp::egress::AssistClient> {
        let reach = if self.inner.reach_anything { Reach::Any } else { Reach::Public };
        Ok(self.inner.egress.assist_client(reach))
    }

    /// The person's use today, per provider they may use.
    pub async fn today(&self, account: &Account) -> Result<Vec<TodayUsage>> {
        let policy = self.store().assist_policy().await?;
        let mut out = Vec::new();
        for available in self.available(account, &policy).await? {
            let (requests, tokens) = self.store().assist_used_today(account.id, available.record.id).await?;
            out.push(TodayUsage {
                provider_id: available.record.id,
                provider_name: available.record.name.clone(),
                requests,
                tokens,
                requests_per_day: available.server.then_some(available.record.requests_per_day).flatten(),
                tokens_per_day: available.server.then_some(available.record.tokens_per_day).flatten(),
            });
        }
        Ok(out)
    }

    /// Asks the model for `feature` on behalf of `account`, within the policy and the quota, and
    /// counts it.
    pub(crate) async fn run(
        &self,
        account: &Account,
        feature: &str,
        prompt: &Prompt,
        deltas: Option<&mpsc::Sender<String>>,
    ) -> Result<(Completion, Effective)> {
        let store = self.store();
        let policy = store.assist_policy().await?;
        if !policy.features.get(feature) {
            return Err(AssistError::Unavailable(format!("{feature} is switched off on this server")));
        }
        let available = self.available(account, &policy).await?;
        let prefs = store.assist_prefs(account.id).await?;
        let (provider, model) = Self::effective(&available, &prefs.choices, feature)
            .ok_or_else(|| AssistError::Unavailable(format!("no AI provider can be used for {feature}")))?;
        let record = &provider.record;
        if provider.server && (record.requests_per_day.is_some() || record.tokens_per_day.is_some()) {
            let (requests, tokens) = store.assist_used_today(account.id, record.id).await?;
            if let Some(limit) = record.requests_per_day
                && requests >= limit
            {
                return Err(AssistError::OverQuota(format!(
                    "the {limit} requests a day to {} are used up; the count starts again at midnight UTC",
                    record.name
                )));
            }
            if let Some(limit) = record.tokens_per_day
                && tokens >= limit
            {
                return Err(AssistError::OverQuota(format!(
                    "the {limit} tokens a day for {} are used up; the count starts again at midnight UTC",
                    record.name
                )));
            }
        }
        let _running = self.begin(account.id)?;
        let target = self.target(&provider, &policy, model.clone()).await?;
        let result = llm::complete(&target, prompt, deltas).await;
        let (input, output) = match &result {
            Ok(completion) => (completion.input_tokens, completion.output_tokens),
            Err(_) => (0, 0),
        };
        if let Err(err) = store.record_assist_usage(account.id, record.id, feature, input, output).await {
            tracing::warn!(%err, "counting an AI request failed");
        }
        let completion = result.map_err(provider_failed)?;
        let effective =
            Effective { provider_id: record.id, provider_name: record.name.clone(), model, scope: provider.scope() };
        Ok((completion, effective))
    }

    async fn target(&self, provider: &Available, policy: &AssistPolicy, model: String) -> Result<Target> {
        let record = &provider.record;
        let info = provider.info;
        let reach = self.reach_of(provider, policy);
        let secret = self.store().assist_provider_secret(record.id).await?;
        let client = self.inner.egress.assist_client(reach);
        let (base_url, key, account_id) = if info.shape == Shape::Codex {
            let tokens: Tokens = secret
                .and_then(|json| serde_json::from_str(&json).ok())
                .ok_or_else(|| AssistError::Unavailable("sign in with ChatGPT first".into()))?;
            let tokens = if chatgpt::needs_refresh(&tokens, now()) {
                let chatgpt_client = self.chatgpt_client().await?;
                let fresh =
                    chatgpt::refresh(&chatgpt_client, &self.inner.chatgpt, &tokens, now()).await.map_err(|why| {
                        AssistError::ProviderFailed {
                            description: format!("the ChatGPT sign-in could not be renewed ({why}); sign in again"),
                            retry_after: None,
                            transient: false,
                        }
                    })?;
                let json = serde_json::to_string(&fresh).map_err(|err| StoreError::Internal(err.to_string()))?;
                self.store().set_assist_provider_secret(record.id, SecretChange::Set(json, None)).await?;
                fresh
            } else {
                tokens
            };
            (self.inner.chatgpt.backend.clone(), Some(tokens.access_token), tokens.account_id)
        } else {
            let base = kinds::endpoint(info, record.base_url.as_deref())
                .ok_or_else(|| AssistError::Unavailable("the provider has no address".into()))?;
            (base, secret, None)
        };
        Ok(Target { shape: info.shape, flavor: info.flavor, base_url, key, model, account_id, client })
    }
}

fn usable(record: &AssistProviderRecord, info: &KindInfo) -> bool {
    let has_key = match info.key {
        Key::Required | Key::Login => record.has_secret,
        Key::Optional | Key::None => true,
    };
    has_key && kinds::endpoint(info, record.base_url.as_deref()).is_some()
}

fn too_many(err: StoreError) -> AssistError {
    match err {
        StoreError::Rule { code: "overQuota", message } => AssistError::invalid("tooManyProviders", "id", message),
        other => AssistError::Store(other),
    }
}

pub(crate) fn provider_failed(err: ProviderError) -> AssistError {
    let retry_after = match &err {
        ProviderError::RateLimited { retry_after } => *retry_after,
        _ => None,
    };
    AssistError::ProviderFailed { description: err.to_string(), retry_after, transient: err.is_transient() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_by_reach() {
        assert!(check_base_url("https://api.example.com/v1", Reach::Public).is_ok());
        let code = |url: &str, reach| match check_base_url(url, reach) {
            Err(AssistError::Invalid { code, .. }) => code,
            _ => "ok",
        };
        assert_eq!(code("http://api.example.com/v1", Reach::Public), "plainHttpPublic");
        assert_eq!(code("http://api.example.com:11434", Reach::Any), "plainHttpPublic");
        assert_eq!(code("http://192.0.2.10:11434", Reach::Any), "ok");
        assert_eq!(code("http://192.168.1.5:11434", Reach::Public), "privateAddress");
        assert_eq!(code("http://192.168.1.5:11434", Reach::Lan), "ok");
        assert_eq!(code("http://127.0.0.1:11434", Reach::Lan), "privateAddress");
        assert_eq!(code("http://169.254.169.254/latest", Reach::Lan), "privateAddress");
        assert_eq!(code("http://127.0.0.1:11434", Reach::Any), "ok");
        assert_eq!(code("http://ollama:11434", Reach::Public), "privateAddress");
        assert_eq!(code("http://ollama:11434", Reach::Lan), "ok");
        assert_eq!(code("https://user:pw@api.example.com", Reach::Any), "badProviderUrl");
        assert_eq!(code("ftp://api.example.com", Reach::Any), "badProviderUrl");
        assert_eq!(code("https://api.example.com/v1?x=1", Reach::Any), "badProviderUrl");
        assert_eq!(check_base_url("https://api.example.com/v1/", Reach::Public).unwrap(), "https://api.example.com/v1");
    }

    #[test]
    fn hints_show_only_the_end() {
        assert_eq!(key_hint("sk-abcdefghijkl1234").as_deref(), Some("…1234"));
        assert_eq!(key_hint("short"), None);
    }
}
