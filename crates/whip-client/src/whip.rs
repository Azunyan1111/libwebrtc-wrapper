//! WHIP (WebRTC-HTTP Ingestion Protocol) client implementation
//!
//! stdinからMKVストリームを読み込み、VP8/Opusでエンコードして送信する。

use crate::mkv_reader::{MkvFrame, MkvReader};
use anyhow::{anyhow, Result};
use libwebrtc::audio_frame::AudioFrame;
use libwebrtc::audio_source::native::NativeAudioSource;
use libwebrtc::audio_source::AudioSourceOptions;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::prelude::*;
use cxx::SharedPtr;
use webrtc_sys::rtp_parameters as sys_rp;
use webrtc_sys::rtp_sender as sys_rs;
use libwebrtc::video_frame::{I420Buffer, VideoFrame, VideoRotation};
use libwebrtc::video_source::native::NativeVideoSource;
use libwebrtc::video_source::VideoResolution;
use reqwest::header::HeaderMap;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use url::Url;

const AUDIO_QUEUE_SIZE_MS: u32 = 100;
const LATE_DROP_THRESHOLD_MS: i64 = 150;

/// WHIP Client for sending WebRTC streams
pub struct WhipClient {
    whip_url: String,
    http_client: reqwest::Client,
    factory: Option<PeerConnectionFactory>,
    peer_connection: Option<PeerConnection>,
    resource_url: Option<String>,
    callback_context: Option<Arc<CallbackContext>>,
    ice_candidate_rx: Option<mpsc::UnboundedReceiver<IceCandidate>>,
    end_of_candidates_sent: bool,
    debug: bool,
    // Video/Audio sources for sending
    video_source: Option<NativeVideoSource>,
    audio_source: Option<NativeAudioSource>,
    // Video dimensions
    video_width: u32,
    video_height: u32,
    audio_sample_rate: u32,
}

/// Context for frame counting, statistics, and ICE management.
pub struct CallbackContext {
    pub video_frame_count: AtomicU64,
    pub audio_frame_count: AtomicU64,
    pub ice_gathering_complete: AtomicBool,
    pub ice_candidate_tx: mpsc::UnboundedSender<IceCandidate>,
    // Timestamp tracking
    pub pacing_pts_start_ms: OnceLock<i64>,
    pub pacing_wall_start: OnceLock<Instant>,
    pub first_video_timestamp_ms: AtomicI64,
    pub first_audio_timestamp_ms: AtomicI64,
    pub first_audio_mkv_timestamp_ms: AtomicI64,
    pub audio_total_samples: AtomicU64,
    pub last_video_timestamp_ms: AtomicI64,
    pub last_audio_timestamp_ms: AtomicI64,
    pub last_audio_mkv_timestamp_ms: AtomicI64,
    pub last_video_drift_ms: AtomicI64,
    pub last_audio_drift_ms: AtomicI64,
    pub last_audio_capture_wait_ms: AtomicI64,
    pub audio_capture_wait_total_ms: AtomicU64,
    pub audio_capture_wait_count: AtomicU64,
    pub audio_capture_wait_max_ms: AtomicI64,
    pub video_queue_backlog: AtomicI64,
    pub audio_queue_backlog: AtomicI64,
    pub video_queue_backlog_max: AtomicI64,
    pub audio_queue_backlog_max: AtomicI64,
}

impl WhipClient {
    /// Create a new WHIP client
    pub fn new(whip_url: &str, debug: bool) -> Result<Self> {
        let http_client = reqwest::Client::builder().build()?;

        Ok(Self {
            whip_url: whip_url.to_string(),
            http_client,
            factory: None,
            peer_connection: None,
            resource_url: None,
            callback_context: None,
            ice_candidate_rx: None,
            end_of_candidates_sent: false,
            debug,
            video_source: None,
            audio_source: None,
            video_width: 0,
            video_height: 0,
            audio_sample_rate: 0,
        })
    }

    /// Connect to the WHIP endpoint
    pub async fn connect(
        &mut self,
        video_width: u32,
        video_height: u32,
        audio_sample_rate: u32,
        audio_channels: u32,
    ) -> Result<()> {
        info!("Creating PeerConnectionFactory...");

        self.video_width = video_width;
        self.video_height = video_height;
        self.audio_sample_rate = audio_sample_rate;

        // Create factory
        let factory = PeerConnectionFactory::default();
        self.factory = Some(factory.clone());

        info!("Creating PeerConnection...");

        // Create peer connection with default config (ICE servers applied after WHIP response)
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
            pacing_pts_start_ms: OnceLock::new(),
            pacing_wall_start: OnceLock::new(),
            first_video_timestamp_ms: AtomicI64::new(-1),
            first_audio_timestamp_ms: AtomicI64::new(-1),
            first_audio_mkv_timestamp_ms: AtomicI64::new(-1),
            audio_total_samples: AtomicU64::new(0),
            last_video_timestamp_ms: AtomicI64::new(-1),
            last_audio_timestamp_ms: AtomicI64::new(-1),
            last_audio_mkv_timestamp_ms: AtomicI64::new(-1),
            last_video_drift_ms: AtomicI64::new(-1),
            last_audio_drift_ms: AtomicI64::new(-1),
            last_audio_capture_wait_ms: AtomicI64::new(-1),
            audio_capture_wait_total_ms: AtomicU64::new(0),
            audio_capture_wait_count: AtomicU64::new(0),
            audio_capture_wait_max_ms: AtomicI64::new(-1),
            video_queue_backlog: AtomicI64::new(0),
            audio_queue_backlog: AtomicI64::new(0),
            video_queue_backlog_max: AtomicI64::new(0),
            audio_queue_backlog_max: AtomicI64::new(0),
        });
        self.callback_context = Some(ctx.clone());
        self.ice_candidate_rx = Some(ice_rx);

        // Set up ICE callbacks
        self.setup_ice_callbacks(&pc, ctx.clone());

        // Create video source and track
        info!(
            "Creating video source ({}x{})...",
            video_width, video_height
        );
        let video_resolution = VideoResolution {
            width: video_width,
            height: video_height,
        };
        let video_source = NativeVideoSource::new(video_resolution);
        let video_track = factory.create_video_track("video0", video_source.clone());
        self.video_source = Some(video_source);

        // Create audio source and track
        info!(
            "Creating audio source ({}Hz {}ch)...",
            audio_sample_rate, audio_channels
        );
        // queue_size_ms: 0 = no buffering, requires 10ms frames
        // Non-zero enables buffering (in 10ms units)
        let queue_size_ms = AUDIO_QUEUE_SIZE_MS; // low-latency: disable internal buffering
        let audio_source = NativeAudioSource::new(
            AudioSourceOptions::default(),
            audio_sample_rate,
            audio_channels,
            queue_size_ms,
        );
        let audio_track = factory.create_audio_track("audio0", audio_source.clone());
        self.audio_source = Some(audio_source);

        // Add transceivers for sending audio and video (SendOnly)
        let video_init = RtpTransceiverInit {
            direction: RtpTransceiverDirection::SendOnly,
            stream_ids: vec!["stream0".to_string()],
            send_encodings: vec![],
        };
        let audio_init = RtpTransceiverInit {
            direction: RtpTransceiverDirection::SendOnly,
            stream_ids: vec!["stream0".to_string()],
            send_encodings: vec![],
        };

        pc.add_transceiver(MediaStreamTrack::Video(video_track), video_init)
            .map_err(|e| anyhow!("Failed to add video transceiver: {}", e.message))?;
        pc.add_transceiver(MediaStreamTrack::Audio(audio_track), audio_init)
            .map_err(|e| anyhow!("Failed to add audio transceiver: {}", e.message))?;

        // Create offer
        info!("Creating offer...");
        let offer_options = OfferOptions {
            ice_restart: false,
            offer_to_receive_audio: false,
            offer_to_receive_video: false,
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

        // Send offer to WHIP endpoint
        info!("Sending offer to WHIP endpoint...");
        let response = self.send_offer(&offer_sdp).await?;
        let WhipResponse {
            sdp,
            resource_url,
            ice_servers,
        } = response;

        if let Some(ref pc) = self.peer_connection {
            if ice_servers.is_empty() {
                info!("No ICE servers provided by WHIP response");
            } else {
                let mut config = RtcConfiguration::default();
                config.ice_servers = ice_servers;
                pc.set_configuration(config)
                    .map_err(|e| anyhow!("Failed to apply ICE servers: {}", e.message))?;
                info!("Applied ICE servers from WHIP response");
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

        // Set DegradationPreference::MaintainResolution to prevent resolution scaling
        self.set_maintain_resolution()?;

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
    }

    /// Wait for connection and send frames from MKV reader
    pub async fn run<R: Read>(&mut self, reader: &mut MkvReader<R>) -> Result<()> {
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

        info!("Connection established. Sending frames...");

        // Send frames from MKV reader
        self.send_frames(reader).await
    }

    /// Send frames from MKV reader to WebRTC
    ///
    /// ビデオとオーディオを別タスクで並列処理する。
    /// メインループはMKV読取+チャネルディスパッチ+ICE処理のみ行い、
    /// ビデオのペーシングsleepがオーディオをブロックしないようにする。
    async fn send_frames<R: Read>(&mut self, reader: &mut MkvReader<R>) -> Result<()> {
        let ctx = self
            .callback_context
            .as_ref()
            .ok_or_else(|| anyhow!("CallbackContext not initialized"))?
            .clone();

        // Clone sources to avoid borrow issues
        let video_source = self
            .video_source
            .clone()
            .ok_or_else(|| anyhow!("VideoSource not initialized"))?;

        let audio_source = self
            .audio_source
            .clone()
            .ok_or_else(|| anyhow!("AudioSource not initialized"))?;

        let video_width = self.video_width;
        let video_height = self.video_height;
        let audio_sample_rate = self.audio_sample_rate;
        let debug = self.debug;

        // unbounded channel: stdinからのMKV読取は自然にペーシングされるため安全
        let (video_tx, mut video_rx) = mpsc::unbounded_channel::<MkvFrame>();
        let (audio_tx, mut audio_rx) = mpsc::unbounded_channel::<MkvFrame>();

        // ビデオタスク: ペーシング + フレーム投入
        let video_ctx = ctx.clone();
        let video_task = tokio::spawn(async move {
            let mut video_log_count: u64 = 0;
            while let Some(frame) = video_rx.recv().await {
                video_ctx
                    .video_queue_backlog
                    .fetch_sub(1, Ordering::Relaxed);
                if let MkvFrame::Video {
                    timestamp_ms,
                    is_keyframe,
                    ..
                } = &frame
                {
                    let (wall_start, pts_start_ms) =
                        ensure_pacing_base(&video_ctx, *timestamp_ms);
                    let elapsed_ms = wall_start.elapsed().as_millis() as i64;
                    let target_ms = *timestamp_ms - pts_start_ms;
                    let wait_ms = target_ms - elapsed_ms;
                    let drift_ms = elapsed_ms - target_ms;
                    video_ctx
                        .last_video_drift_ms
                        .store(drift_ms, Ordering::Relaxed);
                    if wait_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms as u64)).await;
                    }

                    let payload = frame.payload();
                    feed_video_frame(
                        &video_ctx,
                        &video_source,
                        payload,
                        *timestamp_ms,
                        *is_keyframe,
                        video_width,
                        video_height,
                    );
                    video_log_count = video_log_count.wrapping_add(1);
                    if debug && video_log_count % 30 == 1 {
                        let first_ts = video_ctx.first_video_timestamp_ms.load(Ordering::Relaxed);
                        let write_ts_ms = if first_ts >= 0 {
                            *timestamp_ms - first_ts
                        } else {
                            0
                        };
                        eprintln!(
                            "[DEBUG] Video ts: read_ms={}, write_ms={}, first_read_ms={}",
                            *timestamp_ms, write_ts_ms, first_ts
                        );
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        });

        // オーディオタスク: 10msフレーム化 + MKV PTSでペーシング
        let audio_ctx = ctx.clone();
        let audio_task = tokio::spawn(async move {
            struct AudioSegment {
                pts_ms: i64,
                samples: Vec<i16>,
                offset: usize,
            }

            let audio_sample_rate_u32 = audio_sample_rate;
            let audio_channels_u32 = audio_source.num_channels();
            if audio_sample_rate_u32 == 0 || audio_channels_u32 == 0 {
                return Err(anyhow!("Invalid audio configuration"));
            }

            let samples_per_10ms = (audio_sample_rate_u32 / 100) as usize;
            let num_channels = audio_channels_u32 as usize;
            let chunk_samples = samples_per_10ms * num_channels;

            let mut segments: VecDeque<AudioSegment> = VecDeque::new();
            let mut pending_samples = 0usize;
            let mut audio_log_count: u64 = 0;

            while let Some(frame) = audio_rx.recv().await {
                audio_ctx
                    .audio_queue_backlog
                    .fetch_sub(1, Ordering::Relaxed);
                if let MkvFrame::Audio { timestamp_ms, .. } = &frame {
                    let mkv_timestamp_ms = *timestamp_ms;
                    let payload = frame.payload();
                    let mut samples = Vec::with_capacity(payload.len() / 2);
                    for chunk in payload.chunks_exact(2) {
                        samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
                    }
                    if samples.is_empty() {
                        continue;
                    }

                    pending_samples += samples.len();
                    segments.push_back(AudioSegment {
                        pts_ms: mkv_timestamp_ms,
                        samples,
                        offset: 0,
                    });

                    while pending_samples >= chunk_samples {
                        let mut chunk = Vec::with_capacity(chunk_samples);
                        let mut chunk_pts_ms: Option<i64> = None;
                        let mut chunk_pts_offset = 0usize;

                        while chunk.len() < chunk_samples {
                            let Some(front) = segments.front_mut() else {
                                break;
                            };

                            if chunk_pts_ms.is_none() {
                                chunk_pts_ms = Some(front.pts_ms);
                                chunk_pts_offset = front.offset;
                            }

                            let available = front.samples.len().saturating_sub(front.offset);
                            let remaining = chunk_samples - chunk.len();
                            let take = remaining.min(available);

                            if take == 0 {
                                break;
                            }

                            let end = front.offset + take;
                            chunk.extend_from_slice(&front.samples[front.offset..end]);
                            front.offset = end;
                            pending_samples = pending_samples.saturating_sub(take);

                            if front.offset >= front.samples.len() {
                                segments.pop_front();
                            }
                        }

                        if chunk.len() < chunk_samples {
                            break;
                        }

                        let Some(base_pts_ms) = chunk_pts_ms else {
                            break;
                        };
                        let offset_per_channel = chunk_pts_offset / num_channels;
                        let chunk_pts_ms = base_pts_ms
                            + ((offset_per_channel as u64 * 1000 / audio_sample_rate_u32 as u64)
                                as i64);

                        if audio_ctx
                            .first_audio_mkv_timestamp_ms
                            .load(Ordering::Relaxed)
                            < 0
                        {
                            audio_ctx
                                .first_audio_mkv_timestamp_ms
                                .store(chunk_pts_ms, Ordering::Relaxed);
                        }
                        audio_ctx
                            .last_audio_mkv_timestamp_ms
                            .store(chunk_pts_ms, Ordering::Relaxed);

                        let (wall_start, pts_start_ms) =
                            ensure_pacing_base(&audio_ctx, chunk_pts_ms);
                        let elapsed_ms = wall_start.elapsed().as_millis() as i64;
                        let target_rel_ms = chunk_pts_ms - pts_start_ms;
                        let wait_ms = target_rel_ms - elapsed_ms;
                        let drift_ms = elapsed_ms - target_rel_ms;
                        audio_ctx
                            .last_audio_drift_ms
                            .store(drift_ms, Ordering::Relaxed);

                        if drift_ms > LATE_DROP_THRESHOLD_MS {
                            continue;
                        }

                        if wait_ms > 0 {
                            tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms as u64))
                                .await;
                        }

                        feed_audio_frame(
                            &audio_ctx,
                            &audio_source,
                            chunk,
                            samples_per_10ms as u32,
                            chunk_pts_ms,
                            chunk_pts_ms,
                        )
                        .await?;
                        audio_log_count = audio_log_count.wrapping_add(1);
                        if debug && audio_log_count % 100 == 1 {
                            eprintln!(
                                "[DEBUG] Audio ts: read_ms={}, write_ms={}",
                                chunk_pts_ms, chunk_pts_ms
                            );
                        }
                    }
                } else {
                    continue;
                }
            }
            Ok::<(), anyhow::Error>(())
        });

        // メインループ: MKV読取 + チャネルディスパッチ + ICE処理 + デバッグログ
        let mut last_debug_log = Instant::now();
        let mut last_video_count = 0u64;
        let mut last_audio_count = 0u64;

        let dispatch_result: Result<()> = loop {
            // Check connection state
            if let Some(ref pc) = self.peer_connection {
                let state = pc.ice_connection_state();
                if matches!(
                    state,
                    IceConnectionState::Failed | IceConnectionState::Closed
                ) {
                    warn!("Connection lost (state={:?})", state);
                    break Ok(());
                }
            }

            // Read next frame from MKV
            match reader.read_frame() {
                Ok(Some(frame)) => {
                    match &frame {
                        MkvFrame::Video { .. } => {
                            if video_tx.send(frame).is_err() {
                                break Err(anyhow!("Video task terminated unexpectedly"));
                            }
                            let new_backlog =
                                ctx.video_queue_backlog.fetch_add(1, Ordering::Relaxed) + 1;
                            let mut current_max =
                                ctx.video_queue_backlog_max.load(Ordering::Relaxed);
                            while new_backlog > current_max {
                                match ctx.video_queue_backlog_max.compare_exchange(
                                    current_max,
                                    new_backlog,
                                    Ordering::Relaxed,
                                    Ordering::Relaxed,
                                ) {
                                    Ok(_) => break,
                                    Err(actual) => current_max = actual,
                                }
                            }
                        }
                        MkvFrame::Audio { .. } => {
                            if audio_tx.send(frame).is_err() {
                                break Err(anyhow!("Audio task terminated unexpectedly"));
                            }
                            let new_backlog =
                                ctx.audio_queue_backlog.fetch_add(1, Ordering::Relaxed) + 1;
                            let mut current_max =
                                ctx.audio_queue_backlog_max.load(Ordering::Relaxed);
                            while new_backlog > current_max {
                                match ctx.audio_queue_backlog_max.compare_exchange(
                                    current_max,
                                    new_backlog,
                                    Ordering::Relaxed,
                                    Ordering::Relaxed,
                                ) {
                                    Ok(_) => break,
                                    Err(actual) => current_max = actual,
                                }
                            }
                        }
                    }
                }
                Ok(None) => {
                    // EOF reached
                    info!("End of MKV stream");
                    break Ok(());
                }
                Err(e) => {
                    break Err(e);
                }
            }

            // Process ICE candidates periodically
            self.process_ice_candidates().await;
            self.check_and_send_end_of_candidates().await;

            // yield to allow video/audio tasks to progress
            tokio::task::yield_now().await;

            // Log frame statistics in debug mode
            if debug && last_debug_log.elapsed() >= std::time::Duration::from_secs(5) {
                let video_count = ctx.video_frame_count.load(Ordering::Relaxed);
                let audio_count = ctx.audio_frame_count.load(Ordering::Relaxed);
                let last_video_ts = ctx.last_video_timestamp_ms.load(Ordering::Relaxed);
                let last_audio_ts = ctx.last_audio_timestamp_ms.load(Ordering::Relaxed);
                let last_audio_mkv_ts =
                    ctx.last_audio_mkv_timestamp_ms.load(Ordering::Relaxed);
                let first_audio_mkv_ts =
                    ctx.first_audio_mkv_timestamp_ms.load(Ordering::Relaxed);
                let first_audio_ts = ctx.first_audio_timestamp_ms.load(Ordering::Relaxed);
                let audio_total_samples = ctx.audio_total_samples.load(Ordering::Relaxed);
                let last_video_drift_ms = ctx.last_video_drift_ms.load(Ordering::Relaxed);
                let last_audio_drift_ms = ctx.last_audio_drift_ms.load(Ordering::Relaxed);
                let last_audio_capture_wait_ms =
                    ctx.last_audio_capture_wait_ms.load(Ordering::Relaxed);
                let audio_capture_wait_total_ms =
                    ctx.audio_capture_wait_total_ms.load(Ordering::Relaxed);
                let audio_capture_wait_count = ctx.audio_capture_wait_count.load(Ordering::Relaxed);
                let audio_capture_wait_max_ms =
                    ctx.audio_capture_wait_max_ms.load(Ordering::Relaxed);
                let video_queue_backlog = ctx.video_queue_backlog.load(Ordering::Relaxed);
                let audio_queue_backlog = ctx.audio_queue_backlog.load(Ordering::Relaxed);
                let video_queue_backlog_max =
                    ctx.video_queue_backlog_max.load(Ordering::Relaxed);
                let audio_queue_backlog_max =
                    ctx.audio_queue_backlog_max.load(Ordering::Relaxed);
                let audio_capture_wait_avg_ms = if audio_capture_wait_count > 0 {
                    (audio_capture_wait_total_ms / audio_capture_wait_count) as i64
                } else {
                    -1
                };
                let audio_clock_ms = if last_audio_ts >= 0 && first_audio_ts >= 0 {
                    last_audio_ts - first_audio_ts
                } else {
                    -1
                };
                let mkv_audio_elapsed_ms =
                    if last_audio_mkv_ts >= 0 && first_audio_mkv_ts >= 0 {
                        last_audio_mkv_ts - first_audio_mkv_ts
                } else {
                    -1
                };
                let audio_mkv_diff_ms = if audio_clock_ms >= 0 && mkv_audio_elapsed_ms >= 0 {
                    mkv_audio_elapsed_ms - audio_clock_ms
                } else {
                    -1
                };
                let av_diff = if last_video_ts >= 0 && last_audio_ts >= 0 {
                    last_video_ts - last_audio_ts
                } else {
                    -1
                };
                let video_delta = video_count.saturating_sub(last_video_count);
                let audio_delta = audio_count.saturating_sub(last_audio_count);
                let interval_secs = last_debug_log.elapsed().as_secs_f64();
                let video_fps = if interval_secs > 0.0 {
                    video_delta as f64 / interval_secs
                } else {
                    0.0
                };

                eprintln!(
                    "[DEBUG] Frame stats: video={} (+{}), audio={} (+{}), interval={:.2}s, video_fps={:.2}, last_video_ts={}ms, last_audio_ts={}ms, last_audio_mkv_ts={}ms, av_diff={}ms, audio_total_samples={}, audio_clock_ms={}, mkv_audio_elapsed_ms={}, audio_mkv_diff_ms={}, last_video_drift_ms={}, last_audio_drift_ms={}, last_audio_capture_wait_ms={}, audio_capture_wait_avg_ms={}, audio_capture_wait_max_ms={}, video_queue_backlog={}, audio_queue_backlog={}, video_queue_backlog_max={}, audio_queue_backlog_max={}",
                    video_count,
                    video_delta,
                    audio_count,
                    audio_delta,
                    interval_secs,
                    video_fps,
                    last_video_ts,
                    last_audio_ts,
                    last_audio_mkv_ts,
                    av_diff,
                    audio_total_samples,
                    audio_clock_ms,
                    mkv_audio_elapsed_ms,
                    audio_mkv_diff_ms,
                    last_video_drift_ms,
                    last_audio_drift_ms,
                    last_audio_capture_wait_ms,
                    audio_capture_wait_avg_ms,
                    audio_capture_wait_max_ms,
                    video_queue_backlog,
                    audio_queue_backlog,
                    video_queue_backlog_max,
                    audio_queue_backlog_max
                );

                last_video_count = video_count;
                last_audio_count = audio_count;
                last_debug_log = Instant::now();
            }
        };

        // シャットダウン: チャネルdropでタスクにEOF通知
        drop(video_tx);
        drop(audio_tx);
        let video_result = video_task.await.map_err(|e| anyhow!("Video task panicked: {}", e))?;
        let audio_result = audio_task.await.map_err(|e| anyhow!("Audio task panicked: {}", e))?;
        dispatch_result?;
        video_result?;
        audio_result?;
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

    /// Send a single ICE candidate to the WHIP resource URL via PATCH
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

    /// Send SDP offer to WHIP endpoint and receive answer
    async fn send_offer(&self, sdp: &str) -> Result<WhipResponse> {
        let response = self
            .http_client
            .post(&self.whip_url)
            .header("Content-Type", "application/sdp")
            .header("Accept", "application/sdp")
            .body(sdp.to_string())
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!(
                "WHIP request failed with status {}: {}",
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
                    Url::parse(&self.whip_url)
                        .and_then(|base| base.join(location))
                        .map(|u| u.to_string())
                        .unwrap_or_else(|_| {
                            warn!("Failed to parse base URL, using fallback concatenation");
                            format!(
                                "{}/{}",
                                self.whip_url.trim_end_matches('/'),
                                location.trim_start_matches('/')
                            )
                        })
                }
            });

        let ice_servers = parse_ice_servers_from_headers(headers);
        let sdp = response.text().await?;

        Ok(WhipResponse {
            sdp,
            resource_url,
            ice_servers,
        })
    }

    /// Set DegradationPreference to MaintainResolution to prevent resolution scaling
    fn set_maintain_resolution(&self) -> Result<()> {
        let pc = self
            .peer_connection
            .as_ref()
            .ok_or_else(|| anyhow!("PeerConnection not initialized"))?;

        for sender in pc.senders() {
            // Get current parameters via webrtc-sys
            let sys_sender = extract_sys_sender(&sender);
            let mut params = sys_sender.get_parameters();

            // Set DegradationPreference to MaintainResolution
            params.has_degradation_preference = true;
            params.degradation_preference = sys_rp::ffi::DegradationPreference::MaintainResolution;

            // Apply parameters
            sys_sender
                .set_parameters(params)
                .map_err(|e| anyhow!("Failed to set parameters: {:?}", e))?;

            info!("Set DegradationPreference::MaintainResolution for sender");
        }

        Ok(())
    }

    /// Close the connection
    pub async fn close(&mut self) -> Result<()> {
        // Log final statistics
        if let Some(ref ctx) = self.callback_context {
            let video_frames = ctx.video_frame_count.load(Ordering::Relaxed);
            let audio_frames = ctx.audio_frame_count.load(Ordering::Relaxed);
            let audio_samples = ctx.audio_total_samples.load(Ordering::Relaxed);

            eprintln!(
                "[INFO] Send complete: {} video frames ({}x{}), {} audio frames ({} samples)",
                video_frames, self.video_width, self.video_height, audio_frames, audio_samples
            );
        }

        // Close peer connection
        if let Some(ref pc) = self.peer_connection {
            pc.close();
            info!("PeerConnection closed");
        }

        // Send DELETE to resource URL if available
        if let Some(ref url) = self.resource_url {
            let _ = self.http_client.delete(url).send().await;
            info!("WHIP resource deleted");
        }

        Ok(())
    }
}

struct WhipResponse {
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

fn ensure_pacing_base(ctx: &Arc<CallbackContext>, pts_ms: i64) -> (Instant, i64) {
    let pts_start_ms = *ctx.pacing_pts_start_ms.get_or_init(|| pts_ms);
    let wall_start = *ctx.pacing_wall_start.get_or_init(Instant::now);
    (wall_start, pts_start_ms)
}

/// Feed a video frame to the video source
fn feed_video_frame(
    ctx: &Arc<CallbackContext>,
    source: &NativeVideoSource,
    data: &[u8],
    timestamp_ms: i64,
    is_keyframe: bool,
    video_width: u32,
    video_height: u32,
) {
    let count = ctx.video_frame_count.fetch_add(1, Ordering::Relaxed) + 1;

    // Store first timestamp for reference
    if count == 1 {
        ctx.first_video_timestamp_ms
            .store(timestamp_ms, Ordering::Relaxed);
        eprintln!(
            "[INFO] First video frame: {}x{} ts={}ms keyframe={}",
            video_width, video_height, timestamp_ms, is_keyframe
        );
    }

    ctx.last_video_timestamp_ms
        .store(timestamp_ms, Ordering::Relaxed);

    // Create I420 buffer and copy data
    let width = video_width;
    let height = video_height;

    let mut i420 = I420Buffer::new(width, height);

    // Copy I420 data to buffer
    let y_size = (width * height) as usize;
    let uv_w = (width + 1) / 2;
    let uv_h = (height + 1) / 2;
    let uv_size = (uv_w * uv_h) as usize;

    if data.len() >= y_size + uv_size * 2 {
        let (y_data, u_data, v_data) = i420.data_mut();

        // Copy Y plane
        let y_copy_len = y_data.len().min(y_size);
        y_data[..y_copy_len].copy_from_slice(&data[..y_copy_len]);

        // Copy U plane
        let u_copy_len = u_data.len().min(uv_size);
        u_data[..u_copy_len].copy_from_slice(&data[y_size..y_size + u_copy_len]);

        // Copy V plane
        let v_copy_len = v_data.len().min(uv_size);
        v_data[..v_copy_len].copy_from_slice(&data[y_size + uv_size..y_size + uv_size + v_copy_len]);
    }

    // Calculate timestamp in microseconds relative to first frame
    let first_ts = ctx.first_video_timestamp_ms.load(Ordering::Relaxed);
    let relative_ts_ms = timestamp_ms - first_ts;
    let timestamp_us = relative_ts_ms * 1000;

    // Create and capture frame
    let frame = VideoFrame {
        rotation: VideoRotation::VideoRotation0,
        timestamp_us,
        buffer: i420,
    };
    source.capture_frame(&frame);

    // Log every 30 frames
    if count % 30 == 1 {
        eprintln!(
            "[TRACE] Video frame #{}: {}x{} ts={}ms keyframe={}",
            count, width, height, timestamp_ms, is_keyframe
        );
    }
}

/// Feed an audio frame to the audio source
async fn feed_audio_frame(
    ctx: &Arc<CallbackContext>,
    source: &NativeAudioSource,
    samples: Vec<i16>,
    samples_per_channel: u32,
    audio_clock_ms: i64,
    mkv_timestamp_ms: i64,
) -> Result<()> {
    let count = ctx.audio_frame_count.fetch_add(1, Ordering::Relaxed) + 1;

    if count == 1 {
        ctx.first_audio_timestamp_ms
            .store(audio_clock_ms, Ordering::Relaxed);
        if ctx.first_audio_mkv_timestamp_ms.load(Ordering::Relaxed) < 0 {
            ctx.first_audio_mkv_timestamp_ms
                .store(mkv_timestamp_ms, Ordering::Relaxed);
        }
        eprintln!(
            "[INFO] First audio frame: ts={}ms mkv_ts={}ms",
            audio_clock_ms,
            mkv_timestamp_ms
        );
    }

    ctx.last_audio_timestamp_ms
        .store(audio_clock_ms, Ordering::Relaxed);
    ctx.last_audio_mkv_timestamp_ms
        .store(mkv_timestamp_ms, Ordering::Relaxed);
    if ctx.first_audio_mkv_timestamp_ms.load(Ordering::Relaxed) < 0 {
        ctx.first_audio_mkv_timestamp_ms
            .store(mkv_timestamp_ms, Ordering::Relaxed);
    }

    let sample_rate = source.sample_rate();
    let num_channels = source.num_channels();

    // Track total samples for statistics
    ctx.audio_total_samples
        .fetch_add(samples_per_channel as u64, Ordering::Relaxed);

    // Create and capture audio frame
    let frame = AudioFrame {
        data: Cow::Owned(samples),
        sample_rate,
        num_channels,
        samples_per_channel,
        callback_time_ms: 0,
        absolute_capture_timestamp_ms: None,
    };

    let capture_start = Instant::now();
    source
        .capture_frame(&frame)
        .await
        .map_err(|e| anyhow!("Failed to capture audio frame: {}", e.message))?;
    let capture_wait_ms = capture_start.elapsed().as_millis() as i64;
    ctx.last_audio_capture_wait_ms
        .store(capture_wait_ms, Ordering::Relaxed);
    ctx.audio_capture_wait_total_ms.fetch_add(
        capture_wait_ms.max(0) as u64,
        Ordering::Relaxed,
    );
    ctx.audio_capture_wait_count
        .fetch_add(1, Ordering::Relaxed);
    let mut current_max = ctx.audio_capture_wait_max_ms.load(Ordering::Relaxed);
    while capture_wait_ms > current_max {
        match ctx.audio_capture_wait_max_ms.compare_exchange(
            current_max,
            capture_wait_ms,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => current_max = actual,
        }
    }

    // Log every 100 frames
    if count % 100 == 1 {
        eprintln!(
            "[TRACE] Audio frame #{}: {}Hz {}ch {} samples ts={}ms mkv_ts={}ms",
            count, sample_rate, num_channels, samples_per_channel, audio_clock_ms, mkv_timestamp_ms
        );
    }

    Ok(())
}

/// Extract webrtc-sys RtpSender from libwebrtc RtpSender
///
/// libwebrtc::RtpSender has internal structure:
///   RtpSender { handle: native::RtpSender { sys_handle: SharedPtr<sys_rs::ffi::RtpSender> } }
///
/// Since sys_handle is pub(crate), we use transmute to access it.
fn extract_sys_sender(sender: &libwebrtc::rtp_sender::RtpSender) -> &sys_rs::ffi::RtpSender {
    // libwebrtc::RtpSender layout:
    //   struct RtpSender { handle: imp_rs::RtpSender }
    //   struct imp_rs::RtpSender { sys_handle: SharedPtr<sys_rs::ffi::RtpSender> }
    //
    // So RtpSender is effectively just SharedPtr<sys_rs::ffi::RtpSender>
    #[repr(C)]
    struct RtpSenderInner {
        sys_handle: SharedPtr<sys_rs::ffi::RtpSender>,
    }

    let inner: &RtpSenderInner = unsafe { std::mem::transmute(sender) };
    inner.sys_handle.as_ref().expect("sys_handle is null")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_link_header_value() {
        let value = r#"<stun:stun.l.google.com:19302>; rel="ice-server""#;
        let entries = split_link_header_value(value);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_split_link_header_value_multiple() {
        let value = r#"<stun:stun1>; rel="ice-server", <turn:turn1>; rel="ice-server"; username="user"; credential="pass""#;
        let entries = split_link_header_value(value);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_parse_ice_server_entry_stun() {
        let entry = r#"<stun:stun.l.google.com:19302>; rel="ice-server""#;
        let server = parse_ice_server_entry(entry);
        assert!(server.is_some());
        let server = server.unwrap();
        assert_eq!(server.urls, vec!["stun:stun.l.google.com:19302"]);
    }

    #[test]
    fn test_parse_ice_server_entry_turn() {
        let entry = r#"<turn:turn.example.com:3478>; rel="ice-server"; username="user"; credential="pass""#;
        let server = parse_ice_server_entry(entry);
        assert!(server.is_some());
        let server = server.unwrap();
        assert_eq!(server.urls, vec!["turn:turn.example.com:3478"]);
        assert_eq!(server.username, "user");
        assert_eq!(server.password, "pass");
    }

    #[test]
    fn test_parse_ice_server_entry_not_ice() {
        let entry = r#"<http://example.com>; rel="something-else""#;
        let server = parse_ice_server_entry(entry);
        assert!(server.is_none());
    }

    #[test]
    fn test_parse_param_value_quoted() {
        assert_eq!(parse_param_value(r#""value""#), Some("value".to_string()));
    }

    #[test]
    fn test_parse_param_value_unquoted() {
        assert_eq!(parse_param_value("value"), Some("value".to_string()));
    }

    #[test]
    fn test_parse_param_value_empty() {
        assert_eq!(parse_param_value(""), None);
        assert_eq!(parse_param_value(r#""""#), None);
    }
}
