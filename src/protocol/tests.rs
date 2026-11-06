use proptest::prelude::*;
use rosc::{OscBundle, OscTime};

use super::*;

/// Helper function to convert a slice of string slices into a vector of OscType::String.
/// # Arguments
/// * `v` - A slice of string slices.
/// # Returns
/// A vector of OscType::String.
fn string_args(v: &[&str]) -> Vec<OscType> {
    v.iter().map(|s| OscType::String(s.to_string())).collect()
}

/// Helper function to create an OscPacket from an address and a vector of OscType arguments.
/// # Arguments
/// * `addr` - The OSC address as a string slice.
/// * `args` - A vector of OscType arguments.
/// # Returns
/// An OscPacket containing the given address and arguments.
fn packet(addr: &str, args: Vec<OscType>) -> OscPacket {
    OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args,
    })
}

/// Assert that the wire format of a message matches the expected OSC address and arguments, and that the message can be round-tripped through OscPacket.
/// # Arguments
/// * `message` - The Message to test.
/// * `addr` - The expected OSC address as a string slice.
/// * `args` - A slice of string slices representing the expected arguments.
/// # Panics
/// Panics if the wire format does not match the expected OSC address and arguments, or if the message cannot be round-tripped through OscPacket.
fn assert_wire_format(message: Message, addr: &str, args: &[&str]) {
    match OscPacket::try_from(&message) {
        Ok(packet) => {
            assert_eq!(packet, self::packet(addr, string_args(args))); // encoding
            assert_eq!(Message::try_from(&packet).unwrap(), message); // decoding / round trip
            assert_eq!(
                Message::try_from(&packet).unwrap().args(),
                string_args(args)
            ); // argument extraction        
        }
        Err(err) => panic!("Failed to convert message to OscPacket: {}", err),
    };
}

/// Helper function to create a sample Say message for testing.
/// # Returns
/// A sample Say message for testing.
fn sample_say() -> Say {
    Say {
        message_uuid: "message_uuid".to_string(),
        client_uuid: "client_uuid".to_string(),
        message: "Hello, world!".to_string(),
    }
}

/// Test the wire format of the say message, including encoding and decoding.
#[test]
fn say_wire_format() {
    assert_wire_format(
        Message::Me(Me::Say(sample_say())),
        OSC_ADDRESS_ME_SAY,
        &["message_uuid", "client_uuid", "Hello, world!"],
    );
}

/// Tests Say::from_args and to_args methods for round-trip conversion.
#[test]
fn say_round_trip() {
    let say = sample_say();
    let args = say.to_args();
    let say2 = Say::from_args(&args).unwrap();
    assert_eq!(say, say2);
}

/// Tests Say::from_args for too few arguments, expecting an InvalidArgumentCount error.
#[test]
fn say_from_args_error_too_few() {
    let args = string_args(&["message_uuid", "client_uuid"]);
    let err = Say::from_args(&args).unwrap_err();
    assert!(matches!(
        err,
        MessengerProtocolError::InvalidArgumentCount(_)
    ));
}

/// Tests Say::from_args for too many arguments, expecting an InvalidArgumentCount error.
#[test]
fn say_from_args_error_too_many() {
    let args = string_args(&["message_uuid", "client_uuid", "Hello, world!", "extra_arg"]);
    let err = Say::from_args(&args).unwrap_err();
    assert!(matches!(
        err,
        MessengerProtocolError::InvalidArgumentCount(_)
    ));
}

/// Tests Say::from_args for non-string argument types, expecting an ArgumentTypeMismatch error.
#[test]
fn say_from_args_error_non_string() {
    let args = vec![
        OscType::Int(42),
        OscType::Float(3.14),
        OscType::String("Hello, world!".to_string()),
    ];
    let err = Say::from_args(&args).unwrap_err();
    assert!(matches!(
        err,
        MessengerProtocolError::ArgumentTypeMismatch(_)
    ));
}

/// Tests Say::from_args for invalid argument types, expecting an ArgumentTypeMismatch error.
#[test]
fn say_from_args_error_type() {
    let args = vec![
        OscType::String("message_uuid".to_string()),
        OscType::Int(42),
        OscType::String("Hello, world!".to_string()),
    ];
    let err = Say::from_args(&args).unwrap_err();
    assert!(matches!(
        err,
        MessengerProtocolError::ArgumentTypeMismatch(_)
    ));
}

/// Helper function to create a sample MessengerClient for testing.
/// # Returns
/// A sample MessengerClient for testing.
fn sample_client() -> MessengerClient {
    MessengerClient {
        client_uuid: "client_uuid".to_string(),
        client_name: "Alice".to_string(),
        application_name: "Protocol Test Client".to_string(),
    }
}

/// tests encoding of null bytes in strings, which should be rejected by the protocol.
//TODO: Fix this test. A zero byte seems to cause issues for rust as well as osc. I need fto handle it better.
#[test]
fn null_bytes_in_strings() {
    let args = string_args(&["message_uuid", "client_uuid", "Hello, \0world!"]);
    let say = Say::from_args(&args).unwrap();
    let err = Message::Me(Me::Say(say)).to_osc().unwrap_err();
    assert!(matches!(err, MessengerProtocolError::NullByteInString(_)));
}

/// Test the wire format of the introduce message, including encoding and decoding.
#[test]
fn introduce_wire_format() {
    assert_wire_format(
        Message::Me(Me::Introduce(sample_client())),
        OSC_ADDRESS_ME_INTRODUCE,
        &["client_uuid", "Alice", "Protocol Test Client"],
    );
}

/// Test the wire format of the welcome message, including encoding and decoding.
#[test]
fn welcome_wire_format() {
    assert_wire_format(
        Message::Me(Me::Welcome(sample_client())),
        OSC_ADDRESS_ME_WELCOME,
        &["client_uuid", "Alice", "Protocol Test Client"],
    );
}

/// Test the room-enter address and client payload encoding/decoding.
#[test]
fn room_enter_wire_format() {
    assert_wire_format(
        Message::Me(Me::EnterRoom(RoomEnter::new(
            "lobby one".to_string(),
            sample_client(),
        ))),
        "/messenger/me/room/lobby one/enter",
        &["client_uuid", "Alice", "Protocol Test Client"],
    );
}

#[test]
fn room_exit_wire_format() {
    assert_wire_format(
        Message::Me(Me::ExitRoom(RoomExit::new(
            "lobby one".to_string(),
            sample_client(),
        ))),
        "/messenger/me/room/lobby one/exit",
        &["client_uuid", "Alice", "Protocol Test Client"],
    );
}

#[test]
fn room_enter_rejects_empty_or_slash_containing_names() {
    for room_name in ["", "parent/child"] {
        let message = Message::Me(Me::EnterRoom(RoomEnter::new(
            room_name.to_string(),
            sample_client(),
        )));
        assert!(matches!(
            message.to_osc(),
            Err(MessengerProtocolError::InvalidRoomName(_))
        ));
    }
}

#[test]
fn room_exit_rejects_empty_or_slash_containing_names() {
    for room_name in ["", "parent/child"] {
        let message = Message::Me(Me::ExitRoom(RoomExit::new(
            room_name.to_string(),
            sample_client(),
        )));
        assert!(matches!(
            message.to_osc(),
            Err(MessengerProtocolError::InvalidRoomName(_))
        ));
    }
}

/// Tests MessengerClient::from_args and to_args methods for round-trip conversion.
#[test]
fn messenger_client_round_trip() {
    let client = sample_client();
    let args = client.to_args();
    let client2 = MessengerClient::from_args(&args).unwrap();
    assert_eq!(client, client2);
}

/// Tests MessengerClient::from_args for too few arguments, expecting an InvalidArgumentCount error.
#[test]
fn messenger_client_from_args_error_too_few() {
    let args = string_args(&["client_uuid", "Alice"]);
    let err = MessengerClient::from_args(&args).unwrap_err();
    assert!(matches!(
        err,
        MessengerProtocolError::InvalidArgumentCount(_)
    ));
}

/// Tests MessengerClient::from_args for too many arguments, expecting an InvalidArgumentCount error.
#[test]
fn messenger_client_from_args_error_too_many() {
    let args = string_args(&["client_uuid", "Alice", "Protocol Test Client", "extra_arg"]);
    let err = MessengerClient::from_args(&args).unwrap_err();
    assert!(matches!(
        err,
        MessengerProtocolError::InvalidArgumentCount(_)
    ));
}

/// Tests MessengerClient::from_args for invalid argument types, expecting an ArgumentTypeMismatch error.
#[test]
fn messenger_client_from_args_error_type() {
    let args = vec![
        OscType::String("client_uuid".to_string()),
        OscType::Int(42),
        OscType::String("Protocol Test Client".to_string()),
    ];
    let err = MessengerClient::from_args(&args).unwrap_err();
    assert!(matches!(
        err,
        MessengerProtocolError::ArgumentTypeMismatch(_)
    ));
}

/// Tests Message::try_from with an unknown address.
#[test]
fn message_try_from_unknown_address() {
    let packet = packet("/unknown/address", vec![]);
    let err = Message::try_from(&packet).unwrap_err();
    assert!(matches!(err, MessengerProtocolError::UnknownAddress(_)));
}

/// Tests Message::try_from with /messenger/who, which should give NotImplemented.
#[test]
fn message_try_from_who() {
    let packet = packet("/messenger/who", vec![]);
    let err = Message::try_from(&packet).unwrap_err();
    assert!(matches!(err, MessengerProtocolError::NotImplemented(_)));
}

/// Tests Message::try_from with a bundle, which should give BundleNotSupported.
#[test]
fn message_try_from_bundle() {
    let bundle = OscPacket::Bundle(OscBundle {
        timetag: OscTime {
            seconds: 1,
            fractional: 1,
        },
        content: vec![],
    });
    let err = Message::try_from(&bundle).unwrap_err();
    assert!(matches!(err, MessengerProtocolError::BundleNotSupported));
}

/// Test try_from on /messenger/me/introduce with bad args, to confirm the error is propagated.
#[test]
fn message_try_from_introduce_bad_args() {
    let packet = packet(
        "/messenger/me/introduce",
        string_args(&["client_uuid", "Alice"]), // missing application name
    );
    let err = Message::try_from(&packet).unwrap_err();
    assert!(matches!(
        err,
        MessengerProtocolError::InvalidArgumentCount(_)
    ));
}

#[test]
fn message_try_from_room_enter_bad_args() {
    let packet = packet(
        "/messenger/me/room/lobby/enter",
        string_args(&["client_uuid", "Alice"]),
    );
    let err = Message::try_from(&packet).unwrap_err();
    assert!(matches!(
        err,
        MessengerProtocolError::InvalidArgumentCount(_)
    ));
}

/// Test Say::new for unique message UUIDs
#[test]
fn say_new_unique_message_uuids() {
    let say1 = Say::new("client_uuid".into(), "Hello, World!".into());
    let say2 = Say::new("client_uuid".into(), "Hello, World!".into());
    assert_ne!(say1.message_uuid, say2.message_uuid);
}

/// Test Say::new for storing inputs.
#[test]
fn say_new_storing_inputs() {
    let say = Say::new("client_uuid".into(), "Hello, World!".into());
    assert_eq!(say.client_uuid, "client_uuid");
    assert_eq!(say.message, "Hello, World!");
}

/// Test MessengerClient::new for unique UUIDs
#[test]
fn messenger_client_new_unique_uuids() {
    let client1 = MessengerClient::new("Alice".into(), "MyApp".into());
    let client2 = MessengerClient::new("Bob".into(), "MyApp".into());
    assert_ne!(client1.client_uuid, client2.client_uuid);
}

/// Test MessengerClient::new for storing inputs
#[test]
fn messenger_client_new_storing_inputs() {
    let client = MessengerClient::new("Alice".into(), "MyApp".into());
    assert_eq!(client.client_name, "Alice");
    assert_eq!(client.application_name, "MyApp");
}

/// Test MessengerClient accessors (client_uuid(), client_name(), application_name()) for returning the correct values.
#[test]
fn messenger_client_accessors() {
    let client = sample_client();
    assert_eq!(client.client_uuid(), "client_uuid");
    assert_eq!(client.client_name(), "Alice");
    assert_eq!(client.application_name(), "Protocol Test Client");
}

// TODO: Add tests for other message types and error cases as needed.

// Property-testing

/// Generates an arbitrary MessengerClient for property testing.
fn arb_client() -> impl Strategy<Value = MessengerClient> {
    (any::<String>(), any::<String>(), any::<String>()).prop_map(|(u, n, a)| MessengerClient {
        client_uuid: u,
        client_name: n,
        application_name: a,
    })
}

/// Generates an arbitrary Say for property testing.
fn arb_say() -> impl Strategy<Value = Say> {
    (any::<String>(), any::<String>(), any::<String>()).prop_map(|(m, c, t)| Say {
        message_uuid: m,
        client_uuid: c,
        message: t,
    })
}

/// Generates an arbitrary Message for property testing.
fn arb_message() -> impl Strategy<Value = Message> {
    prop_oneof![
        arb_client().prop_map(|c| Message::Me(Me::Introduce(c))),
        arb_client().prop_map(|c| Message::Me(Me::Welcome(c))),
        arb_say().prop_map(|s| Message::Me(Me::Say(s))),
    ]
}

proptest! {
    /// Tests that a Message can be converted to an OscPacket, encoded to osc and back without loss of information.
    #[test]
    fn message_round_trip(message in arb_message()) {
        if let Ok(packet) = message.to_osc(){
        let encoded = rosc::encoder::encode(&packet).unwrap();
        let (_, decoded_packet) = rosc::decoder::decode_udp(&encoded).unwrap();
        let decoded_message = Message::try_from(decoded_packet).unwrap();
        prop_assert_eq!(decoded_message, message);
        } else {
            panic!("Failed to convert message to OscPacket: {:?}", message);
        }
    }

    /// Tests that MessengerClient::from_args handles arbitrary arg counts correctly, never panics and that wrong counts are an error.
    #[test]
    fn client_from_args_wrong_count_errors(n in 0usize..10) {
        prop_assume!(n != 3);
        let args = vec![OscType::String("x".into()); n];
        prop_assert!(matches!(
            MessengerClient::from_args(&args),
            Err(MessengerProtocolError::InvalidArgumentCount(_))
        ));
    }

    /// Tests that MessengerClient::from_args(&client.to_args()) handles arbitrary args correctly.
    #[test]
    fn client_from_args_round_trip(client in arb_client()) {
        let args = client.to_args();
        let reconstructed = MessengerClient::from_args(&args).unwrap();
        prop_assert_eq!(reconstructed, client);
    }

    /// Tests that Say::from_args handles arbitrary arg counts correctly, never panics and that wrong counts are an error.
    #[test]
    fn say_from_args_wrong_count_errors(n in 0usize..10) {
        prop_assume!(n != 3);
        let args = vec![OscType::String("x".into()); n];
        prop_assert!(matches!(
            Say::from_args(&args),
            Err(MessengerProtocolError::InvalidArgumentCount(_))
        ));
    }

    /// Tests that Say::from_args(&say.to_args()) handles arbitrary args correctly.
    #[test]
    fn say_from_args_round_trip(say in arb_say()) {
        let args = say.to_args();
        let reconstructed = Say::from_args(&args).unwrap();
        prop_assert_eq!(reconstructed, say);
    }

}
