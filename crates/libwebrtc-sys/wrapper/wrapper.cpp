// libwebrtc C-compatible wrapper implementation
//
// NOTE: libcxx_abi.h is force-included via -include flag in build.rs
// This sets _LIBCPP_ABI_NAMESPACE to __Cr before any libc++ headers
// are processed, ensuring ABI compatibility with libwebrtc.a

#include "wrapper.h"

#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <condition_variable>

#include "api/audio_codecs/builtin_audio_decoder_factory.h"
#include "api/audio_codecs/builtin_audio_encoder_factory.h"
#include "api/create_peerconnection_factory.h"
#include "api/peer_connection_interface.h"
#include "api/video_codecs/builtin_video_decoder_factory.h"
#include "api/video_codecs/builtin_video_encoder_factory.h"
#include "api/video/i420_buffer.h"
#include "api/video/video_frame.h"
#include "api/media_stream_interface.h"
#include "api/scoped_refptr.h"
#include "api/ref_count.h"
#include "api/make_ref_counted.h"
#include "rtc_base/thread.h"

namespace {

// Helper to duplicate a string for C API
char* strdup_wrapper(const std::string& s) {
    char* result = static_cast<char*>(malloc(s.size() + 1));
    if (result) {
        memcpy(result, s.c_str(), s.size() + 1);
    }
    return result;
}

// Global threads for libwebrtc
std::unique_ptr<webrtc::Thread> g_signaling_thread;
std::unique_ptr<webrtc::Thread> g_worker_thread;
std::unique_ptr<webrtc::Thread> g_network_thread;
std::mutex g_threads_mutex;

void EnsureThreads() {
    std::lock_guard<std::mutex> lock(g_threads_mutex);
    if (!g_signaling_thread) {
        g_network_thread = webrtc::Thread::CreateWithSocketServer();
        g_network_thread->SetName("network_thread", nullptr);
        g_network_thread->Start();

        g_worker_thread = webrtc::Thread::Create();
        g_worker_thread->SetName("worker_thread", nullptr);
        g_worker_thread->Start();

        g_signaling_thread = webrtc::Thread::Create();
        g_signaling_thread->SetName("signaling_thread", nullptr);
        g_signaling_thread->Start();
    }
}

}  // namespace

// Opaque struct definitions
struct WebrtcPeerConnectionFactory {
    webrtc::scoped_refptr<webrtc::PeerConnectionFactoryInterface> factory;
};

struct WebrtcPeerConnection;

// Observer implementation
class PeerConnectionObserverImpl : public webrtc::PeerConnectionObserver {
public:
    PeerConnectionObserverImpl(WebrtcPeerConnection* pc) : pc_(pc) {}

    void OnSignalingChange(webrtc::PeerConnectionInterface::SignalingState new_state) override;
    void OnDataChannel(webrtc::scoped_refptr<webrtc::DataChannelInterface> data_channel) override {}
    void OnRenegotiationNeeded() override {}
    void OnIceConnectionChange(webrtc::PeerConnectionInterface::IceConnectionState new_state) override;
    void OnIceGatheringChange(webrtc::PeerConnectionInterface::IceGatheringState new_state) override {}
    void OnIceCandidate(const webrtc::IceCandidateInterface* candidate) override {}
    void OnTrack(webrtc::scoped_refptr<webrtc::RtpTransceiverInterface> transceiver) override;

private:
    WebrtcPeerConnection* pc_;
};

struct WebrtcPeerConnection {
    webrtc::scoped_refptr<webrtc::PeerConnectionInterface> pc;
    std::unique_ptr<PeerConnectionObserverImpl> observer;

    OnTrackCallback on_track_callback = nullptr;
    void* on_track_user_data = nullptr;

    OnIceConnectionStateChangeCallback on_ice_state_callback = nullptr;
    void* on_ice_state_user_data = nullptr;
};

// Observer implementations
void PeerConnectionObserverImpl::OnSignalingChange(
    webrtc::PeerConnectionInterface::SignalingState new_state) {
    // Could add callback here if needed
}

void PeerConnectionObserverImpl::OnIceConnectionChange(
    webrtc::PeerConnectionInterface::IceConnectionState new_state) {
    if (pc_->on_ice_state_callback) {
        pc_->on_ice_state_callback(pc_->on_ice_state_user_data, static_cast<int>(new_state));
    }
}

void PeerConnectionObserverImpl::OnTrack(
    webrtc::scoped_refptr<webrtc::RtpTransceiverInterface> transceiver) {
    if (pc_->on_track_callback && transceiver->receiver()) {
        auto track = transceiver->receiver()->track();
        if (track) {
            std::string track_id = track->id();
            int is_video = (track->kind() == webrtc::MediaStreamTrackInterface::kVideoKind) ? 1 : 0;
            pc_->on_track_callback(pc_->on_track_user_data, track_id.c_str(), is_video);
        }
    }
}

// Create/Destroy Set Description Observer
class SetDescriptionObserver : public webrtc::SetSessionDescriptionObserver {
public:
    SetDescriptionObserver() : done_(false), success_(false) {}

    void OnSuccess() override {
        std::lock_guard<std::mutex> lock(mutex_);
        success_ = true;
        done_ = true;
        cv_.notify_all();
    }

    void OnFailure(webrtc::RTCError error) override {
        std::lock_guard<std::mutex> lock(mutex_);
        success_ = false;
        error_message_ = error.message();
        done_ = true;
        cv_.notify_all();
    }

    bool WaitForResult() {
        std::unique_lock<std::mutex> lock(mutex_);
        cv_.wait(lock, [this] { return done_; });
        return success_;
    }

private:
    std::mutex mutex_;
    std::condition_variable cv_;
    bool done_;
    bool success_;
    std::string error_message_;
};

// Create SDP Observer
class CreateDescriptionObserver : public webrtc::CreateSessionDescriptionObserver {
public:
    CreateDescriptionObserver() : done_(false), success_(false) {}

    void OnSuccess(webrtc::SessionDescriptionInterface* desc) override {
        std::lock_guard<std::mutex> lock(mutex_);
        desc->ToString(&sdp_);
        type_ = webrtc::SdpTypeToString(desc->GetType());
        success_ = true;
        done_ = true;
        cv_.notify_all();
    }

    void OnFailure(webrtc::RTCError error) override {
        std::lock_guard<std::mutex> lock(mutex_);
        success_ = false;
        error_message_ = error.message();
        done_ = true;
        cv_.notify_all();
    }

    bool WaitForResult() {
        std::unique_lock<std::mutex> lock(mutex_);
        cv_.wait(lock, [this] { return done_; });
        return success_;
    }

    const std::string& GetSdp() const { return sdp_; }
    const std::string& GetType() const { return type_; }

private:
    std::mutex mutex_;
    std::condition_variable cv_;
    bool done_;
    bool success_;
    std::string sdp_;
    std::string type_;
    std::string error_message_;
};

// C API Implementation

extern "C" {

WebrtcPeerConnectionFactory* webrtc_factory_create(void) {
    EnsureThreads();

    auto factory = webrtc::CreatePeerConnectionFactory(
        g_network_thread.get(),
        g_worker_thread.get(),
        g_signaling_thread.get(),
        nullptr,  // default_adm
        webrtc::CreateBuiltinAudioEncoderFactory(),
        webrtc::CreateBuiltinAudioDecoderFactory(),
        webrtc::CreateBuiltinVideoEncoderFactory(),
        webrtc::CreateBuiltinVideoDecoderFactory(),
        nullptr,  // audio_mixer
        nullptr   // audio_processing
    );

    if (!factory) {
        return nullptr;
    }

    auto* wrapper = new WebrtcPeerConnectionFactory();
    wrapper->factory = factory;
    return wrapper;
}

void webrtc_factory_destroy(WebrtcPeerConnectionFactory* factory) {
    if (factory) {
        factory->factory = nullptr;
        delete factory;
    }
}

WebrtcPeerConnection* webrtc_pc_create(
    WebrtcPeerConnectionFactory* factory,
    const char* ice_servers_json) {

    if (!factory || !factory->factory) {
        return nullptr;
    }

    webrtc::PeerConnectionInterface::RTCConfiguration config;
    config.sdp_semantics = webrtc::SdpSemantics::kUnifiedPlan;

    // TODO: Parse ice_servers_json if needed
    // For now, we use an empty configuration

    auto* wrapper = new WebrtcPeerConnection();
    wrapper->observer = std::make_unique<PeerConnectionObserverImpl>(wrapper);

    webrtc::PeerConnectionDependencies deps(wrapper->observer.get());

    auto result = factory->factory->CreatePeerConnectionOrError(config, std::move(deps));
    if (!result.ok()) {
        delete wrapper;
        return nullptr;
    }

    wrapper->pc = result.MoveValue();
    return wrapper;
}

void webrtc_pc_destroy(WebrtcPeerConnection* pc) {
    if (pc) {
        if (pc->pc) {
            pc->pc->Close();
            pc->pc = nullptr;
        }
        delete pc;
    }
}

char* webrtc_pc_create_offer(WebrtcPeerConnection* pc) {
    if (!pc || !pc->pc) {
        return nullptr;
    }

    webrtc::PeerConnectionInterface::RTCOfferAnswerOptions options;
    options.offer_to_receive_audio = true;
    options.offer_to_receive_video = true;

    auto observer = webrtc::make_ref_counted<CreateDescriptionObserver>();
    pc->pc->CreateOffer(observer.get(), options);

    if (!observer->WaitForResult()) {
        return nullptr;
    }

    return strdup_wrapper(observer->GetSdp());
}

char* webrtc_pc_create_answer(WebrtcPeerConnection* pc) {
    if (!pc || !pc->pc) {
        return nullptr;
    }

    webrtc::PeerConnectionInterface::RTCOfferAnswerOptions options;

    auto observer = webrtc::make_ref_counted<CreateDescriptionObserver>();
    pc->pc->CreateAnswer(observer.get(), options);

    if (!observer->WaitForResult()) {
        return nullptr;
    }

    return strdup_wrapper(observer->GetSdp());
}

int webrtc_pc_set_local_description(WebrtcPeerConnection* pc, const char* sdp, const char* type) {
    if (!pc || !pc->pc || !sdp || !type) {
        return -1;
    }

    webrtc::SdpType sdp_type;
    if (strcmp(type, "offer") == 0) {
        sdp_type = webrtc::SdpType::kOffer;
    } else if (strcmp(type, "answer") == 0) {
        sdp_type = webrtc::SdpType::kAnswer;
    } else if (strcmp(type, "pranswer") == 0) {
        sdp_type = webrtc::SdpType::kPrAnswer;
    } else if (strcmp(type, "rollback") == 0) {
        sdp_type = webrtc::SdpType::kRollback;
    } else {
        return -1;
    }

    webrtc::SdpParseError error;
    auto desc = webrtc::CreateSessionDescription(sdp_type, sdp, &error);
    if (!desc) {
        return -1;
    }

    auto observer = webrtc::make_ref_counted<SetDescriptionObserver>();
    pc->pc->SetLocalDescription(observer.get(), desc.release());

    return observer->WaitForResult() ? 0 : -1;
}

int webrtc_pc_set_remote_description(WebrtcPeerConnection* pc, const char* sdp, const char* type) {
    if (!pc || !pc->pc || !sdp || !type) {
        return -1;
    }

    webrtc::SdpType sdp_type;
    if (strcmp(type, "offer") == 0) {
        sdp_type = webrtc::SdpType::kOffer;
    } else if (strcmp(type, "answer") == 0) {
        sdp_type = webrtc::SdpType::kAnswer;
    } else if (strcmp(type, "pranswer") == 0) {
        sdp_type = webrtc::SdpType::kPrAnswer;
    } else if (strcmp(type, "rollback") == 0) {
        sdp_type = webrtc::SdpType::kRollback;
    } else {
        return -1;
    }

    webrtc::SdpParseError error;
    auto desc = webrtc::CreateSessionDescription(sdp_type, sdp, &error);
    if (!desc) {
        return -1;
    }

    auto observer = webrtc::make_ref_counted<SetDescriptionObserver>();
    pc->pc->SetRemoteDescription(observer.get(), desc.release());

    return observer->WaitForResult() ? 0 : -1;
}

char* webrtc_pc_get_local_description(WebrtcPeerConnection* pc) {
    if (!pc || !pc->pc) {
        return nullptr;
    }

    auto desc = pc->pc->local_description();
    if (!desc) {
        return nullptr;
    }

    std::string sdp;
    desc->ToString(&sdp);
    return strdup_wrapper(sdp);
}

char* webrtc_pc_get_remote_description(WebrtcPeerConnection* pc) {
    if (!pc || !pc->pc) {
        return nullptr;
    }

    auto desc = pc->pc->remote_description();
    if (!desc) {
        return nullptr;
    }

    std::string sdp;
    desc->ToString(&sdp);
    return strdup_wrapper(sdp);
}

int webrtc_pc_add_ice_candidate(WebrtcPeerConnection* pc, const char* candidate, const char* sdp_mid, int sdp_mline_index) {
    if (!pc || !pc->pc || !candidate) {
        return -1;
    }

    webrtc::SdpParseError error;
    std::unique_ptr<webrtc::IceCandidateInterface> ice_candidate(
        webrtc::CreateIceCandidate(sdp_mid ? sdp_mid : "", sdp_mline_index, candidate, &error));

    if (!ice_candidate) {
        return -1;
    }

    if (!pc->pc->AddIceCandidate(ice_candidate.get())) {
        return -1;
    }

    return 0;
}

void webrtc_pc_set_on_track_callback(
    WebrtcPeerConnection* pc,
    OnTrackCallback callback,
    void* user_data) {
    if (pc) {
        pc->on_track_callback = callback;
        pc->on_track_user_data = user_data;
    }
}

void webrtc_pc_set_on_ice_connection_state_change_callback(
    WebrtcPeerConnection* pc,
    OnIceConnectionStateChangeCallback callback,
    void* user_data) {
    if (pc) {
        pc->on_ice_state_callback = callback;
        pc->on_ice_state_user_data = user_data;
    }
}

void webrtc_video_track_set_frame_callback(
    WebrtcVideoTrack* track,
    VideoFrameCallback callback,
    void* user_data) {
    // TODO: Implement video frame sink
}

void webrtc_audio_track_set_frame_callback(
    WebrtcAudioTrack* track,
    AudioFrameCallback callback,
    void* user_data) {
    // TODO: Implement audio frame sink
}

void webrtc_free_string(char* str) {
    free(str);
}

int webrtc_pc_signaling_state(WebrtcPeerConnection* pc) {
    if (!pc || !pc->pc) {
        return -1;
    }
    return static_cast<int>(pc->pc->signaling_state());
}

int webrtc_pc_ice_connection_state(WebrtcPeerConnection* pc) {
    if (!pc || !pc->pc) {
        return -1;
    }
    return static_cast<int>(pc->pc->ice_connection_state());
}

int webrtc_pc_ice_gathering_state(WebrtcPeerConnection* pc) {
    if (!pc || !pc->pc) {
        return -1;
    }
    return static_cast<int>(pc->pc->ice_gathering_state());
}

}  // extern "C"
