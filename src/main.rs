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

    // socket
    //     .join_multicast_v4(&multicast_group, &interface)
    //     .unwrap();

    // Optional: allow this machine to receive its own multicast packets.
    // socket.set_multicast_loop_v4(true).unwrap();

    // // Restrict multicast propagation to the local network.
    // socket.set_multicast_ttl_v4(1).unwrap();

    // let destination = SocketAddrV4::new(Ipv4Addr::new(239, 254, 0, 1), 3090);

    // let msg_buf = encoder::encode(&OscPacket::Message(OscMessage {
    //     addr: "/messenger/me/say".to_string(),
    //     args: vec![OscType::String("multicast test".to_string())],
    // }))
    // .unwrap();

    // println!("Listening for OSC on 239.254.0.1:3090 via {}", interface);

    // println!("sending: multicast test");

    // match socket.send_to(&msg_buf, to_addr) {
    //     Ok(ok) => {
    //         println!("Sent {} bytes OK.", ok);
    //     }
    //     Err(err) => {
    //         println!("Error sending OSC: {}", err)
    //     }
    // }

    // let host_addr = get_addr_from_arg(&args[1]);
    // let to_addr = get_addr_from_arg(&args[2]);

    // let host_addr = "0.0.0.0:3091";
    // let to_addr = "127.0.0.1:3090";

    // // println!("Host Address:{}", host_addr);
    // // println!("To Address:{}", to_addr);

    // let sock = UdpSocket::bind(host_addr).unwrap();
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

    // switch view
    // let msg_buf = encoder::encode(&OscPacket::Message(OscMessage {
    //     addr: "/3".to_string(),
    //     args: vec![],
    // }))
    // .unwrap();

    // sock.send_to(&msg_buf, to_addr).unwrap();

    // send random values to xy fields
    // let steps = 128;
    // let step_size: f32 = 2.0 * f32::consts::PI / steps as f32;
    // for i in 0.. {
    //     let x = 0.5 + (step_size * (i % steps) as f32).sin() / 2.0;
    //     let y = 0.5 + (step_size * (i % steps) as f32).cos() / 2.0;
    //     let mut msg_buf = encoder::encode(&OscPacket::Message(OscMessage {
    //         addr: "/3/xy1".to_string(),
    //         args: vec![OscType::Float(x), OscType::Float(y)],
    //     }))
    //     .unwrap();

    //     sock.send_to(&msg_buf, to_addr).unwrap();
    //     msg_buf = encoder::encode(&OscPacket::Message(OscMessage {
    //         addr: "/3/xy2".to_string(),
    //         args: vec![OscType::Float(y), OscType::Float(x)],
    //     }))
    //     .unwrap();
    //     sock.send_to(&msg_buf, to_addr).unwrap();
    //     thread::sleep(Duration::from_millis(200));
    // }
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
