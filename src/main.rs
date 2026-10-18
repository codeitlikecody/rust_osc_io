extern crate rosc;

use rosc::{OscMessage, OscPacket, OscType, decoder, encoder};
use std::io;
use std::io::prelude::*;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::str::FromStr;
use std::time::Duration;
use std::{env, f32, thread};

// Set default network configuration for the OSC Messenger standard client 1
const GROUP: Ipv4Addr = Ipv4Addr::new(239, 254, 0, 1);
const PORT: u16 = 3090;

fn main() {
    let receiver1 = UdpSocket::bind(("0.0.0.0", PORT)).unwrap();
    receiver1
        .join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)
        .unwrap();
    receiver1.set_multicast_loop_v4(true).unwrap();

    let receiver_thread = thread::spawn(move || receiver_thread(receiver1));

    let stdin = io::stdin();
    let client1 = UdpSocket::bind("0.0.0.0:0").unwrap();
    loop {
        print!("Input text: ");
        io::stdout().flush().unwrap();
        let mut input = String::new();
        if stdin.read_line(&mut input).unwrap() == 0 {
            break; // Handle EOF (e.g., Ctrl+D or Ctrl+Z)
        }

        let trimmed = input.trim();

        println!("Sending: {}", trimmed);
        send_osc(
            &client1,
            "/messenger/me/say",
            vec![OscType::String(trimmed.to_string())],
        );

        if trimmed == "x" {
            println!("Exiting...");
            break;
        }
    }
}
fn send_osc(socket: &UdpSocket, address: &str, args: Vec<OscType>) {
    let msg_buf = encoder::encode(&OscPacket::Message(OscMessage {
        addr: address.to_string(),
        args: args,
    }))
    .unwrap();

    let destination = SocketAddrV4::new(GROUP, PORT);
    match socket.send_to(&msg_buf, destination) {
        Ok(ok) => {
            println!("SENDER: Sent {} bytes OK.", ok);
        }
        Err(err) => {
            println!("SENDER: Error sending OSC: {}", err)
        }
    }
}

fn receiver_thread(receiver1: UdpSocket) {
    let mut buffer = [0u8; 65536];

    loop {
        match receiver1.recv_from(&mut buffer) {
            Ok((size, sender)) => match decoder::decode_udp(&buffer[..size]) {
                Ok(packet) => {
                    println!("RECEIVER: Received from {sender}: {packet:?}");
                }
                Err(error) => {
                    eprintln!("RECEIVER: OSC decode error from {sender}: {error}");
                }
            },
            Err(error) => {
                eprintln!("RECEIVER: Receive error: {error}");
                break;
            }
        }
    }
}
