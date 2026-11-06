use super::*;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

fn client(client_uuid: &str) -> MessengerClient {
    MessengerClient::new_with_uuid(
        client_uuid.to_string(),
        "User".to_string(),
        "App".to_string(),
    )
}

#[test]
fn welcomes_other_introducing_clients_with_own_identity() {
    let local_client = client("local");
    let introduced_client = client("remote");
    let introduce = Message::Me(Me::Introduce(introduced_client));

    assert_eq!(
        welcome_response(&introduce, &local_client),
        Some(Message::Me(Me::Welcome(local_client)))
    );
}

#[test]
fn does_not_welcome_own_looped_back_introduction() {
    let local_client = client("local");
    let introduce = Message::Me(Me::Introduce(local_client.clone()));

    assert_eq!(welcome_response(&introduce, &local_client), None);
}

#[test]
fn does_not_welcome_non_introduction_messages() {
    let local_client = client("local");
    let welcome = Message::Me(Me::Welcome(client("remote")));

    assert_eq!(welcome_response(&welcome, &local_client), None);
}

fn received_welcome_from(handle: &OscWorkerHandle, expected_client_uuid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return false;
        }

        match handle.event_receiver().recv_timeout(timeout) {
            Ok(event) => match event {
                OscWorkerEvent::MessageReceived(_, Message::Me(Me::Welcome(client)))
                    if client.client_uuid() == expected_client_uuid =>
                {
                    return true;
                }
                OscWorkerEvent::Error(error) => {
                    eprintln!("wire test worker error: {error:?}");
                }
                _ => {}
            },
            Err(_) => return false,
        }
    }
}

#[test]
#[ignore = "requires multicast loopback and shared-port support"]
fn two_workers_exchange_welcomes_over_multicast() {
    let probe_socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let port = probe_socket.local_addr().unwrap().port();
    drop(probe_socket);

    let worker_config = |client_name: &str| {
        config::Config::new(
            Some(config::DEFAULT_MULTICAST_GROUP),
            Some(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)),
            None,
            Some(client_name.to_string()),
            Some("wire test".to_string()),
        )
    };

    let first = OscWorkerHandle::spawn(worker_config("first client")).unwrap();
    let second = OscWorkerHandle::spawn(worker_config("second client")).unwrap();
    let first_uuid = first.client.client_uuid().to_string();
    let second_uuid = second.client.client_uuid().to_string();

    first
        .send_osc(Message::Me(Me::Introduce(first.client.clone())))
        .unwrap();
    second
        .send_osc(Message::Me(Me::Introduce(second.client.clone())))
        .unwrap();

    let first_received_welcome = received_welcome_from(&first, &second_uuid);
    let second_received_welcome = received_welcome_from(&second, &first_uuid);

    first.stop_osc_worker().unwrap();
    second.stop_osc_worker().unwrap();
    first.join().unwrap();
    second.join().unwrap();

    assert!(
        first_received_welcome,
        "first client did not receive second client's welcome"
    );
    assert!(
        second_received_welcome,
        "second client did not receive first client's welcome"
    );
}
