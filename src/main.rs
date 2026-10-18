use rosc::OscType;
use rust_osc_io::{GROUP, OscWorkerHandle, PORT};
use std::io;
use std::io::prelude::*;
use std::net::{Ipv4Addr, SocketAddrV4};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Try to start the receiver but don't crash if it fails; continue without receiver.
    let multicast_group = Some(GROUP);
    let listener_ip = Some(Ipv4Addr::UNSPECIFIED);
    let listener_port = Some(PORT);
    let sender_ip = Some(Ipv4Addr::UNSPECIFIED);
    let sender_port = Some(0);
    let osc_worker_handle = match OscWorkerHandle::spawn(
        multicast_group,
        listener_ip,
        listener_port,
        sender_ip,
        sender_port,
        |sender, packet| {
            println!("WORKER: Received from {sender}: {packet:?}");
        },
    ) {
        Ok(handle) => {
            println!("Worker started");
            handle
        }
        Err(err) => {
            eprintln!("Failed to start worker. Exiting program...");
            return Err(Box::new(err));
        }
    };

    let stdin = io::stdin();

    loop {
        print!("Input text: ");
        if let Err(err) = io::stdout().flush() {
            eprintln!("Warning: failed to flush stdout: {}. Continuing.", err);
        }

        let mut input = String::new();
        match stdin.read_line(&mut input) {
            Ok(0) => {
                println!("EOF; exiting input loop.");
                break;
            }
            Ok(_) => { /* proceed */ }
            Err(err) => {
                eprintln!("Failed to read input: {}. Continuing.", err);
                continue;
            }
        }

        let trimmed = input.trim();
        println!("Sending: {}", trimmed);
        osc_worker_handle.send_message(
            "/messenger/me/say",
            vec![OscType::String(trimmed.to_string())],
        )?;

        if trimmed == "x" {
            println!("Exiting...");
            break;
        }
    }

    osc_worker_handle.stop_osc_worker()?;

    println!("Main loop ended; program exiting (worker thread may still be running).");
    Ok(())
}
