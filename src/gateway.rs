use crate::config;
use crate::net::{create_receiver_socket, create_sender_socket};
use crate::protocol::validate_uuid;
use rosc::{OscBundle, OscMessage, OscPacket, OscType, decoder, encoder};
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const GATEWAY_PORT: u16 = 9030;
const MAX_TCP_PACKET_BYTES: usize = 65_507;
const OUTBOUND_QUEUE_CAPACITY: usize = 256;
const MAX_PENDING_CONNECTIONS: usize = 32;
const MAX_ACCEPTS_PER_TICK: usize = 32;
const TCP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const TCP_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const PATRON_READ_POLL_INTERVAL: Duration = Duration::from_millis(250);
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
    writer_finished: Receiver<()>,
    control_stream: TcpStream,
    last_pong: Instant,
}

struct LoopbackPacket {
    received_at: Instant,
    source_port: u16,
    bytes: Vec<u8>,
}

struct GatewayShared {
    gateway_uuid: String,
    max_patrons: usize,
    multicast_destination: SocketAddrV4,
    multicast_sender: UdpSocket,
    multicast_sender_port: u16,
    shutting_down: AtomicBool,
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
    let multicast_sender_port = multicast_sender.local_addr()?.port();
    let multicast_destination = SocketAddrV4::new(
        config.multicast_group,
        config.multicast_listener_address.port(),
    );
    let shared = Arc::new(GatewayShared {
        gateway_uuid: config.gateway_uuid,
        max_patrons: config.max_patrons,
        multicast_destination,
        multicast_sender,
        multicast_sender_port,
        shutting_down: AtomicBool::new(false),
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

        let mut accepted_this_tick = 0;
        loop {
            if accepted_this_tick >= MAX_ACCEPTS_PER_TICK {
                break;
            }
            match listener.accept() {
                Ok((stream, address)) => {
                    accepted_this_tick += 1;
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
                        let pending = PendingConnection(Arc::clone(&connection_shared));
                        handle_patron_connection(stream, connection_shared, pending);
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
                forward_multicast_datagram(&shared, sender, &buffer[..size]);
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

    shared.shutting_down.store(true, Ordering::Release);
    send_gateway_shutdown(&shared);
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

fn handle_patron_connection(
    mut stream: TcpStream,
    shared: Arc<GatewayShared>,
    pending: PendingConnection,
) {
    if let Err(error) = stream
        .set_nonblocking(false)
        .and_then(|_| stream.set_read_timeout(Some(TCP_HANDSHAKE_TIMEOUT)))
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
    let (writer_finished_tx, writer_finished) = mpsc::channel();
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

    let accept = string_message(ACCEPT, &[&shared.gateway_uuid, &patron_uuid]);
    let accept_frame = match encoder::encode_tcp(&accept) {
        Ok(frame) => frame,
        Err(error) => {
            drop(patrons);
            eprintln!("Failed to encode gateway acceptance: {error}");
            return;
        }
    };

    let writer_shared = Arc::clone(&shared);
    let writer_stream_for_thread = Arc::clone(&writer_stream);
    let writer = match thread::Builder::new().spawn(move || {
        patron_writer(
            patron_id,
            writer_stream_for_thread,
            outbound_rx,
            writer_shared,
            writer_finished_tx,
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

    let accept_sender = outbound.clone();
    patrons.insert(
        patron_id,
        Patron {
            uuid: patron_uuid.clone(),
            outbound,
            writer_finished,
            control_stream,
            last_pong: Instant::now(),
        },
    );
    if let Err(error) = accept_sender.try_send(accept_frame) {
        drop(accept_sender);
        drop(patrons);
        remove_patron(&shared, patron_id);
        let _ = writer.join();
        eprintln!("Failed to queue gateway acceptance: {error}");
        return;
    }
    drop(accept_sender);
    drop(patrons);
    drop(pending);
    eprintln!("Accepted patron {patron_uuid}");

    if let Err(error) = stream.set_read_timeout(Some(PATRON_READ_POLL_INTERVAL)) {
        eprintln!("Failed to configure patron stream: {error}");
        remove_patron(&shared, patron_id);
        let _ = writer.join();
        return;
    }

    let mut packet_reader = TcpPacketReader::default();
    loop {
        if shared.shutting_down.load(Ordering::Acquire) {
            break;
        }
        match packet_reader.read_next(&mut stream) {
            Ok(TcpRead::Packet(packet)) => {
                if !handle_patron_packet(&shared, patron_id, &patron_uuid, &packet) {
                    break;
                }
            }
            Ok(TcpRead::Idle) => {}
            Ok(TcpRead::Closed) => break,
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
    finished: mpsc::Sender<()>,
) {
    while let Ok(frame) = receiver.recv() {
        let result = lock(&stream).write_all(&frame);
        if let Err(error) = result {
            eprintln!("Failed to write to patron {patron_id}: {error}");
            remove_patron(&shared, patron_id);
            break;
        }
    }
    let _ = finished.send(());
}

fn handle_patron_packet(
    shared: &Arc<GatewayShared>,
    patron_id: u64,
    patron_uuid: &str,
    packet: &OscPacket,
) -> bool {
    if !handle_patron_controls(shared, patron_id, patron_uuid, packet) {
        return false;
    }
    if let Some(packet) = filter_gateway_messages(packet) {
        forward_patron_packet(shared, patron_id, &packet);
    }
    true
}

fn handle_patron_controls(
    shared: &Arc<GatewayShared>,
    patron_id: u64,
    patron_uuid: &str,
    packet: &OscPacket,
) -> bool {
    match packet {
        OscPacket::Message(message) => match message.addr.as_str() {
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
            _ => true,
        },
        OscPacket::Bundle(bundle) => bundle
            .content
            .iter()
            .all(|packet| handle_patron_controls(shared, patron_id, patron_uuid, packet)),
    }
}

fn forward_patron_packet(shared: &Arc<GatewayShared>, patron_id: u64, packet: &OscPacket) {
    match encoder::encode(packet) {
        Ok(bytes) => match shared
            .multicast_sender
            .send_to(&bytes, shared.multicast_destination)
        {
            Ok(_) => remember_loopback_packet(shared, shared.multicast_sender_port, bytes),
            Err(error) => eprintln!("Failed to multicast patron message: {error}"),
        },
        Err(error) => eprintln!("Failed to encode patron packet for multicast: {error}"),
    }
    broadcast_packet(shared, packet, Some(patron_id));
}

fn forward_multicast_datagram(shared: &Arc<GatewayShared>, sender: SocketAddr, bytes: &[u8]) {
    if consume_loopback_packet(shared, sender.port(), bytes) {
        return;
    }
    match decoder::decode_udp(bytes) {
        Ok((_, packet)) => {
            if let Some(packet) = filter_gateway_messages(&packet) {
                broadcast_packet(shared, &packet, None);
            }
        }
        Err(error) => eprintln!("Ignoring invalid multicast packet from {sender}: {error}"),
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
    let patrons = std::mem::take(&mut *lock(&shared.patrons));
    let mut writers = Vec::with_capacity(patrons.len());
    for (patron_id, patron) in patrons {
        if let Err(error) = patron.outbound.send(frame.clone()) {
            eprintln!("Failed to queue gateway shutdown for patron {patron_id}: {error}");
        }
        drop(patron.outbound);
        writers.push((patron_id, patron.writer_finished, patron.control_stream));
    }

    let deadline = Instant::now() + TCP_WRITE_TIMEOUT + Duration::from_secs(1);
    for (patron_id, writer_finished, control_stream) in writers {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if writer_finished.recv_timeout(remaining).is_err() {
            eprintln!("Timed out draining gateway shutdown for patron {patron_id}");
            let _ = control_stream.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn remove_patron(shared: &Arc<GatewayShared>, patron_id: u64) {
    if let Some(patron) = lock(&shared.patrons).remove(&patron_id) {
        let _ = patron.control_stream.shutdown(std::net::Shutdown::Both);
    }
}

fn remember_loopback_packet(shared: &Arc<GatewayShared>, source_port: u16, bytes: Vec<u8>) {
    let mut cache = lock(&shared.loopback_packets);
    let now = Instant::now();
    cache.retain(|entry| now.duration_since(entry.received_at) < LOOPBACK_CACHE_WINDOW);
    if cache.len() == LOOPBACK_CACHE_CAPACITY {
        cache.pop_front();
    }
    cache.push_back(LoopbackPacket {
        received_at: now,
        source_port,
        bytes,
    });
}

fn consume_loopback_packet(shared: &Arc<GatewayShared>, source_port: u16, bytes: &[u8]) -> bool {
    let mut cache = lock(&shared.loopback_packets);
    let now = Instant::now();
    cache.retain(|entry| now.duration_since(entry.received_at) < LOOPBACK_CACHE_WINDOW);
    if let Some(index) = cache
        .iter()
        .position(|entry| entry.source_port == source_port && entry.bytes == bytes)
    {
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

enum TcpRead {
    Packet(OscPacket),
    Idle,
    Closed,
}

#[derive(Default)]
struct TcpPacketReader {
    buffer: Vec<u8>,
}

impl TcpPacketReader {
    fn read_next(&mut self, stream: &mut TcpStream) -> io::Result<TcpRead> {
        loop {
            if self.buffer.len() >= 4 {
                let packet_length =
                    u32::from_be_bytes(self.buffer[..4].try_into().unwrap()) as usize;
                if packet_length == 0 || packet_length > MAX_TCP_PACKET_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("Invalid OSC/TCP packet length: {packet_length}"),
                    ));
                }
                let frame_length = 4 + packet_length;
                if self.buffer.len() >= frame_length {
                    let payload = &self.buffer[4..frame_length];
                    let (remaining, packet) = decoder::decode_udp(payload)
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                    if !remaining.is_empty() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "OSC/TCP frame contains trailing bytes",
                        ));
                    }
                    self.buffer.drain(..frame_length);
                    return Ok(TcpRead::Packet(packet));
                }
            }

            let mut chunk = [0u8; 8192];
            match stream.read(&mut chunk) {
                Ok(0) if self.buffer.is_empty() => return Ok(TcpRead::Closed),
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Patron closed during an OSC/TCP frame",
                    ));
                }
                Ok(bytes_read) => self.buffer.extend_from_slice(&chunk[..bytes_read]),
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock
                        || error.kind() == io::ErrorKind::TimedOut =>
                {
                    return Ok(TcpRead::Idle);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
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

fn filter_gateway_messages(packet: &OscPacket) -> Option<OscPacket> {
    match packet {
        OscPacket::Message(message) => {
            (!message.addr.starts_with(GATEWAY_PREFIX)).then(|| packet.clone())
        }
        OscPacket::Bundle(bundle) => {
            let content: Vec<_> = bundle
                .content
                .iter()
                .filter_map(filter_gateway_messages)
                .collect();
            if content.is_empty() {
                None
            } else {
                Some(OscPacket::Bundle(OscBundle {
                    timetag: bundle.timetag.clone(),
                    content,
                }))
            }
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
        let multicast_sender = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let multicast_sender_port = multicast_sender.local_addr().unwrap().port();
        Arc::new(GatewayShared {
            gateway_uuid: "B0BAE0A0-3BBE-4004-888B-F61CA125D0B0".to_string(),
            max_patrons,
            multicast_destination,
            multicast_sender,
            multicast_sender_port,
            shutting_down: AtomicBool::new(false),
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
        shared.pending_connections.fetch_add(1, Ordering::Relaxed);
        let connection_shared = Arc::clone(&shared);
        let handler = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let pending = PendingConnection(Arc::clone(&connection_shared));
            handle_patron_connection(stream, connection_shared, pending);
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
    fn gateway_controls_are_filtered_from_bundles() {
        let packet = OscPacket::Bundle(OscBundle {
            timetag: rosc::OscTime {
                seconds: 1,
                fractional: 1,
            },
            content: vec![
                string_message(WHO_ACTIVE, &["gateway-id"]),
                string_message("/messenger/me/say", &["message-id", "client-id", "hello"]),
            ],
        });

        let filtered = filter_gateway_messages(&packet).unwrap();
        let OscPacket::Bundle(filtered) = filtered else {
            panic!("expected filtered OSC bundle");
        };
        assert_eq!(filtered.content.len(), 1);
        assert!(matches!(
            &filtered.content[0],
            OscPacket::Message(OscMessage { addr, .. }) if addr == "/messenger/me/say"
        ));
    }

    #[test]
    fn gateway_control_only_bundle_is_removed() {
        let packet = OscPacket::Bundle(OscBundle {
            timetag: rosc::OscTime {
                seconds: 1,
                fractional: 1,
            },
            content: vec![string_message(WHO_ACTIVE, &["gateway-id"])],
        });

        assert!(filter_gateway_messages(&packet).is_none());
    }

    #[test]
    fn loopback_suppression_matches_source_port_and_packet() {
        let shared = test_shared(1, SocketAddrV4::new(Ipv4Addr::LOCALHOST, 3090));
        let packet = encoder::encode(&string_message("/messenger/me/say", &["same"])).unwrap();
        let source_port = shared.multicast_sender_port;
        remember_loopback_packet(&shared, source_port, packet.clone());

        assert!(!consume_loopback_packet(
            &shared,
            source_port.wrapping_add(1),
            &packet
        ));
        assert!(consume_loopback_packet(&shared, source_port, &packet));
    }

    #[test]
    fn gateway_keepalive_can_be_queued_to_patrons() {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let control_stream = stream.try_clone().unwrap();
        let (outbound, outbound_rx) = mpsc::sync_channel(1);
        let (_writer_finished_tx, writer_finished) = mpsc::channel();
        let multicast_sender =
            UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let multicast_sender_port = multicast_sender.local_addr().unwrap().port();
        let shared = Arc::new(GatewayShared {
            gateway_uuid: "gateway-id".to_string(),
            max_patrons: 1,
            multicast_destination: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 3090),
            multicast_sender,
            multicast_sender_port,
            shutting_down: AtomicBool::new(false),
            patrons: Mutex::new(HashMap::from([(
                1,
                Patron {
                    uuid: "patron-id".to_string(),
                    outbound,
                    writer_finished,
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
    fn multicast_packets_forward_to_patrons_but_gateway_controls_do_not() {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let control_stream = stream.try_clone().unwrap();
        let (outbound, outbound_rx) = mpsc::sync_channel(2);
        let (_writer_finished_tx, writer_finished) = mpsc::channel();
        let shared = test_shared(1, SocketAddrV4::new(Ipv4Addr::LOCALHOST, 3090));
        lock(&shared.patrons).insert(
            1,
            Patron {
                uuid: "patron-id".to_string(),
                outbound,
                writer_finished,
                control_stream,
                last_pong: Instant::now(),
            },
        );
        let sender = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 40000));
        let chat = string_message("/messenger/me/say", &["message-id", "client-id", "hello"]);
        let chat_bytes = encoder::encode(&chat).unwrap();

        forward_multicast_datagram(&shared, sender, &chat_bytes);

        let tcp_frame = outbound_rx.try_recv().unwrap();
        let (_, forwarded_chat) = decoder::decode_tcp(&tcp_frame).unwrap();
        assert_eq!(forwarded_chat, Some(chat));

        let gateway_control = string_message(WHO_ACTIVE, &["gateway-id"]);
        let control_bytes = encoder::encode(&gateway_control).unwrap();
        forward_multicast_datagram(&shared, sender, &control_bytes);
        assert!(matches!(
            outbound_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
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
        assert_eq!(shared.pending_connections.load(Ordering::Relaxed), 0);
        assert_eq!(lock(&shared.patrons).len(), 1);

        let (mut second, second_handler, second_response) =
            connect_patron(Arc::clone(&shared), "second-patron");
        assert!(matches!(
            second_response,
            OscPacket::Message(OscMessage { addr, .. }) if addr == ACCEPT
        ));
        assert_eq!(shared.pending_connections.load(Ordering::Relaxed), 0);
        assert_eq!(lock(&shared.patrons).len(), 2);

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
        assert_eq!(shared.pending_connections.load(Ordering::Relaxed), 0);
        assert_eq!(lock(&shared.patrons).len(), 2);

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

    #[test]
    #[ignore = "requires local multicast membership support"]
    fn gateway_server_accepts_patron_and_notifies_shutdown() {
        let tcp_probe = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let listen_address = tcp_probe.local_addr().unwrap();
        drop(tcp_probe);

        let udp_probe = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let multicast_listener_address = SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            udp_probe.local_addr().unwrap().port(),
        );
        drop(udp_probe);

        let config = GatewayConfig {
            gateway_uuid: "B0BAE0A0-3BBE-4004-888B-F61CA125D0B0".to_string(),
            listen_address,
            multicast_group: config::DEFAULT_MULTICAST_GROUP,
            multicast_listener_address,
            multicast_sender_address: SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0),
            max_patrons: 1,
        };
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let gateway = thread::spawn(move || run_gateway(config, shutdown_rx));
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut patron = loop {
            match TcpStream::connect_timeout(&listen_address, Duration::from_millis(100)) {
                Ok(stream) => break stream,
                Err(error) if Instant::now() < deadline => {
                    if gateway.is_finished() {
                        panic!("gateway exited before accepting patrons: {error}");
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("gateway did not begin listening: {error}"),
            }
        };
        patron
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let patron_uuid = "C0BAE0A0-3BBE-4004-888B-F61CA125D0B0";
        write_tcp_packet(&mut patron, &string_message(REQUEST, &[patron_uuid])).unwrap();
        assert_eq!(
            read_tcp_packet(&mut patron).unwrap(),
            Some(string_message(
                ACCEPT,
                &["B0BAE0A0-3BBE-4004-888B-F61CA125D0B0", patron_uuid]
            ))
        );

        shutdown_tx.send(()).unwrap();
        assert_eq!(
            read_tcp_packet(&mut patron).unwrap(),
            Some(string_message(
                ME_SHUTDOWN,
                &["B0BAE0A0-3BBE-4004-888B-F61CA125D0B0"]
            ))
        );
        gateway.join().unwrap().unwrap();
    }
}
