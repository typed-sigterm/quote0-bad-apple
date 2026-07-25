//! quote0-bad-apple firmware (ESP32-C3, MindReset Quote/0).
//!
//! Behaviour:
//! 1. Init the UC8251D e-paper and show a checkerboard (proves the firmware is alive).
//! 2. Connect WiFi (credentials from build-time env `WIFI_SSID` / `WIFI_PASSWORD`).
//! 3. DHCP, then wait for UDP broadcasts `Q0BA_SERVER <ip> <port>` on :18888.
//! 4. TCP-connect to the host and receive frames:
//!    header 12 bytes = magic "Q0BA" + seq u32 LE + len u32 LE, then `len` frame bytes.
//! 5. Display each frame (full refresh) and reply with one ack byte 'K'.
//!    On any failure, go back to discovery.

#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]
#![deny(clippy::large_stack_frames)]

extern crate alloc;

mod uc8251d;

use alloc::vec;
use core::net::Ipv4Addr;

use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::main;
use esp_hal::time::{Duration, Instant};
use esp_hal::timer::timg::TimerGroup;
use esp_println::println;
use esp_radio::wifi::{AuthMethod, ClientConfig, ModeConfig};
use smoltcp::iface::{Config as IfaceConfig, Interface, SocketSet, SocketStorage};
use smoltcp::socket::{dhcpv4, tcp, udp};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr};

esp_bootloader_esp_idf::esp_app_desc!();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("PANIC: {}", info);
    loop {}
}

const DISCOVER_PORT: u16 = 18888;
const FRAME_MAGIC: &[u8; 4] = b"Q0BA";
const FRAME_BYTES: usize = uc8251d::BUFFER_SIZE;
const HEADER_BYTES: usize = 12;
const RX_BUF_BYTES: usize = HEADER_BYTES + FRAME_BYTES;
const ACK: [u8; 1] = *b"K";

/// Build-time WiFi credentials: `WIFI_SSID` / `WIFI_PASSWORD` env vars.
const WIFI_SSID: &str = match option_env!("WIFI_SSID") {
    Some(s) => s,
    None => "",
};
const WIFI_PASSWORD: &str = match option_env!("WIFI_PASSWORD") {
    Some(s) => s,
    None => "",
};

fn smol_now() -> SmolInstant {
    SmolInstant::from_micros(Instant::now().duration_since_epoch().as_micros() as i64)
}

/// 8px checkerboard, drawn at boot so a bare device shows the firmware is alive.
fn checkerboard(frame: &mut [u8]) {
    const ROW_BYTES: usize = uc8251d::WIDTH / 8;
    for y in 0..uc8251d::HEIGHT {
        for xb in 0..ROW_BYTES {
            let mut byte = 0u8;
            for bit in 0..8 {
                let x = xb * 8 + bit;
                if ((x / 8) + (y / 8)) % 2 == 0 {
                    byte |= 1 << (7 - bit);
                }
            }
            frame[y * ROW_BYTES + xb] = byte;
        }
    }
}

struct ServerAddr {
    ip: Ipv4Addr,
    port: u16,
}

/// Parse a `Q0BA_SERVER <ipv4> <port>` announce payload.
fn parse_announce(data: &[u8]) -> Option<ServerAddr> {
    let text = core::str::from_utf8(data).ok()?.trim();
    let mut parts = text.split_whitespace();
    if parts.next()? != "Q0BA_SERVER" {
        return None;
    }
    let ip_str = parts.next()?;
    let port: u16 = parts.next()?.parse().ok()?;
    let mut octets = [0u8; 4];
    let mut count = 0usize;
    for (i, part) in ip_str.split('.').enumerate() {
        if i >= 4 {
            return None;
        }
        octets[i] = part.parse().ok()?;
        count += 1;
    }
    if count != 4 {
        return None;
    }
    Some(ServerAddr {
        ip: Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3]),
        port,
    })
}

#[allow(
    clippy::large_stack_frames,
    reason = "large buffers are heap-allocated; the frame is a boxed slice"
)]
#[main]
fn main() -> ! {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    // esp-radio's WiFi stack allocates most of its buffers from the global
    // heap at runtime, so a single 64 KB reclaimed region leaves too little
    // for the display/TCP buffers (prev-frame, rx_buf, tcp). Register the main
    // DRAM region as well; esp-alloc merges regions via add_region.
    esp_alloc::heap_allocator!(size: 32 * 1024);
    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    println!("quote0-bad-apple firmware boot");

    // --- display: checkerboard proves the firmware is running
    let mut display = match uc8251d::Uc8251d::new(
        peripherals.SPI2,
        peripherals.GPIO10,
        peripherals.GPIO7,
        peripherals.GPIO6,
        peripherals.GPIO5,
        peripherals.GPIO4,
        peripherals.GPIO3,
        peripherals.GPIO20,
    ) {
        Ok(d) => d,
        Err(e) => {
            println!("display init failed: {:?}", e);
            loop {
                Delay::new().delay_millis(1000);
            }
        }
    };
    // Clear to white first, then show a checkerboard: panel is ready.
    match display.clear(true) {
        Ok(()) => println!("display cleared"),
        Err(e) => println!("display clear failed: {:?}", e),
    }
    {
        let mut frame = vec![0u8; FRAME_BYTES].into_boxed_slice();
        checkerboard(&mut frame);
        match display.display(&frame) {
            Ok(()) => println!("display ready"),
            Err(e) => println!("display refresh failed: {:?}", e),
        }
    }

    if WIFI_SSID.is_empty() {
        println!("WIFI_SSID not set at build time - idling.");
        println!(
            "rebuild with: $env:WIFI_SSID='ssid'; $env:WIFI_PASSWORD='pass'; cargo run --release"
        );
        loop {
            Delay::new().delay_millis(1000);
        }
    }

    // --- wifi
    let radio_init = esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller");
    let (mut controller, interfaces) =
        esp_radio::wifi::new(&radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

    let mut client_config = ClientConfig::default()
        .with_ssid(WIFI_SSID.into())
        .with_password(WIFI_PASSWORD.into());
    if WIFI_PASSWORD.is_empty() {
        client_config = client_config.with_auth_method(AuthMethod::None);
    }
    controller
        .set_config(&ModeConfig::Client(client_config))
        .expect("wifi set_config");
    controller.start().expect("wifi start");
    println!("wifi connecting to SSID: {}", WIFI_SSID);
    controller.connect().expect("wifi connect");

    // --- smoltcp
    let mut device = interfaces.sta;
    let mac = device.mac_address();
    let iface_config = IfaceConfig::new(HardwareAddress::Ethernet(EthernetAddress(mac)));
    let mut iface = Interface::new(iface_config, &mut device, smol_now());

    let mut storage = [SocketStorage::EMPTY; 6];
    let mut sockets = SocketSet::new(&mut storage[..]);

    let dhcp_handle = sockets.add(dhcpv4::Socket::new());

    let udp_rx = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 4], vec![0; 256]);
    let udp_tx = udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 1], vec![0; 64]);
    let mut udp_socket = udp::Socket::new(udp_rx, udp_tx);
    udp_socket.bind(DISCOVER_PORT).expect("udp bind");
    let udp_handle = sockets.add(udp_socket);

    let tcp_socket = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; 8192]),
        tcp::SocketBuffer::new(vec![0; 1024]),
    );
    let tcp_handle = sockets.add(tcp_socket);

    let delay = Delay::new();
    let mut rx_buf = vec![0u8; RX_BUF_BYTES].into_boxed_slice();
    let mut rx_len = 0usize;
    let mut dhcp_configured = false;
    let mut tcp_active = false;
    let mut tcp_connecting_since: Option<Instant> = None;
    let mut last_rx = Instant::now();
    let mut wifi_retry = Instant::now();

    loop {
        let stamp = smol_now();
        iface.poll(stamp, &mut device, &mut sockets);

        // DHCP lease events
        match sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll() {
            None => {}
            Some(dhcpv4::Event::Configured(conf)) => {
                iface.update_ip_addrs(|addrs| {
                    addrs.clear();
                    let _ = addrs.push(IpCidr::Ipv4(conf.address));
                });
                if let Some(router) = conf.router {
                    let _ = iface.routes_mut().add_default_ipv4_route(router);
                }
                dhcp_configured = true;
                println!("dhcp: got ip {}", conf.address.address());
            }
            Some(dhcpv4::Event::Deconfigured) => {
                iface.update_ip_addrs(|addrs| addrs.clear());
                iface.routes_mut().remove_default_ipv4_route();
                dhcp_configured = false;
                println!("dhcp: lost ip");
            }
        }

        // WiFi link watchdog
        if wifi_retry.elapsed() > Duration::from_millis(5000) {
            match controller.is_connected() {
                Ok(true) => {}
                _ => {
                    println!("wifi: reconnecting");
                    let _ = controller.connect();
                }
            }
            wifi_retry = Instant::now();
        }

        if tcp_active {
            // --- receive frame bytes
            {
                let tcp = sockets.get_mut::<tcp::Socket>(tcp_handle);
                if tcp.may_recv() {
                    match tcp.recv_slice(&mut rx_buf[rx_len..]) {
                        Ok(n) => {
                            if n > 0 {
                                rx_len += n;
                                last_rx = Instant::now();
                            }
                        }
                        Err(_) => {
                            println!("tcp: peer closed, back to discovery");
                            tcp.abort();
                            tcp_active = false;
                            rx_len = 0;
                        }
                    }
                }
                if tcp_active && !tcp.is_open() {
                    println!("tcp: connection closed, back to discovery");
                    tcp.abort();
                    tcp_active = false;
                    rx_len = 0;
                }
                if tcp_active && last_rx.elapsed() > Duration::from_millis(30_000) {
                    println!("tcp: rx timeout, back to discovery");
                    tcp.abort();
                    tcp_active = false;
                    rx_len = 0;
                }
            }

            // --- try to assemble one full frame
            let mut frame_ready = false;
            if tcp_active && rx_len >= HEADER_BYTES {
                if &rx_buf[0..4] != FRAME_MAGIC {
                    rx_buf.copy_within(1..rx_len, 0);
                    rx_len -= 1;
                    println!("tcp: bad magic, resync");
                } else {
                    let len = u32::from_le_bytes(rx_buf[8..12].try_into().unwrap()) as usize;
                    if len != FRAME_BYTES {
                        rx_buf.copy_within(1..rx_len, 0);
                        rx_len -= 1;
                        println!("tcp: bad len {}, resync", len);
                    } else if rx_len >= RX_BUF_BYTES {
                        frame_ready = true;
                    }
                }
            }

            // --- display + ack
            if frame_ready {
                let seq = u32::from_le_bytes(rx_buf[4..8].try_into().unwrap());
                let refresh_start = Instant::now();
                match display.display(&rx_buf[HEADER_BYTES..RX_BUF_BYTES]) {
                    Ok(()) => println!(
                        "frame {} shown, refresh {} ms",
                        seq,
                        refresh_start.elapsed().as_millis()
                    ),
                    Err(e) => println!("display error: {:?}", e),
                }
                rx_len = 0;

                let tcp = sockets.get_mut::<tcp::Socket>(tcp_handle);
                if tcp.may_send() && tcp.send_slice(&ACK).is_ok() {
                    // ack sent, host will pace the next frame
                } else {
                    println!("tcp: ack send failed, back to discovery");
                    tcp.abort();
                    tcp_active = false;
                }
            }
        } else if tcp_connecting_since.is_some() {
            let since = tcp_connecting_since.unwrap();
            let tcp = sockets.get_mut::<tcp::Socket>(tcp_handle);
            if tcp.state() == tcp::State::Established {
                println!("tcp: connected to host");
                tcp_connecting_since = None;
                tcp_active = true;
                rx_len = 0;
                last_rx = Instant::now();
            } else if tcp.state() == tcp::State::Closed
                || since.elapsed() > Duration::from_millis(10_000)
            {
                println!("tcp: connect failed, back to discovery");
                tcp.abort();
                tcp_connecting_since = None;
            }
        } else if dhcp_configured {
            // --- discovery: wait for the host announce broadcast
            let mut discovered: Option<ServerAddr> = None;
            {
                let udp = sockets.get_mut::<udp::Socket>(udp_handle);
                let mut pkt = [0u8; 128];
                if let Ok((n, _meta)) = udp.recv_slice(&mut pkt) {
                    discovered = parse_announce(&pkt[..n]);
                }
            }
            if let Some(server) = discovered {
                println!("discovered host {}:{}", server.ip, server.port);
                let tcp = sockets.get_mut::<tcp::Socket>(tcp_handle);
                match tcp.connect(
                    iface.context(),
                    (IpAddress::Ipv4(server.ip), server.port),
                    49152,
                ) {
                    Ok(()) => tcp_connecting_since = Some(Instant::now()),
                    Err(e) => println!("tcp connect error: {:?}", e),
                }
            }
        }

        delay.delay_millis(2);
    }
}
