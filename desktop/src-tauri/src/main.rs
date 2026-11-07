use rosc::OscType;
use rust_osc_io::config::{Config, DEFAULT_MULTICAST_GROUP, NetworkMode};
use rust_osc_io::gateway::{GatewayConfig, discover_active_gateways, run_gateway};
use rust_osc_io::worker::{OscWorkerEvent, OscWorkerHandle};
use serde::Serialize;
use std::collections::HashSet;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Mutex, MutexGuard, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

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

#[derive(Default)]
struct AppState {
    session: Mutex<Session>,
    gateway: Mutex<Option<GatewayRuntime>>,
}

struct GatewayRuntime {
    shutdown: mpsc::Sender<()>,
    thread: JoinHandle<()>,
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
fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    let worker = {
        let mut session = lock_session(&state);
        session.active_room = None;
        session.worker.take()
    };
    if let Some(worker) = worker {
        worker
            .join()
            .map_err(|_| "Worker thread panicked during shutdown".to_string())?;
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
                    let _ = app.emit(
                        "worker-event",
                        UiEvent::DroppedEvents {
                            total: dropped_events,
                        },
                    );
                }

                match event {
                    Ok(OscWorkerEvent::Started) => {}
                    Ok(OscWorkerEvent::Stopped) => {
                        stopped_event_seen = true;
                        let _ = app.emit("worker-event", UiEvent::Stopped);
                    }
                    Ok(OscWorkerEvent::MessageSent(bytes)) => {
                        let _ = app.emit("worker-event", UiEvent::MessageSent { bytes });
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
                        let _ = app.emit(
                            "worker-event",
                            UiEvent::MessageReceived {
                                sender: sender.to_string(),
                                address: message.address().into_owned(),
                                args,
                            },
                        );
                    }
                    Ok(OscWorkerEvent::Error(error)) => {
                        let _ = app.emit(
                            "worker-event",
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
                        let _ = app.emit("worker-event", UiEvent::Stopped);
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
    let _ = app.emit(
        "gateway-status",
        GatewayStatus {
            state: state.to_string(),
            gateway_uuid,
            detail,
        },
    );
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
            }
        })
        .run(tauri::generate_context!())?;
    Ok(())
}
