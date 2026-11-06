use rust_osc_io::gateway::{GatewayConfig, run_gateway};
use std::io;
use std::sync::mpsc;

fn main() -> io::Result<()> {
    let mut config = GatewayConfig::default();
    if let Ok(gateway_uuid) = std::env::var("OSC_GATEWAY_UUID") {
        config.gateway_uuid = gateway_uuid;
    }
    if let Ok(max_patrons) = std::env::var("OSC_GATEWAY_MAX_PATRONS") {
        config.max_patrons = max_patrons.parse().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid OSC_GATEWAY_MAX_PATRONS: {error}"),
            )
        })?;
    }

    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    ctrlc::set_handler(move || {
        let _ = shutdown_tx.send(());
    })
    .map_err(|error| io::Error::other(format!("Failed to install shutdown handler: {error}")))?;

    run_gateway(config, shutdown_rx)
}
