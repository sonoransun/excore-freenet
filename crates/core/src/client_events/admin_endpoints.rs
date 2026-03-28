//! Admin REST API endpoints for node management, health checks, and metrics.
//!
//! These endpoints provide programmatic access to node status for production
//! deployment, monitoring, and orchestration (e.g., Kubernetes probes).
//!
//! All endpoints are mounted under `/admin/` and return JSON responses.

use axum::{response::IntoResponse, routing::get, Json, Router};
use serde::Serialize;

use crate::node::network_status::{self, HealthLevel};
use crate::transport::metrics::TRANSPORT_METRICS;

/// Build the admin API router. All routes are prefixed with `/admin`.
pub(crate) fn admin_router() -> Router {
    Router::new()
        .route("/admin/health", get(health))
        .route("/admin/ready", get(ready))
        .route("/admin/metrics", get(metrics))
        .route("/admin/node/status", get(node_status))
        .route("/admin/network/peers", get(network_peers))
        .route("/admin/config", get(config_endpoint))
}

// ---------------------------------------------------------------------------
// GET /admin/health
// ---------------------------------------------------------------------------

/// Liveness probe: returns 200 if the node process is running and the network
/// status subsystem has been initialised. Returns 503 if the node is in trouble.
async fn health() -> impl IntoResponse {
    let snap = network_status::get_snapshot();
    let (status_code, body) = match &snap {
        Some(s) => {
            let healthy = !matches!(s.health, HealthLevel::Trouble);
            let code = if healthy {
                axum::http::StatusCode::OK
            } else {
                axum::http::StatusCode::SERVICE_UNAVAILABLE
            };
            (
                code,
                HealthResponse {
                    status: health_level_str(s.health),
                    version: s.version.clone(),
                    uptime_secs: s.elapsed_secs,
                    checks: HealthChecks {
                        network: s.open_connections > 0,
                        has_peers: !s.peers.is_empty(),
                    },
                },
            )
        }
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            HealthResponse {
                status: "initializing".to_string(),
                version: String::new(),
                uptime_secs: 0,
                checks: HealthChecks {
                    network: false,
                    has_peers: false,
                },
            },
        ),
    };
    (status_code, Json(body))
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    version: String,
    uptime_secs: u64,
    checks: HealthChecks,
}

#[derive(Serialize)]
struct HealthChecks {
    network: bool,
    has_peers: bool,
}

// ---------------------------------------------------------------------------
// GET /admin/ready
// ---------------------------------------------------------------------------

/// Readiness probe: returns 200 only when the node has at least one peer
/// connection and is considered healthy or degraded (but functional).
/// Returns 503 while still connecting or in trouble.
async fn ready() -> impl IntoResponse {
    let snap = network_status::get_snapshot();
    let is_ready = snap
        .as_ref()
        .is_some_and(|s| matches!(s.health, HealthLevel::Healthy | HealthLevel::Degraded));
    let code = if is_ready {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    };
    let body = ReadyResponse { ready: is_ready };
    (code, Json(body))
}

#[derive(Serialize)]
struct ReadyResponse {
    ready: bool,
}

// ---------------------------------------------------------------------------
// GET /admin/metrics
// ---------------------------------------------------------------------------

/// Prometheus-compatible metrics export in text exposition format.
async fn metrics() -> impl IntoResponse {
    let mut out = String::with_capacity(2048);

    // --- Network status metrics ---
    if let Some(snap) = network_status::get_snapshot() {
        push_metric(&mut out, "freenet_up", "gauge", "Whether the node is up", 1);
        push_metric(
            &mut out,
            "freenet_uptime_seconds",
            "gauge",
            "Node uptime in seconds",
            snap.elapsed_secs,
        );
        push_metric(
            &mut out,
            "freenet_peers_connected",
            "gauge",
            "Number of connected peers",
            snap.open_connections,
        );
        push_metric(
            &mut out,
            "freenet_connection_attempts_total",
            "counter",
            "Total connection attempts",
            snap.connection_attempts,
        );

        // Operation counters
        push_labeled_pair(
            &mut out,
            "freenet_operations",
            "counter",
            "Operation counts by type and result",
            "get",
            snap.op_stats.gets,
        );
        push_labeled_pair(
            &mut out,
            "freenet_operations",
            "counter",
            "",
            "put",
            snap.op_stats.puts,
        );
        push_labeled_pair(
            &mut out,
            "freenet_operations",
            "counter",
            "",
            "update",
            snap.op_stats.updates,
        );
        push_labeled_pair(
            &mut out,
            "freenet_operations",
            "counter",
            "",
            "subscribe",
            snap.op_stats.subscribes,
        );
        push_metric(
            &mut out,
            "freenet_updates_received_total",
            "counter",
            "Broadcast updates received via subscriptions",
            snap.op_stats.updates_received,
        );

        // NAT stats
        push_metric(
            &mut out,
            "freenet_nat_attempts_total",
            "counter",
            "Total NAT traversal attempts",
            snap.nat_stats.attempts,
        );
        push_metric(
            &mut out,
            "freenet_nat_successes_total",
            "counter",
            "Total NAT traversal successes",
            snap.nat_stats.successes,
        );

        // Health
        let health_val: u8 = match snap.health {
            HealthLevel::Healthy => 0,
            HealthLevel::Degraded => 1,
            HealthLevel::Connecting => 2,
            HealthLevel::Trouble => 3,
        };
        push_metric(
            &mut out,
            "freenet_health_status",
            "gauge",
            "Node health (0=healthy, 1=degraded, 2=connecting, 3=trouble)",
            health_val,
        );
    } else {
        push_metric(&mut out, "freenet_up", "gauge", "Whether the node is up", 0);
    }

    // --- Transport metrics (cumulative, non-resetting) ---
    push_metric(
        &mut out,
        "freenet_bytes_sent_total",
        "counter",
        "Cumulative bytes sent",
        TRANSPORT_METRICS.cumulative_bytes_sent(),
    );
    push_metric(
        &mut out,
        "freenet_bytes_received_total",
        "counter",
        "Cumulative bytes received",
        TRANSPORT_METRICS.cumulative_bytes_received(),
    );

    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        out,
    )
}

fn push_metric(out: &mut String, name: &str, typ: &str, help: &str, value: impl std::fmt::Display) {
    if !help.is_empty() {
        out.push_str(&format!("# HELP {name} {help}\n"));
        out.push_str(&format!("# TYPE {name} {typ}\n"));
    }
    out.push_str(&format!("{name} {value}\n"));
}

fn push_labeled_pair(
    out: &mut String,
    name: &str,
    typ: &str,
    help: &str,
    op: &str,
    (success, failure): (u32, u32),
) {
    if !help.is_empty() {
        out.push_str(&format!("# HELP {name} {help}\n"));
        out.push_str(&format!("# TYPE {name} {typ}\n"));
    }
    out.push_str(&format!(
        "{name}{{op=\"{op}\",result=\"success\"}} {success}\n"
    ));
    out.push_str(&format!(
        "{name}{{op=\"{op}\",result=\"failure\"}} {failure}\n"
    ));
}

// ---------------------------------------------------------------------------
// GET /admin/node/status
// ---------------------------------------------------------------------------

/// Detailed node status including connections, location, operation stats.
async fn node_status() -> impl IntoResponse {
    let snap = network_status::get_snapshot();
    match snap {
        Some(s) => {
            let body = NodeStatusResponse {
                version: s.version.clone(),
                uptime_secs: s.elapsed_secs,
                health: health_level_str(s.health),
                listening_port: s.listening_port,
                open_connections: s.open_connections,
                own_location: s.own_location,
                external_address: s.external_address.map(|a| a.to_string()),
                gateway_only: s.gateway_only,
                bytes_uploaded: s.bytes_uploaded,
                bytes_downloaded: s.bytes_downloaded,
                subscribed_contracts: s.contracts.len() as u32,
                op_stats: OpStats {
                    gets: s.op_stats.gets.into(),
                    puts: s.op_stats.puts.into(),
                    updates: s.op_stats.updates.into(),
                    subscribes: s.op_stats.subscribes.into(),
                    updates_received: s.op_stats.updates_received,
                },
                nat_stats: NatStats {
                    attempts: s.nat_stats.attempts,
                    successes: s.nat_stats.successes,
                    recent_attempts: s.nat_stats.recent_attempts,
                    recent_successes: s.nat_stats.recent_successes,
                },
            };
            (axum::http::StatusCode::OK, Json(body)).into_response()
        }
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "node not yet initialized"})),
        )
            .into_response(),
    }
}

#[derive(Serialize)]
struct NodeStatusResponse {
    version: String,
    uptime_secs: u64,
    health: String,
    listening_port: u16,
    open_connections: u32,
    own_location: Option<f64>,
    external_address: Option<String>,
    gateway_only: bool,
    bytes_uploaded: u64,
    bytes_downloaded: u64,
    subscribed_contracts: u32,
    op_stats: OpStats,
    nat_stats: NatStats,
}

#[derive(Serialize)]
struct OpStats {
    gets: OpCounter,
    puts: OpCounter,
    updates: OpCounter,
    subscribes: OpCounter,
    updates_received: u32,
}

#[derive(Serialize)]
struct OpCounter {
    success: u32,
    failure: u32,
}

impl From<(u32, u32)> for OpCounter {
    fn from((success, failure): (u32, u32)) -> Self {
        Self { success, failure }
    }
}

#[derive(Serialize)]
struct NatStats {
    attempts: u32,
    successes: u32,
    recent_attempts: u32,
    recent_successes: u32,
}

// ---------------------------------------------------------------------------
// GET /admin/network/peers
// ---------------------------------------------------------------------------

/// Connected peer information with quality metrics.
async fn network_peers() -> impl IntoResponse {
    let snap = network_status::get_snapshot();
    match snap {
        Some(s) => {
            let peers: Vec<PeerInfo> = s
                .peers
                .iter()
                .map(|p| PeerInfo {
                    address: p.address.to_string(),
                    is_gateway: p.is_gateway,
                    location: p.location,
                    connected_secs: p.connected_secs,
                    bytes_sent: p.bytes_sent,
                    bytes_received: p.bytes_received,
                })
                .collect();
            let body = NetworkPeersResponse {
                total: peers.len() as u32,
                peers,
            };
            (axum::http::StatusCode::OK, Json(body)).into_response()
        }
        None => (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "node not yet initialized"})),
        )
            .into_response(),
    }
}

#[derive(Serialize)]
struct NetworkPeersResponse {
    total: u32,
    peers: Vec<PeerInfo>,
}

#[derive(Serialize)]
struct PeerInfo {
    address: String,
    is_gateway: bool,
    location: Option<f64>,
    connected_secs: u64,
    bytes_sent: u64,
    bytes_received: u64,
}

// ---------------------------------------------------------------------------
// GET /admin/config
// ---------------------------------------------------------------------------

/// Read-only view of current runtime configuration.
async fn config_endpoint() -> impl IntoResponse {
    let snap = network_status::get_snapshot();
    let body = ConfigResponse {
        listening_port: snap.as_ref().map(|s| s.listening_port),
        version: snap.as_ref().map(|s| s.version.clone()),
    };
    Json(body)
}

#[derive(Serialize)]
struct ConfigResponse {
    listening_port: Option<u16>,
    version: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn health_level_str(level: HealthLevel) -> String {
    match level {
        HealthLevel::Healthy => "healthy".to_string(),
        HealthLevel::Degraded => "degraded".to_string(),
        HealthLevel::Connecting => "connecting".to_string(),
        HealthLevel::Trouble => "trouble".to_string(),
    }
}
