use rosc::OscType;
use rust_osc_io::{GROUP, OscWorkerEvent, OscWorkerHandle, PORT};
use std::io;
use std::io::prelude::*;
use std::net::Ipv4Addr;

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
        if trimmed == "x" {
            println!("Exiting...");
            break;
        } else {
            println!("Sending: {}", trimmed);
            osc_worker_handle.send_message(
                "/messenger/me/say",
                vec![OscType::String(trimmed.to_string())],
            )?;
        }

        // check for events from the receiver thread to make sure we have successfully sent the message and that the receiver thread is running. This
        // should not be blocking, since the receiver thread should send an event immediately after starting
        // If we don't receive an event within a reasonable time, we should fail.

        match osc_worker_handle
            .events()
            .recv_timeout(std::time::Duration::from_secs(5))
        {
            Ok(OscWorkerEvent::Started) => {
                println!("CLIENT: OSC worker thread has been started.");
            }
            Ok(OscWorkerEvent::Stopped) => {
                println!("CLIENT: OSC worker thread has stopped. Exiting...");
                return Ok(());
            }
            Ok(OscWorkerEvent::MessageReceived(sender, packet)) => {
                println!("CLIENT: Message received from {sender}: {packet:?}");
            }
            Ok(OscWorkerEvent::MessageSent(size)) => {
                println!("CLIENT: Message sent. {size} bytes sent successfully.");
            }

            Ok(OscWorkerEvent::Error(error)) => {
                eprintln!("CLIENT: OSC worker thread error: {error:?}");
            }
            Err(error) => {
                eprintln!("CLIENT: Failed to receive event from worker thread: {error}");
            }
        }
    }

    osc_worker_handle.stop_osc_worker()?;

    println!("Main loop ended; program exiting (worker thread may still be running).");
    Ok(())
}
