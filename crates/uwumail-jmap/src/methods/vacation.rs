//! VacationResponse/get and VacationResponse/set (RFC 8621, section 8).

use serde_json::{Value, json};
use uwumail_store::VacationResponse;

use super::{Ctx, SetResponse, if_in_state};
use crate::dates;
use crate::error::{MethodResult, SetError};

const ID: &str = "singleton";

fn to_json(vacation: &VacationResponse) -> Value {
    json!({
        "id": ID,
        "isEnabled": vacation.is_enabled,
        "fromDate": vacation.from_date.map(dates::format),
        "toDate": vacation.to_date.map(dates::format),
        "subject": vacation.subject,
        "textBody": vacation.text_body,
        "htmlBody": vacation.html_body,
    })
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let state = ctx.state().await?;
    let vacation = ctx.jmap.store.vacation_response(ctx.account.id).await?;
    let wants = |id: &str| id == ID;
    let (list, not_found): (Vec<Value>, Vec<String>) = match super::get_ids(args)? {
        None => (vec![to_json(&vacation)], Vec::new()),
        Some(requested) => {
            let found = requested.iter().any(|id| wants(id));
            (
                if found { vec![to_json(&vacation)] } else { Vec::new() },
                requested.into_iter().filter(|id| !wants(id)).collect(),
            )
        }
    };
    Ok(json!({ "accountId": ctx.account_id(), "state": state, "list": list, "notFound": not_found }))
}

fn apply(vacation: &mut VacationResponse, patch: &serde_json::Map<String, Value>) -> Result<(), SetError> {
    let text = |value: &Value, property: &'static str| -> Result<Option<String>, SetError> {
        match value {
            Value::Null => Ok(None),
            Value::String(text) => Ok(Some(text.clone())),
            _ => Err(SetError::invalid_properties(&[property], format!("{property} must be a string or null"))),
        }
    };
    let date = |value: &Value, property: &'static str| -> Result<Option<i64>, SetError> {
        match value {
            Value::Null => Ok(None),
            Value::String(text) => dates::parse(text)
                .map(Some)
                .ok_or_else(|| SetError::invalid_properties(&[property], format!("{property} must be a UTC date"))),
            _ => Err(SetError::invalid_properties(&[property], format!("{property} must be a date or null"))),
        }
    };
    for (key, value) in patch {
        match key.as_str() {
            "isEnabled" => {
                vacation.is_enabled = value
                    .as_bool()
                    .ok_or_else(|| SetError::invalid_properties(&["isEnabled"], "isEnabled must be true or false"))?
            }
            "fromDate" => vacation.from_date = date(value, "fromDate")?,
            "toDate" => vacation.to_date = date(value, "toDate")?,
            "subject" => vacation.subject = text(value, "subject")?,
            "textBody" => vacation.text_body = text(value, "textBody")?,
            "htmlBody" => vacation.html_body = text(value, "htmlBody")?,
            "id" if value.as_str() == Some(ID) => {}
            other => {
                return Err(SetError::invalid_properties(
                    &[other],
                    format!("{other} is not a VacationResponse property"),
                ));
            }
        }
    }
    Ok(())
}

pub async fn set(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let old_state = ctx.state().await?;
    if_in_state(args, &old_state)?;
    let mut response = SetResponse::default();
    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for creation_id in create.keys() {
            response.not_created.insert(
                creation_id.clone(),
                SetError::new("singleton", "there is only one vacation response").to_json(),
            );
        }
    }
    if let Some(update) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in update {
            let result: Result<(), SetError> = async {
                if id != ID {
                    return Err(SetError::not_found());
                }
                let patch =
                    patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))?;
                let mut vacation = ctx.jmap.store.vacation_response(ctx.account.id).await?;
                apply(&mut vacation, patch)?;
                Ok(ctx.jmap.store.set_vacation_response(ctx.account.id, vacation).await?)
            }
            .await;
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
            response.not_destroyed.insert(
                id.to_owned(),
                SetError::new("singleton", "the vacation response cannot be destroyed").to_json(),
            );
        }
    }
    let new_state = ctx.state().await?;
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}
