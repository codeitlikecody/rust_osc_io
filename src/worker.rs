use crate::config;
use crate::net::{create_receiver_socket, leave_multicast_group, send_osc_message};
use crate::protocol::{
    Me, Message, MessengerClient, MessengerProtocolError, RoomEnter, RoomExit, RoomListening,
    RoomSay, Say, Who, validate_uuid,
};
pub use rosc::OscError;
use rosc::decoder;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self};
use std::thread::{self, JoinHandle};

const EVENT_QUEUE_CAPACITY: usize = 1024;
const COMMAND_QUEUE_CAPACITY: usize = 256;

#[derive(Debug)]
pub enum OscWorkerError {
    IoError(io::Error),
    OscError(OscError),
    MessengerProtocolError(MessengerProtocolError),
}
#[derive(Debug)]
pub enum OscWorkerEvent {
    Started,
    Stopped,
    MessageSent(usize),
    MessageReceived(SocketAddr, Message),
    Error(OscWorkerError), // Error event can be used to report non fatal errors from the worker thread to the main thread.
}

#[derive(Clone)]
struct WorkerEventSender {
    sender: mpsc::SyncSender<OscWorkerEvent>,
    dropped_events: Arc<AtomicUsize>,
}

impl WorkerEventSender {
    fn send(&self, event: OscWorkerEvent) -> Result<(), &'static str> {
        match self.sender.try_send(event) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => {
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => Err("event receiver disconnected"),
        }
    }
}

#[derive(Debug)]
enum OscCommand {
    Stop,
    SendMessage(Message),
    // Future commands can be added here, such as changing the broadcast group.
    // Changing port will need the socket to be recreated with the new port, so it may be better to just stop the worker and start a new one with the new port.
    // ChangeGroup(Ipv4Addr),
}

pub struct OscWorkerHandle {
    commands_tx: mpsc::SyncSender<OscCommand>,
    events_rx: mpsc::Receiver<OscWorkerEvent>,
    dropped_events: Arc<AtomicUsize>,
    thread: JoinHandle<()>,
    client: MessengerClient,
}

/// A handle to a worker thread that can be used to control it.
/// The worker thread will stop when the handle is dropped, but it can also be stopped explicitly by calling `stop_worker()`.
/// The worker thread can be joined by calling `join()` on the handle.
/// The worker thread will call the provided handler function for each received OSC packet, with the sender's address and the decoded OSC packet.
impl OscWorkerHandle {
    /// Spawns a receiver thread that listens for OSC packets on the given multicast `group` and `port`.
    pub fn spawn(config: config::Config) -> Result<OscWorkerHandle, io::Error> {
        if let Some(client_uuid) = config.client_uuid.as_deref() {
            validate_uuid(client_uuid)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        }

        // Create channels for controlling the receiver and returning events
        let (commands_tx, commands_rx) = mpsc::sync_channel::<OscCommand>(COMMAND_QUEUE_CAPACITY);
        let (events_tx, events_rx) = mpsc::sync_channel::<OscWorkerEvent>(EVENT_QUEUE_CAPACITY);
        let dropped_events = Arc::new(AtomicUsize::new(0));
        let worker_events = WorkerEventSender {
            sender: events_tx,
            dropped_events: Arc::clone(&dropped_events),
        };
        let client = configured_client(&config);
        let worker_client = client.clone();
        let thread = thread::spawn(move || {
            spawn_osc_worker(
                config.multicast_group,
                config.listener_address,
                config.sender_address,
                commands_rx,
                worker_events,
                worker_client,
            )
        });

        // check for events from the receiver thread and make sure we were able to start the receiver thread successfully
        // should not be blocking, since the receiver thread should send an event immediately after starting
        // If we don't receive an event within a reasonable time, we should fail.
        match events_rx.recv_timeout(config::DEFAULT_WORKER_START_TIMEOUT_MS) {
            Ok(OscWorkerEvent::Started) => {}
            Ok(OscWorkerEvent::Error(error)) => {
                eprintln!("WORKER: Failed to start worker thread: {error:?}");
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to start worker thread: {error:?}"),
                ));
            }
            Ok(event) => {
                eprintln!("WORKER: Unexpected event from worker thread: {event:?}");
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Unexpected event from worker thread: {event:?}"),
                ));
            }
            Err(error) => {
                eprintln!("WORKER: Failed to receive event from worker thread: {error}");
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to receive event from worker thread: {error}"),
                ));
            }
        }
        let handle = OscWorkerHandle {
            commands_tx,
            events_rx,
            dropped_events,
            thread,
            client,
        };

        // Introduce the client to the multicast group
        handle
            .send_osc(Message::Me(Me::Introduce(handle.client.clone())))
            .unwrap_or_else(|e| eprintln!("WORKER: Failed to introduce client: {e}"));

        // Return the handle to the worker thread
        Ok(handle)
    }

    /// Returns a reference to the receiver for events from the worker thread.
    pub fn event_receiver(&self) -> &mpsc::Receiver<OscWorkerEvent> {
        &self.events_rx
    }

    /// Returns the cumulative number of events dropped because the event queue was full.
    pub fn dropped_event_count(&self) -> usize {
        self.dropped_events.load(Ordering::Relaxed)
    }

    pub fn client_uuid(&self) -> &str {
        self.client.client_uuid()
    }

    /// Sends a command to the worker thread to stop it.
    /// Returns `WouldBlock` if the command queue is full.
    pub fn stop_osc_worker(&self) -> Result<(), io::Error> {
        match self.commands_tx.try_send(OscCommand::Stop) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Worker command queue is full",
            )),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Worker thread has stopped",
            )),
        }
    }

    /// Says a message to the multicast group.
    pub fn say(&self, message: String) -> Result<(), io::Error> {
        self.send_osc(Message::Me(Me::Say(Say::new(
            self.client.client_uuid().to_string(),
            message,
        ))))
    }

    pub fn say_with_uuid(&self, message_uuid: String, message: String) -> Result<(), io::Error> {
        self.send_osc(Message::Me(Me::Say(Say::new_with_uuid(
            message_uuid,
            self.client.client_uuid().to_string(),
            message,
        ))))
    }

    pub fn say_in_room(&self, room_name: String, message: String) -> Result<(), io::Error> {
        self.send_osc(Message::Me(Me::SayInRoom(RoomSay::new(
            room_name,
            Say::new(self.client.client_uuid().to_string(), message),
        ))))
    }

    pub fn say_in_room_with_uuid(
        &self,
        room_name: String,
        message_uuid: String,
        message: String,
    ) -> Result<(), io::Error> {
        self.send_osc(Message::Me(Me::SayInRoom(RoomSay::new(
            room_name,
            Say::new_with_uuid(message_uuid, self.client.client_uuid().to_string(), message),
        ))))
    }

    pub fn enter_room(&self, room_name: String) -> Result<(), io::Error> {
        self.send_osc(Message::Me(Me::EnterRoom(RoomEnter::new(
            room_name,
            self.client.clone(),
        ))))
    }

    pub fn exit_room(&self, room_name: String) -> Result<(), io::Error> {
        self.send_osc(Message::Me(Me::ExitRoom(RoomExit::new(
            room_name,
            self.client.clone(),
        ))))
    }

    pub fn query_listening_outside_room(&self, room_name: String) -> Result<(), io::Error> {
        self.send_osc(Message::Who(crate::protocol::Who::Listening(room_name)))
    }

    pub fn query_listening_in_room(&self, room_name: String) -> Result<(), io::Error> {
        self.send_osc(Message::Who(Who::ListeningInRoom(room_name)))
    }

    /// Sends a message to the worker thread.
    /// Returns an io::Error if the worker thread has already stopped.
    fn send_osc(&self, message: Message) -> Result<(), io::Error> {
        message
            .to_osc()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        match self.commands_tx.try_send(OscCommand::SendMessage(message)) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Worker command queue is full",
            )),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Worker thread has stopped",
            )),
        }
    }

    /// Requests worker shutdown and waits for its thread to finish.
    /// A failed stop request is ignored because the worker may already have exited.
    pub fn join(self) -> thread::Result<()> {
        let _ = self.commands_tx.send(OscCommand::Stop);
        self.thread.join()
    }
}

fn configured_client(config: &config::Config) -> MessengerClient {
    match &config.client_uuid {
        Some(client_uuid) => MessengerClient::new_with_uuid(
            client_uuid.clone(),
            config.client_name.clone(),
            config.application_name.clone(),
        ),
        None => MessengerClient::new(config.client_name.clone(), config.application_name.clone()),
    }
}

/// Spawns a thread that receives OSC packets on `socket` and passes each to `handler`.
/// The thread ends on a socket error.
///
/// # Arguments
/// * `multicast_group` - The multicast group to receive packets on.
/// * `listener_address` - The address to listen on.
/// * `sender_address` - The address to send from.
/// * `commands` - A channel to receive commands from the main thread.
/// * `events` - A channel to send events to the main thread.
fn spawn_osc_worker(
    multicast_group: Ipv4Addr,
    listener_address: SocketAddrV4,
    sender_address: SocketAddrV4,
    commands: mpsc::Receiver<OscCommand>,
    events: WorkerEventSender,
    client: MessengerClient,
) -> () {
    let mut buffer = [0u8; 65536];
    let mut current_room: Option<String> = None;
    let destination = SocketAddrV4::new(multicast_group, listener_address.port());
    let socket = match create_receiver_socket(multicast_group, listener_address) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("WORKER: Failed to create receiver socket: {error}");
            events
                .send(OscWorkerEvent::Error(OscWorkerError::IoError(error)))
                .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}"));
            return;
        }
    };
    // The timeout lets the worker poll commands while waiting for incoming packets.
    if let Err(error) = socket.set_read_timeout(Some(std::time::Duration::from_millis(100))) {
        eprintln!("WORKER: Failed to set read timeout: {error}");
        leave_multicast_group(&socket, multicast_group, listener_address.ip())
            .unwrap_or_else(|e| eprintln!("WORKER: Failed to leave multicast group: {e}"));
        events
            .send(OscWorkerEvent::Error(OscWorkerError::IoError(error)))
            .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}"));
        return;
    }

    // Notify the main thread that the worker thread has started successfully
    events
        .send(OscWorkerEvent::Started)
        .unwrap_or_else(|e| eprintln!("WORKER: Failed to send started event: {e}"));

    loop {
        // Check for commands from the main thread
        match commands.try_recv() {
            Ok(command) => match command {
                OscCommand::Stop => {
                    send_farewell(destination, sender_address, &client, &events);
                    leave_multicast_group(&socket, multicast_group, listener_address.ip())
                        .unwrap_or_else(|e| {
                            eprintln!("WORKER: Failed to leave multicast group: {e}")
                        });
                    notify_stopped(&events);
                    break;
                }
                OscCommand::SendMessage(message) => {
                    let room_transition = match &message {
                        Message::Me(Me::EnterRoom(room)) => {
                            Some((room.room_name().to_string(), true))
                        }
                        Message::Me(Me::ExitRoom(room)) => {
                            Some((room.room_name().to_string(), false))
                        }
                        _ => None,
                    };
                    match send_osc_message(destination, sender_address, message) {
                        Ok(bytes_sent) => {
                            if let Some((room_name, entering)) = room_transition {
                                update_current_room(&mut current_room, &room_name, entering);
                            }
                            events
                                .send(OscWorkerEvent::MessageSent(bytes_sent))
                                .unwrap_or_else(|e| eprintln!("WORKER: Failed to send event: {e}"));
                        }
                        Err(error) => {
                            events
                                .send(OscWorkerEvent::Error(error))
                                .unwrap_or_else(|e| {
                                    eprintln!("WORKER: Failed to send error event: {e}")
                                });
                        }
                    }
                }
            },
            Err(mpsc::TryRecvError::Empty) => {
                // No command received, continue to receive OSC packets
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // The main thread has disconnected, stop the receiver
                send_farewell(destination, sender_address, &client, &events);
                leave_multicast_group(&socket, multicast_group, listener_address.ip())
                    .unwrap_or_else(|e| eprintln!("WORKER: Failed to leave multicast group: {e}"));
                events
                    .send(OscWorkerEvent::Error(OscWorkerError::IoError(
                        io::Error::new(io::ErrorKind::ConnectionReset, "Main thread disconnected"),
                    )))
                    .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}"));
                notify_stopped(&events);
                break;
            }
        }

        // Receive an OSC packet from the socket
        match socket.recv_from(&mut buffer) {
            Ok((size, sender)) => match decoder::decode_udp(&buffer[..size]) {
                Ok((_, packet)) => match packet.try_into() {
                    Ok(message) => {
                        let deliver_message =
                            should_deliver_message(&message, current_room.as_deref());
                        let response = response_to(&message, &client, current_room.as_deref());
                        if deliver_message {
                            events
                                    .send(OscWorkerEvent::MessageReceived(sender, message))
                                    .unwrap_or_else(|e| {
                                        eprintln!("Worker Error 822: Failed to send message received event: {e}")
                                    });
                        }
                        if let Some(response) = response {
                            match send_osc_message(destination, sender_address, response) {
                                Ok(bytes_sent) => {
                                    events
                                        .send(OscWorkerEvent::MessageSent(bytes_sent))
                                        .unwrap_or_else(|e| {
                                            eprintln!("WORKER: Failed to send event: {e}")
                                        });
                                }
                                Err(error) => {
                                    events
                                        .send(OscWorkerEvent::Error(error))
                                        .unwrap_or_else(|e| {
                                            eprintln!("WORKER: Failed to send error event: {e}")
                                        });
                                }
                            }
                        }
                    }
                    Err(error) => {
                        eprintln!(
                            "Worker Error 971: Failed to convert OSC packet to Message. : {error}"
                        );
                        events
                            .send(OscWorkerEvent::Error(
                                OscWorkerError::MessengerProtocolError(error),
                            ))
                            .unwrap_or_else(|e| {
                                eprintln!("WORKER: Failed to send error event: {e}")
                            });
                    }
                },

                Err(error) => {
                    eprintln!("WORKER: Failed to decode OSC packet: {error}");
                    events
                        .send(OscWorkerEvent::Error(OscWorkerError::OscError(error)))
                        .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}"));
                }
            },
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut =>
            {
                // Timeout reached. We can ignore the error and continue to check for commands
                continue;
            }
            Err(error) => {
                eprintln!("WORKER: Receive error: {error}");
                events
                    .send(OscWorkerEvent::Error(OscWorkerError::IoError(error)))
                    .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}"));
                send_farewell(destination, sender_address, &client, &events);
                leave_multicast_group(&socket, multicast_group, listener_address.ip())
                    .unwrap_or_else(|e| eprintln!("WORKER: Failed to leave multicast group: {e}"));
                notify_stopped(&events);
                break;
            }
        }
    }
}

fn notify_stopped(events: &WorkerEventSender) {
    events
        .send(OscWorkerEvent::Stopped)
        .unwrap_or_else(|e| eprintln!("WORKER: Failed to send stopped event: {e}"));
}

fn send_farewell(
    destination: SocketAddrV4,
    sender_address: SocketAddrV4,
    client: &MessengerClient,
    events: &WorkerEventSender,
) {
    match send_osc_message(
        destination,
        sender_address,
        Message::Me(Me::Farewell(client.clone())),
    ) {
        Ok(bytes_sent) => events
            .send(OscWorkerEvent::MessageSent(bytes_sent))
            .unwrap_or_else(|e| eprintln!("WORKER: Failed to send event: {e}")),
        Err(error) => events
            .send(OscWorkerEvent::Error(error))
            .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}")),
    }
}

fn response_to(
    message: &Message,
    client: &MessengerClient,
    current_room: Option<&str>,
) -> Option<Message> {
    match message {
        Message::Me(Me::Introduce(introduced_client))
            if introduced_client.client_uuid() != client.client_uuid() =>
        {
            Some(Message::Me(Me::Welcome(client.clone())))
        }
        Message::Who(Who::Listening(room_name)) if current_room != Some(room_name) => {
            Some(Message::Me(Me::Listening(client.clone())))
        }
        Message::Who(Who::ListeningInRoom(room_name)) if current_room == Some(room_name) => {
            Some(Message::Me(Me::ListeningInRoom(RoomListening::new(
                room_name.clone(),
                client.clone(),
            ))))
        }
        _ => None,
    }
}

fn should_deliver_message(message: &Message, current_room: Option<&str>) -> bool {
    match message {
        Message::Me(Me::Say(_)) => current_room.is_none(),
        Message::Me(Me::SayInRoom(room_say)) => current_room == Some(room_say.room_name()),
        _ => true,
    }
}

fn update_current_room(current_room: &mut Option<String>, room_name: &str, entering: bool) {
    if entering {
        *current_room = Some(room_name.to_string());
    } else if current_room.as_deref() == Some(room_name) {
        *current_room = None;
    }
}

#[cfg(test)]
mod tests;
