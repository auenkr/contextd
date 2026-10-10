//! Read-only, loopback-only retrieval interface for personal-context agents.
use crate::Service;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    q: String,
    limit: Option<u32>,
}
async fn search(
    State(service): State<Arc<Service>>,
    Query(query): Query<Search>,
) -> Result<Json<Vec<crate::Transcript>>, (StatusCode, String)> {
    service
        .history(&query.q, query.limit.unwrap_or(20))
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
pub async fn serve(service: Arc<Service>, port: u16) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    serve_listener(service, listener).await
}
pub async fn serve_listener(
    service: Arc<Service>,
    listener: tokio::net::TcpListener,
) -> anyhow::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        anyhow::bail!("Context API must bind to loopback");
    }
    let app = Router::new()
        .route("/v1/memories", get(search))
        .route(
            "/health",
            get(|| async { Json(serde_json::json!({"status": "ok", "retrieval": "sqlite-fts5"})) }),
        )
        .with_state(service);
    axum::serve(listener, app).await?;
    Ok(())
}
