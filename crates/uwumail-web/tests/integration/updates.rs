//! The update centre: one run over the steps that wait, in order, stopping where one fails.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::vpn::{call, login, portal};

fn states(view: &Value) -> Vec<(&str, &str)> {
    view["run"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| (step["kind"].as_str().unwrap(), step["state"].as_str().unwrap()))
        .collect()
}

#[tokio::test]
async fn a_run_takes_the_steps_in_order_and_stops_where_one_fails() {
    let (app, host, _dir) = portal(&["os-update", "reboot", "uwumail-update", "helper-update"]).await;
    let auth = login(&app).await;
    let path = "/api/admin/updates";

    // Packages wait and the helper is an old one; nobody knows of a newer server. A restart is
    // possible, and never part of "everything".
    let (_, view) = call(&app, "GET", path, None, &auth).await;
    assert_eq!(view["plan"], json!(["hostSystem", "helper"]));
    assert_eq!(view["possible"], json!(["hostSystem", "helper", "server", "hostReboot"]));
    assert!(view["run"].is_null());

    let (status, view) = call(&app, "POST", "/api/admin/updates/run", Some(json!({})), &auth).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(states(&view), [("hostSystem", "running"), ("helper", "pending")]);
    assert_eq!(host.asked.lock().unwrap().as_slice(), ["os-update"]);
    let (status, refused) = call(&app, "POST", "/api/admin/updates/run", Some(json!({})), &auth).await;
    assert_eq!((status, refused["code"].as_str()), (StatusCode::CONFLICT, Some("updateRefused")));

    // The helper is done: the next step starts by itself, and only then.
    *host.job.lock().unwrap() = Some(("done", ""));
    let (_, view) = call(&app, "GET", path, None, &auth).await;
    assert_eq!(states(&view), [("hostSystem", "done"), ("helper", "running")]);
    assert_eq!(view["run"]["steps"][0]["log"], "the job's log");
    assert_eq!(host.asked.lock().unwrap().as_slice(), ["os-update", "helper-update"]);

    // A step that fails stops the run until the admin says how to go on.
    *host.job.lock().unwrap() = Some(("failed", "no network"));
    let (_, view) = call(&app, "GET", path, None, &auth).await;
    assert_eq!(states(&view)[1], ("helper", "failed"));
    assert_eq!(view["run"]["steps"][1]["error"], "no network");
    assert!(view["run"]["finishedAt"].is_null());
    let (_, view) = call(&app, "POST", "/api/admin/updates/run/step", Some(json!({ "action": "skip" })), &auth).await;
    assert_eq!(states(&view)[1], ("helper", "skipped"));
    assert!(view["run"]["finishedAt"].is_i64());

    // The server's update follows the channel's tag. When the compose file is in the way, the
    // portal's answer goes along with the second try.
    let body = json!({ "steps": ["server"] });
    let (_, view) = call(&app, "POST", "/api/admin/updates/run", Some(body), &auth).await;
    assert_eq!(states(&view), [("server", "running")]);
    assert_eq!(host.options.lock().unwrap().as_slice(), ["channel=edge"]);
    *host.job.lock().unwrap() = Some(("failed", "compose"));
    let (_, view) = call(&app, "GET", path, None, &auth).await;
    assert_eq!(view["run"]["steps"][0]["reason"], "compose");
    let answer = json!({ "action": "retry", "compose": "anything" });
    let (status, _) = call(&app, "POST", "/api/admin/updates/run/step", Some(answer), &auth).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let answer = json!({ "action": "retry", "compose": "replace" });
    let (_, view) = call(&app, "POST", "/api/admin/updates/run/step", Some(answer), &auth).await;
    assert_eq!(states(&view), [("server", "running")]);
    assert_eq!(host.options.lock().unwrap().as_slice(), ["channel=edge", "compose=replace"]);
    *host.job.lock().unwrap() = Some(("done", ""));
    let (_, view) = call(&app, "GET", path, None, &auth).await;
    assert!(view["run"]["finishedAt"].is_i64());

    // What no helper here can do is refused before anything starts.
    let body = json!({ "steps": ["gatewayReboot"] });
    let (status, _) = call(&app, "POST", "/api/admin/updates/run", Some(body), &auth).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(host.asked.lock().unwrap().len(), 4);

    // A helper from before the portal's updates cannot do it, and is not asked.
    let (app, host, _dir) = portal(&["os-update", "reboot"]).await;
    let auth = login(&app).await;
    let body = json!({ "steps": ["server"] });
    let (status, _) = call(&app, "POST", "/api/admin/updates/run", Some(body), &auth).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(host.asked.lock().unwrap().is_empty());
}
