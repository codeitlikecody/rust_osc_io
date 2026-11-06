use super::*;
use rosc::OscType;
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
        response_to(&introduce, &local_client, None),
        Some(Message::Me(Me::Welcome(local_client)))
    );
}

#[test]
fn does_not_welcome_own_looped_back_introduction() {
    let local_client = client("local");
    let introduce = Message::Me(Me::Introduce(local_client.clone()));

    assert_eq!(response_to(&introduce, &local_client, None), None);
}

#[test]
fn does_not_welcome_non_introduction_messages() {
    let local_client = client("local");
    let welcome = Message::Me(Me::Welcome(client("remote")));

    assert_eq!(response_to(&welcome, &local_client, None), None);
}

#[test]
fn replies_to_outside_room_query_when_not_in_that_room() {
    let local_client = client("local");
    let query = Message::Who(Who::Listening("lobby".to_string()));

    assert_eq!(
        response_to(&query, &local_client, None),
        Some(Message::Me(Me::Listening(local_client)))
    );
}

#[test]
fn does_not_reply_to_outside_room_query_from_inside_that_room() {
    let local_client = client("local");
    let query = Message::Who(Who::Listening("lobby".to_string()));

    assert_eq!(response_to(&query, &local_client, Some("lobby")), None);
}

#[test]
fn replies_to_in_room_query_only_when_in_that_room() {
    let local_client = client("local");
    let query = Message::Who(Who::ListeningInRoom("lobby".to_string()));

    assert_eq!(
        response_to(&query, &local_client, Some("lobby")),
        Some(Message::Me(Me::ListeningInRoom(RoomListening::new(
            "lobby".to_string(),
            local_client.clone(),
        ))))
    );
    assert_eq!(response_to(&query, &local_client, None), None);
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

fn received_listening_from(handle: &OscWorkerHandle, expected_client_uuid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return false;
        }

        match handle.event_receiver().recv_timeout(timeout) {
            Ok(OscWorkerEvent::MessageReceived(_, Message::Me(Me::Listening(client))))
                if client.client_uuid() == expected_client_uuid =>
            {
                return true;
            }
            Ok(OscWorkerEvent::Error(error)) => {
                eprintln!("wire test worker error: {error:?}");
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

fn received_in_room_listening_from(
    handle: &OscWorkerHandle,
    expected_client_uuid: &str,
    room_name: &str,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return false;
        }

        match handle.event_receiver().recv_timeout(timeout) {
            Ok(OscWorkerEvent::MessageReceived(_, Message::Me(Me::ListeningInRoom(listening))))
                if listening.room_name() == room_name
                    && listening.client().client_uuid() == expected_client_uuid =>
            {
                return true;
            }
            Ok(OscWorkerEvent::Error(error)) => {
                eprintln!("wire test worker error: {error:?}");
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

fn received_room_say_from(
    handle: &OscWorkerHandle,
    expected_client_uuid: &str,
    room_name: &str,
    expected_text: &str,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return false;
        }

        match handle.event_receiver().recv_timeout(timeout) {
            Ok(OscWorkerEvent::MessageReceived(_, Message::Me(Me::SayInRoom(room_say)))) => {
                let args = room_say.say().to_args();
                if room_say.room_name() == room_name
                    && args.len() == 3
                    && args[1] == OscType::String(expected_client_uuid.to_string())
                    && args[2] == OscType::String(expected_text.to_string())
                {
                    return true;
                }
            }
            Ok(OscWorkerEvent::Error(error)) => {
                eprintln!("wire test worker error: {error:?}");
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

fn wait_for_message_sent(handle: &OscWorkerHandle) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return false;
        }

        match handle.event_receiver().recv_timeout(timeout) {
            Ok(OscWorkerEvent::MessageSent(_)) => return true,
            Ok(OscWorkerEvent::Error(error)) => {
                eprintln!("wire test worker error: {error:?}");
            }
            Ok(_) => {}
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
    first
        .query_listening_outside_room("lobby".to_string())
        .unwrap();
    let first_received_listening = received_listening_from(&first, &second_uuid);
    let second_received_listening = received_listening_from(&second, &first_uuid);
    first.enter_room("lobby".to_string()).unwrap();
    assert!(
        wait_for_message_sent(&first),
        "first client did not enter lobby"
    );
    second.query_listening_in_room("lobby".to_string()).unwrap();
    let first_received_in_room_listening =
        received_in_room_listening_from(&first, &first_uuid, "lobby");
    first
        .say_in_room("lobby".to_string(), "Hello from room".to_string())
        .unwrap();
    let second_received_room_say =
        received_room_say_from(&second, &first_uuid, "lobby", "Hello from room");

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
    assert!(
        first_received_listening,
        "first client did not receive second client's listening reply"
    );
    assert!(
        second_received_listening,
        "second client did not receive first client's listening reply"
    );
    assert!(
        first_received_in_room_listening,
        "in-room client did not receive its listening reply"
    );
    assert!(
        second_received_room_say,
        "second client did not receive first client's room message"
    );
}
