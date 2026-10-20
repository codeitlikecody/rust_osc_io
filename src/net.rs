use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

pub use rosc::{OscError, encoder};

use crate::protocol::Message;
use crate::worker::OscWorkerError;

/// Binds a UDP socket on `port` and joins the multicast `group` (with loopback enabled).
/// # Arguments
/// * `multicast_group` - The multicast group to join.
/// * `listener_address` - The address to listen on.
/// # Returns
/// A `Result` containing the created `UdpSocket` or an `io::Error`
pub fn create_receiver_socket(
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
pub fn leave_multicast_group(
    socket: &UdpSocket,
    multicast_group: Ipv4Addr,
    interface: &Ipv4Addr,
) -> Result<(), io::Error> {
    socket.leave_multicast_v4(&multicast_group, interface)?;
    Ok(())
}

/// Creates a UDP socket bound to the specified `sender_address` for sending OSC messages.
/// # Arguments
/// * `sender_address` - The address to bind the socket to.
/// # Returns
/// A `Result` containing the created `UdpSocket` or an `io::Error`
pub fn create_sender_socket(sender_address: SocketAddrV4) -> io::Result<UdpSocket> {
    UdpSocket::bind(sender_address).map(|socket| {
        socket.set_multicast_loop_v4(true).unwrap();
        socket
    })
}

/// Sends an OSC message to the specified `destination_address` using the provided `sender_address`.
/// # Arguments
/// * `destination_address` - The address to send the OSC message to.
/// * `sender_address` - The address to bind the sending socket to.
/// * `events` - A channel sender to report events back to the main thread.
/// * `message` - The OSC message to send.
/// # Returns
/// A `Result` indicating success or an `OscWorkerError`.
pub fn send_osc_message(
    destination_address: SocketAddrV4,
    sender_address: SocketAddrV4,
    message: Message,
) -> Result<usize, OscWorkerError> {
    let socket = match create_sender_socket(sender_address) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("WORKER: Failed to create sender socket: {error}");
            return Err(OscWorkerError::IoError(error));
        }
    };
    // Create a sender socket to send the message

    let message_osc = match message.to_osc() {
        Ok(message) => message,
        Err(error) => {
            eprintln!("WORKER: Failed to convert message to OSC packet: {error}");
            return Err(OscWorkerError::MessengerProtocolError(error));
        }
    };

    // Send the message to the multicast group and port
    match encoder::encode(&message_osc) {
        Ok(buf) => match socket.send_to(&buf, destination_address) {
            Ok(bytes_sent) => return Ok(bytes_sent),
            Err(error) => {
                eprintln!("WORKER: Failed to send OSC message: {error}");
                return Err(OscWorkerError::IoError(error));
            }
        },
        Err(error) => {
            eprintln!("WORKER: Failed to encode OSC message: {error}");
            return Err(OscWorkerError::OscError(error));
        }
    }
}
