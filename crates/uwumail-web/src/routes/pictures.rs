//! Profile pictures in the portal (docs/profile-pictures.md): one's own under My account; for admins
//! the pictures of services, shared mailboxes and groups, a logo per domain, and whether pictures
//! may be public at all.
//!
//! Every upload is decoded, cut square, scaled and written anew on the server, whatever the browser
//! did with it before; the file itself is the request body.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use uwumail_smtp::profile_pictures::{self, PictureError};
use uwumail_store::{Account, NewPicture, PictureMeta, PictureOwner, PictureVisibility, ProfileUpdate, StoredPicture};

use super::audit;
use crate::Web;
use crate::error::{ApiError, ApiResult};
use crate::session::{Admin, Session};

/// The largest upload taken; the portal sends a cropped picture far below it.
pub const MAX_PICTURE_BYTES: usize = profile_pictures::MAX_UPLOAD_BYTES;

/// A picture for the app: where to load it (changing with the picture), its type, size and date.
fn picture_json(meta: Option<&PictureMeta>, file_url: &str) -> Value {
    match meta {
        Some(meta) => json!({
            "url": format!("{file_url}?v={}", &meta.hash[..16.min(meta.hash.len())]),
            "type": meta.media_type,
            "size": meta.size,
            "updatedAt": meta.updated_at,
        }),
        None => Value::Null,
    }
}

/// Decodes and writes anew what was uploaded. The codes are the ones the app explains.
async fn prepare(body: Bytes) -> ApiResult<NewPicture> {
    match profile_pictures::prepare_upload(body.to_vec()).await {
        Ok(prepared) => Ok(NewPicture {
            bytes: prepared.bytes,
            media_type: prepared.media_type.to_owned(),
            face: Some(prepared.face),
        }),
        Err(PictureError::Empty) => Err(ApiError::Rule("logoEmpty", "the file is empty".into())),
        Err(PictureError::TooLarge) => Err(ApiError::Rule("pictureTooLarge", "at most 10 MB".into())),
        Err(PictureError::TooManyPixels) => {
            Err(ApiError::Rule("pictureTooManyPixels", "at most 8000 × 8000 pixels".into()))
        }
        Err(PictureError::NotAPicture | PictureError::Broken) => {
            Err(ApiError::Rule("pictureType", "PNG, JPEG, WebP or GIF".into()))
        }
    }
}

/// The picture itself, never shown as a page of this origin.
fn file_response(picture: Option<StoredPicture>) -> Response {
    let Some(picture) = picture else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = Response::new(axum::body::Body::from(picture.bytes));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&picture.media_type).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    // The address carries the picture's hash, so a new picture is a new address.
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable"));
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("default-src 'none'; sandbox"));
    response
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Visibility {
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    send_face: Option<bool>,
}

fn visibility(value: Option<&str>) -> ApiResult<Option<PictureVisibility>> {
    value
        .map(|value| {
            PictureVisibility::parse(value)
                .ok_or_else(|| ApiError::Invalid("visibility is off, server or public".into()))
        })
        .transpose()
}

async fn account_json(web: &Web, account_id: i64, file_url: &str) -> ApiResult<Value> {
    let settings = web.store().profile_settings(account_id).await?;
    Ok(json!({
        "picture": picture_json(settings.picture.as_ref(), file_url),
        "visibility": settings.effective_visibility(),
        "sendFace": settings.send_face,
        "mayBePublic": settings.may_be_public,
    }))
}

const OWN_FILE: &str = "/api/account/picture/file";

pub async fn own(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    Ok(Json(account_json(&web, session.account.id, OWN_FILE).await?))
}

pub async fn own_file(State(web): State<Web>, session: Session) -> ApiResult<Response> {
    Ok(file_response(web.store().picture(PictureOwner::Account(session.account.id)).await?))
}

pub async fn upload_own(State(web): State<Web>, session: Session, body: Bytes) -> ApiResult<Json<Value>> {
    let picture = prepare(body).await?;
    let update = ProfileUpdate { picture: Some(Some(picture)), ..Default::default() };
    web.store().update_profile(session.account.id, update).await?;
    Ok(Json(account_json(&web, session.account.id, OWN_FILE).await?))
}

pub async fn remove_own(State(web): State<Web>, session: Session) -> ApiResult<Json<Value>> {
    let update = ProfileUpdate { picture: Some(None), ..Default::default() };
    web.store().update_profile(session.account.id, update).await?;
    Ok(Json(account_json(&web, session.account.id, OWN_FILE).await?))
}

pub async fn change_own(
    State(web): State<Web>,
    session: Session,
    Json(change): Json<Visibility>,
) -> ApiResult<Json<Value>> {
    let update = ProfileUpdate {
        picture: None,
        visibility: visibility(change.visibility.as_deref())?,
        send_face: change.send_face,
    };
    web.store().update_profile(session.account.id, update).await?;
    Ok(Json(account_json(&web, session.account.id, OWN_FILE).await?))
}

/// A service or shared mailbox, whose picture its admins choose. People choose their own.
async fn service(web: &Web, login: &str) -> ApiResult<Account> {
    let account = web.store().account(login).await?.ok_or_else(|| ApiError::NotFound(format!("account {login}")))?;
    if !account.is_service() {
        return Err(ApiError::Rule("picturePerson", "people choose their own picture".into()));
    }
    Ok(account)
}

fn service_file(login: &str) -> String {
    format!("/api/admin/people/{}/picture/file", urlencode(login))
}

/// Enough percent-encoding for an address in a path.
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' | b'@' | b'+' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub async fn service_picture(
    State(web): State<Web>,
    _admin: Admin,
    Path(login): Path<String>,
) -> ApiResult<Json<Value>> {
    let account = service(&web, &login).await?;
    Ok(Json(account_json(&web, account.id, &service_file(&account.login)).await?))
}

pub async fn service_file_get(State(web): State<Web>, _admin: Admin, Path(login): Path<String>) -> ApiResult<Response> {
    let account = service(&web, &login).await?;
    Ok(file_response(web.store().picture(PictureOwner::Account(account.id)).await?))
}

pub async fn upload_service(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let account = service(&web, &login).await?;
    let picture = prepare(body).await?;
    let details = json!({ "type": picture.media_type, "bytes": picture.bytes.len() });
    web.store().set_picture(PictureOwner::Account(account.id), Some(picture)).await?;
    audit(&web, &session, "person.picture", &account.login, details).await;
    Ok(Json(account_json(&web, account.id, &service_file(&account.login)).await?))
}

pub async fn remove_service(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
) -> ApiResult<Json<Value>> {
    let account = service(&web, &login).await?;
    if web.store().set_picture(PictureOwner::Account(account.id), None).await? {
        audit(&web, &session, "person.pictureRemoved", &account.login, json!({})).await;
    }
    Ok(Json(account_json(&web, account.id, &service_file(&account.login)).await?))
}

pub async fn change_service(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(login): Path<String>,
    Json(change): Json<Visibility>,
) -> ApiResult<Json<Value>> {
    let account = service(&web, &login).await?;
    if change.send_face.is_some() {
        return Err(ApiError::Invalid("the Face header is for people".into()));
    }
    let chosen = visibility(change.visibility.as_deref())?;
    let update = ProfileUpdate { visibility: chosen, ..Default::default() };
    web.store().update_profile(account.id, update).await?;
    if let Some(chosen) = chosen {
        audit(&web, &session, "person.pictureVisibility", &account.login, json!({ "visibility": chosen })).await;
    }
    Ok(Json(account_json(&web, account.id, &service_file(&account.login)).await?))
}

/// A group of a domain, by the part before the @.
async fn group(web: &Web, name: &str, local: &str) -> ApiResult<(i64, String)> {
    let domain = super::domains::load(web, name).await?;
    let address = format!("{local}@{}", domain.name);
    let group =
        web.store().group_delivery(&address).await?.ok_or_else(|| ApiError::NotFound(format!("group {address}")))?;
    Ok((group.id, group.address))
}

async fn group_json(web: &Web, id: i64, address: &str) -> ApiResult<Value> {
    let state = web.store().group_picture(id).await?;
    let (local, domain) = address.rsplit_once('@').unwrap_or((address, ""));
    let file = format!("/api/admin/domains/{}/groups/{}/picture/file", urlencode(domain), urlencode(local));
    Ok(json!({
        "picture": picture_json(state.picture.as_ref(), &file),
        "visibility": match state.visibility {
            PictureVisibility::Public if !state.may_be_public => PictureVisibility::Server,
            other => other,
        },
        "mayBePublic": state.may_be_public,
    }))
}

pub async fn group_picture(
    State(web): State<Web>,
    _admin: Admin,
    Path((name, local)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let (id, address) = group(&web, &name, &local).await?;
    Ok(Json(group_json(&web, id, &address).await?))
}

pub async fn group_file(
    State(web): State<Web>,
    _admin: Admin,
    Path((name, local)): Path<(String, String)>,
) -> ApiResult<Response> {
    let (id, _) = group(&web, &name, &local).await?;
    Ok(file_response(web.store().picture(PictureOwner::Group(id)).await?))
}

pub async fn upload_group(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((name, local)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let (id, address) = group(&web, &name, &local).await?;
    let picture = prepare(body).await?;
    let details = json!({ "type": picture.media_type, "bytes": picture.bytes.len() });
    web.store().set_picture(PictureOwner::Group(id), Some(picture)).await?;
    audit(&web, &session, "group.picture", &address, details).await;
    Ok(Json(group_json(&web, id, &address).await?))
}

pub async fn remove_group(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((name, local)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let (id, address) = group(&web, &name, &local).await?;
    if web.store().set_picture(PictureOwner::Group(id), None).await? {
        audit(&web, &session, "group.pictureRemoved", &address, json!({})).await;
    }
    Ok(Json(group_json(&web, id, &address).await?))
}

pub async fn change_group(
    State(web): State<Web>,
    Admin(session): Admin,
    Path((name, local)): Path<(String, String)>,
    Json(change): Json<Visibility>,
) -> ApiResult<Json<Value>> {
    let (id, address) = group(&web, &name, &local).await?;
    if change.send_face.is_some() {
        return Err(ApiError::Invalid("the Face header is for people".into()));
    }
    if let Some(chosen) = visibility(change.visibility.as_deref())? {
        web.store().set_group_picture_visibility(id, chosen).await?;
        audit(&web, &session, "group.pictureVisibility", &address, json!({ "visibility": chosen })).await;
    }
    Ok(Json(group_json(&web, id, &address).await?))
}

async fn domain_json(web: &Web, id: i64, name: &str) -> ApiResult<Value> {
    let logo = web.store().domain_logo(id).await?;
    let file = format!("/api/admin/domains/{}/logo/file", urlencode(name));
    Ok(json!({
        "picture": picture_json(logo.as_ref(), &file),
        "publicPictures": web.store().domain_public_pictures(id).await?,
        "serverAllowsPublic": web.store().public_pictures_allowed().await?,
    }))
}

pub async fn domain_logo(State(web): State<Web>, _admin: Admin, Path(name): Path<String>) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    Ok(Json(domain_json(&web, domain.id, &domain.name).await?))
}

pub async fn domain_logo_file(State(web): State<Web>, _admin: Admin, Path(name): Path<String>) -> ApiResult<Response> {
    let domain = super::domains::load(&web, &name).await?;
    Ok(file_response(web.store().picture(PictureOwner::Domain(domain.id)).await?))
}

pub async fn upload_domain_logo(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    let mut picture = prepare(body).await?;
    picture.face = None;
    let details = json!({ "type": picture.media_type, "bytes": picture.bytes.len() });
    web.store().set_picture(PictureOwner::Domain(domain.id), Some(picture)).await?;
    audit(&web, &session, "domain.logo", &domain.name, details).await;
    Ok(Json(domain_json(&web, domain.id, &domain.name).await?))
}

pub async fn remove_domain_logo(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    if web.store().set_picture(PictureOwner::Domain(domain.id), None).await? {
        audit(&web, &session, "domain.logoRemoved", &domain.name, json!({})).await;
    }
    Ok(Json(domain_json(&web, domain.id, &domain.name).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Allowed {
    allowed: bool,
}

pub async fn set_domain_public(
    State(web): State<Web>,
    Admin(session): Admin,
    Path(name): Path<String>,
    Json(change): Json<Allowed>,
) -> ApiResult<Json<Value>> {
    let domain = super::domains::load(&web, &name).await?;
    web.store().set_domain_public_pictures(domain.id, change.allowed).await?;
    audit(&web, &session, "domain.publicPictures", &domain.name, json!({ "allowed": change.allowed })).await;
    Ok(Json(domain_json(&web, domain.id, &domain.name).await?))
}

pub async fn server(State(web): State<Web>, _admin: Admin) -> ApiResult<Json<Value>> {
    Ok(Json(json!({ "publicAllowed": web.store().public_pictures_allowed().await? })))
}

pub async fn set_server(
    State(web): State<Web>,
    Admin(session): Admin,
    Json(change): Json<Allowed>,
) -> ApiResult<Json<Value>> {
    web.store().set_public_pictures_allowed(change.allowed).await?;
    audit(&web, &session, "pictures.public", "", json!({ "allowed": change.allowed })).await;
    Ok(Json(json!({ "publicAllowed": web.store().public_pictures_allowed().await? })))
}
