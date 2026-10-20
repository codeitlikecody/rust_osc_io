use rosc::{OscMessage, OscPacket, OscType};

const OSC_ADDRESS_ME_INTRODUCE: &str = "/messenger/me/introduce";
const OSC_ADDRESS_ME_WELCOME: &str = "/messenger/me/welcome";
const OSC_ADDRESS_ME_SAY: &str = "/messenger/me/say";
const OSC_ADDRESS_WHO: &str = "/messenger/who";

/// Protocol error type for the messenger application. This enum represents the different types of errors that can occur when parsing or constructing messages in the messenger protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessengerProtocolError {
    InvalidArgumentCount(String),
    ArgumentTypeMismatch(String),
    UnknownAddress(String),
    NotImplemented(String),
    BundleNotSupported,
}
impl std::fmt::Display for MessengerProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidArgumentCount(s) => write!(f, "Invalid argument count: {s}"),
            Self::ArgumentTypeMismatch(s) => write!(f, "Argument type mismatch: {s}"),
            Self::UnknownAddress(s) => write!(f, "Unknown address: {s}"),
            Self::NotImplemented(s) => write!(f, "Not implemented: {s}"),
            Self::BundleNotSupported => write!(f, "OSC bundles are not supported"),
        }
    }
}

impl std::error::Error for MessengerProtocolError {}

/// This enum represents a message that can be sent or received by the messenger application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Me(Me),
    Who(Who),
}

/// Implementations for MessengerMessage
/// This implementation provides methods to get the OSC address and arguments for each type of MessengerMessage, as well as a method to convert the message into an OscPacket.
impl Message {
    /// Returns the OSC address of the message.
    pub fn address(&self) -> &'static str {
        match self {
            Message::Me(me_message) => match me_message {
                Me::Introduce(_) => OSC_ADDRESS_ME_INTRODUCE,
                Me::Welcome(_) => OSC_ADDRESS_ME_WELCOME,
                Me::Say(_) => OSC_ADDRESS_ME_SAY,
            },
            Message::Who(_) => OSC_ADDRESS_WHO,
        }
    }

    /// Returns the arguments of the message as a vector of OscType.
    pub fn args(&self) -> Vec<OscType> {
        match self {
            Message::Me(me_message) => match me_message {
                Me::Introduce(user) => user.to_args(),
                Me::Welcome(user) => user.to_args(),
                Me::Say(message) => message.to_args(),
            },
            // TODO: Placeholder for Who message arguments.
            Message::Who(_) => vec![],
        }
    }
    /// Converts the message into an OscPacket.
    pub fn to_osc(&self) -> OscPacket {
        self.into()
    }
}

/// Implement the From trait to convert a &Message into an OscPacket.
impl From<&Message> for OscPacket {
    fn from(message: &Message) -> Self {
        let address = message.address();
        let args = message.args();
        OscPacket::Message(OscMessage {
            addr: address.to_string(),
            args,
        })
    }
}

/// Implement the From trait to convert a Message into an OscPacket.
impl From<Message> for OscPacket {
    fn from(message: Message) -> Self {
        Self::from(&message)
    }
}

/// Implement the From trait to convert a &OscPacket into an Message.
impl TryFrom<&OscPacket> for Message {
    type Error = MessengerProtocolError;
    fn try_from(osc_packet: &OscPacket) -> Result<Self, Self::Error> {
        match osc_packet {
            OscPacket::Message(osc_message) => {
                match osc_message.addr.as_str() {
                    OSC_ADDRESS_ME_INTRODUCE => match MessengerClient::from_args(&osc_message.args)
                    {
                        Ok(client) => Ok(Message::Me(Me::Introduce(client))),
                        Err(err) => Err(err),
                    },
                    OSC_ADDRESS_ME_WELCOME => match MessengerClient::from_args(&osc_message.args) {
                        Ok(client) => Ok(Message::Me(Me::Welcome(client))),
                        Err(err) => Err(err),
                    },
                    OSC_ADDRESS_ME_SAY => match Say::from_args(&osc_message.args) {
                        Ok(say) => Ok(Message::Me(Me::Say(say))),
                        Err(err) => Err(err),
                    },
                    // TODO: Implement Who message parsing. For now, just return a placeholder.
                    OSC_ADDRESS_WHO => Err(MessengerProtocolError::NotImplemented(
                        "Who message parsing not implemented".into(),
                    )),
                    _ => Err(MessengerProtocolError::UnknownAddress(format!(
                        "Unknown OSC address: {}",
                        osc_message.addr
                    ))),
                }
            }
            OscPacket::Bundle(_) => Err(MessengerProtocolError::BundleNotSupported),
        }
    }
}

/// Implement the From trait to convert a OscPacket into an Message.
impl TryFrom<OscPacket> for Message {
    type Error = MessengerProtocolError;
    fn try_from(osc_packet: OscPacket) -> Result<Self, Self::Error> {
        Self::try_from(&osc_packet)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Who {
    // TODO: Implement the Who message. For now, just a placeholder.
    Todo,
}

/// This enum represents a message sent by a client to the multicast group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Me {
    Introduce(MessengerClient),
    Welcome(MessengerClient),
    Say(Say),
}
impl Me {
    /// Returns part of the OSC address for the message type.
    pub fn address_part(&self) -> &'static str {
        match self {
            Me::Introduce(_) => "introduce",
            Me::Welcome(_) => "welcome",
            Me::Say(_) => "say",
        }
    }
}

/// This struct represents a messenger client, which is a user of the messenger application.
/// It contains the UUID of the client, the name of the client, and the name of the application that the client is using.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessengerClient {
    client_uuid: String,
    client_name: String,
    application_name: String,
}

impl MessengerClient {
    /// Create a new MessengerClient with a random UUID, given a client name and application name.
    /// # Arguments
    /// * `client_name` - The name of the client.
    /// * `application_name` - The name of the application that the client is using.
    /// # Returns
    /// A new MessengerClient with a random UUID, the given client name, and the given application name.
    pub fn new(client_name: String, application_name: String) -> Self {
        Self {
            client_uuid: uuid::Uuid::new_v4().to_string(),
            client_name,
            application_name,
        }
    }

    /// Create a new MessengerClient with a given UUID, client name, and application name.
    /// This is useful for creating a client that has already been introduced to the multicast group, and therefore has a known UUID.
    /// # Arguments
    /// * `client_uuid` - The UUID of the client.
    /// * `client_name` - The name of the client.
    /// * `application_name` - The name of the application that the client is using.
    /// # Returns
    /// A new MessengerClient with the given UUID, client name, and application name.
    pub fn new_with_uuid(
        client_uuid: String,
        client_name: String,
        application_name: String,
    ) -> Self {
        Self {
            client_uuid,
            client_name,
            application_name,
        }
    }

    /// Converts the MessengerClient into a vector of OscType, which can be used as arguments in an OSC message.
    /// # Returns
    /// A vector of OscType containing the client UUID, client name, and application name.
    pub fn to_args(&self) -> Vec<OscType> {
        vec![
            OscType::String(self.client_uuid.clone()),
            OscType::String(self.client_name.clone()),
            OscType::String(self.application_name.clone()),
        ]
    }

    /// Creates a MessengerClient from a vector of OscType arguments. Returns an error if the arguments are invalid.
    /// # Arguments
    /// * `args` - A slice of OscType arguments.
    /// # Returns
    /// Ok(MessengerClient) if the arguments are valid, or an Err(MessengerProtocolError) if the arguments are invalid.
    pub fn from_args(args: &[OscType]) -> Result<Self, MessengerProtocolError> {
        if args.len() != 3 {
            return Err(MessengerProtocolError::InvalidArgumentCount(format!(
                "Invalid number of arguments for MessengerClient: expected 3, got {}",
                args.len()
            )));
        }
        if let (
            OscType::String(client_uuid),
            OscType::String(client_name),
            OscType::String(application_name),
        ) = (&args[0], &args[1], &args[2])
        {
            Ok(MessengerClient {
                client_uuid: client_uuid.clone(),
                client_name: client_name.clone(),
                application_name: application_name.clone(),
            })
        } else {
            return Err(MessengerProtocolError::ArgumentTypeMismatch(
                "Invalid argument types for MessengerClient".into(),
            ));
        }
    }

    /// Returns the UUID of the client.
    /// # Returns
    /// A string slice containing the UUID of the client.
    pub fn client_uuid(&self) -> &str {
        &self.client_uuid
    }

    /// Returns the name of the client.
    /// # Returns
    /// A string slice containing the name of the client.
    pub fn client_name(&self) -> &str {
        &self.client_name
    }

    /// Returns the name of the application that the client is using.
    /// # Returns
    /// A string slice containing the name of the application that the client is using.
    pub fn application_name(&self) -> &str {
        &self.application_name
    }
}

/// This struct represents a "say" message, which is a message sent by a client to the multicast group.
/// It contains the UUID of the message, the UUID of the client that sent the message, and the text of the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Say {
    message_uuid: String,
    client_uuid: String,
    message: String,
}
impl Say {
    /// Creates a new Say message with a random UUID.
    /// # Arguments
    /// * `client_uuid` - The UUID of the client that is sending the message.
    /// * `message` - The text of the message.
    /// # Returns
    /// A new Say message with a random UUID, the given client UUID, and the given message text.
    pub fn new(client_uuid: String, message: String) -> Self {
        Self {
            message_uuid: uuid::Uuid::new_v4().to_string(),
            client_uuid,
            message,
        }
    }

    /// Converts the SayMessage into a vector of OscType, which can be used as arguments in an OSC message.
    /// # Returns
    /// A vector of OscType containing the message UUID, client UUID, and message text.
    pub fn to_args(&self) -> Vec<OscType> {
        vec![
            OscType::String(self.message_uuid.clone()),
            OscType::String(self.client_uuid.clone()),
            OscType::String(self.message.clone()),
        ]
    }

    /// Creates a SayMessage from a vector of OscType arguments. Returns an error if the arguments are invalid.
    /// # Arguments
    /// * `args` - A slice of OscType arguments.
    /// # Returns
    /// Ok(Say) if the arguments are valid, or an Err(MessengerProtocolError) if the arguments are invalid.
    pub fn from_args(args: &[OscType]) -> Result<Self, MessengerProtocolError> {
        if args.len() != 3 {
            return Err(MessengerProtocolError::InvalidArgumentCount(format!(
                "Invalid number of arguments for Say message: expected 3, got {}",
                args.len()
            )));
        }
        if let (
            OscType::String(message_uuid),
            OscType::String(client_uuid),
            OscType::String(message),
        ) = (&args[0], &args[1], &args[2])
        {
            Ok(Say {
                message_uuid: message_uuid.clone(),
                client_uuid: client_uuid.clone(),
                message: message.clone(),
            })
        } else {
            return Err(MessengerProtocolError::ArgumentTypeMismatch(
                "Invalid argument types for Say message".into(),
            ));
        }
    }

    /// Returns part of the OSC address for the message type.
    /// TODO: This method is currently unused, but it may be useful in the future if we want to construct OSC addresses dynamically based on the message type.
    pub fn address_part(&self) -> &'static str {
        "say"
    }
}

/// tests
#[cfg(test)]
mod tests {
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
        let packet = OscPacket::from(&message);
        assert_eq!(packet, self::packet(addr, string_args(args))); // encoding
        assert_eq!(Message::try_from(&packet).unwrap(), message); // decoding / round trip
        assert_eq!(
            Message::try_from(&packet).unwrap().args(),
            string_args(args)
        ); // argument extraction        
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
            let packet = message.to_osc();
            let encoded = rosc::encoder::encode(&packet).unwrap();
            let (_, decoded_packet) = rosc::decoder::decode_udp(&encoded).unwrap();
            let decoded_message = Message::try_from(decoded_packet).unwrap();
            prop_assert_eq!(decoded_message, message);
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
}
