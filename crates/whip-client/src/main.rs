//! WHIP Client - WebRTC-HTTP Ingestion Protocol client
//!
//! Reads MKV (Matroska) format from stdin and sends via WHIP protocol.
//!
//! Usage:
//!   cat input.mkv | whip-client [WHIP_URL]
//!   ffmpeg -i input.mp4 -c:v rawvideo -pix_fmt yuv420p -c:a pcm_s16le -f matroska - | whip-client [WHIP_URL]
//!
//! Video codec: VP8 (encoded by libwebrtc)
//! Audio codec: Opus (encoded by libwebrtc)
//!
//! Input format:
//!   Video: V_UNCOMPRESSED (I420 YUV) or VP8/VP9
//!   Audio: A_PCM/INT/LIT (PCM S16LE) or Opus

mod mkv_reader;
mod whip;

use anyhow::Result;
use mkv_reader::MkvReader;
use std::io::{self, BufReader};

struct AppConfig {
    whip_url: String,
    debug: bool,
    #[allow(dead_code)]
    output_file: Option<String>,
}

/// Parse command line arguments
fn parse_args() -> AppConfig {
    let args: Vec<String> = std::env::args().collect();

    let mut whip_url = String::new();
    let mut debug = false;
    let mut output_file = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                eprintln!("WHIP Client - WebRTC-HTTP Ingestion Protocol client");
                eprintln!();
                eprintln!("Usage: cat input.mkv | whip-client [options] <WHIP_URL>");
                eprintln!("       whip-client [options] <WHIP_URL> < input.mkv");
                eprintln!();
                eprintln!("Options:");
                eprintln!("  -h, --help              Show this help message");
                eprintln!("  -d, --debug             Enable debug logging");
                eprintln!("  -o, --output <FILE>     Output file for received stream (optional)");
                eprintln!();
                eprintln!("Input: MKV (Matroska) format from stdin");
                eprintln!("  Video: V_UNCOMPRESSED (I420 YUV)");
                eprintln!("  Audio: A_PCM/INT/LIT (PCM S16LE, 48kHz, stereo)");
                eprintln!();
                eprintln!("Examples:");
                eprintln!("  # Send from file");
                eprintln!("  cat input.mkv | whip-client https://example.com/whip");
                eprintln!();
                eprintln!("  # Send from ffmpeg");
                eprintln!("  ffmpeg -i input.mp4 -c:v rawvideo -pix_fmt yuv420p -c:a pcm_s16le -f matroska - | \\");
                eprintln!("      whip-client https://example.com/whip");
                eprintln!();
                eprintln!("  # Relay WHEP to WHIP");
                eprintln!("  whep-client https://source/whep | whip-client https://dest/whip");
                eprintln!();
                eprintln!("  # Debug mode");
                eprintln!("  whip-client -d https://example.com/whip < input.mkv");
                std::process::exit(0);
            }
            "-d" | "--debug" => {
                debug = true;
                i += 1;
            }
            "-o" | "--output" => {
                if i + 1 < args.len() {
                    output_file = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!("Error: --output requires a file path");
                    std::process::exit(1);
                }
            }
            arg if !arg.starts_with('-') => {
                whip_url = arg.to_string();
                i += 1;
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                std::process::exit(1);
            }
        }
    }

    if whip_url.is_empty() {
        eprintln!("Error: WHIP URL is required");
        eprintln!("Usage: cat input.mkv | whip-client <WHIP_URL>");
        eprintln!("Use -h for help");
        std::process::exit(1);
    }

    AppConfig {
        whip_url,
        debug,
        output_file,
    }
}

fn init_tracing(debug: bool) {
    if !debug {
        return;
    }

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::DEBUG)
        .init();
}

// Use current_thread runtime to prevent WebRTC raw pointers from being moved across threads.
// libwebrtc objects are not thread-safe and must be accessed from the thread they were created on.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // All logs go to stderr (stdin is reserved for MKV input)
    eprintln!("[INFO] WHIP Client starting...");

    let config = parse_args();
    init_tracing(config.debug);

    eprintln!("[INFO] Reading MKV from stdin...");

    // Create MKV reader from stdin
    let stdin = io::stdin().lock();
    let reader = BufReader::new(stdin);
    let mut mkv_reader = MkvReader::new(reader)?;

    let track_info = mkv_reader.track_info();
    eprintln!(
        "[INFO] MKV track info: video={}x{} ({}), audio={}Hz {}ch ({})",
        track_info.video_width,
        track_info.video_height,
        track_info.video_codec_id,
        track_info.audio_sample_rate,
        track_info.audio_channels,
        track_info.audio_codec_id
    );

    if track_info.video_width == 0 || track_info.video_height == 0 {
        return Err(anyhow::anyhow!("No video track found in MKV"));
    }

    eprintln!("[INFO] Connecting to WHIP endpoint: {}", config.whip_url);

    let mut client = whip::WhipClient::new(&config.whip_url, config.debug)?;
    client
        .connect(
            track_info.video_width,
            track_info.video_height,
            track_info.audio_sample_rate,
            track_info.audio_channels,
        )
        .await?;

    eprintln!("[INFO] Connected. Sending frames (Press Ctrl+C to stop)...");

    // Run with Ctrl+C handling
    tokio::select! {
        result = async { client.run(&mut mkv_reader).await } => {
            if let Err(e) = result {
                eprintln!("[ERROR] {}", e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("[INFO] Received Ctrl+C, shutting down...");
        }
    }

    client.close().await?;

    Ok(())
}
