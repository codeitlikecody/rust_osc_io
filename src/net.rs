pub use rosc::OscError;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

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
