//! WHEP Client - WebRTC-HTTP Egress Protocol client
//!
//! Connects to a WHEP endpoint and receives WebRTC streams,
//! outputting MKV (Matroska) format to stdout.
//!
//! Usage:
//!   whep-client [WHEP_URL] > output.mkv
//!   whep-client [WHEP_URL] | ffplay -f matroska -i -
//!
//! Video codec: V_UNCOMPRESSED (I420 YUV)
//! Audio codec: A_PCM/INT/LIT (PCM S16LE)

mod mkv_writer;
mod whep;

use anyhow::Result;

struct AppConfig {
    whep_url: String,
    debug: bool,
    debug_libwebrtc: bool,
}

/// Parse command line arguments
/// Returns whep_url
fn parse_args() -> AppConfig {
    let args: Vec<String> = std::env::args().collect();

    let default_url = "https://customer-2y2pi15b1mgfooko.cloudflarestream.com/e94a1943c1b42fef532875db0673477c/webRTC/play".to_string();

    let mut whep_url = default_url;
    let mut debug = false;
    let mut debug_libwebrtc = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                eprintln!("WHEP Client - WebRTC-HTTP Egress Protocol client");
                eprintln!();
                eprintln!("Usage: whep-client [options] [WHEP_URL] > output.mkv");
                eprintln!("       whep-client [WHEP_URL] | ffplay -f matroska -i -");
                eprintln!();
                eprintln!("Options:");
                eprintln!("  -h, --help          Show this help message");
                eprintln!("  -d, --debug         Enable debug logging (this project)");
                eprintln!("  -dlibwebrtc         Enable libwebrtc debug logging");
                eprintln!();
                eprintln!("Output: MKV (Matroska) format to stdout");
                eprintln!("  Video: V_UNCOMPRESSED (I420 YUV)");
                eprintln!("  Audio: A_PCM/INT/LIT (PCM S16LE, 48kHz, stereo)");
                eprintln!();
                eprintln!("Examples:");
                eprintln!("  # Save to file");
                eprintln!("  whep-client https://example.com/whep > output.mkv");
                eprintln!();
                eprintln!("  # Play directly with ffplay");
                eprintln!("  whep-client https://example.com/whep | ffplay -f matroska -i -");
                eprintln!();
                eprintln!("  # Verify with ffprobe");
                eprintln!("  whep-client https://example.com/whep > output.mkv && ffprobe output.mkv");
                std::process::exit(0);
            }
            "-d" | "--debug" => {
                debug = true;
                i += 1;
            }
            "-dlibwebrtc" => {
                debug_libwebrtc = true;
                i += 1;
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

    AppConfig {
        whep_url,
        debug,
        debug_libwebrtc,
    }
}

fn init_tracing(debug_libwebrtc: bool) {
    if !debug_libwebrtc {
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
    // All logs go to stderr (stdout is reserved for MKV output)
    eprintln!("[INFO] WHEP Client starting...");

    let config = parse_args();
    init_tracing(config.debug_libwebrtc);

    eprintln!("[INFO] Connecting to WHEP endpoint: {}", config.whep_url);
    eprintln!("[INFO] MKV output will be streamed to stdout");

    let mut client = whep::WhepClient::new(&config.whep_url, config.debug)?;
    client.connect().await?;

    eprintln!("[INFO] Connected. Receiving frames (Press Ctrl+C to stop)...");

    // Run with Ctrl+C and SIGTERM handling
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        result = async { client.run().await } => {
            if let Err(e) = result {
                eprintln!("[ERROR] {}", e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("[INFO] Received Ctrl+C, shutting down...");
        }
        _ = sigterm.recv() => {
            eprintln!("[INFO] Received SIGTERM, shutting down...");
        }
    }

    client.close().await?;

    Ok(())
}
