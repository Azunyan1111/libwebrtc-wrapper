//! WHEP (WebRTC-HTTP Egress Protocol) client implementation

use anyhow::{anyhow, Result};
use libwebrtc_sys::*;
use std::ffi::{CStr, CString};
use std::os::raw::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

/// WHEP Client for receiving WebRTC streams
pub struct WhepClient {
    whep_url: String,
    http_client: reqwest::Client,
    factory: *mut WebrtcPeerConnectionFactory,
    peer_connection: *mut WebrtcPeerConnection,
    resource_url: Option<String>,
    callback_context: Option<Box<CallbackContext>>,
}

struct CallbackContext {
    video_frame_count: AtomicU64,
    audio_frame_count: AtomicU64,
}

// Safety: WhepClient manages raw pointers but ensures they are only used on the main thread
unsafe impl Send for WhepClient {}

impl WhepClient {
    /// Create a new WHEP client
    pub fn new(whep_url: &str) -> Result<Self> {
        let http_client = reqwest::Client::builder().build()?;

        Ok(Self {
            whep_url: whep_url.to_string(),
            http_client,
            factory: ptr::null_mut(),
            peer_connection: ptr::null_mut(),
            resource_url: None,
            callback_context: None,
        })
    }

    /// Connect to the WHEP endpoint
    pub async fn connect(&mut self) -> Result<()> {
        info!("Creating PeerConnectionFactory...");

        // Create factory
        let factory = unsafe { webrtc_factory_create() };
        if factory.is_null() {
            return Err(anyhow!("Failed to create PeerConnectionFactory"));
        }
        self.factory = factory;

        info!("Creating PeerConnection...");

        // Create peer connection
        let ice_servers = CString::new("[]").unwrap();
        let pc = unsafe { webrtc_pc_create(factory, ice_servers.as_ptr()) };
        if pc.is_null() {
            return Err(anyhow!("Failed to create PeerConnection"));
        }
        self.peer_connection = pc;

        // Set up callback context
        let ctx = Box::new(CallbackContext {
            video_frame_count: AtomicU64::new(0),
            audio_frame_count: AtomicU64::new(0),
        });
        self.callback_context = Some(ctx);

        // Set up callbacks
        self.setup_callbacks();

        // Create offer
        info!("Creating offer...");
        let offer_ptr = unsafe { webrtc_pc_create_offer(pc) };
        if offer_ptr.is_null() {
            return Err(anyhow!("Failed to create offer"));
        }
        let offer_sdp = unsafe { CStr::from_ptr(offer_ptr).to_string_lossy().to_string() };
        unsafe { webrtc_free_string(offer_ptr) };
        debug!("Offer SDP:\n{}", offer_sdp);

        // Set local description
        let offer_cstr = CString::new(offer_sdp.as_str()).unwrap();
        let offer_type = CString::new("offer").unwrap();
        let result = unsafe {
            webrtc_pc_set_local_description(pc, offer_cstr.as_ptr(), offer_type.as_ptr())
        };
        if result != 0 {
            return Err(anyhow!("Failed to set local description"));
        }
        info!("Local description set");

        // Send offer to WHEP endpoint
        info!("Sending offer to WHEP endpoint...");
        let response = self.send_offer(&offer_sdp).await?;

        // Set remote description (answer)
        debug!("Answer SDP:\n{}", response.sdp);
        let answer_cstr = CString::new(response.sdp.as_str()).unwrap();
        let answer_type = CString::new("answer").unwrap();
        let result = unsafe {
            webrtc_pc_set_remote_description(pc, answer_cstr.as_ptr(), answer_type.as_ptr())
        };
        if result != 0 {
            return Err(anyhow!("Failed to set remote description"));
        }
        info!("Remote description set");

        self.resource_url = response.resource_url;

        if let Some(ref url) = self.resource_url {
            info!("Resource URL: {}", url);
        }

        Ok(())
    }

    fn setup_callbacks(&mut self) {
        let pc = self.peer_connection;

        // Set on_track callback
        unsafe extern "C" fn on_track_callback(
            _user_data: *mut c_void,
            track_id: *const std::os::raw::c_char,
            is_video: std::os::raw::c_int,
        ) {
            let track_id_str = CStr::from_ptr(track_id).to_string_lossy();
            if is_video != 0 {
                info!("Video track received: id={}", track_id_str);
            } else {
                info!("Audio track received: id={}", track_id_str);
            }
        }

        unsafe extern "C" fn on_ice_state_callback(
            _user_data: *mut c_void,
            state: std::os::raw::c_int,
        ) {
            let state_str = match state {
                0 => "New",
                1 => "Checking",
                2 => "Connected",
                3 => "Completed",
                4 => "Failed",
                5 => "Disconnected",
                6 => "Closed",
                _ => "Unknown",
            };
            info!("ICE connection state changed: {}", state_str);
        }

        unsafe {
            webrtc_pc_set_on_track_callback(pc, Some(on_track_callback), ptr::null_mut());
            webrtc_pc_set_on_ice_connection_state_change_callback(
                pc,
                Some(on_ice_state_callback),
                ptr::null_mut(),
            );
        }
    }

    /// Wait for connection and frames
    pub async fn run(&self) -> Result<()> {
        info!("Waiting for connection...");

        // Poll ICE connection state
        loop {
            let state = unsafe { webrtc_pc_ice_connection_state(self.peer_connection) };
            match state {
                2 | 3 => {
                    // Connected or Completed
                    info!("ICE connection established");
                    break;
                }
                4 => {
                    // Failed
                    return Err(anyhow!("ICE connection failed"));
                }
                6 => {
                    // Closed
                    return Err(anyhow!("Connection closed"));
                }
                _ => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }
        }

        info!("Connection established. Receiving frames...");

        // Keep running until interrupted
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

            let state = unsafe { webrtc_pc_ice_connection_state(self.peer_connection) };
            if state == 4 || state == 6 {
                // Failed or Closed
                warn!("Connection lost (state={})", state);
                break;
            }
        }

        Ok(())
    }

    /// Send SDP offer to WHEP endpoint and receive answer
    async fn send_offer(&self, sdp: &str) -> Result<WhepResponse> {
        let response = self
            .http_client
            .post(&self.whep_url)
            .header("Content-Type", "application/sdp")
            .header("Accept", "application/sdp")
            .body(sdp.to_string())
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "WHEP request failed with status {}: {}",
                status,
                body
            ));
        }

        // Get resource URL from Location header
        let resource_url = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(|s| {
                if s.starts_with("http") {
                    s.to_string()
                } else {
                    // Relative URL - resolve against base
                    format!("{}{}", self.whep_url.trim_end_matches("/webRTC/play"), s)
                }
            });

        let sdp = response.text().await?;

        Ok(WhepResponse { sdp, resource_url })
    }

    /// Close the connection
    pub async fn close(&mut self) -> Result<()> {
        if !self.peer_connection.is_null() {
            unsafe {
                webrtc_pc_destroy(self.peer_connection);
            }
            self.peer_connection = ptr::null_mut();
            info!("PeerConnection closed");
        }

        if !self.factory.is_null() {
            unsafe {
                webrtc_factory_destroy(self.factory);
            }
            self.factory = ptr::null_mut();
        }

        // Send DELETE to resource URL if available
        if let Some(ref url) = self.resource_url {
            let _ = self.http_client.delete(url).send().await;
            info!("WHEP resource deleted");
        }

        Ok(())
    }
}

impl Drop for WhepClient {
    fn drop(&mut self) {
        if !self.peer_connection.is_null() {
            unsafe {
                webrtc_pc_destroy(self.peer_connection);
            }
        }
        if !self.factory.is_null() {
            unsafe {
                webrtc_factory_destroy(self.factory);
            }
        }
    }
}

struct WhepResponse {
    sdp: String,
    resource_url: Option<String>,
}
