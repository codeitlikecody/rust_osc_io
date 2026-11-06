use rosc::OscType;
use rust_osc_io::config::Config;
use rust_osc_io::worker::{OscWorkerEvent, OscWorkerHandle};
use serde::Serialize;
use std::io;
use std::sync::{Mutex, MutexGuard};
use std::thread;
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
}

#[derive(Serialize)]
struct ConnectionInfo {
    client_uuid: String,
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
) -> Result<ConnectionInfo, String> {
    let mut session = lock_session(&state);
    if let Some(worker) = session.worker.as_ref() {
        if !worker.is_finished() {
            return Ok(ConnectionInfo {
                client_uuid: worker.client_uuid().to_string(),
            });
        }
    }

    let mut config = Config::default()
        .set_name(client_name)
        .set_application_name(application_name());
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
            }
        })
        .run(tauri::generate_context!())?;
    Ok(())
}
