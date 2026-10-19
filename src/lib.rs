pub use rosc::OscError;
use rosc::{OscMessage, OscPacket, OscType, decoder, encoder};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::ops::ControlFlow;
use std::sync::mpsc::{self};
use std::thread::{self, JoinHandle};

// Default network configuration for the OSC Messenger standard client 1
pub const GROUP: Ipv4Addr = Ipv4Addr::new(239, 254, 0, 1);
pub const PORT: u16 = 3090;

#[derive(Debug)]
pub enum OscWorkerError {
    IoError(io::Error),
    OscError(OscError),
}
#[derive(Debug)]
pub enum OscWorkerEvent {
    Started,
    Stopped,
    MessageSent(usize),
    MessageReceived(SocketAddr, OscPacket),
    Error(OscWorkerError), // Error event can be used to report non fatal errors from the worker thread to the main thread.
}
#[derive(Debug)]
enum OscCommand {
    Stop,
    SendMessage(String, Vec<OscType>),
    // Future commands can be added here, such as changing the broadcast group.
    // Changing port will need the socket to be recreated with the new port, so it may be better to just stop the worker and start a new one with the new port.
    // ChangeGroup(Ipv4Addr),
}

pub struct OscWorkerHandle {
    commands: mpsc::Sender<OscCommand>,
    events: mpsc::Receiver<OscWorkerEvent>,
    thread: JoinHandle<()>,
}

/// A handle to a worker thread that can be used to control it.
/// The worker thread will stop when the handle is dropped, but it can also be stopped explicitly by calling `stop_worker()`.
/// The worker thread can be joined by calling `join()` on the handle.
/// The worker thread will call the provided handler function for each received OSC packet, with the sender's address and the decoded OSC packet.
impl OscWorkerHandle {
    /// Spawns a receiver thread that listens for OSC packets on the given multicast `group` and `port`.
    /// If `group` or `port` is None, the default values of GROUP and PORT will be used.
    /// If `ip` is None, the default value of Ipv4Addr::UNSPECIFIED will be used.
    pub fn spawn(
        multicast_group: Option<Ipv4Addr>,
        listener_ip: Option<Ipv4Addr>,
        listener_port: Option<u16>,
        sender_ip: Option<Ipv4Addr>,
        sender_port: Option<u16>,
    ) -> Result<OscWorkerHandle, io::Error> {
        // Use the provided group, port, and ip, or fall back to protocol defaults if None.
        let multicast_group = multicast_group.unwrap_or(GROUP);
        let listener_ip = listener_ip.unwrap_or(Ipv4Addr::UNSPECIFIED);
        let listener_port = listener_port.unwrap_or(PORT);
        let sender_ip = sender_ip.unwrap_or(Ipv4Addr::UNSPECIFIED);
        let sender_port = sender_port.unwrap_or(0);

        // Create a channel for controlling the receiver
        let (commands_tx, commands_rx) = mpsc::channel::<OscCommand>();
        let (events_tx, events_rx) = mpsc::channel::<OscWorkerEvent>();
        let thread = thread::spawn(move || {
            spawn_osc_worker(
                multicast_group,
                listener_ip,
                listener_port,
                sender_ip,
                sender_port,
                commands_rx,
                events_tx,
            )
        });

        // check for events from the receiver thread and make sure we were able to start the receiver thread successfully
        // should not be blocking, since the receiver thread should send an event immediately after starting
        // If we don't receive an event within a reasonable time, we should fail.
        match events_rx.recv_timeout(std::time::Duration::from_secs(5)) {
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
        // Return the handle to the worker thread
        Ok(OscWorkerHandle {
            commands: commands_tx,
            events: events_rx,
            thread,
        })
    }

    /// Returns a reference to the receiver for events from the worker thread.
    pub fn events(&self) -> &mpsc::Receiver<OscWorkerEvent> {
        &self.events
    }

    /// Sends a command to the worker thread to stop it.
    /// Returns an io::Error if the worker thread has already stopped.
    pub fn stop_osc_worker(&self) -> Result<(), io::Error> {
        match self.commands.send(OscCommand::Stop) {
            Ok(()) => Ok(()),
            Err(error) => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to send stop command to worker thread: {error}"),
            )),
        }
    }

    /// Sends a message to the worker thread.
    /// Returns an io::Error if the worker thread has already stopped.
    pub fn send_message(&self, osc_address: &str, osc_args: Vec<OscType>) -> Result<(), io::Error> {
        match self
            .commands
            .send(OscCommand::SendMessage(osc_address.to_string(), osc_args))
        {
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
/// * `listener_ip` - The IP address to bind to.
/// * `listener_port` - The port to listen on.
/// * `sender_ip` - The IP address to send from.
/// * `sender_port` - The port to send from.
/// * `commands` - A channel to receive commands from the main thread.
/// * `events` - A channel to send events to the main thread.
fn spawn_osc_worker(
    multicast_group: Ipv4Addr,
    listener_ip: Ipv4Addr,
    listener_port: u16,
    sender_ip: Ipv4Addr,
    sender_port: u16,
    commands: mpsc::Receiver<OscCommand>,
    events: mpsc::Sender<OscWorkerEvent>,
) -> () {
    let mut buffer = [0u8; 65536];
    let listener_address = SocketAddrV4::new(listener_ip, listener_port);
    let sender_address = SocketAddrV4::new(sender_ip, sender_port);
    let destination = SocketAddrV4::new(multicast_group, listener_port);
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
    loop {
        // Check for commands from the main thread
        match commands.try_recv() {
            Ok(command) => match command {
                OscCommand::Stop => {
                    break;
                }
                OscCommand::SendMessage(osc_address, osc_args) => {
                    if let ControlFlow::Break(_) = send_osc_message(
                        destination,
                        sender_address,
                        &events,
                        osc_address,
                        osc_args,
                    ) {
                        continue;
                    }
                }
            },
            Err(mpsc::TryRecvError::Empty) => {
                // No command received, continue to receive OSC packets
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // The main thread has disconnected, stop the receiver
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
                    events
                        .send(OscWorkerEvent::MessageReceived(sender, packet))
                        .unwrap_or_else(|e| {
                            eprintln!("WORKER: Failed to send mesage received event: {e}")
                        });
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
    osc_address: String,
    osc_args: Vec<OscType>,
) -> ControlFlow<()> {
    let sender_socket = match create_sender_socket(sender_address) {
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
    match send_osc(&sender_socket, destination_address, &osc_address, osc_args) {
        Ok(bytes_sent) => {
            events
                .send(OscWorkerEvent::MessageSent(bytes_sent))
                .unwrap_or_else(|e| eprintln!("WORKER: Failed to send message sent event: {e}"));
        }
        Err(error) => {
            eprintln!("WORKER: Failed to send OSC message: {error}");
            events
                .send(OscWorkerEvent::Error(OscWorkerError::IoError(error)))
                .unwrap_or_else(|e| eprintln!("WORKER: Failed to send error event: {e}"));
        }
    }
    ControlFlow::Continue(())
}

/// Binds a UDP socket on `port` and joins the multicast `group` (with loopback enabled).
/// # Arguments
/// * `multicast_group` - The multicast group to join.
/// * `listener_address` - The address to listen on.
pub fn create_receiver_socket(
    multicast_group: Ipv4Addr,
    listener_address: SocketAddrV4,
) -> Result<UdpSocket, io::Error> {
    let socket = UdpSocket::bind(listener_address)?;
    socket.join_multicast_v4(&multicast_group, &listener_address.ip())?;
    socket.set_multicast_loop_v4(true)?;
    Ok(socket)
}
/// * `sender_address` - The address to send the message from.
pub fn create_sender_socket(sender_address: SocketAddrV4) -> io::Result<UdpSocket> {
    UdpSocket::bind(sender_address).map(|socket| {
        socket.set_multicast_loop_v4(true).unwrap();
        socket
    })
}

/// Encodes and sends an OSC message to `destination_ip`; returns bytes sent.
pub fn send_osc(
    socket: &UdpSocket,
    destination_ip: SocketAddrV4,
    osc_address: &str,
    osc_args: Vec<OscType>,
) -> io::Result<usize> {
    let buf = encoder::encode(&OscPacket::Message(OscMessage {
        addr: osc_address.to_string(),
        args: osc_args,
    }))
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;
    socket.send_to(&buf, destination_ip)
}
