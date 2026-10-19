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
