use super::*;

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
