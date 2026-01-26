//! Low-level FFI bindings for libwebrtc
//!
//! This crate provides unsafe C-compatible bindings to libwebrtc.
//! For a safe Rust API, use the `libwebrtc-rs` crate instead.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]

use std::os::raw::{c_char, c_int, c_void};

// Opaque handles
#[repr(C)]
pub struct WebrtcPeerConnectionFactory {
    _private: [u8; 0],
}

#[repr(C)]
pub struct WebrtcPeerConnection {
    _private: [u8; 0],
}

#[repr(C)]
pub struct WebrtcVideoTrack {
    _private: [u8; 0],
}

#[repr(C)]
pub struct WebrtcAudioTrack {
    _private: [u8; 0],
}

// Callback types
pub type VideoFrameCallback = Option<
    unsafe extern "C" fn(
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
    ),
>;

pub type AudioFrameCallback = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        sample_rate: i32,
        num_channels: usize,
        samples_per_channel: usize,
        data: *const i16,
    ),
>;

pub type OnTrackCallback = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        track_id: *const c_char,
        is_video: c_int, // 1 = video, 0 = audio
    ),
>;

pub type OnIceConnectionStateChangeCallback = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        state: c_int, // PeerConnectionInterface::IceConnectionState
    ),
>;

pub type OnIceCandidateCallback = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        candidate: *const c_char, // SDP formatted ICE candidate
        sdp_mid: *const c_char,
        sdp_mline_index: c_int,
    ),
>;

pub type OnIceGatheringStateChangeCallback = Option<
    unsafe extern "C" fn(
        user_data: *mut c_void,
        state: c_int, // PeerConnectionInterface::IceGatheringState
    ),
>;

extern "C" {
    // Factory functions
    pub fn webrtc_factory_create() -> *mut WebrtcPeerConnectionFactory;
    pub fn webrtc_factory_destroy(factory: *mut WebrtcPeerConnectionFactory);

    // PeerConnection functions
    pub fn webrtc_pc_create(
        factory: *mut WebrtcPeerConnectionFactory,
        ice_servers_json: *const c_char,
    ) -> *mut WebrtcPeerConnection;
    pub fn webrtc_pc_destroy(pc: *mut WebrtcPeerConnection);

    // SDP functions
    pub fn webrtc_pc_create_offer(pc: *mut WebrtcPeerConnection) -> *mut c_char;
    pub fn webrtc_pc_create_answer(pc: *mut WebrtcPeerConnection) -> *mut c_char;
    pub fn webrtc_pc_set_local_description(
        pc: *mut WebrtcPeerConnection,
        sdp: *const c_char,
        sdp_type: *const c_char,
    ) -> c_int;
    pub fn webrtc_pc_set_remote_description(
        pc: *mut WebrtcPeerConnection,
        sdp: *const c_char,
        sdp_type: *const c_char,
    ) -> c_int;
    pub fn webrtc_pc_get_local_description(pc: *mut WebrtcPeerConnection) -> *mut c_char;
    pub fn webrtc_pc_get_remote_description(pc: *mut WebrtcPeerConnection) -> *mut c_char;

    // ICE functions
    pub fn webrtc_pc_add_ice_candidate(
        pc: *mut WebrtcPeerConnection,
        candidate: *const c_char,
        sdp_mid: *const c_char,
        sdp_mline_index: c_int,
    ) -> c_int;

    // Callback registration
    pub fn webrtc_pc_set_on_track_callback(
        pc: *mut WebrtcPeerConnection,
        callback: OnTrackCallback,
        user_data: *mut c_void,
    );
    pub fn webrtc_pc_set_on_ice_connection_state_change_callback(
        pc: *mut WebrtcPeerConnection,
        callback: OnIceConnectionStateChangeCallback,
        user_data: *mut c_void,
    );
    pub fn webrtc_pc_set_on_ice_candidate_callback(
        pc: *mut WebrtcPeerConnection,
        callback: OnIceCandidateCallback,
        user_data: *mut c_void,
    );
    pub fn webrtc_pc_set_on_ice_gathering_state_change_callback(
        pc: *mut WebrtcPeerConnection,
        callback: OnIceGatheringStateChangeCallback,
        user_data: *mut c_void,
    );

    // Video track functions
    pub fn webrtc_video_track_set_frame_callback(
        track: *mut WebrtcVideoTrack,
        callback: VideoFrameCallback,
        user_data: *mut c_void,
    );

    // Audio track functions (deprecated)
    pub fn webrtc_audio_track_set_frame_callback(
        track: *mut WebrtcAudioTrack,
        callback: AudioFrameCallback,
        user_data: *mut c_void,
    );

    // Frame callback registration on PeerConnection
    pub fn webrtc_pc_set_video_frame_callback(
        pc: *mut WebrtcPeerConnection,
        callback: VideoFrameCallback,
        user_data: *mut c_void,
    );
    pub fn webrtc_pc_set_audio_frame_callback(
        pc: *mut WebrtcPeerConnection,
        callback: AudioFrameCallback,
        user_data: *mut c_void,
    );

    // Utility functions
    pub fn webrtc_free_string(str: *mut c_char);

    // State query functions
    pub fn webrtc_pc_signaling_state(pc: *mut WebrtcPeerConnection) -> c_int;
    pub fn webrtc_pc_ice_connection_state(pc: *mut WebrtcPeerConnection) -> c_int;
    pub fn webrtc_pc_ice_gathering_state(pc: *mut WebrtcPeerConnection) -> c_int;
}

/// ICE connection state values
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceConnectionState {
    New = 0,
    Checking = 1,
    Connected = 2,
    Completed = 3,
    Failed = 4,
    Disconnected = 5,
    Closed = 6,
}

impl TryFrom<c_int> for IceConnectionState {
    type Error = c_int;

    fn try_from(value: c_int) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(IceConnectionState::New),
            1 => Ok(IceConnectionState::Checking),
            2 => Ok(IceConnectionState::Connected),
            3 => Ok(IceConnectionState::Completed),
            4 => Ok(IceConnectionState::Failed),
            5 => Ok(IceConnectionState::Disconnected),
            6 => Ok(IceConnectionState::Closed),
            _ => Err(value),
        }
    }
}

/// Signaling state values
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalingState {
    Stable = 0,
    HaveLocalOffer = 1,
    HaveLocalPrAnswer = 2,
    HaveRemoteOffer = 3,
    HaveRemotePrAnswer = 4,
    Closed = 5,
}

impl TryFrom<c_int> for SignalingState {
    type Error = c_int;

    fn try_from(value: c_int) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(SignalingState::Stable),
            1 => Ok(SignalingState::HaveLocalOffer),
            2 => Ok(SignalingState::HaveLocalPrAnswer),
            3 => Ok(SignalingState::HaveRemoteOffer),
            4 => Ok(SignalingState::HaveRemotePrAnswer),
            5 => Ok(SignalingState::Closed),
            _ => Err(value),
        }
    }
}

/// ICE gathering state values
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IceGatheringState {
    New = 0,
    Gathering = 1,
    Complete = 2,
}

impl TryFrom<c_int> for IceGatheringState {
    type Error = c_int;

    fn try_from(value: c_int) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(IceGatheringState::New),
            1 => Ok(IceGatheringState::Gathering),
            2 => Ok(IceGatheringState::Complete),
            _ => Err(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn test_factory_create_destroy() {
        unsafe {
            let factory = webrtc_factory_create();
            assert!(!factory.is_null(), "Factory creation should succeed");
            webrtc_factory_destroy(factory);
        }
    }

    #[test]
    fn test_peer_connection_create_destroy() {
        unsafe {
            let factory = webrtc_factory_create();
            assert!(!factory.is_null());

            let ice_servers = CString::new("[]").unwrap();
            let pc = webrtc_pc_create(factory, ice_servers.as_ptr());
            assert!(!pc.is_null(), "PeerConnection creation should succeed");

            webrtc_pc_destroy(pc);
            webrtc_factory_destroy(factory);
        }
    }

    #[test]
    fn test_create_offer() {
        unsafe {
            let factory = webrtc_factory_create();
            assert!(!factory.is_null());

            let ice_servers = CString::new("[]").unwrap();
            let pc = webrtc_pc_create(factory, ice_servers.as_ptr());
            assert!(!pc.is_null());

            let offer = webrtc_pc_create_offer(pc);
            assert!(!offer.is_null(), "Create offer should succeed");

            // Check the SDP contains expected content
            let offer_str = std::ffi::CStr::from_ptr(offer).to_str().unwrap();
            assert!(offer_str.contains("v=0"), "SDP should contain v=0");
            assert!(
                offer_str.contains("a=group:BUNDLE"),
                "SDP should contain BUNDLE"
            );

            webrtc_free_string(offer);
            webrtc_pc_destroy(pc);
            webrtc_factory_destroy(factory);
        }
    }

    #[test]
    fn test_signaling_state() {
        unsafe {
            let factory = webrtc_factory_create();
            let ice_servers = CString::new("[]").unwrap();
            let pc = webrtc_pc_create(factory, ice_servers.as_ptr());

            let state = webrtc_pc_signaling_state(pc);
            assert_eq!(
                state, 0,
                "Initial signaling state should be Stable (0)"
            );

            webrtc_pc_destroy(pc);
            webrtc_factory_destroy(factory);
        }
    }

    #[test]
    fn test_ice_connection_state() {
        unsafe {
            let factory = webrtc_factory_create();
            let ice_servers = CString::new("[]").unwrap();
            let pc = webrtc_pc_create(factory, ice_servers.as_ptr());

            let state = webrtc_pc_ice_connection_state(pc);
            assert_eq!(state, 0, "Initial ICE connection state should be New (0)");

            webrtc_pc_destroy(pc);
            webrtc_factory_destroy(factory);
        }
    }

    #[test]
    fn test_ice_gathering_state() {
        unsafe {
            let factory = webrtc_factory_create();
            let ice_servers = CString::new("[]").unwrap();
            let pc = webrtc_pc_create(factory, ice_servers.as_ptr());

            let state = webrtc_pc_ice_gathering_state(pc);
            assert_eq!(state, 0, "Initial ICE gathering state should be New (0)");

            webrtc_pc_destroy(pc);
            webrtc_factory_destroy(factory);
        }
    }

    #[test]
    fn test_set_local_description() {
        unsafe {
            let factory = webrtc_factory_create();
            let ice_servers = CString::new("[]").unwrap();
            let pc = webrtc_pc_create(factory, ice_servers.as_ptr());

            // Create an offer first
            let offer = webrtc_pc_create_offer(pc);
            assert!(!offer.is_null());

            // Set it as local description
            let offer_type = CString::new("offer").unwrap();
            let result = webrtc_pc_set_local_description(pc, offer, offer_type.as_ptr());
            assert_eq!(result, 0, "Setting local description should succeed");

            // Verify we can get the local description back
            let local_desc = webrtc_pc_get_local_description(pc);
            assert!(!local_desc.is_null());
            webrtc_free_string(local_desc);

            webrtc_free_string(offer);
            webrtc_pc_destroy(pc);
            webrtc_factory_destroy(factory);
        }
    }
}
