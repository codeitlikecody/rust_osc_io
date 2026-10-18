use rosc::OscType;
use rust_osc_io::{GROUP, PORT, create_sender_socket, send_osc, start_receiver};
use std::io;
use std::io::prelude::*;
use std::net::SocketAddrV4;

fn main() {
    let _receiver_thread = start_receiver(GROUP, PORT, |sender, packet| {
        println!("RECEIVER: Received from {sender}: {packet:?}");
    })
    .unwrap();

    let stdin = io::stdin();
    let client1 = create_sender_socket().unwrap();
    loop {
        print!("Input text: ");
        io::stdout().flush().unwrap();
        let mut input = String::new();
        if stdin.read_line(&mut input).unwrap() == 0 {
            break; // Handle EOF (e.g., Ctrl+D or Ctrl+Z)
        }

        let trimmed = input.trim();

        println!("Sending: {}", trimmed);
        match send_osc(
            &client1,
            SocketAddrV4::new(GROUP, PORT),
            "/messenger/me/say",
            vec![OscType::String(trimmed.to_string())],
        ) {
            Ok(n) => println!("SENDER: Sent {n} bytes OK."),
            Err(err) => println!("SENDER: Error sending OSC: {err}"),
        }

        if trimmed == "x" {
            println!("Exiting...");
            break;
        }
    }
}
