use rosc::{OscMessage, OscPacket, OscType, decoder, encoder};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::thread::{self, JoinHandle};

// Default network configuration for the OSC Messenger standard client 1
pub const GROUP: Ipv4Addr = Ipv4Addr::new(239, 254, 0, 1);
pub const PORT: u16 = 3090;

/// Binds a UDP socket on `port` and joins the multicast `group` (with loopback enabled).
pub fn create_receiver_socket(group: Ipv4Addr, port: u16) -> io::Result<UdpSocket> {
    let socket = UdpSocket::bind(("0.0.0.0", port))?;
    socket.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_loop_v4(true)?;
    Ok(socket)
}

/// Creates an ephemeral-port socket for sending.
pub fn create_sender_socket() -> io::Result<UdpSocket> {
    UdpSocket::bind("0.0.0.0:0")
}

/// Spawns a thread that receives OSC packets on `socket` and passes each to `handler`.
/// The thread ends on a socket error.
pub fn spawn_receiver<F>(socket: UdpSocket, mut handler: F) -> JoinHandle<()>
where
    F: FnMut(SocketAddr, OscPacket) + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        loop {
            match socket.recv_from(&mut buffer) {
                Ok((size, sender)) => match decoder::decode_udp(&buffer[..size]) {
                    Ok((_, packet)) => handler(sender, packet),
                    Err(error) => {
                        eprintln!("RECEIVER: OSC decode error from {sender}: {error}")
                    }
                },
                Err(error) => {
                    eprintln!("RECEIVER: Receive error: {error}");
                    break;
                }
            }
        }
    })
}

/// Convenience: creates the multicast receiver socket and spawns the receiver thread.
pub fn start_receiver<F>(group: Ipv4Addr, port: u16, handler: F) -> io::Result<JoinHandle<()>>
where
    F: FnMut(SocketAddr, OscPacket) + Send + 'static,
{
    Ok(spawn_receiver(create_receiver_socket(group, port)?, handler))
}

/// Encodes and sends an OSC message to `destination`; returns bytes sent.
pub fn send_osc(
    socket: &UdpSocket,
    destination: SocketAddrV4,
    address: &str,
    args: Vec<OscType>,
) -> io::Result<usize> {
    let buf = encoder::encode(&OscPacket::Message(OscMessage {
        addr: address.to_string(),
        args,
    }))
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e:?}")))?;
    socket.send_to(&buf, destination)
}
