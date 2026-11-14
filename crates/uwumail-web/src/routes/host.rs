//! Server → the machine this runs on: what it has waiting.
//!
//! Only reachable when a helper is installed beside the container (`deploy/host/`). What gets
//! installed, and when the machine restarts, is asked for in one place: routes::updates.

use axum::Json;
use axum::extract::State;

use crate::Web;
use crate::host::HostView;
use crate::session::Admin;

pub async fn show(State(web): State<Web>, _admin: Admin) -> Json<HostView> {
    Json(web.host().map(|host| host.view()).unwrap_or_default())
}
