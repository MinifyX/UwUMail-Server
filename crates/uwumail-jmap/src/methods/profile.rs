//! ProfilePicture/get and ProfilePicture/set: the account's own picture and who sees it, one
//! singleton per account (docs/profile-pictures.md, `urn:uwumail:jmap:profile`).

use serde_json::{Map, Value, json};
use uwumail_smtp::profile_pictures::{self, PictureError};
use uwumail_store::{NewPicture, PictureOwner, PictureVisibility, ProfileSettings, ProfileUpdate, StoreError};

use super::{Ctx, SetResponse, get_ids, if_in_state, pick, properties};
use crate::error::{MethodResult, SetError};
use crate::{dates, ids};

const ID: &str = "singleton";
const DEFAULTS: &[&str] = &["id", "blobId", "type", "visibility", "sendFace", "updated"];

fn object(settings: &ProfileSettings) -> Map<String, Value> {
    let picture = settings.picture.as_ref();
    let mut object = Map::new();
    object.insert("id".into(), json!(ID));
    object.insert("blobId".into(), json!(picture.map(|p| format!("b{}", p.hash))));
    object.insert("type".into(), json!(picture.map(|p| p.media_type.as_str())));
    object.insert("visibility".into(), json!(settings.effective_visibility().as_str()));
    object.insert("sendFace".into(), json!(settings.send_face));
    object.insert("updated".into(), json!(picture.map(|p| dates::format(p.updated_at))));
    object
}

pub async fn get(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    let settings = ctx.jmap.store.profile_settings(ctx.account.id).await?;
    let properties = properties(args, "properties", DEFAULTS)?;
    let (list, not_found): (Vec<Value>, Vec<String>) = match get_ids(args)? {
        None => (vec![pick(object(&settings), &properties)], Vec::new()),
        Some(requested) => {
            let found = requested.iter().any(|id| id == ID);
            let list = if found { vec![pick(object(&settings), &properties)] } else { Vec::new() };
            (list, requested.into_iter().filter(|id| id != ID).collect())
        }
    };
    Ok(json!({
        "accountId": ctx.account_id(),
        "state": settings.state.to_string(),
        "list": list,
        "notFound": not_found,
    }))
}

/// What a patch asks for, checked before anything is read or written.
struct Patch {
    /// `Some(None)` removes the picture.
    blob: Option<Option<String>>,
    visibility: Option<PictureVisibility>,
    send_face: Option<bool>,
}

fn parse_patch(patch: &Map<String, Value>) -> Result<Patch, SetError> {
    let mut parsed = Patch { blob: None, visibility: None, send_face: None };
    let mut bad = Vec::new();
    for (property, value) in patch {
        match (property.as_str(), value) {
            ("blobId", Value::Null) => parsed.blob = Some(None),
            ("blobId", Value::String(id)) => parsed.blob = Some(Some(id.clone())),
            ("visibility", Value::String(name)) => match PictureVisibility::parse(name) {
                Some(visibility) => parsed.visibility = Some(visibility),
                None => bad.push("visibility"),
            },
            ("sendFace", Value::Bool(on)) => parsed.send_face = Some(*on),
            ("id", Value::String(id)) if id == ID => {}
            _ => bad.push(property.as_str()),
        }
    }
    if !bad.is_empty() {
        bad.sort_unstable();
        return Err(SetError::invalid_properties(&bad, "these properties or their values are not allowed"));
    }
    Ok(parsed)
}

/// The uploaded picture a patch names, decoded and written anew.
async fn prepared_upload(ctx: &Ctx<'_>, blob_id: &str) -> Result<NewPicture, SetError> {
    let store = &ctx.jmap.store;
    let hash = match ctx.resolve(blob_id).and_then(ids::parse_blob) {
        Some(ids::BlobRef::Whole(hash)) => hash,
        _ => return Err(SetError::blob_not_found(vec![blob_id.to_owned()])),
    };
    if !store.blob_accessible(ctx.account.id, &hash).await.unwrap_or(false) {
        return Err(SetError::blob_not_found(vec![blob_id.to_owned()]));
    }
    let size = store.blob_size(&hash).await.map_err(SetError::from)?.unwrap_or(0);
    if size > profile_pictures::MAX_UPLOAD_BYTES as u64 {
        return Err(SetError::new("tooLarge", "the picture is larger than maxSize"));
    }
    let bytes = store.blob(&hash).await.map_err(|_| SetError::blob_not_found(vec![blob_id.to_owned()]))?;
    match profile_pictures::prepare_upload(bytes).await {
        Ok(prepared) => Ok(NewPicture {
            bytes: prepared.bytes,
            media_type: prepared.media_type.to_owned(),
            face: Some(prepared.face),
        }),
        Err(PictureError::TooLarge) => Err(SetError::new("tooLarge", "the picture is larger than maxSize")),
        Err(err) => Err(SetError::invalid_properties(&["blobId"], format!("not a usable picture: {err}"))),
    }
}

async fn update(ctx: &Ctx<'_>, patch: &Value, current: &ProfileSettings) -> Result<Option<Value>, SetError> {
    let patch = patch.as_object().ok_or_else(|| SetError::new("invalidPatch", "the patch must be an object"))?;
    let parsed = parse_patch(patch)?;
    if parsed.visibility == Some(PictureVisibility::Public) && !current.may_be_public {
        return Err(SetError::invalid_properties(&["visibility"], "public pictures are not allowed on this server"));
    }
    let current_blob = current.picture.as_ref().map(|picture| format!("b{}", picture.hash));
    // The picture that is already there, sent back as it came, is no new upload.
    let picture = match parsed.blob {
        Some(Some(id)) if Some(&id) == current_blob.as_ref() => None,
        Some(Some(id)) => Some(Some(prepared_upload(ctx, &id).await?)),
        Some(None) => Some(None),
        None => None,
    };
    let changed_picture = picture.is_some();
    let update = ProfileUpdate { picture, visibility: parsed.visibility, send_face: parsed.send_face };
    let settings = match ctx.jmap.store.update_profile(ctx.account.id, update).await {
        Ok(settings) => settings,
        Err(StoreError::Rule { code: "publicNotAllowed", message }) => {
            return Err(SetError::invalid_properties(&["visibility"], message));
        }
        Err(err) => return Err(SetError::from(err)),
    };
    if !changed_picture {
        return Ok(None);
    }
    let object = object(&settings);
    Ok(Some(pick(object, &["blobId".into(), "type".into(), "updated".into()])))
}

pub async fn set(ctx: &Ctx<'_>, args: &Value) -> MethodResult<Value> {
    super::check_set_size(args)?;
    let store = &ctx.jmap.store;
    let current = store.profile_settings(ctx.account.id).await?;
    let old_state = current.state.to_string();
    if_in_state(args, &old_state)?;
    let mut response = SetResponse::default();

    if let Some(create) = args.get("create").and_then(Value::as_object) {
        for creation_id in create.keys() {
            let error = SetError::new("singleton", "there is only one ProfilePicture object");
            response.not_created.insert(creation_id.clone(), error.to_json());
        }
    }
    if let Some(updates) = args.get("update").and_then(Value::as_object) {
        for (id, patch) in updates {
            if id != ID {
                let error = SetError::new("singleton", "the only ProfilePicture object is `singleton`");
                response.not_updated.insert(id.clone(), error.to_json());
                continue;
            }
            let current = store.profile_settings(ctx.account.id).await?;
            match update(ctx, patch, &current).await {
                Ok(changed) => {
                    response.updated.insert(id.clone(), changed.unwrap_or(Value::Null));
                }
                Err(err) => {
                    response.not_updated.insert(id.clone(), err.to_json());
                }
            }
        }
    }
    if let Some(destroy) = args.get("destroy").and_then(Value::as_array) {
        for id in destroy.iter().filter_map(Value::as_str) {
            let error = SetError::new("singleton", "ProfilePicture cannot be destroyed; set blobId to null");
            response.not_destroyed.insert(id.to_owned(), error.to_json());
        }
    }
    let new_state = store.profile_settings(ctx.account.id).await?.state.to_string();
    Ok(response.finish(ctx.account_id(), old_state, new_state))
}

/// The account's own picture for a blob download, when `hash` is it.
pub async fn own_picture_blob(store: &uwumail_store::Store, account_id: i64, hash: &str) -> Option<(Vec<u8>, String)> {
    let picture = store.picture(PictureOwner::Account(account_id)).await.ok().flatten()?;
    (picture.hash == hash).then_some((picture.bytes, picture.media_type))
}

/// The capability for an account: the largest upload, and whether its picture may be public.
pub async fn capability(store: &uwumail_store::Store, account_id: i64) -> Value {
    let may_be_public = match store.profile_settings(account_id).await {
        Ok(settings) => settings.may_be_public,
        Err(err) => {
            tracing::warn!(%err, account_id, "reading the picture settings failed");
            false
        }
    };
    json!({ "maxSize": profile_pictures::MAX_UPLOAD_BYTES, "mayBePublic": may_be_public })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_name_only_known_properties() {
        let patch = |value: Value| parse_patch(value.as_object().unwrap());
        assert!(patch(json!({ "visibility": "public", "sendFace": true, "blobId": null })).is_ok());
        let err = patch(json!({ "visibility": "everyone", "colour": "pink" })).err().unwrap();
        assert_eq!(err.properties.unwrap(), ["colour", "visibility"]);
        assert!(patch(json!({ "sendFace": "yes" })).is_err());
        assert!(patch(json!({ "type": "image/png" })).is_err(), "set by the server");
    }
}
