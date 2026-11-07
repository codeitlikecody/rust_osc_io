use axum::extract::ws::{Message as WebSocketMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State as AxumState};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rosc::OscType;
use rust_embed::RustEmbed;
use rust_osc_io::config::{Config, DEFAULT_MULTICAST_GROUP, NetworkMode};
use rust_osc_io::gateway::{GatewayConfig, discover_active_gateways, run_gateway};
use rust_osc_io::worker::{OscWorkerEvent, OscWorkerHandle};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::HashSet;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::sync::{Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{broadcast, watch};

const WEB_INTERFACE_PORT: u16 = 9140;

#[derive(RustEmbed)]
#[folder = "../dist/"]
struct EmbeddedFrontend;

fn application_name() -> String {
    let platform = match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        platform => platform,
    };
    format!(
        "OSCMessenger Desktop v{} ({platform})",
        env!("CARGO_PKG_VERSION")
    )
}

#[derive(Default)]
struct Session {
    worker: Option<OscWorkerHandle>,
    active_room: Option<String>,
}

struct AppState {
    session: Mutex<Session>,
    gateway: Mutex<Option<GatewayRuntime>>,
    browser_events: broadcast::Sender<BrowserEvent>,
    web_server: Mutex<Option<WebServerRuntime>>,
    web_server_control: Mutex<()>,
    web_interface_url: Mutex<Option<String>>,
}

impl Default for AppState {
    fn default() -> Self {
        let (browser_events, _) = broadcast::channel(512);
        Self {
            session: Mutex::new(Session::default()),
            gateway: Mutex::new(None),
            browser_events,
            web_server: Mutex::new(None),
            web_server_control: Mutex::new(()),
            web_interface_url: Mutex::new(None),
        }
    }
}

struct GatewayRuntime {
    shutdown: mpsc::Sender<()>,
    thread: JoinHandle<()>,
}

struct WebServerRuntime {
    port: u16,
    shutdown: watch::Sender<bool>,
    thread: JoinHandle<()>,
}

#[derive(Clone)]
struct WebServerState {
    app: AppHandle,
    events: broadcast::Sender<BrowserEvent>,
    shutdown: watch::Receiver<bool>,
    port: u16,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GatewayConflictPolicy {
    StartAnyway,
    StopLocalServer,
    NeverStartLocalServer,
}

#[derive(Clone, Serialize)]
struct GatewayStatus {
    state: String,
    gateway_uuid: Option<String>,
    detail: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "event", content = "payload")]
enum BrowserEvent {
    #[serde(rename = "worker-event")]
    Worker(UiEvent),
    #[serde(rename = "gateway-status")]
    Gateway(GatewayStatus),
}

#[derive(Serialize)]
struct ConnectionInfo {
    client_uuid: String,
}

#[derive(Serialize)]
struct LocalNetworkInterface {
    name: String,
    address: String,
    is_loopback: bool,
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum UiEvent {
    MessageSent {
        bytes: usize,
    },
    MessageReceived {
        sender: String,
        address: String,
        args: Vec<String>,
    },
    Error {
        message: String,
    },
    Stopped,
    DroppedEvents {
        total: usize,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectRequest {
    client_name: String,
    client_uuid: Option<String>,
    network_mode: String,
    multicast_group: String,
    multicast_interfaces: Vec<String>,
    listener_port: u16,
    unicast_address: String,
    unicast_port: u16,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RoomRequest {
    room_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageRequest {
    message_uuid: String,
    text: String,
    room_name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListeningRequest {
    room_name: String,
    within_room: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConfigureGatewayRequest {
    enabled: bool,
    conflict_policy: String,
    multicast_group: String,
    multicast_interfaces: Vec<String>,
    listener_port: u16,
}

fn parse_browser_request<T: DeserializeOwned>(value: Value) -> Result<T, (StatusCode, String)> {
    serde_json::from_value(value).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            format!("Invalid command payload: {error}"),
        )
    })
}

fn browser_command_result<T: Serialize>(
    result: Result<T, String>,
) -> Result<Json<Value>, (StatusCode, String)> {
    match result {
        Ok(value) => serde_json::to_value(value)
            .map(Json)
            .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string())),
        Err(error) => Err((StatusCode::BAD_REQUEST, error)),
    }
}

fn is_local_browser_request(headers: &HeaderMap, port: u16) -> bool {
    let expected_hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    let host_is_local = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| expected_hosts.iter().any(|expected| host == expected));
    let origin_is_local = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|origin| {
            origin == format!("http://127.0.0.1:{port}")
                || origin == format!("http://localhost:{port}")
        });
    host_is_local && origin_is_local
}

async fn browser_command(
    AxumState(web): AxumState<WebServerState>,
    Path(command): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, String)> {
    if !is_local_browser_request(&headers, web.port) {
        return Err((
            StatusCode::FORBIDDEN,
            "Local browser origin required".to_string(),
        ));
    }
    let app = web.app.clone();
    match command.as_str() {
        "connect" => {
            let request: ConnectRequest = parse_browser_request(payload)?;
            browser_command_result(connect(
                app.clone(),
                app.state::<AppState>(),
                request.client_name,
                request.client_uuid,
                request.network_mode,
                request.multicast_group,
                request.multicast_interfaces,
                request.listener_port,
                request.unicast_address,
                request.unicast_port,
            ))
        }
        "get_network_interfaces" => browser_command_result(get_network_interfaces()),
        "configure_gateway" => {
            let request: ConfigureGatewayRequest = parse_browser_request(payload)?;
            browser_command_result(configure_gateway(
                app,
                web.app.state::<AppState>(),
                request.enabled,
                request.conflict_policy,
                request.multicast_group,
                request.multicast_interfaces,
                request.listener_port,
            ))
        }
        "disconnect" => browser_command_result(disconnect(app.clone(), app.state::<AppState>())),
        "enter_room" => {
            let request: RoomRequest = parse_browser_request(payload)?;
            browser_command_result(enter_room(app.state::<AppState>(), request.room_name))
        }
        "exit_room" => {
            let request: RoomRequest = parse_browser_request(payload)?;
            browser_command_result(exit_room(app.state::<AppState>(), request.room_name))
        }
        "send_message" => {
            let request: MessageRequest = parse_browser_request(payload)?;
            browser_command_result(send_message(
                app.state::<AppState>(),
                request.message_uuid,
                request.text,
                request.room_name,
            ))
        }
        "query_listening" => {
            let request: ListeningRequest = parse_browser_request(payload)?;
            browser_command_result(query_listening(
                app.state::<AppState>(),
                request.room_name,
                request.within_room,
            ))
        }
        "get_web_interface_url" => {
            browser_command_result(get_web_interface_url(app.state::<AppState>()))
        }
        _ => Err((StatusCode::NOT_FOUND, "Unknown desktop command".to_string())),
    }
}

async fn browser_events(
    upgrade: WebSocketUpgrade,
    headers: HeaderMap,
    AxumState(web): AxumState<WebServerState>,
) -> Result<Response, (StatusCode, String)> {
    if !is_local_browser_request(&headers, web.port) {
        return Err((
            StatusCode::FORBIDDEN,
            "Local browser origin required".to_string(),
        ));
    }
    let events = web.events.subscribe();
    let shutdown = web.shutdown.clone();
    Ok(upgrade
        .on_upgrade(move |socket| pump_browser_events(socket, events, shutdown))
        .into_response())
}

async fn pump_browser_events(
    mut socket: WebSocket,
    mut events: broadcast::Receiver<BrowserEvent>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(WebSocketMessage::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            event = events.recv() => match event {
                Ok(event) => {
                    let Ok(json) = serde_json::to_string(&event) else { continue };
                    if socket.send(WebSocketMessage::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    let _ = socket.send(WebSocketMessage::Close(None)).await;
                    break;
                }
            }
        }
    }
}

async fn serve_web_asset(uri: Uri) -> Response {
    let requested = uri.path().trim_start_matches('/');
    let asset_path = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    let Some(asset) =
        EmbeddedFrontend::get(asset_path).or_else(|| EmbeddedFrontend::get("index.html"))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (path, bytes) = if asset_path == "index.html" || EmbeddedFrontend::get(asset_path).is_none()
    {
        let html = String::from_utf8_lossy(&asset.data).replace(
            "</head>",
            "<script>window.__OSCMESSENGER_WEB_BRIDGE__=true;</script></head>",
        );
        ("index.html", html.into_bytes())
    } else {
        (asset_path, asset.data.into_owned())
    };
    let content_type = mime_guess::from_path(path).first_or_octet_stream();
    Response::builder()
        .header(header::CONTENT_TYPE, content_type.as_ref())
        .header(header::CACHE_CONTROL, "no-cache")
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn start_web_interface(app: AppHandle) -> io::Result<WebServerRuntime> {
    let listener = (WEB_INTERFACE_PORT..WEB_INTERFACE_PORT.saturating_add(11))
        .find_map(|port| {
            TcpListener::bind(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))).ok()
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                "No available local web interface port",
            )
        })?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let web = WebServerState {
        events: app.state::<AppState>().browser_events.clone(),
        app,
        shutdown: shutdown_rx.clone(),
        port,
    };
    let thread = thread::Builder::new()
        .name("oscmessenger-web-interface".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("Failed to create web interface runtime: {error}");
                    return;
                }
            };
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        eprintln!("Failed to create web interface listener: {error}");
                        return;
                    }
                };
                let router = Router::new()
                    .route("/api/commands/{command}", post(browser_command))
                    .route("/api/events", get(browser_events))
                    .fallback(get(serve_web_asset))
                    .with_state(web);
                let shutdown_signal = async move {
                    let mut receiver = shutdown_rx;
                    if !*receiver.borrow() {
                        let _ = receiver.changed().await;
                    }
                };
                if let Err(error) = axum::serve(listener, router)
                    .with_graceful_shutdown(shutdown_signal)
                    .await
                {
                    eprintln!("Web interface server stopped: {error}");
                }
            });
        })?;
    Ok(WebServerRuntime {
        port,
        shutdown,
        thread,
    })
}

fn stop_web_interface(state: &AppState) {
    let runtime = state
        .web_server
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(runtime) = runtime {
        let _ = runtime.shutdown.send(true);
        if runtime.thread.join().is_err() {
            eprintln!("Web interface thread panicked during shutdown");
        }
    }
}

#[tauri::command]
fn get_web_interface_url(state: State<'_, AppState>) -> Result<String, String> {
    lock_web_url(&state)
        .clone()
        .ok_or_else(|| "Web interface server is not running".to_string())
}

#[tauri::command]
fn configure_web_interface(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<Option<String>, String> {
    let _control = state
        .web_server_control
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !enabled {
        stop_web_interface(&state);
        *lock_web_url(&state) = None;
        return Ok(None);
    }

    if let Some(url) = lock_web_url(&state).clone() {
        return Ok(Some(url));
    }

    let runtime = start_web_interface(app).map_err(|error| error.to_string())?;
    let url = format!("http://127.0.0.1:{}/", runtime.port);
    *state
        .web_server
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(runtime);
    *lock_web_url(&state) = Some(url.clone());
    Ok(Some(url))
}

fn lock_web_url(state: &AppState) -> MutexGuard<'_, Option<String>> {
    state
        .web_interface_url
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[tauri::command]
fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    client_name: String,
    client_uuid: Option<String>,
    network_mode: String,
    multicast_group: String,
    multicast_interfaces: Vec<String>,
    listener_port: u16,
    unicast_address: String,
    unicast_port: u16,
) -> Result<ConnectionInfo, String> {
    let mut session = lock_session(&state);
    if let Some(worker) = session.worker.as_ref() {
        if !worker.is_finished() {
            return Ok(ConnectionInfo {
                client_uuid: worker.client_uuid().to_string(),
            });
        }
    }
    if listener_port == 0 {
        return Err("Listener port must be between 1 and 65535".to_string());
    }
    let network_mode = match network_mode.as_str() {
        "multicast" => NetworkMode::Multicast,
        "unicast" => NetworkMode::Unicast,
        _ => return Err("Network mode must be multicast or unicast".to_string()),
    };
    let multicast_group = if network_mode == NetworkMode::Multicast {
        let address = multicast_group
            .parse::<Ipv4Addr>()
            .map_err(|_| "Multicast group must be a valid IPv4 address".to_string())?;
        if !address.is_multicast() {
            return Err("Multicast group must be in the IPv4 multicast range".to_string());
        }
        address
    } else {
        DEFAULT_MULTICAST_GROUP
    };
    let multicast_interfaces = if network_mode == NetworkMode::Multicast {
        let available_interfaces = if_addrs::get_if_addrs()
            .map_err(|error| format!("Failed to enumerate local network interfaces: {error}"))?;
        let available_addresses = available_interfaces
            .iter()
            .filter_map(|interface| match &interface.addr {
                if_addrs::IfAddr::V4(address) => Some(address.ip),
                if_addrs::IfAddr::V6(_) => None,
            })
            .collect::<HashSet<_>>();
        let mut selected = Vec::new();
        for value in multicast_interfaces {
            let address = value
                .parse::<Ipv4Addr>()
                .map_err(|_| format!("Invalid multicast interface IPv4 address: {value}"))?;
            if !available_addresses.contains(&address) {
                return Err(format!("{address} is not a local IPv4 interface"));
            }
            if !selected.contains(&address) {
                selected.push(address);
            }
        }
        selected
    } else {
        Vec::new()
    };
    let unicast_destination = if network_mode == NetworkMode::Unicast {
        if unicast_port == 0 {
            return Err("Unicast destination port must be between 1 and 65535".to_string());
        }
        let address = unicast_address
            .parse::<Ipv4Addr>()
            .map_err(|_| "Unicast destination must be a valid IPv4 address".to_string())?;
        if address.is_multicast() || address.is_unspecified() || address.octets()[0] == 255 {
            return Err("Unicast destination must be a unicast IPv4 address".to_string());
        }
        Some(SocketAddrV4::new(address, unicast_port))
    } else {
        None
    };

    let mut config = Config::new(
        Some(multicast_group),
        Some(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, listener_port)),
        None,
        Some(client_name),
        Some(application_name()),
    );
    config.network_mode = network_mode;
    config.multicast_interfaces = multicast_interfaces;
    config.unicast_destination = unicast_destination;
    if let Some(client_uuid) = client_uuid.filter(|value| !value.trim().is_empty()) {
        config = config.set_client_uuid(client_uuid);
    }

    let worker = OscWorkerHandle::spawn(config).map_err(|error| error.to_string())?;
    let connection_info = ConnectionInfo {
        client_uuid: worker.client_uuid().to_string(),
    };
    session.active_room = None;
    session.worker = Some(worker);
    drop(session);

    if let Err(error) = start_event_pump(app) {
        let worker = { lock_session(&state).worker.take() };
        if let Some(worker) = worker {
            let _ = worker.join();
        }
        return Err(error.to_string());
    }

    Ok(connection_info)
}

#[tauri::command]
fn get_network_interfaces() -> Result<Vec<LocalNetworkInterface>, String> {
    let interfaces = if_addrs::get_if_addrs().map_err(|error| error.to_string())?;
    let mut result = interfaces
        .into_iter()
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(address) => Some(LocalNetworkInterface {
                name: interface.name,
                address: address.ip.to_string(),
                is_loopback: address.ip.is_loopback(),
            }),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect::<Vec<_>>();
    result.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.address.cmp(&right.address))
    });
    Ok(result)
}

#[tauri::command]
fn configure_gateway(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
    conflict_policy: String,
    multicast_group: String,
    multicast_interfaces: Vec<String>,
    listener_port: u16,
) -> Result<(), String> {
    stop_gateway_runtime(&state);
    if !enabled {
        emit_gateway_status(&app, "disabled", None, None);
        return Ok(());
    }

    let policy = match conflict_policy.as_str() {
        "start_anyway" => GatewayConflictPolicy::StartAnyway,
        "stop_local" => GatewayConflictPolicy::StopLocalServer,
        "never_start" => GatewayConflictPolicy::NeverStartLocalServer,
        _ => return Err("Invalid Gateway conflict policy".to_string()),
    };
    if policy == GatewayConflictPolicy::NeverStartLocalServer {
        emit_gateway_status(&app, "disabled", None, None);
        return Ok(());
    }
    if listener_port == 0 {
        return Err("Gateway listener port must be between 1 and 65535".to_string());
    }

    let multicast_group = multicast_group
        .parse::<Ipv4Addr>()
        .map_err(|_| "Gateway multicast group must be a valid IPv4 address".to_string())?;
    if !multicast_group.is_multicast() {
        return Err("Gateway group must be in the IPv4 multicast range".to_string());
    }
    let available_addresses = if_addrs::get_if_addrs()
        .map_err(|error| format!("Failed to enumerate local network interfaces: {error}"))?
        .into_iter()
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(address) => Some(address.ip),
            if_addrs::IfAddr::V6(_) => None,
        })
        .collect::<HashSet<_>>();
    let mut selected_interfaces = Vec::new();
    for value in multicast_interfaces {
        let address = value
            .parse::<Ipv4Addr>()
            .map_err(|_| format!("Invalid Gateway interface IPv4 address: {value}"))?;
        if !available_addresses.contains(&address) {
            return Err(format!("{address} is not a local IPv4 interface"));
        }
        if !selected_interfaces.contains(&address) {
            selected_interfaces.push(address);
        }
    }

    let mut gateway_config = GatewayConfig::default();
    gateway_config.multicast_group = multicast_group;
    gateway_config.multicast_interfaces = selected_interfaces;
    gateway_config.multicast_listener_address =
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, listener_port);
    let (shutdown, shutdown_rx) = mpsc::channel();
    let gateway_app = app.clone();
    let thread = thread::Builder::new()
        .name("gateway-supervisor".to_string())
        .spawn(move || run_gateway_supervisor(gateway_app, gateway_config, policy, shutdown_rx))
        .map_err(|error| error.to_string())?;
    let mut runtime = state
        .gateway
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *runtime = Some(GatewayRuntime { shutdown, thread });
    Ok(())
}

#[tauri::command]
fn disconnect(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let worker = {
        let mut session = lock_session(&state);
        session.active_room = None;
        session.worker.take()
    };
    if let Some(worker) = worker {
        let result = worker
            .join()
            .map_err(|_| "Worker thread panicked during shutdown".to_string());
        emit_worker_ui_event(&app, UiEvent::Stopped);
        result?;
    }
    Ok(())
}

#[tauri::command]
fn enter_room(state: State<'_, AppState>, room_name: String) -> Result<(), String> {
    let mut session = lock_session(&state);
    let worker = session
        .worker
        .as_ref()
        .ok_or_else(|| "Not connected".to_string())?;
    worker
        .enter_room(room_name.clone())
        .map_err(|error| error.to_string())?;
    session.active_room = Some(room_name);
    Ok(())
}

#[tauri::command]
fn exit_room(state: State<'_, AppState>, room_name: String) -> Result<(), String> {
    let mut session = lock_session(&state);
    let worker = session
        .worker
        .as_ref()
        .ok_or_else(|| "Not connected".to_string())?;
    worker
        .exit_room(room_name.clone())
        .map_err(|error| error.to_string())?;
    if session.active_room.as_deref() == Some(&room_name) {
        session.active_room = None;
    }
    Ok(())
}

#[tauri::command]
fn send_message(
    state: State<'_, AppState>,
    message_uuid: String,
    text: String,
    room_name: Option<String>,
) -> Result<(), String> {
    let session = lock_session(&state);
    let worker = session
        .worker
        .as_ref()
        .ok_or_else(|| "Not connected".to_string())?;
    match room_name.filter(|room| !room.is_empty()) {
        Some(room_name) => worker.say_in_room_with_uuid(room_name, message_uuid, text),
        None => worker.say_with_uuid(message_uuid, text),
    }
    .map_err(|error| error.to_string())
}

#[tauri::command]
fn query_listening(
    state: State<'_, AppState>,
    room_name: String,
    within_room: bool,
) -> Result<(), String> {
    let session = lock_session(&state);
    let worker = session
        .worker
        .as_ref()
        .ok_or_else(|| "Not connected".to_string())?;
    if within_room {
        worker.query_listening_in_room(room_name)
    } else {
        worker.query_listening_outside_room(room_name)
    }
    .map_err(|error| error.to_string())
}

fn start_event_pump(app: AppHandle) -> io::Result<()> {
    thread::Builder::new()
        .name("osc-worker-events".to_string())
        .spawn(move || {
            let mut last_dropped = 0;
            let mut stopped_event_seen = false;
            loop {
                let app_state = app.state::<AppState>();
                let (event, worker_finished, dropped_events) = {
                    let session = lock_session(&app_state);
                    let Some(worker) = session.worker.as_ref() else {
                        break;
                    };
                    (
                        worker.event_receiver().try_recv(),
                        worker.is_finished(),
                        worker.dropped_event_count(),
                    )
                };

                if dropped_events > last_dropped {
                    last_dropped = dropped_events;
                    emit_worker_ui_event(
                        &app,
                        UiEvent::DroppedEvents {
                            total: dropped_events,
                        },
                    );
                }

                match event {
                    Ok(OscWorkerEvent::Started) => {}
                    Ok(OscWorkerEvent::Stopped) => {
                        stopped_event_seen = true;
                        emit_worker_ui_event(&app, UiEvent::Stopped);
                    }
                    Ok(OscWorkerEvent::MessageSent(bytes)) => {
                        emit_worker_ui_event(&app, UiEvent::MessageSent { bytes });
                    }
                    Ok(OscWorkerEvent::MessageReceived(sender, message)) => {
                        let args = message
                            .args()
                            .into_iter()
                            .map(|argument| match argument {
                                OscType::String(value) => value,
                                value => format!("{value:?}"),
                            })
                            .collect();
                        emit_worker_ui_event(
                            &app,
                            UiEvent::MessageReceived {
                                sender: sender.to_string(),
                                address: message.address().into_owned(),
                                args,
                            },
                        );
                    }
                    Ok(OscWorkerEvent::Error(error)) => {
                        emit_worker_ui_event(
                            &app,
                            UiEvent::Error {
                                message: format!("{error:?}"),
                            },
                        );
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }

                if worker_finished {
                    if let Some(worker) = lock_session(&app.state::<AppState>()).worker.take() {
                        let _ = worker.join();
                    }
                    if !stopped_event_seen {
                        emit_worker_ui_event(&app, UiEvent::Stopped);
                    }
                    break;
                }

                thread::sleep(Duration::from_millis(16));
            }
        })
        .map(|_| ())
}

fn run_gateway_supervisor(
    app: AppHandle,
    config: GatewayConfig,
    policy: GatewayConflictPolicy,
    shutdown: mpsc::Receiver<()>,
) {
    emit_gateway_status(&app, "checking", None, None);
    let active_gateways = discover_active_gateways(&config, Duration::from_secs(1));
    let remote_gateway = match active_gateways {
        Ok(gateways) => gateways.into_iter().next(),
        Err(error) => {
            if policy == GatewayConflictPolicy::StopLocalServer {
                emit_gateway_status(
                    &app,
                    "discovery_error",
                    None,
                    Some(format!("Could not check for another Gateway: {error}")),
                );
                return;
            }
            None
        }
    };

    if let Some(gateway_uuid) = remote_gateway.as_ref()
        && policy == GatewayConflictPolicy::StopLocalServer
    {
        emit_gateway_status(
            &app,
            "remote_active",
            Some(gateway_uuid.clone()),
            Some("Using the active Gateway; no local server was started.".to_string()),
        );
        return;
    }

    let local_gateway_uuid = config.gateway_uuid.clone();
    let (gateway_shutdown, gateway_shutdown_rx) = mpsc::channel();
    let server_config = config.clone();
    let server = match thread::Builder::new()
        .name("local-gateway-server".to_string())
        .spawn(move || run_gateway(server_config, gateway_shutdown_rx))
    {
        Ok(server) => server,
        Err(error) => {
            emit_gateway_status(
                &app,
                "error",
                None,
                Some(format!("Could not start the local Gateway: {error}")),
            );
            return;
        }
    };

    emit_gateway_status(
        &app,
        "local_active",
        Some(local_gateway_uuid.clone()),
        remote_gateway
            .map(|uuid| format!("Local Gateway started alongside active Gateway {uuid}.")),
    );

    loop {
        if server.is_finished() {
            match server.join() {
                Ok(Ok(())) => emit_gateway_status(&app, "stopped", Some(local_gateway_uuid), None),
                Ok(Err(error)) => emit_gateway_status(
                    &app,
                    "error",
                    Some(local_gateway_uuid),
                    Some(format!("Local Gateway stopped: {error}")),
                ),
                Err(_) => emit_gateway_status(
                    &app,
                    "error",
                    Some(local_gateway_uuid),
                    Some("Local Gateway thread panicked.".to_string()),
                ),
            }
            return;
        }

        match shutdown.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => {
                let _ = gateway_shutdown.send(());
                let _ = server.join();
                emit_gateway_status(&app, "stopped", Some(local_gateway_uuid), None);
                return;
            }
            Err(mpsc::TryRecvError::Empty) => {}
        }

        if policy == GatewayConflictPolicy::StopLocalServer {
            if let Ok(gateways) = discover_active_gateways(&config, Duration::from_millis(750))
                && let Some(remote_uuid) = gateways
                    .into_iter()
                    .find(|uuid| uuid != &local_gateway_uuid)
            {
                let _ = gateway_shutdown.send(());
                let _ = server.join();
                emit_gateway_status(
                    &app,
                    "remote_active",
                    Some(remote_uuid.clone()),
                    Some(format!(
                        "Stopped local Gateway because Gateway {remote_uuid} became active."
                    )),
                );
                return;
            }
        } else {
            match shutdown.recv_timeout(Duration::from_millis(750)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = gateway_shutdown.send(());
                    let _ = server.join();
                    emit_gateway_status(&app, "stopped", Some(local_gateway_uuid), None);
                    return;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

fn emit_gateway_status(
    app: &AppHandle,
    state: &str,
    gateway_uuid: Option<String>,
    detail: Option<String>,
) {
    let status = GatewayStatus {
        state: state.to_string(),
        gateway_uuid,
        detail,
    };
    let _ = app.emit("gateway-status", status.clone());
    let _ = app
        .state::<AppState>()
        .browser_events
        .send(BrowserEvent::Gateway(status));
}

fn emit_worker_ui_event(app: &AppHandle, event: UiEvent) {
    let _ = app.emit("worker-event", event.clone());
    let _ = app
        .state::<AppState>()
        .browser_events
        .send(BrowserEvent::Worker(event));
}

fn stop_gateway_runtime(state: &AppState) {
    let runtime = state
        .gateway
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    if let Some(runtime) = runtime {
        let _ = runtime.shutdown.send(());
        if runtime.thread.join().is_err() {
            eprintln!("Gateway supervisor thread panicked during shutdown");
        }
    }
}

fn lock_session(state: &AppState) -> MutexGuard<'_, Session> {
    state
        .session
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            connect,
            get_network_interfaces,
            configure_gateway,
            get_web_interface_url,
            configure_web_interface,
            disconnect,
            enter_room,
            exit_room,
            send_message,
            query_listening,
        ])
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                let state = window.state::<AppState>();
                if let Some(worker) = lock_session(&state).worker.take() {
                    let _ = worker.join();
                }
                stop_gateway_runtime(&state);
                stop_web_interface(&state);
            }
        })
        .run(tauri::generate_context!())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_requests_require_local_host_and_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "127.0.0.1:9140".parse().unwrap());
        headers.insert(header::ORIGIN, "http://127.0.0.1:9140".parse().unwrap());
        assert!(is_local_browser_request(&headers, 9140));

        headers.insert(header::ORIGIN, "https://example.com".parse().unwrap());
        assert!(!is_local_browser_request(&headers, 9140));
        headers.insert(header::ORIGIN, "http://localhost:9141".parse().unwrap());
        assert!(!is_local_browser_request(&headers, 9140));
        headers.insert(header::ORIGIN, "http://localhost:9140".parse().unwrap());
        headers.insert(header::HOST, "example.com:9140".parse().unwrap());
        assert!(!is_local_browser_request(&headers, 9140));
    }

    #[test]
    fn browser_worker_events_use_frontend_envelope() {
        let event = BrowserEvent::Worker(UiEvent::MessageSent { bytes: 42 });
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({"event": "worker-event", "payload": {"kind": "message_sent", "bytes": 42}})
        );
    }
}
