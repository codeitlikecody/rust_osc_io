pub mod config;
pub mod protocol;
use protocol::{Me, Message, MessengerClient, Say};
pub use rosc::OscError;
use rosc::{decoder, encoder};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::ops::ControlFlow;
use std::sync::mpsc::{self};
use std::thread::{self, JoinHandle};

use crate::Me::Introduce;

#[derive(Debug)]
pub enum OscWorkerError {
    IoError(io::Error),
    OscError(OscError),
    MessengerProtocolError(protocol::MessengerProtocolError),
}
#[derive(Debug)]
pub enum OscWorkerEvent {
    Started,
    Stopped,
    MessageSent(usize),
    MessageReceived(SocketAddr, Message),
    Error(OscWorkerError), // Error event can be used to report non fatal errors from the worker thread to the main thread.
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
    commands_tx: mpsc::Sender<OscCommand>,
    events_rx: mpsc::Receiver<OscWorkerEvent>,
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
        // Create channels for controlling the receiver and returning events
        let (commands_tx, commands_rx) = mpsc::channel::<OscCommand>();
        let (events_tx, events_rx) = mpsc::channel::<OscWorkerEvent>();
        let thread = thread::spawn(move || {
            spawn_osc_worker(
                config.multicast_group,
                config.listener_address,
                config.sender_address,
                commands_rx,
                events_tx,
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
        let client = MessengerClient::new(config.client_name, config.application_name);
        let handle = OscWorkerHandle {
            commands_tx,
            events_rx,
            thread,
            client,
        };

        // Introduce the client to the multicast group
        handle
            .send_osc(Message::Me(Introduce(handle.client.clone())))
            .unwrap_or_else(|e| eprintln!("WORKER: Failed to introduce client: {e}"));

        // Return the handle to the worker thread
        Ok(handle)
    }

    /// Returns a reference to the receiver for events from the worker thread.
    pub fn event_receiver(&self) -> &mpsc::Receiver<OscWorkerEvent> {
        &self.events_rx
    }

    /// Sends a command to the worker thread to stop it.
    /// Returns an io::Error if the worker thread has already stopped.
    pub fn stop_osc_worker(&self) -> Result<(), io::Error> {
        match self.commands_tx.send(OscCommand::Stop) {
            Ok(()) => Ok(()),
            Err(error) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to send stop command to worker thread: {error}"),
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

    /// Sends a message to the worker thread.
    /// Returns an io::Error if the worker thread has already stopped.
    fn send_osc(&self, message: Message) -> Result<(), io::Error> {
        match self.commands_tx.send(OscCommand::SendMessage(message)) {
            Ok(()) => Ok(()),
            Err(error) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to send message to worker thread: {error}"),
            )),
        }
    }

    pub fn join(self) -> thread::Result<()> {
        self.thread.join()
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
    events: mpsc::Sender<OscWorkerEvent>,
) -> () {
    let mut buffer = [0u8; 65536];
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
    // set a read timeout so that the thread can check for commands from the main thread periodically
    socket
        .set_read_timeout(Some(std::time::Duration::from_millis(100)))
        .unwrap_or_else(|e| eprintln!("WORKER: Failed to set read timeout: {e}"));

    // Notify the main thread that the worker thread has started successfully
    events
        .send(OscWorkerEvent::Started)
        .unwrap_or_else(|e| eprintln!("WORKER: Failed to send started event: {e}"));

    // Send a second notification to the client that the worker thread has started successfully
    events
        .send(OscWorkerEvent::Started)
        .unwrap_or_else(|e| eprintln!("WORKER: Failed to send started event: {e}"));

    loop {
        // Check for commands from the main thread
        match commands.try_recv() {
            Ok(command) => match command {
                OscCommand::Stop => {
                    break;
                }
                OscCommand::SendMessage(message) => {
                    if let ControlFlow::Break(_) =
                        send_osc_message(destination, sender_address, &events, message)
                    {
                        continue;
                    }
                }
            },
            Err(mpsc::TryRecvError::Empty) => {
                // No command received, continue to receive OSC packets
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // The main thread has disconnected, stop the receiver
                leave_multicast_group(&socket, multicast_group, listener_address.ip())
                    .unwrap_or_else(|e| eprintln!("WORKER: Failed to leave multicast group: {e}"));
                events
                    .send(OscWorkerEvent::Error(OscWorkerError::IoError(
                        io::Error::new(io::ErrorKind::ConnectionReset, "Main thread disconnected"),
                    )))
                    .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}"));
                break;
            }
        }

        // Receive an OSC packet from the socket
        match socket.recv_from(&mut buffer) {
            Ok((size, sender)) => match decoder::decode_udp(&buffer[..size]) {
                Ok((_, packet)) => {
                    match packet.try_into() {
                        Ok(message) => {
                            events
                            .send(OscWorkerEvent::MessageReceived(sender, message))
                            .unwrap_or_else(|e| {
                                eprintln!("Worker Error 822: Failed to send message received event: {e}")
                            });
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
                    }
                }

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
                // return Err(OscWorkerError::IoError(error));
            }
        }
    }
}

fn send_osc_message(
    destination_address: SocketAddrV4,
    sender_address: SocketAddrV4,
    events: &mpsc::Sender<OscWorkerEvent>,
    message: Message,
) -> ControlFlow<()> {
    let socket = match create_sender_socket(sender_address) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("WORKER: Failed to create sender socket: {error}");
            events
                .send(OscWorkerEvent::Error(OscWorkerError::IoError(error)))
                .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}"));
            return ControlFlow::Break(());
        }
    };
    // Create a sender socket to send the message

    // Send the message to the multicast group and port
    match encoder::encode(&message.to_osc()) {
        Ok(buf) => match socket.send_to(&buf, destination_address) {
            Ok(bytes_sent) => {
                events
                    .send(OscWorkerEvent::MessageSent(bytes_sent))
                    .unwrap_or_else(|e| eprintln!("WORKER: Failed to send event: {e}"));
            }
            Err(error) => {
                eprintln!("WORKER: Failed to send OSC message: {error}");
                events
                    .send(OscWorkerEvent::Error(OscWorkerError::IoError(error)))
                    .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}"));
            }
        },
        Err(error) => {
            eprintln!("WORKER: Failed to encode OSC message: {error}");
            // events
            //     .send(OscWorkerEvent::Error(OscWorkerError::IoError(error)))
            //     .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}"));
        }
    }

    ControlFlow::Continue(())
}

/// Binds a UDP socket on `port` and joins the multicast `group` (with loopback enabled).
/// # Arguments
/// * `multicast_group` - The multicast group to join.
/// * `listener_address` - The address to listen on.
fn create_receiver_socket(
    multicast_group: Ipv4Addr,
    listener_address: SocketAddrV4,
) -> Result<UdpSocket, io::Error> {
    let socket = UdpSocket::bind(listener_address)?;
    socket.join_multicast_v4(&multicast_group, &listener_address.ip())?;
    socket.set_multicast_loop_v4(true)?;
    Ok(socket)
}

/// Leaves the multicast group and closes the socket.
/// # Arguments
/// * `socket` - The UDP socket to leave the multicast group on.
/// * `multicast_group` - The multicast group to leave.
/// * `interface` - The network interface to use.
fn leave_multicast_group(
    socket: &UdpSocket,
    multicast_group: Ipv4Addr,
    interface: &Ipv4Addr,
) -> Result<(), io::Error> {
    socket.leave_multicast_v4(&multicast_group, interface)?;
    Ok(())
}

/// * `sender_address` - The address to send the message from.
fn create_sender_socket(sender_address: SocketAddrV4) -> io::Result<UdpSocket> {
    UdpSocket::bind(sender_address).map(|socket| {
        socket.set_multicast_loop_v4(true).unwrap();
        socket
    })
}
