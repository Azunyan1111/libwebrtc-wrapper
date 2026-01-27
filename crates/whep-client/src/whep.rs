//! WHEP (WebRTC-HTTP Egress Protocol) client implementation

use crate::mkv_writer::{MkvConfig, MkvWriter};
use anyhow::{anyhow, Result};
use libwebrtc_sys::*;
use std::ffi::{CStr, CString};
use std::io::{BufWriter, Stdout};
use std::os::raw::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use tracing::{debug, info, warn};
use url::Url;

/// ICE candidate information for trickle ICE
#[derive(Debug, Clone)]
pub struct IceCandidate {
    pub candidate: String,
    pub sdp_mid: String,
    pub sdp_mline_index: i32,
}

/// WHEP Client for receiving WebRTC streams
pub struct WhepClient {
    whep_url: String,
    http_client: reqwest::Client,
    factory: *mut WebrtcPeerConnectionFactory,
    peer_connection: *mut WebrtcPeerConnection,
    resource_url: Option<String>,
    callback_context: Option<Box<CallbackContext>>,
    ice_candidate_rx: Option<Receiver<IceCandidate>>,
    end_of_candidates_sent: bool,
}

use std::sync::atomic::Ordering;

/// Context for frame counting, statistics, and MKV output.
/// Passed to C callbacks as user_data pointer.
pub struct CallbackContext {
    pub video_frame_count: AtomicU64,
    pub audio_frame_count: AtomicU64,
    pub ice_gathering_complete: AtomicBool,
    pub ice_candidate_tx: Sender<IceCandidate>,
    // MKV writer for stdout output
    pub mkv_writer: Mutex<Option<MkvWriter<BufWriter<Stdout>>>>,
    // Metadata storage (set on first frame)
    pub video_width: AtomicU64,
    pub video_height: AtomicU64,
    pub audio_sample_rate: AtomicU64,
    pub audio_channels: AtomicU64,
    // Timestamp tracking
    pub first_video_timestamp_us: AtomicI64,
    // Audio sample counter for precise timing
    pub audio_total_samples: AtomicU64,
}

// WhepClient intentionally does NOT implement Send.
// libwebrtc raw pointers are not thread-safe and must remain on the thread they were created on.
// Use #[tokio::main(flavor = "current_thread")] to ensure single-threaded execution.

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
            ice_candidate_rx: None,
            end_of_candidates_sent: false,
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

        // Set up callback context with ICE candidate channel and MKV output
        let (ice_tx, ice_rx) = mpsc::channel();

        // MKV writer will be initialized on first frame when we know video dimensions
        let ctx = Box::new(CallbackContext {
            video_frame_count: AtomicU64::new(0),
            audio_frame_count: AtomicU64::new(0),
            ice_gathering_complete: AtomicBool::new(false),
            ice_candidate_tx: ice_tx,
            mkv_writer: Mutex::new(None),
            video_width: AtomicU64::new(0),
            video_height: AtomicU64::new(0),
            audio_sample_rate: AtomicU64::new(0),
            audio_channels: AtomicU64::new(0),
            first_video_timestamp_us: AtomicI64::new(-1),
            audio_total_samples: AtomicU64::new(0),
        });
        self.callback_context = Some(ctx);
        self.ice_candidate_rx = Some(ice_rx);

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

        // Get user_data pointer from callback_context
        let user_data = self.callback_context.as_ref()
            .map(|ctx| ctx.as_ref() as *const CallbackContext as *mut c_void)
            .unwrap_or(ptr::null_mut());

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

        // Video frame callback - writes I420 YUV data to MKV
        unsafe extern "C" fn on_video_frame(
            user_data: *mut c_void,
            width: i32,
            height: i32,
            timestamp_us: i64,
            y_data: *const u8,
            y_stride: i32,
            u_data: *const u8,
            u_stride: i32,
            v_data: *const u8,
            v_stride: i32,
        ) {
            if user_data.is_null() {
                return;
            }
            let ctx = &*(user_data as *const CallbackContext);
            let count = ctx.video_frame_count.fetch_add(1, Ordering::Relaxed) + 1;

            // Store metadata and initialize MKV writer on first frame
            if count == 1 {
                ctx.video_width.store(width as u64, Ordering::Relaxed);
                ctx.video_height.store(height as u64, Ordering::Relaxed);
                ctx.first_video_timestamp_us.store(timestamp_us, Ordering::Relaxed);
                eprintln!(
                    "[INFO] Video format detected: {}x{} (y_stride={}, u_stride={}, v_stride={})",
                    width, height, y_stride, u_stride, v_stride
                );

                // Initialize MKV writer with default audio params (will be updated on first audio frame if different)
                let config = MkvConfig {
                    video_width: width as u32,
                    video_height: height as u32,
                    audio_sample_rate: 48000,
                    audio_channels: 2,
                };
                if let Ok(mut guard) = ctx.mkv_writer.lock() {
                    match MkvWriter::new(BufWriter::new(std::io::stdout()), config) {
                        Ok(writer) => {
                            *guard = Some(writer);
                            eprintln!("[INFO] MKV writer initialized, streaming to stdout");
                        }
                        Err(e) => {
                            eprintln!("[ERROR] Failed to initialize MKV writer: {}", e);
                        }
                    }
                }
            }

            // Build I420 frame data (packed, no stride padding)
            let w = width as usize;
            let h = height as usize;
            let y_stride_usize = y_stride as usize;
            let u_stride_usize = u_stride as usize;
            let v_stride_usize = v_stride as usize;

            let y_size = w * h;
            let uv_w = w / 2;
            let uv_h = h / 2;
            let uv_size = uv_w * uv_h;
            let frame_size = y_size + uv_size * 2;

            let mut frame_data = Vec::with_capacity(frame_size);

            // Copy Y plane (row by row to handle stride)
            for row in 0..h {
                let row_start = row * y_stride_usize;
                let row_data = std::slice::from_raw_parts(y_data.add(row_start), w);
                frame_data.extend_from_slice(row_data);
            }

            // Copy U plane (half width, half height)
            for row in 0..uv_h {
                let row_start = row * u_stride_usize;
                let row_data = std::slice::from_raw_parts(u_data.add(row_start), uv_w);
                frame_data.extend_from_slice(row_data);
            }

            // Copy V plane (half width, half height)
            for row in 0..uv_h {
                let row_start = row * v_stride_usize;
                let row_data = std::slice::from_raw_parts(v_data.add(row_start), uv_w);
                frame_data.extend_from_slice(row_data);
            }

            // Calculate timestamp relative to first frame
            let first_ts = ctx.first_video_timestamp_us.load(Ordering::Relaxed);
            let timestamp_ms = (timestamp_us - first_ts) / 1000;

            // Write to MKV (all I420 frames are keyframes for uncompressed video)
            if let Ok(mut guard) = ctx.mkv_writer.lock() {
                if let Some(ref mut writer) = *guard {
                    if let Err(e) = writer.write_video_frame(&frame_data, timestamp_ms, true) {
                        eprintln!("[ERROR] Failed to write video frame: {}", e);
                    }
                }
            }

            // Log every 30 frames (approximately once per second at 30fps)
            if count % 30 == 1 {
                eprintln!(
                    "[TRACE] Video frame #{}: {}x{} ts={}ms",
                    count, width, height, timestamp_ms
                );
            }
        }

        // Audio frame callback - writes PCM S16LE interleaved data to MKV
        unsafe extern "C" fn on_audio_frame(
            user_data: *mut c_void,
            sample_rate: i32,
            num_channels: usize,
            samples_per_channel: usize,
            data: *const i16,
        ) {
            if user_data.is_null() {
                return;
            }
            let ctx = &*(user_data as *const CallbackContext);

            // Check if MKV writer is initialized (waits for first video frame)
            let first_video_ts = ctx.first_video_timestamp_us.load(Ordering::Relaxed);
            if first_video_ts < 0 {
                // Video not started yet, discard audio frames but don't count them
                return;
            }

            let count = ctx.audio_frame_count.fetch_add(1, Ordering::Relaxed) + 1;

            // Store metadata on first frame
            if count == 1 {
                ctx.audio_sample_rate.store(sample_rate as u64, Ordering::Relaxed);
                ctx.audio_channels.store(num_channels as u64, Ordering::Relaxed);
                eprintln!(
                    "[INFO] Audio format detected: {}Hz {}ch ({} samples/channel)",
                    sample_rate, num_channels, samples_per_channel
                );
            }

            // Calculate audio timestamp based on accumulated samples
            // This ensures audio timestamp increases at the correct rate
            let prev_samples = ctx.audio_total_samples.fetch_add(samples_per_channel as u64, Ordering::Relaxed);
            let timestamp_ms = (prev_samples as i64 * 1000) / sample_rate as i64;

            // Convert PCM data to bytes (S16LE)
            let total_samples = samples_per_channel * num_channels;
            let audio_data = std::slice::from_raw_parts(data, total_samples);
            let byte_data = std::slice::from_raw_parts(
                audio_data.as_ptr() as *const u8,
                total_samples * 2,
            );

            // Write to MKV
            if let Ok(mut guard) = ctx.mkv_writer.lock() {
                if let Some(ref mut writer) = *guard {
                    if let Err(e) = writer.write_audio_frame(byte_data, timestamp_ms) {
                        eprintln!("[ERROR] Failed to write audio frame: {}", e);
                    }
                }
            }

            // Log every 100 frames (approximately once per second at 48kHz/480 samples)
            if count % 100 == 1 {
                eprintln!(
                    "[TRACE] Audio frame #{}: {}Hz {}ch {} samples ts={}ms",
                    count, sample_rate, num_channels, samples_per_channel, timestamp_ms
                );
            }
        }

        // ICE candidate callback - sends candidates to channel for trickle ICE
        unsafe extern "C" fn on_ice_candidate_callback(
            user_data: *mut c_void,
            candidate: *const std::os::raw::c_char,
            sdp_mid: *const std::os::raw::c_char,
            sdp_mline_index: std::os::raw::c_int,
        ) {
            if user_data.is_null() || candidate.is_null() {
                return;
            }
            let ctx = &*(user_data as *const CallbackContext);
            let candidate_str = CStr::from_ptr(candidate).to_string_lossy().to_string();
            let sdp_mid_str = if sdp_mid.is_null() {
                String::new()
            } else {
                CStr::from_ptr(sdp_mid).to_string_lossy().to_string()
            };

            debug!(
                "ICE candidate gathered: mid={} index={} candidate={}",
                sdp_mid_str, sdp_mline_index, candidate_str
            );

            let ice_candidate = IceCandidate {
                candidate: candidate_str,
                sdp_mid: sdp_mid_str,
                sdp_mline_index,
            };

            // Send to channel (ignore error if receiver dropped)
            let _ = ctx.ice_candidate_tx.send(ice_candidate);
        }

        // ICE gathering state change callback
        unsafe extern "C" fn on_ice_gathering_state_callback(
            user_data: *mut c_void,
            state: std::os::raw::c_int,
        ) {
            let state_str = match state {
                0 => "New",
                1 => "Gathering",
                2 => "Complete",
                _ => "Unknown",
            };
            info!("ICE gathering state changed: {}", state_str);

            if !user_data.is_null() && state == 2 {
                // Complete
                let ctx = &*(user_data as *const CallbackContext);
                ctx.ice_gathering_complete.store(true, Ordering::SeqCst);
            }
        }

        unsafe {
            // Register frame callbacks BEFORE creating offer
            webrtc_pc_set_video_frame_callback(pc, Some(on_video_frame), user_data);
            webrtc_pc_set_audio_frame_callback(pc, Some(on_audio_frame), user_data);

            webrtc_pc_set_on_track_callback(pc, Some(on_track_callback), user_data);
            webrtc_pc_set_on_ice_connection_state_change_callback(
                pc,
                Some(on_ice_state_callback),
                user_data,
            );
            webrtc_pc_set_on_ice_candidate_callback(
                pc,
                Some(on_ice_candidate_callback),
                user_data,
            );
            webrtc_pc_set_on_ice_gathering_state_change_callback(
                pc,
                Some(on_ice_gathering_state_callback),
                user_data,
            );
        }
    }

    /// Wait for connection and frames
    pub async fn run(&mut self) -> Result<()> {
        info!("Waiting for connection...");

        // Poll ICE connection state and send trickle ICE candidates
        loop {
            // Process any pending ICE candidates (trickle ICE)
            self.process_ice_candidates().await;

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

            // Continue processing ICE candidates (may still arrive after connection)
            self.process_ice_candidates().await;

            // Check if ICE gathering is complete and send end-of-candidates
            self.check_and_send_end_of_candidates().await;

            let state = unsafe { webrtc_pc_ice_connection_state(self.peer_connection) };
            if state == 4 || state == 6 {
                // Failed or Closed
                warn!("Connection lost (state={})", state);
                break;
            }

            // Log frame statistics
            if let Some(ref ctx) = self.callback_context {
                let video_count = ctx.video_frame_count.load(Ordering::Relaxed);
                let audio_count = ctx.audio_frame_count.load(Ordering::Relaxed);
                debug!("Frame stats: video={}, audio={}", video_count, audio_count);
            }
        }

        Ok(())
    }

    /// Process pending ICE candidates and send them via PATCH (trickle ICE)
    async fn process_ice_candidates(&mut self) {
        let candidates: Vec<IceCandidate> = if let Some(ref rx) = self.ice_candidate_rx {
            rx.try_iter().collect()
        } else {
            return;
        };

        if candidates.is_empty() {
            return;
        }

        let resource_url = match &self.resource_url {
            Some(url) => url.clone(),
            None => {
                warn!("Cannot send ICE candidates: resource URL not available");
                return;
            }
        };

        for candidate in candidates {
            if let Err(e) = self.send_ice_candidate(&resource_url, &candidate).await {
                warn!("Failed to send ICE candidate: {}", e);
            }
        }
    }

    /// Send a single ICE candidate to the WHEP resource URL via PATCH
    async fn send_ice_candidate(&self, resource_url: &str, candidate: &IceCandidate) -> Result<()> {
        // Format as SDP fragment per RFC 8840 / draft-ietf-wish-whip
        // Include a=mid to identify the m-line for multiple media sections
        let sdp_fragment = format!(
            "a=mid:{}\r\na={}\r\n",
            candidate.sdp_mid,
            candidate.candidate
        );

        debug!(
            "Sending ICE candidate to {}: mid={} index={}",
            resource_url, candidate.sdp_mid, candidate.sdp_mline_index
        );

        let response = self
            .http_client
            .patch(resource_url)
            .header("Content-Type", "application/trickle-ice-sdpfrag")
            .body(sdp_fragment)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "PATCH request failed with status {}: {}",
                status,
                body
            ));
        }

        debug!("ICE candidate sent successfully");
        Ok(())
    }

    /// Check if ICE gathering is complete and send end-of-candidates if not already sent
    async fn check_and_send_end_of_candidates(&mut self) {
        if self.end_of_candidates_sent {
            return;
        }

        let gathering_complete = self.callback_context.as_ref()
            .map(|ctx| ctx.ice_gathering_complete.load(Ordering::SeqCst))
            .unwrap_or(false);

        if !gathering_complete {
            return;
        }

        let resource_url = match &self.resource_url {
            Some(url) => url.clone(),
            None => {
                warn!("Cannot send end-of-candidates: resource URL not available");
                return;
            }
        };

        if let Err(e) = self.send_end_of_candidates(&resource_url).await {
            warn!("Failed to send end-of-candidates: {}", e);
        } else {
            self.end_of_candidates_sent = true;
        }
    }

    /// Send end-of-candidates indication per RFC 8840
    async fn send_end_of_candidates(&self, resource_url: &str) -> Result<()> {
        // Per RFC 8840, a=end-of-candidates signals no more candidates
        let sdp_fragment = "a=end-of-candidates\r\n";

        info!("Sending end-of-candidates to {}", resource_url);

        let response = self
            .http_client
            .patch(resource_url)
            .header("Content-Type", "application/trickle-ice-sdpfrag")
            .body(sdp_fragment)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "PATCH end-of-candidates failed with status {}: {}",
                status,
                body
            ));
        }

        info!("end-of-candidates sent successfully");
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
        // Use Url::join for proper relative URL resolution per RFC 3986
        let resource_url = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(|location| {
                if location.starts_with("http://") || location.starts_with("https://") {
                    // Absolute URL - use as-is
                    location.to_string()
                } else {
                    // Relative URL - resolve against base WHEP URL
                    Url::parse(&self.whep_url)
                        .and_then(|base| base.join(location))
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| {
                            // Fallback: simple concatenation
                            warn!("Failed to parse base URL, using fallback concatenation");
                            format!("{}/{}", self.whep_url.trim_end_matches('/'), location.trim_start_matches('/'))
                        })
                }
            });

        let sdp = response.text().await?;

        Ok(WhepResponse { sdp, resource_url })
    }

    /// Close the connection
    pub async fn close(&mut self) -> Result<()> {
        // Log final statistics
        if let Some(ref ctx) = self.callback_context {
            let video_width = ctx.video_width.load(Ordering::Relaxed);
            let video_height = ctx.video_height.load(Ordering::Relaxed);
            let audio_sample_rate = ctx.audio_sample_rate.load(Ordering::Relaxed);
            let audio_channels = ctx.audio_channels.load(Ordering::Relaxed);
            let video_frames = ctx.video_frame_count.load(Ordering::Relaxed);
            let audio_frames = ctx.audio_frame_count.load(Ordering::Relaxed);

            eprintln!(
                "[INFO] Capture complete: {} video frames ({}x{}), {} audio frames ({}Hz {}ch)",
                video_frames, video_width, video_height,
                audio_frames, audio_sample_rate, audio_channels
            );

            // Flush MKV writer
            if let Ok(mut guard) = ctx.mkv_writer.lock() {
                if let Some(ref mut writer) = *guard {
                    let _ = writer.flush();
                }
            }
        }

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
