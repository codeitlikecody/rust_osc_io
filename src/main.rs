use rosc::OscType;
use rust_osc_io::{GROUP, OscWorkerEvent, OscWorkerHandle, PORT};
use std::error::Error;
use std::io;
use std::io::prelude::*;
use std::net::{Ipv4Addr, SocketAddrV4};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Try to start the receiver but don't crash if it fails; continue without receiver.
    let multicast_group = Some(GROUP);
    let listener_address = Some(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT));
    let sender_address = Some(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
    let name = Some("Test client 1".into());
    let osc_worker_handle =
        match OscWorkerHandle::spawn(multicast_group, listener_address, sender_address, name) {
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
        // Check for messages from the worker thread. Make sure to check for all messages, since the worker thread may have sent multiple messages since the last time we checked. If we don't check for messages, the worker thread may block when trying to send a message to the main thread, which will cause the worker thread to stop responding.
        while let Ok(message) = check_for_messages(&osc_worker_handle) {
            match message {
                OscWorkerEvent::Stopped => {
                    println!("Worker thread has stopped. Exiting input loop.");
                    break;
                }
                _ => {}
            }
        }

        // Prompt the user for input and send it to the worker thread. If the user enters "x", exit the loop and stop the worker thread.
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
            osc_worker_handle.say(trimmed.to_string())?;
        }
    }

    match osc_worker_handle.stop_osc_worker() {
        Ok(_) => {
            println!("Worker thread stopped successfully. Program exiting...");
            Ok(())
        }
        Err(error) => {
            eprintln!("Failed to stop worker: {error}");
            println!("Program exiting...");
            Err(Box::new(io::Error::new(
                io::ErrorKind::Other,
                "Failed to stop worker thread",
            )))
        }
    }
}

fn check_for_messages(
    osc_worker_handle: &OscWorkerHandle,
) -> Result<OscWorkerEvent, Box<dyn Error>> {
    // check for events from the receiver thread to make sure we have successfully sent the message and that the receiver thread is running. This
    // should not be blocking, since the receiver thread should send an event immediately after starting
    // If we don't receive an event within a reasonable time, we should fail.

    match osc_worker_handle
        .event_receiver()
        .recv_timeout(std::time::Duration::from_millis(200))
    {
        Ok(OscWorkerEvent::Started) => {
            println!("MESSAGE RECEIVED: OSC worker thread has been started.");
            return Ok(OscWorkerEvent::Started);
        }
        Ok(OscWorkerEvent::Stopped) => {
            println!("MESSAGE RECEIVED: OSC worker thread has stopped. Exiting...");
            return Ok(OscWorkerEvent::Stopped);
        }
        Ok(OscWorkerEvent::MessageReceived(sender, packet)) => {
            println!("MESSAGE RECEIVED: Message received from {sender}: {packet:?}");
            return Ok(OscWorkerEvent::MessageReceived(sender, packet));
        }
        Ok(OscWorkerEvent::MessageSent(size)) => {
            println!("MESSAGE RECEIVED: Message sent. {size} bytes sent successfully.");
            return Ok(OscWorkerEvent::MessageSent(size));
        }
        Ok(OscWorkerEvent::Error(error)) => {
            eprintln!("MESSAGE RECEIVED: OSC worker thread error: {error:?}");
            return Ok(OscWorkerEvent::Error(error));
        }
        Err(error) => {
            eprintln!("No messages in queue: {error}");
            return Err(Box::new(error));
        }
    }
}
