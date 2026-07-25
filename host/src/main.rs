//! quote0-bad-apple host tool.
//!
//! Extracts one frame per `--interval` from a local video with ffmpeg,
//! Floyd-Steinberg dithers it to the Quote/0's 152x296 1bpp format and streams
//! frames over WiFi to a device running the companion firmware.
//!
//! Discovery: this host broadcasts `Q0BA_SERVER <ip> <port>` (UDP :18888) every
//! 2s; the device connects back on TCP and receives frames; each frame is acked
//! with 'K' after the display refresh, which throttles the pipeline.
//!
//! Diagnostics use `tracing` (set `RUST_LOG` to tune, default `info`); the
//! streaming progress bar / ETA is rendered by `indicatif` via the
//! `tracing-indicatif` bridge, so log lines print cleanly above the bar.

mod dither;
mod server;
mod video;

use std::io::{self, Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use clap::{Parser, crate_description, crate_name, crate_version};
use indicatif::ProgressStyle;
use tracing::{error, info, info_span, warn};
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use video::Rotate;

const DEFAULT_PORT: u16 = 8899;
const DEFAULT_DISCOVER_PORT: u16 = 18888;
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Parser)]
#[command(version = crate_version!(), about = crate_description!())]
struct Cli {
    /// Video file to stream (any format ffmpeg can read).
    #[arg(required_unless_present = "mock_device", value_name = "VIDEO")]
    video: Option<PathBuf>,

    /// Seconds each frame stays on the display.
    #[arg(long, default_value_t = 1.0, value_name = "SEC")]
    interval: f64,

    /// Video sampling rate [default: 1/interval, i.e. near real-time].
    #[arg(long, value_name = "FPS")]
    fps: Option<f64>,

    /// TCP port the device connects back to.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,

    /// UDP discovery broadcast port.
    #[arg(long, default_value_t = DEFAULT_DISCOVER_PORT)]
    discover_port: u16,

    /// Rotation for the landscape video on the portrait panel.
    #[arg(long, value_enum, default_value_t = Rotate::Ccw)]
    rotate: Rotate,

    /// Invert black/white.
    #[arg(long)]
    invert: bool,

    /// Loop the video forever.
    #[arg(long = "loop")]
    loop_video: bool,

    /// Run a fake device (discovery + TCP + ack) for loopback tests.
    #[arg(long)]
    mock_device: bool,

    /// Mock-device simulated panel refresh time, in seconds.
    #[arg(long, default_value_t = 1.6, value_name = "SEC")]
    refresh: f64,
}

fn main() {
    let cli = Cli::parse();
    init_tracing();

    let result = if cli.mock_device {
        run_mock_device(&cli)
    } else {
        // `required_unless_present` guarantees the video is present here.
        let video = cli
            .video
            .clone()
            .expect("video required unless --mock-device");
        // Friendly, multi-line guidance is clearer as plain stderr than as logs.
        if let Err(msg) = preflight(&video) {
            eprintln!("{msg}");
            process::exit(1);
        }
        run_host(&cli, &video)
    };

    if let Err(e) = result {
        error!("{e}");
        process::exit(1);
    }
}

/// Route diagnostics through `tracing`; `indicatif` progress bars render above
/// log lines via the `tracing-indicatif` layer. Honors `RUST_LOG` (default info).
fn init_tracing() {
    let indicatif_layer = IndicatifLayer::new();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(indicatif_layer.get_stderr_writer())
                .with_target(false)
                .without_time(),
        )
        .with(indicatif_layer)
        .init();
}

/// Fail fast with friendly guidance before opening sockets or spawning ffmpeg.
fn preflight(video: &Path) -> Result<(), String> {
    if !video.is_file() {
        return Err(format!(
            "error: video file not found:\n  {}\n\
             tip: tab-complete the name or drag the file into the terminal - subtle\n     \
             differences in spaces or full-width characters are easy to mistype.",
            video.display()
        ));
    }
    match Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err("error: `ffmpeg -version` failed - your ffmpeg install looks broken.".into()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Err("error: ffmpeg was not found on PATH.\n\
             Install it, then open a NEW terminal so PATH refreshes:\n  \
               winget install Gyan.FFmpeg\n  \
               (or: scoop install ffmpeg  /  https://ffmpeg.org/download.html)\n\
             Verify with:  ffmpeg -version"
                .into())
        }
        Err(e) => Err(format!("error: could not launch ffmpeg: {e}")),
    }
}

fn run_host(cli: &Cli, video: &Path) -> io::Result<()> {
    let interval = Duration::from_secs_f64(cli.interval);
    let fps = cli.fps.unwrap_or(1.0 / cli.interval);
    let total_frames = if cli.loop_video {
        None
    } else {
        probe_duration(video).map(|secs| ((secs * fps).round() as u64).max(1))
    };

    info!("{} v{}", crate_name!(), crate_version!());
    info!("video    : {}", video.display());
    info!(
        "interval : {:.2}s/frame (sampling {fps:.3} fps){}",
        interval.as_secs_f64(),
        if cli.loop_video { ", looping" } else { "" }
    );
    info!("transform: rotate={:?}  invert={}", cli.rotate, cli.invert);
    if let Some(total) = total_frames {
        info!("frames   : ~{total} (progress + ETA enabled)");
    }

    let server = server::StreamServer::bind(cli.port, cli.discover_port)?;
    info!(
        "host     : {}  (tcp :{}, discover udp :{})",
        server.announce_addr(),
        cli.port,
        cli.discover_port
    );
    server.spawn_announcer()?;
    let connections = server.connections()?;

    let mut source = video::FrameSource::spawn(video, fps, cli.rotate, cli.loop_video)?;
    let mut stream: Option<TcpStream> = None;
    let mut seq: u32 = 0;
    let mut hinted = false;
    let mut last_send = Instant::now()
        .checked_sub(interval)
        .unwrap_or_else(Instant::now);

    // The bar belongs to this span; `indicatif` derives speed + ETA from how
    // fast we `pb_inc`, and `tracing-indicatif` prints logs above the bar.
    let span = info_span!("stream");
    let _guard = span.enter();
    span.pb_set_style(&progress_style(total_frames.is_some()));
    match total_frames {
        Some(total) => span.pb_set_length(total),
        None if cli.loop_video => span.pb_set_message("looping"),
        None => span.pb_set_message("unknown length"),
    }

    loop {
        // Establish or refresh the device connection.
        if stream.is_none() {
            info!("waiting for device to connect...");
            if !hinted {
                info!(
                    "checklist: device & this PC on the SAME WiFi, firmware flashed with your \
                     WiFi credentials, and this app allowed through the firewall"
                );
                hinted = true;
            }
            match connections.recv() {
                Ok(s) => {
                    info!("device connected: {:?}", s.peer_addr().ok());
                    stream = Some(s);
                }
                Err(_) => return Err(io::Error::other("accept thread terminated")),
            }
        } else {
            // A newly arrived connection replaces the stale one.
            match connections.try_recv() {
                Ok(s) => {
                    info!("device reconnected: {:?}", s.peer_addr().ok());
                    stream = Some(s);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {}
            }
        }

        let gray = match source.next_frame()? {
            Some(f) => f,
            None => {
                info!("video finished ({seq} frames shown)");
                return Ok(());
            }
        };
        let packed = dither::dither_pack(&gray, cli.invert);

        // Pace: at most one frame per interval (ack below throttles further).
        let elapsed = last_send.elapsed();
        if elapsed < interval {
            std::thread::sleep(interval - elapsed);
        }

        let s = stream.as_mut().expect("stream checked above");
        match server::send_frame(s, seq, &packed, ACK_TIMEOUT) {
            Ok(()) => {
                seq += 1;
                span.pb_inc(1);
                last_send = Instant::now();
            }
            Err(e) => {
                warn!("send/ack failed: {e}");
                stream = None;
            }
        }
    }
}

/// Progress-bar style: a determinate bar + ETA, or a spinner when length is
/// unknown (looping / ffprobe unavailable).
fn progress_style(determinate: bool) -> ProgressStyle {
    let template = if determinate {
        "[{bar:24}] {pos:>4}/{len} {percent:>3}%   ETA {eta}"
    } else {
        "{spinner} frame {pos}  {per_sec}  elapsed {elapsed}  {msg}"
    };
    ProgressStyle::with_template(template)
        .expect("valid progress template")
        .progress_chars("=> ")
}

/// Probe total video duration in seconds via ffprobe (None if unavailable).
fn probe_duration(video: &Path) -> Option<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nokey=1",
        ])
        .arg(video)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|d| *d > 0.0)
}

/// Loopback test device: discovery + TCP client + frame validation + ack.
fn run_mock_device(cli: &Cli) -> io::Result<()> {
    let refresh = Duration::from_secs_f64(cli.refresh);
    info!(
        "mock device: listening udp :{} for Q0BA_SERVER ...",
        cli.discover_port
    );
    let sock = UdpSocket::bind(("0.0.0.0", cli.discover_port))?;
    let (ip, port) = loop {
        let mut buf = [0u8; 256];
        let (n, from) = sock.recv_from(&mut buf)?;
        let text = String::from_utf8_lossy(&buf[..n]);
        let mut parts = text.split_whitespace();
        if parts.next() == Some("Q0BA_SERVER") {
            let ip = parts.next().unwrap_or("127.0.0.1").to_string();
            let port = parts
                .next()
                .and_then(|p| p.parse().ok())
                .unwrap_or(DEFAULT_PORT);
            info!("mock device: announce from {from}: {ip}:{port}");
            break (ip, port);
        }
    };

    let mut stream = TcpStream::connect((ip.as_str(), port))?;
    info!("mock device: connected to host");
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;

    let mut expected_seq = 0u32;
    loop {
        let mut header = [0u8; server::HEADER_BYTES];
        stream.read_exact(&mut header)?;
        if &header[0..4] != server::FRAME_MAGIC {
            return Err(io::Error::other("bad frame magic"));
        }
        let seq = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        if len != dither::FRAME_BYTES {
            return Err(io::Error::other(format!("bad frame len {len}")));
        }
        if seq != expected_seq {
            return Err(io::Error::other(format!(
                "seq jump: got {seq}, expected {expected_seq}"
            )));
        }
        let mut frame = vec![0u8; len];
        stream.read_exact(&mut frame)?;
        let white_px: u64 = frame.iter().map(|b| b.count_ones() as u64).sum();
        info!(
            "mock frame {seq}: {len} bytes, {white_px} white px, refreshing {:.2}s ...",
            refresh.as_secs_f64()
        );
        std::thread::sleep(refresh);
        stream.write_all(&[server::ACK])?;
        expected_seq += 1;
    }
}
