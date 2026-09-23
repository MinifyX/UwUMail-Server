//! UserSettings/get and UserSettings/set: the settings the webmail and the UwUMail apps keep in
//! sync, one singleton per account like VacationResponse. See docs/jmap-settings.md.

use serde_json::{Map, Value, json};
use uwumail_store::{SettingProblem, SettingsChange, StoreError, USER_SETTINGS_MAX_KEYS, validate_setting};

use super::{Ctx, SetResponse, get_ids, if_in_state, pick, properties};
use crate::error::{MethodError, MethodResult, SetError};

const ID: &str = "singleton";
const DEFAULTS: &[&str] = &["id", "values"];

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let settings = ctx.jmap.store.user_settings(ctx.account.id).await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let object = || {
        let mut object = Map::new();
        object.insert("id".into(), json!(ID));
        object.insert("values".into(), Value::Object(settings.values.clone()));
        pick(object, &properties)
    };
    let (list, not_found): (Vec<Value>, Vec<String>) = match get_ids(args)? {
        None => (vec![object()], Vec::new()),
        Some(requested) => {
            let found = requested.iter().any(|id| id == ID);
            (if found { vec![object()] } else { Vec::new() }, requested.into_iter().filter(|id| id != ID).collect())
        }
    };
    Ok(json!({ "accountId": ctx.account_id(), "state": settings.state, "list": list, "notFound": not_found }))
}

/// A JSON pointer token: `~1` is `/`, `~0` is `~`. A `/` of its own would point inside a value,
/// which cannot be patched; values are always set whole.
fn unescape(token: &str) -> Option<String> {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        match c {
            '~' => match chars.next() {
                Some('0') => out.push('~'),
                Some('1') => out.push('/'),
                _ => return None,
            },
            '/' => return None,
            other => out.push(other),
        }
    }
    Some(out)
}

/// Turns the patch into a change the store applies, checking every key and value first so that a
/// single bad one refuses the whole update.
fn parse_patch(patch: &Map<String, Value>) -> Result<SettingsChange, SetError> {
    // One write never needs to name more keys than an account may keep; removals count too, so a
    // request full of `null`s can't turn into hundreds of thousands of statements.
    let named = patch.keys().filter(|path| path.starts_with("values/")).count()
        + patch.get("values").and_then(Value::as_object).map_or(0, Map::len);
    if named > USER_SETTINGS_MAX_KEYS {
        return Err(SetError::new("overQuota", format!("at most {USER_SETTINGS_MAX_KEYS} settings in one write")));
    }
    let mut writes: Vec<(String, Option<Value>)> = Vec::new();
    let mut replace: Option<Map<String, Value>> = None;
    let mut bad: Vec<String> = Vec::new();
    for (path, value) in patch {
        if path == "values" {
            match value {
                Value::Object(all) => replace = Some(all.clone()),
                _ => return Err(SetError::invalid_properties(&["values"], "values must be an object")),
            }
        } else if let Some(token) = path.strip_prefix("values/") {
            let key = unescape(token).ok_or_else(|| {
                SetError::new("invalidPatch", format!("{path} does not name one setting; values are set whole"))
            })?;
            writes.push((key, (!value.is_null()).then(|| value.clone())));
        } else if path == "id" {
            if value.as_str() != Some(ID) {
                bad.push("id".into());
            }
        } else {
            bad.push(path.clone());
        }
    }
    if replace.is_some() && !writes.is_empty() {
        return Err(SetError::new("invalidPatch", "values and values/… cannot be patched together"));
    }
    let checked: Vec<(&String, &Value)> = match &replace {
        Some(all) => all.iter().filter(|(_, value)| !value.is_null()).collect(),
        None => writes.iter().filter_map(|(key, value)| value.as_ref().map(|value| (key, value))).collect(),
    };
    let mut too_large = None;
    for (key, value) in checked {
        match validate_setting(key, value) {
            Ok(()) => {}
            Err(SettingProblem::Invalid(_)) => bad.push(key.clone()),
            Err(SettingProblem::TooLarge) => too_large = too_large.or(Some(key.clone())),
        }
    }
    // Removing a key never needs checking, except that it has to be a key that can exist.
    if replace.is_none() {
        for (key, value) in &writes {
            if value.is_none() && validate_removal(key).is_err() {
                bad.push(key.clone());
            }
        }
    }
    if !bad.is_empty() {
        bad.sort();
        bad.dedup();
        let properties: Vec<&str> = bad.iter().map(String::as_str).collect();
        return Err(SetError::invalid_properties(&properties, "these settings or their values are not allowed"));
    }
    if let Some(key) = too_large {
        return Err(SetError::new("tooLarge", format!("{key} is larger than maxValueSize")));
    }
    Ok(match replace {
        Some(all) => SettingsChange::Replace(all),
        None => SettingsChange::Patch(writes),
    })
}

/// A key can only be removed if it could have been set: checked with a value every key of that
/// name accepts, so the key's own rules decide.
fn validate_removal(key: &str) -> Result<(), SettingProblem> {
    let sample = match key {
        "theme" => json!("system"),
        "tone" => json!("playful"),
        "language" => json!("system"),
        "conversations" | "senderPictures" | "linkConfirm" | "darkImages" => json!(true),
        "remoteImages" => json!("ask"),
        "mailAppearance" => json!("auto"),
        "undoSendSeconds" => json!(0),
        _ if key.starts_with("senderAppearance:") => json!("light"),
        _ if key.starts_with("signature:") => {
            json!({ "email": "", "name": "", "html": "", "forNew": false, "forReplies": false })
        }
        _ => json!(true),
    };
    validate_setting(key, &sample)
}

pub async fn set(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    super::check_set_size(args)?;
    let store = &ctx.jmap.store;
    let account_id = ctx.account.id;
    let old_state = store.user_settings_state(account_id).await?;
    if_in_state(args, &old_state)?;
    let expected = args.get("ifInState").and_then(Value::as_str).map(str::to_owned);
    let mut response = SetResponse::default();

    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for creation_id in create.keys() {
            let error = SetError::new("singleton", "there is only one UserSettings object");
            response.not_created.insert(creation_id.clone(), error.to_json());
        }
    }
    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in update {
            if id != ID {
                response.not_updated.insert(id.clone(), SetError::not_found().to_json());
                continue;
            }
            let change = patch
                .as_object()
                .ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))
                .and_then(parse_patch);
            let result = match change {
                Ok(change) => match store.update_user_settings(account_id, change, expected.clone()).await {
                    Ok(_) => Ok(()),
                    // Someone else wrote in between the check above and this write.
                    Err(StoreError::Rule { code: "stateMismatch", .. }) => {
                        return Err(MethodError::kind("stateMismatch"));
                    }
                    Err(StoreError::Invalid(message)) => Err(SetError::new("invalidProperties", message)),
                    Err(err) => Err(SetError::from(err)),
                },
                Err(err) => Err(err),
            };
            match result {
                Ok(()) => {
                    response.updated.insert(id.clone(), Value::Null);
                }
                Err(err) => {
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }
    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for id in destroy.iter().filter_map(Value::as_str) {
            let error = SetError::new("singleton", "UserSettings cannot be destroyed");
            response.not_destroyed.insert(id.to_owned(), error.to_json());
        }
    }
    let new_state = store.user_settings_state(account_id).await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(value: Value) -> Result<SettingsChange, SetError> {
        parse_patch(value.as_object().unwrap())
    }

    #[test]
    fn pointers_name_one_setting() {
        assert_eq!(unescape("linkDomains:example.org").as_deref(), Some("linkDomains:example.org"));
        assert_eq!(unescape("a~1b~0c").as_deref(), Some("a/b~c"));
        assert_eq!(unescape("signature:w/html"), None);
        assert_eq!(unescape("bad~2"), None);
        assert_eq!(unescape("bad~"), None);
    }

    #[test]
    fn patches_are_checked_as_a_whole() {
        let Ok(SettingsChange::Patch(writes)) =
            patch(json!({ "values/theme": "dark", "values/trustedSenders:@shop.example": null, "id": "singleton" }))
        else {
            panic!("a good patch");
        };
        assert_eq!(writes.len(), 2);
        assert!(writes.contains(&("trustedSenders:@shop.example".into(), None)));

        let err = patch(json!({ "values/theme": "dark", "values/colour": "pink", "values/tone": 1, "name": "x" }))
            .unwrap_err();
        assert_eq!(err.kind, "invalidProperties");
        assert_eq!(err.properties, Some(vec!["colour".into(), "name".into(), "tone".into()]));

        let err = patch(json!({ "values/nope:x": null })).unwrap_err();
        assert_eq!(err.properties, Some(vec!["nope:x".into()]));
        assert!(patch(json!({ "values/signature:w": null })).is_ok());

        assert_eq!(patch(json!({ "values/signature:w/html": "x" })).unwrap_err().kind, "invalidPatch");
        assert_eq!(patch(json!({ "values": {}, "values/theme": "dark" })).unwrap_err().kind, "invalidPatch");
        assert_eq!(patch(json!({ "values": [] })).unwrap_err().kind, "invalidProperties");
        assert!(matches!(
            patch(json!({ "values": { "theme": "dark", "tone": null } })),
            Ok(SettingsChange::Replace(_))
        ));

        let huge = json!({ "email": "", "name": "", "html": "x".repeat(300_000), "forNew": true, "forReplies": true });
        assert_eq!(patch(json!({ "values/signature:w": huge })).unwrap_err().kind, "tooLarge");
    }

    #[test]
    fn a_write_names_no_more_keys_than_an_account_keeps() {
        let removals: Map<String, Value> =
            (0..=USER_SETTINGS_MAX_KEYS).map(|n| (format!("values/linkDomains:h{n}.example"), Value::Null)).collect();
        assert_eq!(parse_patch(&removals).unwrap_err().kind, "overQuota");
        let nulls: Map<String, Value> =
            (0..=USER_SETTINGS_MAX_KEYS).map(|n| (format!("junk{n}"), Value::Null)).collect();
        assert_eq!(patch(json!({ "values": nulls })).unwrap_err().kind, "overQuota");
        let fits: Map<String, Value> =
            (0..USER_SETTINGS_MAX_KEYS).map(|n| (format!("values/linkDomains:h{n}.example"), Value::Null)).collect();
        assert!(parse_patch(&fits).is_ok());
    }
}
