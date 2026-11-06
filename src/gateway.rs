use crate::config;
use crate::net::{create_receiver_socket, create_sender_socket};
use crate::protocol::validate_uuid;
use rosc::{OscBundle, OscMessage, OscPacket, OscType, decoder, encoder};
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const GATEWAY_PORT: u16 = 9030;
const MAX_TCP_PACKET_BYTES: usize = 65_507;
const OUTBOUND_QUEUE_CAPACITY: usize = 256;
const MAX_PENDING_CONNECTIONS: usize = 32;
const TCP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const TCP_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const MULTICAST_POLL_INTERVAL: Duration = Duration::from_millis(100);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const PATRON_TIMEOUT: Duration = Duration::from_secs(30);
const LOOPBACK_CACHE_WINDOW: Duration = Duration::from_secs(3);
const LOOPBACK_CACHE_CAPACITY: usize = 256;

const REQUEST: &str = "/messenger/gateway/me/request";
const ACCEPT: &str = "/messenger/gateway/me/accept";
const DENY: &str = "/messenger/gateway/me/deny";
const WHO_ACTIVE: &str = "/messenger/gateway/who/active";
const ME_PATRON: &str = "/messenger/gateway/me/patron";
const ME_LEAVE: &str = "/messenger/gateway/me/leave";
const ME_SHUTDOWN: &str = "/messenger/gateway/me/shutdown";
const GATEWAY_PREFIX: &str = "/messenger/gateway/";

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub gateway_uuid: String,
    pub listen_address: SocketAddr,
    pub multicast_group: Ipv4Addr,
    pub multicast_listener_address: SocketAddrV4,
    pub multicast_sender_address: SocketAddrV4,
    pub max_patrons: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            gateway_uuid: uuid::Uuid::new_v4().to_string().to_uppercase(),
            listen_address: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, GATEWAY_PORT).into(),
            multicast_group: config::DEFAULT_MULTICAST_GROUP,
            multicast_listener_address: SocketAddrV4::new(
                config::DEFAULT_LISTENER_ADDRESS,
                config::DEFAULT_LISTENER_PORT,
            ),
            multicast_sender_address: SocketAddrV4::new(
                config::DEFAULT_SENDER_ADDRESS,
                config::DEFAULT_SENDER_PORT,
            ),
            max_patrons: 64,
        }
    }
}

struct Patron {
    uuid: String,
    outbound: SyncSender<Vec<u8>>,
    write_stream: Arc<Mutex<TcpStream>>,
    control_stream: TcpStream,
    last_pong: Instant,
}

struct LoopbackPacket {
    received_at: Instant,
    bytes: Vec<u8>,
}

struct GatewayShared {
    gateway_uuid: String,
    max_patrons: usize,
    multicast_destination: SocketAddrV4,
    multicast_sender: UdpSocket,
    patrons: Mutex<HashMap<u64, Patron>>,
    next_patron_id: AtomicU64,
    pending_connections: AtomicUsize,
    loopback_packets: Mutex<VecDeque<LoopbackPacket>>,
}

struct PendingConnection(Arc<GatewayShared>);

impl Drop for PendingConnection {
    fn drop(&mut self) {
        self.0.pending_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

pub fn run_gateway(config: GatewayConfig, shutdown: Receiver<()>) -> io::Result<()> {
    validate_uuid(&config.gateway_uuid)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if config.max_patrons == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "max_patrons must be greater than zero",
        ));
    }

    let listener = TcpListener::bind(config.listen_address)?;
    listener.set_nonblocking(true)?;
    let multicast_receiver =
        create_receiver_socket(config.multicast_group, config.multicast_listener_address)?;
    multicast_receiver.set_read_timeout(Some(MULTICAST_POLL_INTERVAL))?;
    let multicast_sender = create_sender_socket(config.multicast_sender_address)?;
    let multicast_destination = SocketAddrV4::new(
        config.multicast_group,
        config.multicast_listener_address.port(),
    );
    let shared = Arc::new(GatewayShared {
        gateway_uuid: config.gateway_uuid,
        max_patrons: config.max_patrons,
        multicast_destination,
        multicast_sender,
        patrons: Mutex::new(HashMap::new()),
        next_patron_id: AtomicU64::new(1),
        pending_connections: AtomicUsize::new(0),
        loopback_packets: Mutex::new(VecDeque::new()),
    });
    let mut connection_threads = Vec::new();
    let mut buffer = [0u8; 65_536];
    let mut last_keepalive = Instant::now();

    eprintln!(
        "Gateway {} listening on {}",
        shared.gateway_uuid, config.listen_address
    );

    let run_result = 'run: loop {
        match shutdown.try_recv() {
            Ok(()) | Err(mpsc::TryRecvError::Disconnected) => break Ok(()),
            Err(mpsc::TryRecvError::Empty) => {}
        }

        loop {
            match listener.accept() {
                Ok((stream, address)) => {
                    let pending = shared.pending_connections.fetch_add(1, Ordering::Relaxed);
                    if pending >= MAX_PENDING_CONNECTIONS {
                        shared.pending_connections.fetch_sub(1, Ordering::Relaxed);
                        eprintln!(
                            "Rejecting connection from {address}: too many pending connections"
                        );
                        continue;
                    }
                    let connection_shared = Arc::clone(&shared);
                    match thread::Builder::new().spawn(move || {
                        let _pending = PendingConnection(Arc::clone(&connection_shared));
                        handle_patron_connection(stream, connection_shared);
                    }) {
                        Ok(thread) => connection_threads.push(thread),
                        Err(error) => {
                            eprintln!("Failed to spawn patron handler for {address}: {error}");
                            shared.pending_connections.fetch_sub(1, Ordering::Relaxed);
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => break 'run Err(error),
            }
        }

        match multicast_receiver.recv_from(&mut buffer) {
            Ok((size, sender)) => {
                let bytes = &buffer[..size];
                if !consume_loopback_packet(&shared, bytes) {
                    match decoder::decode_udp(bytes) {
                        Ok((_, packet)) if !contains_gateway_message(&packet) => {
                            broadcast_packet(&shared, &packet, None);
                        }
                        Ok(_) => {}
                        Err(error) => {
                            eprintln!("Ignoring invalid multicast packet from {sender}: {error}")
                        }
                    }
                }
            }
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::TimedOut => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => break Err(error),
        }

        if last_keepalive.elapsed() >= KEEPALIVE_INTERVAL {
            let active = string_message(WHO_ACTIVE, &[&shared.gateway_uuid]);
            broadcast_packet(&shared, &active, None);
            last_keepalive = Instant::now();
        }
        disconnect_expired_patrons(&shared);
        reap_connection_threads(&mut connection_threads);
    };

    send_gateway_shutdown(&shared);
    let patron_ids: Vec<_> = lock(&shared.patrons).keys().copied().collect();
    for patron_id in patron_ids {
        remove_patron(&shared, patron_id);
    }
    for thread in connection_threads {
        if thread.join().is_err() {
            eprintln!("Patron connection thread panicked during shutdown");
        }
    }
    crate::net::leave_multicast_group(
        &multicast_receiver,
        config.multicast_group,
        config.multicast_listener_address.ip(),
    )?;
    run_result
}

fn handle_patron_connection(mut stream: TcpStream, shared: Arc<GatewayShared>) {
    if let Err(error) = stream
        .set_read_timeout(Some(TCP_HANDSHAKE_TIMEOUT))
        .and_then(|_| stream.set_write_timeout(Some(TCP_WRITE_TIMEOUT)))
    {
        eprintln!("Failed to configure patron connection: {error}");
        return;
    }

    let request = match read_tcp_packet(&mut stream) {
        Ok(Some(request)) => request,
        Ok(None) => return,
        Err(error) => {
            eprintln!("Failed to read gateway request: {error}");
            return;
        }
    };
    let patron_uuid = match parse_gateway_request(&request) {
        Ok(patron_uuid) => patron_uuid,
        Err(error) => {
            eprintln!("Rejecting invalid gateway request: {error}");
            return;
        }
    };

    let patron_id = shared.next_patron_id.fetch_add(1, Ordering::Relaxed);
    let (outbound, outbound_rx) = mpsc::sync_channel(OUTBOUND_QUEUE_CAPACITY);
    let writer_stream = match stream.try_clone() {
        Ok(stream) => Arc::new(Mutex::new(stream)),
        Err(error) => {
            eprintln!("Failed to clone patron stream: {error}");
            return;
        }
    };
    let control_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("Failed to clone patron control stream: {error}");
            return;
        }
    };

    let mut patrons = lock(&shared.patrons);
    let duplicate = patrons.values().any(|patron| patron.uuid == patron_uuid);
    if patrons.len() >= shared.max_patrons || duplicate {
        drop(patrons);
        let deny = string_message(DENY, &[&shared.gateway_uuid, &patron_uuid]);
        if let Err(error) = write_tcp_packet(&mut stream, &deny) {
            eprintln!("Failed to send gateway denial: {error}");
        }
        return;
    }

    let writer_shared = Arc::clone(&shared);
    let writer_stream_for_thread = Arc::clone(&writer_stream);
    let writer = match thread::Builder::new().spawn(move || {
        patron_writer(
            patron_id,
            writer_stream_for_thread,
            outbound_rx,
            writer_shared,
        );
    }) {
        Ok(writer) => writer,
        Err(error) => {
            drop(patrons);
            let deny = string_message(DENY, &[&shared.gateway_uuid, &patron_uuid]);
            if let Err(write_error) = write_tcp_packet(&mut stream, &deny) {
                eprintln!("Failed to deny patron after thread-spawn failure: {write_error}");
            }
            eprintln!("Failed to spawn patron writer: {error}");
            return;
        }
    };

    let accept = string_message(ACCEPT, &[&shared.gateway_uuid, &patron_uuid]);
    if let Err(error) = write_tcp_packet(&mut stream, &accept) {
        drop(outbound);
        drop(patrons);
        let _ = control_stream.shutdown(std::net::Shutdown::Both);
        let _ = writer.join();
        eprintln!("Failed to send gateway acceptance: {error}");
        return;
    }

    patrons.insert(
        patron_id,
        Patron {
            uuid: patron_uuid.clone(),
            outbound,
            write_stream: writer_stream,
            control_stream,
            last_pong: Instant::now(),
        },
    );
    drop(patrons);
    eprintln!("Accepted patron {patron_uuid}");

    if let Err(error) = stream.set_read_timeout(None) {
        eprintln!("Failed to configure patron stream: {error}");
        remove_patron(&shared, patron_id);
        let _ = writer.join();
        return;
    }

    loop {
        match read_tcp_packet(&mut stream) {
            Ok(Some(packet)) => {
                if !handle_patron_packet(&shared, patron_id, &patron_uuid, &packet) {
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => {
                eprintln!("Patron {patron_uuid} disconnected: {error}");
                break;
            }
        }
    }

    remove_patron(&shared, patron_id);
    if writer.join().is_err() {
        eprintln!("Patron {patron_uuid} writer thread panicked");
    }
}

fn patron_writer(
    patron_id: u64,
    stream: Arc<Mutex<TcpStream>>,
    receiver: Receiver<Vec<u8>>,
    shared: Arc<GatewayShared>,
) {
    while let Ok(frame) = receiver.recv() {
        let result = lock(&stream).write_all(&frame);
        if let Err(error) = result {
            eprintln!("Failed to write to patron {patron_id}: {error}");
            remove_patron(&shared, patron_id);
            break;
        }
    }
}

fn handle_patron_packet(
    shared: &Arc<GatewayShared>,
    patron_id: u64,
    patron_uuid: &str,
    packet: &OscPacket,
) -> bool {
    let Some(message) = packet_message(packet) else {
        eprintln!("Ignoring OSC bundle from patron {patron_uuid}");
        return true;
    };

    match message.addr.as_str() {
        ME_PATRON => {
            if string_argument(&message.args).as_deref() == Some(patron_uuid)
                && let Some(patron) = lock(&shared.patrons).get_mut(&patron_id)
            {
                patron.last_pong = Instant::now();
            }
            true
        }
        ME_LEAVE => string_argument(&message.args).as_deref() != Some(patron_uuid),
        address if address.starts_with(GATEWAY_PREFIX) => true,
        _ => {
            match encoder::encode(packet) {
                Ok(bytes) => {
                    match shared
                        .multicast_sender
                        .send_to(&bytes, shared.multicast_destination)
                    {
                        Ok(_) => remember_loopback_packet(shared, bytes),
                        Err(error) => eprintln!("Failed to multicast patron message: {error}"),
                    }
                }
                Err(error) => eprintln!("Failed to encode patron packet for multicast: {error}"),
            }
            broadcast_packet(shared, packet, Some(patron_id));
            true
        }
    }
}

fn broadcast_packet(shared: &Arc<GatewayShared>, packet: &OscPacket, excluded_patron: Option<u64>) {
    let frame = match encoder::encode_tcp(packet) {
        Ok(frame) => frame,
        Err(error) => {
            eprintln!("Failed to encode packet for patron forwarding: {error}");
            return;
        }
    };

    let failed: Vec<_> = {
        let patrons = lock(&shared.patrons);
        patrons
            .iter()
            .filter(|(id, _)| Some(**id) != excluded_patron)
            .filter_map(|(id, patron)| patron.outbound.try_send(frame.clone()).err().map(|_| *id))
            .collect()
    };
    for patron_id in failed {
        eprintln!("Removing patron {patron_id}: outbound queue is full or closed");
        remove_patron(shared, patron_id);
    }
}

fn disconnect_expired_patrons(shared: &Arc<GatewayShared>) {
    let now = Instant::now();
    let expired: Vec<_> = lock(&shared.patrons)
        .iter()
        .filter_map(|(id, patron)| {
            (now.duration_since(patron.last_pong) >= PATRON_TIMEOUT).then_some(*id)
        })
        .collect();
    for patron_id in expired {
        eprintln!("Removing patron {patron_id}: keepalive timed out");
        remove_patron(shared, patron_id);
    }
}

fn send_gateway_shutdown(shared: &Arc<GatewayShared>) {
    let packet = string_message(ME_SHUTDOWN, &[&shared.gateway_uuid]);
    let frame = match encoder::encode_tcp(&packet) {
        Ok(frame) => frame,
        Err(error) => {
            eprintln!("Failed to encode gateway shutdown message: {error}");
            return;
        }
    };
    let streams: Vec<_> = lock(&shared.patrons)
        .values()
        .map(|patron| Arc::clone(&patron.write_stream))
        .collect();
    for stream in streams {
        if let Err(error) = lock(&stream).write_all(&frame) {
            eprintln!("Failed to notify patron of gateway shutdown: {error}");
        }
    }
}

fn remove_patron(shared: &Arc<GatewayShared>, patron_id: u64) {
    if let Some(patron) = lock(&shared.patrons).remove(&patron_id) {
        let _ = patron.control_stream.shutdown(std::net::Shutdown::Both);
    }
}

fn remember_loopback_packet(shared: &Arc<GatewayShared>, bytes: Vec<u8>) {
    let mut cache = lock(&shared.loopback_packets);
    let now = Instant::now();
    cache.retain(|entry| now.duration_since(entry.received_at) < LOOPBACK_CACHE_WINDOW);
    if cache.len() == LOOPBACK_CACHE_CAPACITY {
        cache.pop_front();
    }
    cache.push_back(LoopbackPacket {
        received_at: now,
        bytes,
    });
}

fn consume_loopback_packet(shared: &Arc<GatewayShared>, bytes: &[u8]) -> bool {
    let mut cache = lock(&shared.loopback_packets);
    let now = Instant::now();
    cache.retain(|entry| now.duration_since(entry.received_at) < LOOPBACK_CACHE_WINDOW);
    if let Some(index) = cache.iter().position(|entry| entry.bytes == bytes) {
        cache.remove(index);
        true
    } else {
        false
    }
}

fn read_tcp_packet<R: Read>(reader: &mut R) -> io::Result<Option<OscPacket>> {
    let mut length_bytes = [0u8; 4];
    loop {
        match reader.read(&mut length_bytes[..1]) {
            Ok(0) => return Ok(None),
            Ok(1) => break,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Reader returned more bytes than requested",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    reader.read_exact(&mut length_bytes[1..])?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length == 0 || length > MAX_TCP_PACKET_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid OSC/TCP packet length: {length}"),
        ));
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;
    let (remaining, packet) = decoder::decode_udp(&payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "OSC/TCP frame contains trailing bytes",
        ));
    }
    Ok(Some(packet))
}

fn write_tcp_packet<W: Write>(writer: &mut W, packet: &OscPacket) -> io::Result<()> {
    let frame = encoder::encode_tcp(packet)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    writer.write_all(&frame)
}

fn parse_gateway_request(packet: &OscPacket) -> io::Result<String> {
    let message = packet_message(packet)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Expected OSC message"))?;
    if message.addr != REQUEST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Expected gateway request message",
        ));
    }
    string_argument(&message.args)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid patron UUID argument"))
}

fn packet_message(packet: &OscPacket) -> Option<&OscMessage> {
    match packet {
        OscPacket::Message(message) => Some(message),
        OscPacket::Bundle(_) => None,
    }
}

fn string_argument(args: &[OscType]) -> Option<String> {
    if let [OscType::String(value)] = args {
        Some(value.clone())
    } else {
        None
    }
}

fn string_message(address: &str, values: &[&str]) -> OscPacket {
    OscPacket::Message(OscMessage {
        addr: address.to_string(),
        args: values
            .iter()
            .map(|value| OscType::String((*value).to_string()))
            .collect(),
    })
}

fn contains_gateway_message(packet: &OscPacket) -> bool {
    match packet {
        OscPacket::Message(message) => message.addr.starts_with(GATEWAY_PREFIX),
        OscPacket::Bundle(OscBundle { content, .. }) => {
            content.iter().any(contains_gateway_message)
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn reap_connection_threads(threads: &mut Vec<JoinHandle<()>>) {
    let mut index = 0;
    while index < threads.len() {
        if threads[index].is_finished() {
            let thread = threads.swap_remove(index);
            if thread.join().is_err() {
                eprintln!("Patron connection thread panicked");
            }
        } else {
            index += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn test_shared(max_patrons: usize, multicast_destination: SocketAddrV4) -> Arc<GatewayShared> {
        Arc::new(GatewayShared {
            gateway_uuid: "B0BAE0A0-3BBE-4004-888B-F61CA125D0B0".to_string(),
            max_patrons,
            multicast_destination,
            multicast_sender: UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap(),
            patrons: Mutex::new(HashMap::new()),
            next_patron_id: AtomicU64::new(1),
            pending_connections: AtomicUsize::new(0),
            loopback_packets: Mutex::new(VecDeque::new()),
        })
    }

    fn connect_patron(
        shared: Arc<GatewayShared>,
        patron_uuid: &str,
    ) -> (TcpStream, JoinHandle<()>, OscPacket) {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let handler = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle_patron_connection(stream, shared);
        });
        let mut patron = TcpStream::connect(address).unwrap();
        patron
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write_tcp_packet(&mut patron, &string_message(REQUEST, &[patron_uuid])).unwrap();
        let response = read_tcp_packet(&mut patron).unwrap().unwrap();
        (patron, handler, response)
    }

    #[test]
    fn tcp_frame_round_trips_request_packet() {
        let packet = string_message(REQUEST, &["B0BAE0A0-3BBE-4004-888B-F61CA125D0B0"]);
        let mut frame = Vec::new();
        write_tcp_packet(&mut frame, &packet).unwrap();

        let mut cursor = Cursor::new(frame);
        assert_eq!(read_tcp_packet(&mut cursor).unwrap(), Some(packet));
        assert_eq!(read_tcp_packet(&mut cursor).unwrap(), None);
    }

    #[test]
    fn gateway_request_parses_patron_uuid() {
        let packet = string_message(REQUEST, &["patron-id"]);
        assert_eq!(parse_gateway_request(&packet).unwrap(), "patron-id");
    }

    #[test]
    fn gateway_control_bundles_are_filtered_from_forwarding() {
        let packet = OscPacket::Bundle(OscBundle {
            timetag: rosc::OscTime {
                seconds: 1,
                fractional: 1,
            },
            content: vec![string_message(WHO_ACTIVE, &["gateway-id"])],
        });
        assert!(contains_gateway_message(&packet));
    }

    #[test]
    fn gateway_keepalive_can_be_queued_to_patrons() {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let write_stream = Arc::new(Mutex::new(stream.try_clone().unwrap()));
        let control_stream = stream.try_clone().unwrap();
        let (outbound, outbound_rx) = mpsc::sync_channel(1);
        let multicast_sender =
            UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let shared = Arc::new(GatewayShared {
            gateway_uuid: "gateway-id".to_string(),
            max_patrons: 1,
            multicast_destination: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 3090),
            multicast_sender,
            patrons: Mutex::new(HashMap::from([(
                1,
                Patron {
                    uuid: "patron-id".to_string(),
                    outbound,
                    write_stream,
                    control_stream,
                    last_pong: Instant::now(),
                },
            )])),
            next_patron_id: AtomicU64::new(2),
            pending_connections: AtomicUsize::new(0),
            loopback_packets: Mutex::new(VecDeque::new()),
        });
        let active = string_message(WHO_ACTIVE, &["gateway-id"]);

        broadcast_packet(&shared, &active, None);

        let tcp_frame = outbound_rx.try_recv().unwrap();
        let (_, decoded) = decoder::decode_tcp(&tcp_frame).unwrap();
        assert_eq!(decoded, Some(active));
        drop(peer);
    }

    #[test]
    fn patron_handshake_capacity_forwarding_and_leave() {
        let multicast_receiver =
            UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        multicast_receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let multicast_destination = SocketAddrV4::new(
            Ipv4Addr::LOCALHOST,
            multicast_receiver.local_addr().unwrap().port(),
        );
        let shared = test_shared(2, multicast_destination);

        let (mut first, first_handler, first_response) =
            connect_patron(Arc::clone(&shared), "first-patron");
        assert_eq!(
            first_response,
            string_message(
                ACCEPT,
                &["B0BAE0A0-3BBE-4004-888B-F61CA125D0B0", "first-patron"]
            )
        );

        let (mut second, second_handler, second_response) =
            connect_patron(Arc::clone(&shared), "second-patron");
        assert!(matches!(
            second_response,
            OscPacket::Message(OscMessage { addr, .. }) if addr == ACCEPT
        ));

        let (_third, third_handler, third_response) =
            connect_patron(Arc::clone(&shared), "third-patron");
        assert_eq!(
            third_response,
            string_message(
                DENY,
                &["B0BAE0A0-3BBE-4004-888B-F61CA125D0B0", "third-patron"]
            )
        );
        third_handler.join().unwrap();

        let patron_say = OscPacket::Message(OscMessage {
            addr: "/messenger/me/say".to_string(),
            args: vec![
                OscType::String("C0BAE0A0-3BBE-4004-888B-F61CA125D0B0".to_string()),
                OscType::String("first-patron".to_string()),
                OscType::String("hello".to_string()),
            ],
        });
        write_tcp_packet(&mut first, &patron_say).unwrap();

        let mut udp_buffer = [0u8; 2048];
        let (udp_size, _) = multicast_receiver.recv_from(&mut udp_buffer).unwrap();
        let (_, multicast_packet) = decoder::decode_udp(&udp_buffer[..udp_size]).unwrap();
        assert_eq!(multicast_packet, patron_say);
        assert_eq!(read_tcp_packet(&mut second).unwrap(), Some(patron_say));

        write_tcp_packet(&mut first, &string_message(ME_LEAVE, &["first-patron"])).unwrap();
        first_handler.join().unwrap();
        write_tcp_packet(&mut second, &string_message(ME_LEAVE, &["second-patron"])).unwrap();
        second_handler.join().unwrap();
    }
}
