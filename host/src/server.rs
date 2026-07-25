//! LAN discovery (UDP broadcast) + single-client TCP frame streaming.
//!
//! Wire protocol (host -> device):
//!   header 12 bytes: magic "Q0BA" + seq u32 LE + len u32 LE
//!   then `len` frame bytes (152x296 1bpp, 5616 bytes)
//! Device -> host: one ack byte 'K' after the frame has been displayed.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

pub const FRAME_MAGIC: &[u8; 4] = b"Q0BA";
pub const ACK: u8 = b'K';
pub const HEADER_BYTES: usize = 12;

/// Primary LAN IPv4 address of this machine (no traffic is actually sent).
pub fn primary_ip() -> io::Result<String> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    // TEST-NET-1: connecting a UDP socket sends nothing, it only makes the
    // kernel pick the outbound interface (and thus the right local address).
    sock.connect("192.0.2.1:80")?;
    Ok(sock.local_addr()?.ip().to_string())
}

pub struct StreamServer {
    listener: TcpListener,
    announce_ip: String,
    port: u16,
    discover_port: u16,
}

impl StreamServer {
    pub fn bind(port: u16, discover_port: u16) -> io::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        let announce_ip = primary_ip().unwrap_or_else(|_| "127.0.0.1".into());
        Ok(Self {
            listener,
            announce_ip,
            port,
            discover_port,
        })
    }

    pub fn announce_addr(&self) -> String {
        format!("{}:{}", self.announce_ip, self.port)
    }

    /// Broadcast `Q0BA_SERVER <ip> <port>` every 2 seconds so devices can find us.
    pub fn spawn_announcer(&self) -> io::Result<()> {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.set_broadcast(true)?;
        let payload = format!("Q0BA_SERVER {} {}\n", self.announce_ip, self.port);
        let dest = ("255.255.255.255", self.discover_port);
        thread::Builder::new()
            .name("q0ba-announce".into())
            .spawn(move || {
                loop {
                    if let Err(e) = sock.send_to(payload.as_bytes(), dest) {
                        eprintln!("[announce] send failed: {e}");
                    }
                    thread::sleep(Duration::from_secs(2));
                }
            })?;
        Ok(())
    }

    /// Channel of incoming device connections (accept runs on its own thread).
    pub fn connections(&self) -> io::Result<Receiver<TcpStream>> {
        let (tx, rx) = mpsc::channel();
        let listener = self.listener.try_clone()?;
        thread::Builder::new()
            .name("q0ba-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(s) => {
                            if tx.send(s).is_err() {
                                break;
                            }
                        }
                        Err(e) => eprintln!("[accept] {e}"),
                    }
                }
            })?;
        Ok(rx)
    }
}

/// Send one frame and wait for the device ack ('K' within `ack_timeout`).
pub fn send_frame(
    stream: &mut TcpStream,
    seq: u32,
    frame: &[u8],
    ack_timeout: Duration,
) -> io::Result<()> {
    let mut packet = Vec::with_capacity(HEADER_BYTES + frame.len());
    packet.extend_from_slice(FRAME_MAGIC);
    packet.extend_from_slice(&seq.to_le_bytes());
    packet.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    packet.extend_from_slice(frame);
    stream.write_all(&packet)?;
    stream.flush()?;

    stream.set_read_timeout(Some(ack_timeout))?;
    let mut ack = [0u8; 1];
    stream.read_exact(&mut ack)?;
    if ack[0] != ACK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected ack byte {:#04x}", ack[0]),
        ));
    }
    Ok(())
}
