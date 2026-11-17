use std::collections::HashMap;
use std::io::Read;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    extract::{State, WebSocketUpgrade, ConnectInfo},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rcgen::{Certificate, CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    net::TcpListener,
    signal,
    sync::{mpsc, RwLock},
    task,
};
use tokio_rustls::TlsAcceptor;
use tower_http::cors::{Any, CorsLayer};
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use hyper_util::service::TowerToHyperService;
use hyper_util::rt::TokioIo;

// ============================================================================
// Configuration
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BridgeConfig {
    pub http_port: u16,
    pub ws_port: u16,
    pub tls_port: u16,
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
    pub jwt_secret: String,
    pub allowed_origins: Vec<String>,
    pub pty_rows: u16,
    pub pty_cols: u16,
    pub shell: String,
    pub shell_args: Vec<String>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            http_port: 8443,
            ws_port: 8444,
            tls_port: 8445,
            cert_path: None,
            key_path: None,
            jwt_secret: "tcc-dev-secret-change-in-production".to_string(),
            allowed_origins: vec!["*".to_string()],
            pty_rows: 40,
            pty_cols: 120,
            shell: "/bin/bash".to_string(),
            shell_args: vec!["-i".to_string()],
        }
    }
}

// ============================================================================
// JSON-RPC Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<serde_json::Value>,
    pub method: String,
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// ============================================================================
// Bridge State
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionInfo {
    pub id: String,
    pub pid: u32,
    pub created_at: u64,
    pub shell: String,
    pub rows: u16,
    pub cols: u16,
}

type Sessions = Arc<RwLock<HashMap<String, SessionInfo>>>;

#[derive(Clone)]
struct BridgeState {
    config: BridgeConfig,
    sessions: Sessions,
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

// ============================================================================
// PTY Session Management
// ============================================================================

struct PtySession {
    id: String,
    master: Box<dyn portable_pty::MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    // Channel for writing TO the PTY (stdin)
    stdin_tx: mpsc::UnboundedSender<Vec<u8>>,
    // For stdout, we'll read directly in the WebSocket task to avoid channel wake issues
    master_reader: Arc<tokio::sync::Mutex<Box<dyn Read + Send>>>,
}

// Make PtySession Send + Sync by ensuring MasterPty is wrapped properly
unsafe impl Send for PtySession {}
unsafe impl Sync for PtySession {}

impl PtySession {
    async fn spawn(config: &BridgeConfig) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: config.pty_rows,
            cols: config.pty_cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(&config.shell);
        cmd.args(&config.shell_args);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let child = pair.slave.spawn_command(cmd)?;
        let session_id = Uuid::new_v4().to_string();

        // Channel for stdin (input to PTY)
        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel();

        // Clone master for reading (we'll use it directly in handle_ws)
        let master_reader = Arc::new(tokio::sync::Mutex::new(pair.master.try_clone_reader().unwrap()));

        let session = Self {
            id: session_id,
            master: pair.master,
            _child: child,
            stdin_tx,
            master_reader,
        };

        // Spawn PTY stdin writer task: reads from stdin_rx -> writes to PTY master
        let mut master_writer = session.master.take_writer().unwrap();
        task::spawn(async move {
            let mut stdin_rx = stdin_rx;
            while let Some(data) = stdin_rx.recv().await {
                debug!("PTY writer: writing {} bytes", data.len());
                if let Err(e) = std::io::Write::write_all(&mut master_writer, &data) {
                    debug!("PTY writer: error: {}", e);
                    break;
                }
            }
        });

        Ok(session)
    }

    fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    async fn write_stdin(&self, data: &[u8]) -> Result<()> {
        self.stdin_tx.send(data.to_vec())?;
        Ok(())
    }

    async fn read_stdout(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut reader = self.master_reader.lock().await;
        std::io::Read::read(&mut *reader, buf)
    }
}

// ============================================================================
// Certificate Generation
// ============================================================================

fn generate_self_signed_cert() -> Result<(Vec<u8>, Vec<u8>)> {
    let mut params = CertificateParams::new(vec!["tcc-bridge.local".to_string()]);
    params.distinguished_name = DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, "tcc-bridge.local");
    params.distinguished_name.push(DnType::OrganizationName, "TCC");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];

    // Generate key pair and certificate
    let key_pair = KeyPair::generate(&PKCS_ECDSA_P256_SHA256)?;
    params.alg = &PKCS_ECDSA_P256_SHA256;
    params.key_pair = Some(key_pair);
    let cert = Certificate::from_params(params)?;

    let cert_der = cert.serialize_der()?;
    let key_der = cert.get_key_pair().serialize_der();

    Ok((cert_der, key_der))
}

fn load_or_generate_certs(config: &BridgeConfig) -> Result<(Vec<u8>, Vec<u8>)> {
    if let (Some(cert_path), Some(key_path)) = (&config.cert_path, &config.key_path) {
        if cert_path.exists() && key_path.exists() {
            let cert_der = std::fs::read(cert_path)?;
            let key_der = std::fs::read(key_path)?;
            return Ok((cert_der, key_der));
        }
    }
    info!("Generating self-signed certificate...");
    generate_self_signed_cert()
}

fn make_tls_config(cert_der: &[u8], key_der: &[u8]) -> Result<ServerConfig> {
    let certs = vec![CertificateDer::from(cert_der.to_vec())];
    let key = PrivateKeyDer::try_from(key_der.to_vec())
        .map_err(|e| anyhow::anyhow!("Failed to parse private key: {}", e))?;

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;

    Ok(config)
}

// ============================================================================
// HTTP Handlers
// ============================================================================

async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "tcc-bridge",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

async fn json_rpc_handler(
    State(state): State<BridgeState>,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    debug!("JSON-RPC: {} {:?}", req.method, req.params);

    let response = match req.method.as_str() {
        "bridge.create_session" => {
            let params: serde_json::Value = req.params.unwrap_or_default();
            let rows = params.get("rows").and_then(|v| v.as_u64()).unwrap_or(40) as u16;
            let cols = params.get("cols").and_then(|v| v.as_u64()).unwrap_or(120) as u16;

            match PtySession::spawn(&state.config).await {
                Ok(session) => {
                    let session_id = session.id.clone();
                    let info = SessionInfo {
                        id: session_id.clone(),
                        pid: 0,
                        created_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs(),
                        shell: state.config.shell.clone(),
                        rows,
                        cols,
                    };

                    state.sessions.write().await.insert(session_id.clone(), info.clone());

                    Json(JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: req.id,
                        result: Some(serde_json::to_value(info).unwrap()),
                        error: None,
                    }).into_response()
                }
                Err(e) => Json(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req.id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32603,
                        message: format!("Failed to create session: {}", e),
                        data: None,
                    }),
                }).into_response()
            }
        }
        "bridge.list_sessions" => {
            let sessions = state.sessions.read().await;
            Json(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: Some(serde_json::to_value(sessions.values().cloned().collect::<Vec<_>>()).unwrap()),
                error: None,
            }).into_response()
        }
        "bridge.get_session" => {
            let params: serde_json::Value = req.params.unwrap_or_default();
            let session_id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let sessions = state.sessions.read().await;
            if let Some(info) = sessions.get(session_id) {
                Json(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req.id,
                    result: Some(serde_json::to_value(info).unwrap()),
                    error: None,
                }).into_response()
            } else {
                Json(JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: req.id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32602,
                        message: "Session not found".to_string(),
                        data: None,
                    }),
                }).into_response()
            }
        }
        "bridge.resize" => {
            let params: serde_json::Value = req.params.unwrap_or_default();
            let _session_id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let rows = params.get("rows").and_then(|v| v.as_u64()).unwrap_or(40) as u16;
            let cols = params.get("cols").and_then(|v| v.as_u64()).unwrap_or(120) as u16;
            Json(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: Some(serde_json::json!({"resized": true, "rows": rows, "cols": cols})),
                error: None,
            }).into_response()
        }
        "bridge.kill_session" => {
            let params: serde_json::Value = req.params.unwrap_or_default();
            let session_id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
            state.sessions.write().await.remove(session_id);
            Json(JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: req.id,
                result: Some(serde_json::json!({"killed": true})),
                error: None,
            }).into_response()
        }
        _ => Json(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: req.id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", req.method),
                data: None,
            }),
        }).into_response(),
    };

    response
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    State(state): State<BridgeState>,
) -> Response {
    let addr = connect_info.map(|c| c.0.to_string()).unwrap_or_else(|| "unknown".to_string());
    info!("WebSocket connection from {}", addr);
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(socket: axum::extract::ws::WebSocket, state: BridgeState) {
    let (mut ws_tx, mut ws_rx) = socket.split();

    // Create PTY session for this WebSocket
    let pty_session = match PtySession::spawn(&state.config).await {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to create PTY: {}", e);
            return;
        }
    };
    let pty_session = Arc::new(pty_session);

    let session_id = pty_session.id.clone();
    info!("Created PTY session: {}", session_id);

    // Track session in state
    let info = SessionInfo {
        id: session_id.clone(),
        pid: 0,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        shell: state.config.shell.clone(),
        rows: state.config.pty_rows,
        cols: state.config.pty_cols,
    };
    state.sessions.write().await.insert(session_id.clone(), info);

    // PTY -> WS task: reads directly from PTY master -> WebSocket
    let pty_session_clone = pty_session.clone();
    let mut ws_tx_clone = ws_tx;
    let pty_to_ws = tokio::spawn(async move {
        let mut buf = vec![0u8; 8192];
        loop {
            match pty_session_clone.read_stdout(&mut buf).await {
                Ok(0) => {
                    debug!("pty_to_ws: PTY EOF");
                    break;
                }
                Ok(n) => {
                    debug!("pty_to_ws: read {} bytes from PTY, forwarding to WebSocket", n);
                    if ws_tx_clone.send(axum::extract::ws::Message::Binary(buf[..n].to_vec())).await.is_err() {
                        debug!("pty_to_ws: WebSocket send error, breaking");
                        break;
                    }
                }
                Err(e) => {
                    debug!("pty_to_ws: PTY read error: {}", e);
                    break;
                }
            }
        }
        debug!("pty_to_ws: task ended");
    });

    // WS -> PTY task
    let ws_to_pty = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                axum::extract::ws::Message::Binary(data) => {
                    if pty_session.write_stdin(&data).await.is_err() {
                        break;
                    }
                }
                axum::extract::ws::Message::Text(text) => {
                    // Handle control messages
                    if let Ok(ctrl) = serde_json::from_str::<serde_json::Value>(&text) {
                        if ctrl.get("type").and_then(|v| v.as_str()) == Some("resize") {
                            let rows = ctrl.get("rows").and_then(|v| v.as_u64()).unwrap_or(40) as u16;
                            let cols = ctrl.get("cols").and_then(|v| v.as_u64()).unwrap_or(120) as u16;
                            let _ = pty_session.resize(rows, cols);
                        }
                    } else if pty_session.write_stdin(text.as_bytes()).await.is_err() {
                        break;
                    }
                }
                axum::extract::ws::Message::Close(_) => break,
                _ => {}
            }
        }
        debug!("ws_to_pty: task ended");
    });

    // Wait for either task to complete
    tokio::select! {
        _ = pty_to_ws => {},
        _ = ws_to_pty => {},
    }

    info!("WebSocket session {} closed", session_id);
    state.sessions.write().await.remove(&session_id);
}

// ============================================================================
// CLI
// ============================================================================

#[derive(Parser, Debug)]
#[command(name = "tcc-bridge", version, about = "TCC Bridge - PTY over WebSocket")]
struct Args {
    #[arg(short, long, default_value = "8443")]
    http_port: u16,

    #[arg(short, long, default_value = "8444")]
    ws_port: u16,

    #[arg(short, long, default_value = "8445")]
    tls_port: u16,

    #[arg(short, long)]
    config: Option<PathBuf>,

    #[arg(short, long)]
    cert: Option<PathBuf>,

    #[arg(short, long)]
    key: Option<PathBuf>,

    #[arg(short, long, default_value = "info")]
    log_level: String,
}

// ============================================================================
// Main
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| args.log_level.into()),
        )
        .with_target(false)
        .init();

    info!("Starting TCC Bridge v{}", env!("CARGO_PKG_VERSION"));

    // Load config
    let mut config = BridgeConfig::default();
    config.http_port = args.http_port;
    config.ws_port = args.ws_port;
    config.tls_port = args.tls_port;
    if let Some(cert) = args.cert {
        config.cert_path = Some(cert);
    }
    if let Some(key) = args.key {
        config.key_path = Some(key);
    }

    if let Some(config_path) = args.config {
        if config_path.exists() {
            let content = std::fs::read_to_string(config_path)?;
            config = toml::from_str(&content)?;
        }
    }

    // Generate or load certificates
    let (cert_der, key_der) = load_or_generate_certs(&config)?;

    // SHA256 fingerprint for pinning
    let mut hasher = Sha256::new();
    hasher.update(&cert_der);
    let fingerprint = BASE64.encode(hasher.finalize());
    info!("Certificate SHA256 (pinning): {}", fingerprint);

    let tls_config = make_tls_config(&cert_der, &key_der)?;
    let tls_acceptor = TlsAcceptor::from(Arc::new(tls_config));

    let state = BridgeState {
        config: config.clone(),
        sessions: Arc::new(RwLock::new(HashMap::new())),
        cert_der,
        key_der,
    };

    // CORS layer
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Build router
    let app = Router::new()
        .route("/health", get(health_check))
        .route("/rpc", post(json_rpc_handler))
        .route("/ws", get(ws_handler))
        .layer(cors.clone())
        .with_state(state.clone());

    // HTTP server
    let http_addr = SocketAddr::from(([0, 0, 0, 0], config.http_port));
    let http_listener = TcpListener::bind(http_addr).await?;
    info!("HTTP server listening on http://{}", http_addr);

    // WebSocket server (plain)
    let ws_addr = SocketAddr::from(([0, 0, 0, 0], config.ws_port));
    let ws_listener = TcpListener::bind(ws_addr).await?;
    info!("WebSocket server listening on ws://{}", ws_addr);

    // HTTPS/TLS server
    let tls_addr = SocketAddr::from(([0, 0, 0, 0], config.tls_port));
    let tls_listener = TcpListener::bind(tls_addr).await?;
    info!("HTTPS/WSS server listening on https://{}", tls_addr);

    // Run servers
    let http_server = tokio::spawn(async move {
        axum::serve(http_listener, app).await.unwrap();
    });

    // WS-only router for plain WS
    let ws_state = BridgeState {
        config: config.clone(),
        sessions: Arc::new(RwLock::new(HashMap::new())),
        cert_der: vec![],
        key_der: vec![],
    };
    let app_ws_only = Router::new()
        .route("/ws", get(ws_handler))
        .layer(cors)
        .with_state(ws_state);
    let ws_server = tokio::spawn(async move {
        axum::serve(ws_listener, app_ws_only).await.unwrap();
    });

    // TLS server using hyper directly with axum router
    let tls_config_arc = tls_acceptor.clone();
    let app_tls = Router::new()
        .route("/health", get(health_check))
        .route("/rpc", post(json_rpc_handler))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let tls_server = tokio::spawn(async move {
        loop {
            let (stream, addr) = tls_listener.accept().await.unwrap();
            let tls_acceptor = tls_config_arc.clone();
            let app = app_tls.clone();
            tokio::spawn(async move {
                match tls_acceptor.accept(stream).await {
                    Ok(tls_stream) => {
                        let io = TokioIo::new(tls_stream);
                        let service = TowerToHyperService::new(app);
                        let conn = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, service)
                            .await;
                        if let Err(e) = conn {
                            warn!("TLS connection error from {}: {}", addr, e);
                        }
                    }
                    Err(e) => warn!("TLS handshake error from {}: {}", addr, e),
                }
            });
        }
    });

    // Wait for shutdown signal
    signal::ctrl_c().await?;
    info!("Shutdown signal received, stopping...");

    http_server.abort();
    ws_server.abort();
    tls_server.abort();

    info!("TCC Bridge stopped");
    Ok(())
}
