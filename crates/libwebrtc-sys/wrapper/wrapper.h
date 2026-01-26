// libwebrtc C-compatible wrapper
// This header provides a C interface to libwebrtc for use with Rust FFI

#ifndef LIBWEBRTC_WRAPPER_H
#define LIBWEBRTC_WRAPPER_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

// Opaque handles
typedef struct WebrtcPeerConnectionFactory WebrtcPeerConnectionFactory;
typedef struct WebrtcPeerConnection WebrtcPeerConnection;
typedef struct WebrtcVideoTrack WebrtcVideoTrack;
typedef struct WebrtcAudioTrack WebrtcAudioTrack;

// Callback types
typedef void (*VideoFrameCallback)(
    void* user_data,
    int32_t width,
    int32_t height,
    int64_t timestamp_us,
    const uint8_t* y_data, int32_t y_stride,
    const uint8_t* u_data, int32_t u_stride,
    const uint8_t* v_data, int32_t v_stride
);

typedef void (*AudioFrameCallback)(
    void* user_data,
    int32_t sample_rate,
    size_t num_channels,
    size_t samples_per_channel,
    const int16_t* data
);

typedef void (*OnTrackCallback)(
    void* user_data,
    const char* track_id,
    int is_video  // 1 = video, 0 = audio
);

typedef void (*OnIceConnectionStateChangeCallback)(
    void* user_data,
    int state  // PeerConnectionInterface::IceConnectionState
);

typedef void (*OnIceCandidateCallback)(
    void* user_data,
    const char* candidate,  // SDP formatted ICE candidate
    const char* sdp_mid,
    int sdp_mline_index
);

typedef void (*OnIceGatheringStateChangeCallback)(
    void* user_data,
    int state  // PeerConnectionInterface::IceGatheringState
);

// Factory functions
WebrtcPeerConnectionFactory* webrtc_factory_create(void);
void webrtc_factory_destroy(WebrtcPeerConnectionFactory* factory);

// PeerConnection functions
WebrtcPeerConnection* webrtc_pc_create(
    WebrtcPeerConnectionFactory* factory,
    const char* ice_servers_json  // JSON array of ICE servers, e.g. "[]"
);
void webrtc_pc_destroy(WebrtcPeerConnection* pc);

// SDP functions
char* webrtc_pc_create_offer(WebrtcPeerConnection* pc);
char* webrtc_pc_create_answer(WebrtcPeerConnection* pc);
int webrtc_pc_set_local_description(WebrtcPeerConnection* pc, const char* sdp, const char* type);
int webrtc_pc_set_remote_description(WebrtcPeerConnection* pc, const char* sdp, const char* type);
char* webrtc_pc_get_local_description(WebrtcPeerConnection* pc);
char* webrtc_pc_get_remote_description(WebrtcPeerConnection* pc);

// ICE functions
int webrtc_pc_add_ice_candidate(WebrtcPeerConnection* pc, const char* candidate, const char* sdp_mid, int sdp_mline_index);

// Callback registration
void webrtc_pc_set_on_track_callback(
    WebrtcPeerConnection* pc,
    OnTrackCallback callback,
    void* user_data
);

void webrtc_pc_set_on_ice_connection_state_change_callback(
    WebrtcPeerConnection* pc,
    OnIceConnectionStateChangeCallback callback,
    void* user_data
);

void webrtc_pc_set_on_ice_candidate_callback(
    WebrtcPeerConnection* pc,
    OnIceCandidateCallback callback,
    void* user_data
);

void webrtc_pc_set_on_ice_gathering_state_change_callback(
    WebrtcPeerConnection* pc,
    OnIceGatheringStateChangeCallback callback,
    void* user_data
);

// Video track functions
void webrtc_video_track_set_frame_callback(
    WebrtcVideoTrack* track,
    VideoFrameCallback callback,
    void* user_data
);

// Audio track functions (deprecated - use pc callbacks instead)
void webrtc_audio_track_set_frame_callback(
    WebrtcAudioTrack* track,
    AudioFrameCallback callback,
    void* user_data
);

// Frame callback registration on PeerConnection
// These callbacks are invoked for all received tracks
// Must be set BEFORE creating offer/answer to receive frames
void webrtc_pc_set_video_frame_callback(
    WebrtcPeerConnection* pc,
    VideoFrameCallback callback,
    void* user_data
);

void webrtc_pc_set_audio_frame_callback(
    WebrtcPeerConnection* pc,
    AudioFrameCallback callback,
    void* user_data
);

// Utility functions
void webrtc_free_string(char* str);

// State query functions
int webrtc_pc_signaling_state(WebrtcPeerConnection* pc);
int webrtc_pc_ice_connection_state(WebrtcPeerConnection* pc);
int webrtc_pc_ice_gathering_state(WebrtcPeerConnection* pc);

#ifdef __cplusplus
}
#endif

#endif // LIBWEBRTC_WRAPPER_H
