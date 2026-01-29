//! WHEP (WebRTC-HTTP Egress Protocol) client implementation

use crate::mkv_writer::{MkvConfig, MkvWriter};
use anyhow::{anyhow, Result};
use futures::stream::StreamExt;
use libwebrtc::audio_stream::native::NativeAudioStream;
use libwebrtc::prelude::*;
use libwebrtc::video_stream::native::NativeVideoStream;
use parking_lot::Mutex as ParkingMutex;
use reqwest::header::HeaderMap;
use std::io::{BufWriter, Stdout};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use url::Url;

/// WHEP Client for receiving WebRTC streams
pub struct WhepClient {
    whep_url: String,
    http_client: reqwest::Client,
    factory: Option<PeerConnectionFactory>,
    peer_connection: Option<PeerConnection>,
    resource_url: Option<String>,
    callback_context: Option<Arc<CallbackContext>>,
    ice_candidate_rx: Option<mpsc::UnboundedReceiver<IceCandidate>>,
    end_of_candidates_sent: bool,
    debug: bool,
    // Video/Audio streams
    video_stream: Option<NativeVideoStream>,
    audio_stream: Option<NativeAudioStream>,
}

/// Context for frame counting, statistics, and MKV output.
pub struct CallbackContext {
    pub video_frame_count: AtomicU64,
    pub audio_frame_count: AtomicU64,
    pub ice_gathering_complete: AtomicBool,
    pub ice_candidate_tx: mpsc::UnboundedSender<IceCandidate>,
    // MKV writer for stdout output
    pub mkv_writer: ParkingMutex<Option<MkvWriter<BufWriter<Stdout>>>>,
    // Flag to track if MKV writer has been initialized
    pub mkv_writer_initialized: AtomicBool,
    // Metadata storage (set on first frame)
    pub video_width: AtomicU64,
    pub video_height: AtomicU64,
    pub audio_sample_rate: AtomicU64,
    pub audio_channels: AtomicU64,
    // Timestamp tracking
    pub first_video_timestamp_us: AtomicI64,
    // Audio sample counter for precise timing
    pub audio_total_samples: AtomicU64,
    pub last_video_timestamp_ms: AtomicI64,
    pub last_audio_timestamp_ms: AtomicI64,
}

impl WhepClient {
    /// Create a new WHEP client
    pub fn new(whep_url: &str, debug: bool) -> Result<Self> {
        let http_client = reqwest::Client::builder().build()?;

        Ok(Self {
            whep_url: whep_url.to_string(),
            http_client,
            factory: None,
            peer_connection: None,
            resource_url: None,
            callback_context: None,
            ice_candidate_rx: None,
            end_of_candidates_sent: false,
            debug,
            video_stream: None,
            audio_stream: None,
        })
    }

    /// Connect to the WHEP endpoint
    pub async fn connect(&mut self) -> Result<()> {
        info!("Creating PeerConnectionFactory...");

        // Create factory
        let factory = PeerConnectionFactory::default();
        self.factory = Some(factory.clone());

        info!("Creating PeerConnection...");

        // Create peer connection with default config (ICE servers applied after WHEP response)
        let config = RtcConfiguration::default();
        let pc = factory
            .create_peer_connection(config)
            .map_err(|e| anyhow!("Failed to create PeerConnection: {}", e.message))?;

        // Set up callback context with ICE candidate channel
        let (ice_tx, ice_rx) = mpsc::unbounded_channel();

        let ctx = Arc::new(CallbackContext {
            video_frame_count: AtomicU64::new(0),
            audio_frame_count: AtomicU64::new(0),
            ice_gathering_complete: AtomicBool::new(false),
            ice_candidate_tx: ice_tx,
            mkv_writer: ParkingMutex::new(None),
            mkv_writer_initialized: AtomicBool::new(false),
            video_width: AtomicU64::new(0),
            video_height: AtomicU64::new(0),
            audio_sample_rate: AtomicU64::new(0),
            audio_channels: AtomicU64::new(0),
            first_video_timestamp_us: AtomicI64::new(-1),
            audio_total_samples: AtomicU64::new(0),
            last_video_timestamp_ms: AtomicI64::new(-1),
            last_audio_timestamp_ms: AtomicI64::new(-1),
        });
        self.callback_context = Some(ctx.clone());
        self.ice_candidate_rx = Some(ice_rx);

        // Set up ICE callbacks
        self.setup_ice_callbacks(&pc, ctx.clone());

        // Add transceivers for receiving audio and video
        let video_init = RtpTransceiverInit {
            direction: RtpTransceiverDirection::RecvOnly,
            stream_ids: vec![],
            send_encodings: vec![],
        };
        let audio_init = RtpTransceiverInit {
            direction: RtpTransceiverDirection::RecvOnly,
            stream_ids: vec![],
            send_encodings: vec![],
        };

        pc.add_transceiver_for_media(MediaType::Video, video_init)
            .map_err(|e| anyhow!("Failed to add video transceiver: {}", e.message))?;
        pc.add_transceiver_for_media(MediaType::Audio, audio_init)
            .map_err(|e| anyhow!("Failed to add audio transceiver: {}", e.message))?;

        // Create offer
        info!("Creating offer...");
        let offer_options = OfferOptions {
            ice_restart: false,
            offer_to_receive_audio: true,
            offer_to_receive_video: true,
        };
        let offer = pc
            .create_offer(offer_options)
            .await
            .map_err(|e| anyhow!("Failed to create offer: {}", e.message))?;

        let offer_sdp = offer.to_string();
        debug!("Offer SDP:\n{}", offer_sdp);

        // Set local description
        pc.set_local_description(offer)
            .await
            .map_err(|e| anyhow!("Failed to set local description: {}", e.message))?;
        info!("Local description set");

        // Store peer connection
        self.peer_connection = Some(pc);

        // Send offer to WHEP endpoint
        info!("Sending offer to WHEP endpoint...");
        let response = self.send_offer(&offer_sdp).await?;
        let WhepResponse {
            sdp,
            resource_url,
            ice_servers,
        } = response;

        if let Some(ref pc) = self.peer_connection {
            if ice_servers.is_empty() {
                info!("No ICE servers provided by WHEP response");
            } else {
                let mut config = RtcConfiguration::default();
                config.ice_servers = ice_servers;
                pc.set_configuration(config)
                    .map_err(|e| anyhow!("Failed to apply ICE servers: {}", e.message))?;
                info!("Applied ICE servers from WHEP response");
            }
        }

        // Set remote description (answer)
        debug!("Answer SDP:\n{}", sdp);
        let answer = SessionDescription::parse(&sdp, SdpType::Answer)
            .map_err(|e| anyhow!("Failed to parse answer SDP: {:?}", e))?;

        if let Some(ref pc) = self.peer_connection {
            pc.set_remote_description(answer)
                .await
                .map_err(|e| anyhow!("Failed to set remote description: {}", e.message))?;
        }
        info!("Remote description set");

        self.resource_url = resource_url;

        if let Some(ref url) = self.resource_url {
            info!("Resource URL: {}", url);
        }

        Ok(())
    }

    fn setup_ice_callbacks(&self, pc: &PeerConnection, ctx: Arc<CallbackContext>) {
        // ICE connection state change callback
        pc.on_ice_connection_state_change(Some(Box::new(move |state| {
            let state_str = match state {
                IceConnectionState::New => "New",
                IceConnectionState::Checking => "Checking",
                IceConnectionState::Connected => "Connected",
                IceConnectionState::Completed => "Completed",
                IceConnectionState::Failed => "Failed",
                IceConnectionState::Disconnected => "Disconnected",
                IceConnectionState::Closed => "Closed",
                IceConnectionState::Max => "Max",
            };
            info!("ICE connection state changed: {}", state_str);
        })));

        // ICE candidate callback
        let ctx_candidate = ctx.clone();
        pc.on_ice_candidate(Some(Box::new(move |candidate| {
            let candidate_str = candidate.to_string();
            let sdp_mid = candidate.sdp_mid();
            let sdp_mline_index = candidate.sdp_mline_index();

            debug!(
                "ICE candidate gathered: mid={} index={} candidate={}",
                sdp_mid, sdp_mline_index, candidate_str
            );

            // Send to channel (ignore error if receiver dropped)
            let _ = ctx_candidate.ice_candidate_tx.send(candidate);
        })));

        // ICE gathering state change callback
        let ctx_gathering = ctx.clone();
        pc.on_ice_gathering_state_change(Some(Box::new(move |state| {
            let state_str = match state {
                IceGatheringState::New => "New",
                IceGatheringState::Gathering => "Gathering",
                IceGatheringState::Complete => "Complete",
            };
            info!("ICE gathering state changed: {}", state_str);

            if matches!(state, IceGatheringState::Complete) {
                ctx_gathering
                    .ice_gathering_complete
                    .store(true, Ordering::SeqCst);
            }
        })));

        // Track callback (just for logging)
        pc.on_track(Some(Box::new(move |event| {
            let track = &event.track;
            let track_id = track.id();

            match track {
                MediaStreamTrack::Video(_) => {
                    info!("Video track received: id={}", track_id);
                }
                MediaStreamTrack::Audio(_) => {
                    info!("Audio track received: id={}", track_id);
                }
            }
        })));
    }

    /// Wait for connection and frames
    pub async fn run(&mut self) -> Result<()> {
        info!("Waiting for connection...");

        // Wait for ICE connection to be established
        loop {
            // Process any pending ICE candidates (trickle ICE)
            self.process_ice_candidates().await;

            if let Some(ref pc) = self.peer_connection {
                let state = pc.ice_connection_state();
                match state {
                    IceConnectionState::Connected | IceConnectionState::Completed => {
                        info!("ICE connection established");
                        break;
                    }
                    IceConnectionState::Failed => {
                        return Err(anyhow!("ICE connection failed"));
                    }
                    IceConnectionState::Closed => {
                        return Err(anyhow!("Connection closed"));
                    }
                    _ => {
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                }
            } else {
                return Err(anyhow!("PeerConnection not initialized"));
            }
        }

        // Send remaining ICE candidates and end-of-candidates after connection established
        self.process_ice_candidates().await;
        self.check_and_send_end_of_candidates().await;

        info!("Connection established. Setting up streams...");

        // Set up NativeVideoStream and NativeAudioStream
        if let Some(ref pc) = self.peer_connection {
            let receivers = pc.receivers();
            for receiver in receivers {
                if let Some(track) = receiver.track() {
                    match track {
                        MediaStreamTrack::Video(video_track) => {
                            info!("Setting up video stream...");
                            self.video_stream = Some(NativeVideoStream::new(video_track));
                        }
                        MediaStreamTrack::Audio(audio_track) => {
                            info!("Setting up audio stream (48kHz, 2ch)...");
                            self.audio_stream =
                                Some(NativeAudioStream::new(audio_track, 48000, 2));
                        }
                    }
                }
            }
        }

        info!("Receiving frames...");

        // Process frames using Stream trait
        self.process_frames().await
    }

    /// Process video and audio frames using futures::stream
    async fn process_frames(&mut self) -> Result<()> {
        let ctx = self
            .callback_context
            .as_ref()
            .ok_or_else(|| anyhow!("CallbackContext not initialized"))?
            .clone();

        let mut video_stream = self.video_stream.take();
        let mut audio_stream = self.audio_stream.take();

        let mut last_debug_log = Instant::now();
        let mut last_video_count = 0u64;
        let mut last_audio_count = 0u64;
        // Track last video frame reception time for stall detection
        let mut last_video_frame_time: Option<Instant> = None;
        let video_stall_timeout = std::time::Duration::from_secs(5);

        loop {
            // Check if both streams have ended
            if video_stream.is_none() && audio_stream.is_none() {
                info!("Both video and audio streams have ended");
                break;
            }

            // Periodic checks (every 5 seconds)
            if last_debug_log.elapsed() >= std::time::Duration::from_secs(5) {
                // Check ICE connection state
                if let Some(ref pc) = self.peer_connection {
                    let state = pc.ice_connection_state();
                    if matches!(
                        state,
                        IceConnectionState::Failed | IceConnectionState::Closed
                    ) {
                        warn!("Connection lost (state={:?})", state);
                        break;
                    }
                }

                // Log frame statistics in debug mode
                if self.debug {
                    let video_count = ctx.video_frame_count.load(Ordering::Relaxed);
                    let audio_count = ctx.audio_frame_count.load(Ordering::Relaxed);
                    let last_video_ts = ctx.last_video_timestamp_ms.load(Ordering::Relaxed);
                    let last_audio_ts = ctx.last_audio_timestamp_ms.load(Ordering::Relaxed);
                    let av_diff = if last_video_ts >= 0 && last_audio_ts >= 0 {
                        last_video_ts - last_audio_ts
                    } else {
                        -1
                    };
                    let video_delta = video_count.saturating_sub(last_video_count);
                    let audio_delta = audio_count.saturating_sub(last_audio_count);

                    eprintln!(
                        "[DEBUG] Frame stats: video={} (+{}), audio={} (+{}), last_video_ts={}ms, last_audio_ts={}ms, av_diff={}ms",
                        video_count,
                        video_delta,
                        audio_count,
                        audio_delta,
                        last_video_ts,
                        last_audio_ts,
                        av_diff
                    );

                    last_video_count = video_count;
                    last_audio_count = audio_count;
                }

                last_debug_log = Instant::now();
            }

            // Use tokio::select! to process video and audio frames concurrently
            tokio::select! {
                // Process video frame
                video_frame = async {
                    if let Some(ref mut stream) = video_stream {
                        stream.next().await
                    } else {
                        futures::future::pending().await
                    }
                } => {
                    match video_frame {
                        Some(frame) => {
                            if let Err(e) = self.handle_video_frame(&ctx, frame) {
                                warn!("Video frame write error, stopping: {}", e);
                                break;
                            }
                            last_video_frame_time = Some(Instant::now());
                        }
                        None => {
                            // Stream ended - set to None to prevent CPU spin
                            info!("Video stream ended");
                            video_stream = None;
                        }
                    }
                }

                // Process audio frame
                audio_frame = async {
                    if let Some(ref mut stream) = audio_stream {
                        stream.next().await
                    } else {
                        futures::future::pending().await
                    }
                } => {
                    match audio_frame {
                        Some(frame) => {
                            if let Err(e) = self.handle_audio_frame(&ctx, frame) {
                                warn!("Audio frame write error, stopping: {}", e);
                                break;
                            }
                        }
                        None => {
                            // Stream ended - set to None to prevent CPU spin
                            info!("Audio stream ended");
                            audio_stream = None;
                        }
                    }
                }

                // Video stall detection: break if no video frame for video_stall_timeout
                _ = async {
                    if let Some(last_time) = last_video_frame_time {
                        let elapsed = last_time.elapsed();
                        if elapsed >= video_stall_timeout {
                            // Already stalled, resolve immediately
                            return;
                        }
                        tokio::time::sleep(video_stall_timeout - elapsed).await;
                    } else {
                        // No video frame received yet, don't trigger stall timeout
                        futures::future::pending::<()>().await;
                    }
                } => {
                    eprintln!(
                        "[WARN] Video frames stalled for over {}s, exiting",
                        video_stall_timeout.as_secs()
                    );
                    break;
                }

            }
        }

        // Restore streams for cleanup
        self.video_stream = video_stream;
        self.audio_stream = audio_stream;

        Ok(())
    }

    /// Handle a video frame
    fn handle_video_frame(&self, ctx: &Arc<CallbackContext>, frame: BoxVideoFrame) -> Result<()> {
        let count = ctx.video_frame_count.fetch_add(1, Ordering::Relaxed) + 1;
        let timestamp_us = frame.timestamp_us;

        // Get I420 buffer
        let i420 = frame.buffer.as_ref().to_i420();
        let width = i420.width();
        let height = i420.height();

        let (y_stride, u_stride, v_stride) = i420.strides();

        // Store metadata and initialize MKV writer on first video frame
        if count == 1 {
            ctx.video_width.store(width as u64, Ordering::Relaxed);
            ctx.video_height.store(height as u64, Ordering::Relaxed);
            ctx.first_video_timestamp_us
                .store(timestamp_us, Ordering::Relaxed);
            eprintln!(
                "[INFO] Video format detected: {}x{} (y_stride={}, u_stride={}, v_stride={})",
                width, height, y_stride, u_stride, v_stride
            );

            // Initialize MKV writer (video frame must arrive before audio can be written)
            let config = MkvConfig {
                video_width: width,
                video_height: height,
                audio_sample_rate: 48000,
                audio_channels: 2,
            };
            let mut guard = ctx.mkv_writer.lock();
            match MkvWriter::new(BufWriter::new(std::io::stdout()), config) {
                Ok(writer) => {
                    *guard = Some(writer);
                    ctx.mkv_writer_initialized.store(true, Ordering::SeqCst);
                    eprintln!("[INFO] MKV writer initialized, streaming to stdout");
                }
                Err(e) => {
                    eprintln!("[ERROR] Failed to initialize MKV writer: {}", e);
                }
            }
        }

        // Get Y, U, V data slices
        let (y_data, u_data, v_data) = i420.data();

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
            let row_end = row_start + w;
            if row_end <= y_data.len() {
                frame_data.extend_from_slice(&y_data[row_start..row_end]);
            }
        }

        // Copy U plane (half width, half height)
        for row in 0..uv_h {
            let row_start = row * u_stride_usize;
            let row_end = row_start + uv_w;
            if row_end <= u_data.len() {
                frame_data.extend_from_slice(&u_data[row_start..row_end]);
            }
        }

        // Copy V plane (half width, half height)
        for row in 0..uv_h {
            let row_start = row * v_stride_usize;
            let row_end = row_start + uv_w;
            if row_end <= v_data.len() {
                frame_data.extend_from_slice(&v_data[row_start..row_end]);
            }
        }

        // Calculate timestamp relative to first frame
        let first_ts = ctx.first_video_timestamp_us.load(Ordering::Relaxed);
        let timestamp_ms = (timestamp_us - first_ts) / 1000;
        ctx.last_video_timestamp_ms
            .store(timestamp_ms, Ordering::Relaxed);

        // Write to MKV
        {
            let mut guard = ctx.mkv_writer.lock();
            if let Some(ref mut writer) = *guard {
                if let Err(e) = writer.write_video_frame(&frame_data, timestamp_ms, true) {
                    eprintln!("[ERROR] Failed to write video frame: {}", e);
                    return Err(anyhow!("Failed to write video frame: {}", e));
                }
            }
        }

        // Log every 30 frames
        if count % 30 == 1 {
            eprintln!(
                "[TRACE] Video frame #{}: {}x{} ts={}ms",
                count, width, height, timestamp_ms
            );
        }

        Ok(())
    }

    /// Handle an audio frame
    fn handle_audio_frame(&self, ctx: &Arc<CallbackContext>, frame: AudioFrame<'static>) -> Result<()> {
        // Check if MKV writer is initialized (waits for first video frame)
        if !ctx.mkv_writer_initialized.load(Ordering::Relaxed) {
            return Ok(());
        }

        let sample_rate = frame.sample_rate as u32;
        let nb_channels = frame.num_channels as u32;
        let nb_frames = frame.samples_per_channel;
        let data = frame.data;

        let count = ctx.audio_frame_count.fetch_add(1, Ordering::Relaxed) + 1;

        // Store metadata on first audio frame (after MKV writer is initialized)
        if count == 1 {
            ctx.audio_sample_rate.store(sample_rate as u64, Ordering::Relaxed);
            ctx.audio_channels.store(nb_channels as u64, Ordering::Relaxed);
            eprintln!(
                "[INFO] Audio format detected: {}Hz {}ch ({} samples/channel)",
                sample_rate, nb_channels, nb_frames
            );
        }

        // Calculate audio timestamp based on accumulated samples
        let prev_samples = ctx
            .audio_total_samples
            .fetch_add(nb_frames as u64, Ordering::Relaxed);
        let timestamp_ms = (prev_samples as i64 * 1000) / sample_rate as i64;
        ctx.last_audio_timestamp_ms
            .store(timestamp_ms, Ordering::Relaxed);

        // Convert PCM data to bytes (S16LE)
        let byte_data: &[u8] =
            unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2) };

        // Write to MKV
        {
            let mut guard = ctx.mkv_writer.lock();
            if let Some(ref mut writer) = *guard {
                if let Err(e) = writer.write_audio_frame(byte_data, timestamp_ms) {
                    eprintln!("[ERROR] Failed to write audio frame: {}", e);
                    return Err(anyhow!("Failed to write audio frame: {}", e));
                }
            }
        }

        // Log every 100 frames
        if count % 100 == 1 {
            eprintln!(
                "[TRACE] Audio frame #{}: {}Hz {}ch {} samples ts={}ms",
                count, sample_rate, nb_channels, nb_frames, timestamp_ms
            );
        }

        Ok(())
    }

    /// Process pending ICE candidates and send them via PATCH (trickle ICE)
    async fn process_ice_candidates(&mut self) {
        let mut candidates = Vec::new();
        if let Some(ref mut rx) = self.ice_candidate_rx {
            while let Ok(candidate) = rx.try_recv() {
                candidates.push(candidate);
            }
        }

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
    async fn send_ice_candidate(
        &self,
        resource_url: &str,
        candidate: &IceCandidate,
    ) -> Result<()> {
        // Format as SDP fragment per RFC 8840 / draft-ietf-wish-whip
        let candidate_str = candidate.to_string();
        let sdp_mid = candidate.sdp_mid();
        let sdp_mline_index = candidate.sdp_mline_index();

        let sdp_fragment = format!("a=mid:{}\r\na={}\r\n", sdp_mid, candidate_str);

        debug!(
            "Sending ICE candidate to {}: mid={} index={}",
            resource_url, sdp_mid, sdp_mline_index
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

        let gathering_complete = self
            .callback_context
            .as_ref()
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
            warn!("Failed to send end-of-candidates (will not retry): {}", e);
        }
        self.end_of_candidates_sent = true;
    }

    /// Send end-of-candidates indication per RFC 8840
    async fn send_end_of_candidates(&self, resource_url: &str) -> Result<()> {
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

        let headers = response.headers();

        // Get resource URL from Location header
        let resource_url = headers
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(|location| {
                if location.starts_with("http://") || location.starts_with("https://") {
                    location.to_string()
                } else {
                    Url::parse(&self.whep_url)
                        .and_then(|base| base.join(location))
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| {
                            warn!("Failed to parse base URL, using fallback concatenation");
                            format!(
                                "{}/{}",
                                self.whep_url.trim_end_matches('/'),
                                location.trim_start_matches('/')
                            )
                        })
                }
            });

        let ice_servers = parse_ice_servers_from_headers(headers);
        let sdp = response.text().await?;

        Ok(WhepResponse {
            sdp,
            resource_url,
            ice_servers,
        })
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
                video_frames, video_width, video_height, audio_frames, audio_sample_rate, audio_channels
            );

            // Flush MKV writer
            let mut guard = ctx.mkv_writer.lock();
            if let Some(ref mut writer) = *guard {
                let _ = writer.flush();
            }
        }

        // Close streams (this removes sinks automatically via Drop)
        if let Some(mut stream) = self.video_stream.take() {
            stream.close();
        }
        if let Some(mut stream) = self.audio_stream.take() {
            stream.close();
        }

        // Close peer connection
        if let Some(ref pc) = self.peer_connection {
            pc.close();
            info!("PeerConnection closed");
        }

        // Send DELETE to resource URL if available
        if let Some(ref url) = self.resource_url {
            let _ = self.http_client.delete(url).send().await;
            info!("WHEP resource deleted");
        }

        Ok(())
    }
}

struct WhepResponse {
    sdp: String,
    resource_url: Option<String>,
    ice_servers: Vec<IceServer>,
}

fn parse_ice_servers_from_headers(headers: &HeaderMap) -> Vec<IceServer> {
    let mut servers = Vec::new();
    for value in headers.get_all("link") {
        let Ok(value_str) = value.to_str() else {
            continue;
        };
        for entry in split_link_header_value(value_str) {
            if let Some(server) = parse_ice_server_entry(&entry) {
                servers.push(server);
            }
        }
    }
    servers
}

fn split_link_header_value(value: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in value.chars() {
        match ch {
            '"' => {
                in_quotes = !in_quotes;
                current.push(ch);
            }
            ',' if !in_quotes => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    entries.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        entries.push(trimmed.to_string());
    }

    entries
}

fn parse_ice_server_entry(entry: &str) -> Option<IceServer> {
    let entry = entry.trim();
    let start = entry.find('<')?;
    let end = entry[start + 1..].find('>')? + start + 1;
    let url = entry[start + 1..end].trim();
    if url.is_empty() {
        return None;
    }

    let mut rel_is_ice = false;
    let mut username = String::new();
    let mut password = String::new();

    for param in entry[end + 1..].split(';') {
        let param = param.trim();
        if param.is_empty() {
            continue;
        }
        let mut parts = param.splitn(2, '=');
        let key = parts.next().unwrap_or("").trim();
        let value = parts.next().and_then(parse_param_value);

        match key {
            "rel" => {
                if let Some(rel) = value {
                    if rel.split_whitespace().any(|r| r == "ice-server") {
                        rel_is_ice = true;
                    }
                }
            }
            "username" => {
                if let Some(value) = value {
                    username = value;
                }
            }
            "credential" => {
                if let Some(value) = value {
                    password = value;
                }
            }
            _ => {}
        }
    }

    if !rel_is_ice {
        return None;
    }

    Some(IceServer {
        urls: vec![url.to_string()],
        username,
        password,
    })
}

fn parse_param_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let unquoted = if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    };
    if unquoted.is_empty() {
        None
    } else {
        Some(unquoted)
    }
}
