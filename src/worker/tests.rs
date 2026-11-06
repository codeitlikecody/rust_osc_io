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
fn configured_client_uses_persisted_uuid() {
    let saved_uuid = "b0bae0a0-3bbe-4004-888b-f61ca125d0b0";
    let config = config::Config::default().set_client_uuid(saved_uuid.to_string());

    assert_eq!(configured_client(&config).client_uuid(), saved_uuid);
}

#[test]
fn configured_client_generates_uuid_when_missing() {
    let config = config::Config::default();
    let client = configured_client(&config);

    assert!(uuid::Uuid::parse_str(client.client_uuid()).is_ok());
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
fn global_say_is_delivered_only_to_clients_outside_rooms() {
    let say = Message::Me(Me::Say(Say::new("sender".to_string(), "hello".to_string())));

    assert!(should_deliver_message(&say, None));
    assert!(!should_deliver_message(&say, Some("lobby")));
}

#[test]
fn room_say_is_delivered_only_to_clients_in_that_room() {
    let say = Message::Me(Me::SayInRoom(RoomSay::new(
        "lobby".to_string(),
        Say::new("sender".to_string(), "hello".to_string()),
    )));

    assert!(should_deliver_message(&say, Some("lobby")));
    assert!(!should_deliver_message(&say, None));
    assert!(!should_deliver_message(&say, Some("other")));
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

#[test]
fn entering_and_exiting_updates_current_room_correctly() {
    let local_client = client("local");
    let mut current_room = None;

    update_current_room(&mut current_room, "lobby", true);
    assert_eq!(current_room.as_deref(), Some("lobby"));

    update_current_room(&mut current_room, "other", false);
    assert_eq!(current_room.as_deref(), Some("lobby"));
    assert!(
        response_to(
            &Message::Who(Who::ListeningInRoom("lobby".to_string())),
            &local_client,
            current_room.as_deref()
        )
        .is_some()
    );

    update_current_room(&mut current_room, "other", true);
    assert_eq!(current_room.as_deref(), Some("other"));
    assert!(
        response_to(
            &Message::Who(Who::ListeningInRoom("lobby".to_string())),
            &local_client,
            current_room.as_deref()
        )
        .is_none()
    );
    assert!(
        response_to(
            &Message::Who(Who::ListeningInRoom("other".to_string())),
            &local_client,
            current_room.as_deref()
        )
        .is_some()
    );

    update_current_room(&mut current_room, "lobby", false);
    assert_eq!(current_room.as_deref(), Some("other"));
    update_current_room(&mut current_room, "other", false);
    assert_eq!(current_room, None);
    assert!(
        response_to(
            &Message::Who(Who::Listening("lobby".to_string())),
            &local_client,
            current_room.as_deref()
        )
        .is_some()
    );
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

fn received_farewell_from(handle: &OscWorkerHandle, expected_client_uuid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return false;
        }

        match handle.event_receiver().recv_timeout(timeout) {
            Ok(OscWorkerEvent::MessageReceived(_, Message::Me(Me::Farewell(client))))
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

fn received_listening_from_both(
    handle: &OscWorkerHandle,
    first_client_uuid: &str,
    second_client_uuid: &str,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut received_first = false;
    let mut received_second = false;
    loop {
        if received_first && received_second {
            return true;
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return false;
        }

        match handle.event_receiver().recv_timeout(timeout) {
            Ok(OscWorkerEvent::MessageReceived(_, Message::Me(Me::Listening(client))))
                if client.client_uuid() == first_client_uuid =>
            {
                received_first = true;
            }
            Ok(OscWorkerEvent::MessageReceived(_, Message::Me(Me::Listening(client))))
                if client.client_uuid() == second_client_uuid =>
            {
                received_second = true;
            }
            Ok(OscWorkerEvent::Error(error)) => {
                eprintln!("wire test worker error: {error:?}");
            }
            Ok(_) => {}
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
    expected_message_uuid: &str,
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
                    && args[0] == OscType::String(expected_message_uuid.to_string())
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

fn received_global_say_from(
    handle: &OscWorkerHandle,
    expected_client_uuid: &str,
    expected_message_uuid: &str,
    expected_text: &str,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return false;
        }

        match handle.event_receiver().recv_timeout(timeout) {
            Ok(OscWorkerEvent::MessageReceived(_, Message::Me(Me::Say(say)))) => {
                let args = say.to_args();
                if args.len() == 3
                    && args[0] == OscType::String(expected_message_uuid.to_string())
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

fn did_not_receive_global_say(handle: &OscWorkerHandle, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }

        match handle.event_receiver().recv_timeout(remaining) {
            Ok(OscWorkerEvent::MessageReceived(_, Message::Me(Me::Say(_)))) => return false,
            Ok(OscWorkerEvent::Error(error)) => {
                eprintln!("wire test worker error: {error:?}");
            }
            Ok(_) => {}
            Err(_) => return true,
        }
    }
}

fn did_not_receive_room_say(handle: &OscWorkerHandle, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }

        match handle.event_receiver().recv_timeout(remaining) {
            Ok(OscWorkerEvent::MessageReceived(_, Message::Me(Me::SayInRoom(_)))) => {
                return false;
            }
            Ok(OscWorkerEvent::Error(error)) => {
                eprintln!("wire test worker error: {error:?}");
            }
            Ok(_) => {}
            Err(_) => return true,
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
    let first_received_listening = received_listening_from_both(&first, &first_uuid, &second_uuid);
    let second_received_listening =
        received_listening_from_both(&second, &first_uuid, &second_uuid);
    first.enter_room("lobby".to_string()).unwrap();
    assert!(
        wait_for_message_sent(&first),
        "first client did not enter lobby"
    );
    second
        .say_with_uuid(
            "outside-message-id".to_string(),
            "Hello outside".to_string(),
        )
        .unwrap();
    let second_sent_global_say = wait_for_message_sent(&second);
    let second_received_global_say =
        received_global_say_from(&second, &second_uuid, "outside-message-id", "Hello outside");
    let first_did_not_receive_global_say =
        did_not_receive_global_say(&first, Duration::from_millis(500));
    second.query_listening_in_room("lobby".to_string()).unwrap();
    let first_received_in_room_listening =
        received_in_room_listening_from(&first, &first_uuid, "lobby");
    first
        .say_in_room_with_uuid(
            "lobby".to_string(),
            "room-message-id".to_string(),
            "Hello from room".to_string(),
        )
        .unwrap();
    let first_sent_room_say = wait_for_message_sent(&first);
    let first_received_room_say = received_room_say_from(
        &first,
        &first_uuid,
        "room-message-id",
        "lobby",
        "Hello from room",
    );
    let second_did_not_receive_room_say =
        did_not_receive_room_say(&second, Duration::from_millis(500));
    first.exit_room("lobby".to_string()).unwrap();
    let first_exited_room = wait_for_message_sent(&first);
    second
        .query_listening_outside_room("lobby".to_string())
        .unwrap();
    let first_replied_after_exit = received_listening_from(&first, &first_uuid);

    first.stop_osc_worker().unwrap();
    let first_sent_farewell = received_farewell_from(&second, &first_uuid);
    first.join().unwrap();
    second.stop_osc_worker().unwrap();
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
        second_sent_global_say,
        "outside client did not send its global message"
    );
    assert!(
        second_received_global_say,
        "outside client did not receive its global message"
    );
    assert!(
        first_did_not_receive_global_say,
        "in-room client received an outside-room message"
    );
    assert!(
        first_received_room_say,
        "room member did not receive first client's room message"
    );
    assert!(
        second_did_not_receive_room_say,
        "client outside the room received a room-scoped message"
    );
    assert!(
        first_sent_room_say,
        "first client did not send its room message"
    );
    assert!(first_exited_room, "first client did not exit lobby");
    assert!(
        first_replied_after_exit,
        "first client did not reply to outside-room query after exiting"
    );
    assert!(
        first_sent_farewell,
        "first client did not broadcast its farewell"
    );
}
