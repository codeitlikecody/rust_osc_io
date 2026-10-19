use std::{
    net::{Ipv4Addr, SocketAddrV4},
    time::Duration,
};

// Default configuration for the OSC Messenger standard
pub const DEFAULT_MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(239, 254, 0, 1);
pub const DEFAULT_LISTENER_ADDRESS: Ipv4Addr = Ipv4Addr::UNSPECIFIED;
pub const DEFAULT_LISTENER_PORT: u16 = 3090;
pub const DEFAULT_SENDER_ADDRESS: Ipv4Addr = Ipv4Addr::UNSPECIFIED;
pub const DEFAULT_SENDER_PORT: u16 = 0;
pub const DEFAULT_APPLICATION_NAME: &str = "OSC Messenger Test Client v0.0.1";
pub const DEFAULT_CLIENT_NAME: &str = "OSC Messenger test client";

pub const DEFAULT_WORKER_START_TIMEOUT_MS: Duration = Duration::from_secs(1);

pub struct Config {
    pub multicast_group: Ipv4Addr,
    pub listener_address: SocketAddrV4,
    pub sender_address: SocketAddrV4,
    pub client_name: String,
    pub application_name: String,
}
impl Default for Config {
    /// Create a new Config instance with the default parameters.
    /// # Returns   
    /// A new Config instance with the default parameters.
    fn default() -> Self {
        Config {
            multicast_group: DEFAULT_MULTICAST_GROUP,
            listener_address: SocketAddrV4::new(DEFAULT_LISTENER_ADDRESS, DEFAULT_LISTENER_PORT),
            sender_address: SocketAddrV4::new(DEFAULT_SENDER_ADDRESS, DEFAULT_SENDER_PORT),
            client_name: DEFAULT_CLIENT_NAME.to_string(),
            application_name: DEFAULT_APPLICATION_NAME.to_string(),
        }
    }
}

impl Config {
    /// Create a new Config instance with the specified parameters. If any parameter is None, the default value will be used.
    /// #   Arguments
    /// * `multicast_group` - The multicast group address to join. If None, the default multicast group address will be used.
    /// * `listener_address` - The address and port to listen for incoming OSC messages. If None, the default listener address and port will be used.
    /// * `sender_address` - The address and port to send outgoing OSC messages. If None, the default sender address and port will be used.
    /// * `client_name` - The name of the client. If None, the default client name will be used.
    /// * `application_name` - The name of the application. If None, the default application name will be used.
    /// # Returns
    /// A new Config instance with the specified parameters.
    pub fn new(
        multicast_group: Option<Ipv4Addr>,
        listener_address: Option<SocketAddrV4>,
        sender_address: Option<SocketAddrV4>,
        client_name: Option<String>,
        application_name: Option<String>,
    ) -> Self {
        let default_config = Config::default();
        let multicast_group = multicast_group.unwrap_or(default_config.multicast_group);
        let listener_address = listener_address.unwrap_or(default_config.listener_address);
        let sender_address = sender_address.unwrap_or(default_config.sender_address);
        let client_name = client_name.unwrap_or(default_config.client_name);
        let application_name = application_name.unwrap_or(default_config.application_name);

        Config {
            multicast_group,
            listener_address,
            sender_address,
            client_name,
            application_name,
        }
    }

    /// Set the client name in the configuration.
    /// # Arguments
    /// * `new_name` - The new client name to set.
    /// # Returns   
    /// A new Config instance with the updated client name.
    pub fn set_name(&mut self, client_name: String) -> Self {
        Config {
            multicast_group: self.multicast_group,
            listener_address: self.listener_address,
            sender_address: self.sender_address,
            client_name,
            application_name: self.application_name.clone(),
        }
    }

    /// Set the application name in the configuration.
    /// # Arguments
    /// * `application_name` - The new application name to set.
    /// # Returns
    /// A new Config instance with the updated application name.    
    pub fn set_application_name(&mut self, application_name: String) -> Self {
        Config {
            multicast_group: self.multicast_group,
            listener_address: self.listener_address,
            sender_address: self.sender_address,
            client_name: self.client_name.clone(),
            application_name,
        }
    }
}
