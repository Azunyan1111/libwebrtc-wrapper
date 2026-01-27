//! WHEP Client - WebRTC-HTTP Egress Protocol client
//!
//! Connects to a WHEP endpoint and receives WebRTC streams,
//! outputting decoded video (rawvideo I420) and audio (PCM S16LE) to files.
//!
//! Usage:
//!   whep-client [WHEP_URL] [-o OUTPUT_DIR]
//!
//! Output files:
//!   output/video.yuv   - I420 rawvideo (ffplay -f rawvideo -pix_fmt yuv420p -s WxH video.yuv)
//!   output/audio.pcm   - PCM S16LE (ffplay -f s16le -ar RATE -ac CH audio.pcm)
//!   output/metadata.txt - Playback parameters

mod whep;

use anyhow::Result;
use std::path::PathBuf;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

/// Parse command line arguments
/// Returns (whep_url, output_dir)
fn parse_args() -> (String, PathBuf) {
    let args: Vec<String> = std::env::args().collect();

    let default_url = "https://customer-2y2pi15b1mgfooko.cloudflarestream.com/e94a1943c1b42fef532875db0673477c/webRTC/play".to_string();
    let default_output = PathBuf::from("./output");

    let mut whep_url = default_url;
    let mut output_dir = default_output;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                if i + 1 < args.len() {
                    output_dir = PathBuf::from(&args[i + 1]);
                    i += 2;
                } else {
                    eprintln!("Error: -o requires an argument");
                    std::process::exit(1);
                }
            }
            "-h" | "--help" => {
                eprintln!("WHEP Client - WebRTC-HTTP Egress Protocol client");
                eprintln!();
                eprintln!("Usage: whep-client [WHEP_URL] [-o OUTPUT_DIR]");
                eprintln!();
                eprintln!("Options:");
                eprintln!("  -o, --output <DIR>  Output directory for YUV/PCM files [default: ./output]");
                eprintln!("  -h, --help          Show this help message");
                eprintln!();
                eprintln!("Output files:");
                eprintln!("  video.yuv    - I420 rawvideo");
                eprintln!("  audio.pcm    - PCM S16LE interleaved");
                eprintln!("  metadata.txt - Playback parameters (width, height, sample rate, etc.)");
                std::process::exit(0);
            }
            arg if !arg.starts_with('-') => {
                whep_url = arg.to_string();
                i += 1;
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                std::process::exit(1);
            }
        }
    }

    (whep_url, output_dir)
}

// Use current_thread runtime to prevent WebRTC raw pointers from being moved across threads.
// libwebrtc objects are not thread-safe and must be accessed from the thread they were created on.
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("WHEP Client starting...");

    let (whep_url, output_dir) = parse_args();

    info!("Connecting to WHEP endpoint: {}", whep_url);
    info!("Output directory: {:?}", output_dir);

    let mut client = whep::WhepClient::new(&whep_url, output_dir)?;
    client.connect().await?;

    info!("Connected. Receiving frames (Press Ctrl+C to stop)...");

    // Run with Ctrl+C handling
    tokio::select! {
        result = async { client.run().await } => {
            if let Err(e) = result {
                tracing::error!("Error: {}", e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Received Ctrl+C, shutting down...");
        }
    }

    client.close().await?;

    Ok(())
}
