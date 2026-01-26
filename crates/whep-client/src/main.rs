//! WHEP Client - WebRTC-HTTP Egress Protocol client
//!
//! Connects to a WHEP endpoint and receives WebRTC streams,
//! outputting decoded video (rawvideo I420) and audio (PCM) to MKV format.

mod whep;

use anyhow::Result;
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

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

    // Default WHEP endpoint (can be overridden by CLI args later)
    let whep_url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| {
            "https://customer-2y2pi15b1mgfooko.cloudflarestream.com/e94a1943c1b42fef532875db0673477c/webRTC/play".to_string()
        });

    info!("Connecting to WHEP endpoint: {}", whep_url);

    let mut client = whep::WhepClient::new(&whep_url)?;
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
