// libwebrtc C-compatible wrapper implementation
//
// NOTE: libcxx_abi.h is force-included via -include flag in build.rs
// This sets _LIBCPP_ABI_NAMESPACE to __Cr before any libc++ headers
// are processed, ensuring ABI compatibility with libwebrtc.a

#include "wrapper.h"

#include <atomic>
#include <chrono>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>
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
#include "api/audio/audio_device.h"
#include "rtc_base/thread.h"
#include "api/video/video_sink_interface.h"

namespace {

// Helper to duplicate a string for C API
char* strdup_wrapper(const std::string& s) {
    char* result = static_cast<char*>(malloc(s.size() + 1));
    if (result) {
        memcpy(result, s.c_str(), s.size() + 1);
    }
    return result;
}

// Audio callback type for delivering playout audio data
typedef void (*PlayoutAudioCallback)(
    void* user_data,
    int sample_rate,
    size_t num_channels,
    size_t samples_per_channel,
    const int16_t* data
);

// Dummy AudioDeviceModule that simulates audio playout without actual speakers.
// This enables the audio pipeline to deliver decoded audio to AudioTrackSinkInterface.
// A background thread periodically calls NeedMorePlayData to pull audio from the pipeline.
class DummyAudioDeviceModule : public webrtc::AudioDeviceModule {
public:
    static webrtc::scoped_refptr<DummyAudioDeviceModule> Create() {
        return webrtc::scoped_refptr<DummyAudioDeviceModule>(
            new DummyAudioDeviceModule());
    }

    // Set callback to receive playout audio data
    void SetPlayoutCallback(PlayoutAudioCallback callback, void* user_data) {
        std::lock_guard<std::mutex> lock(callback_mutex_);
        playout_callback_ = callback;
        playout_callback_user_data_ = user_data;
    }

    // RefCountInterface implementation
    void AddRef() const override {
        ref_count_.fetch_add(1, std::memory_order_relaxed);
    }

    webrtc::RefCountReleaseStatus Release() const override {
        int count = ref_count_.fetch_sub(1, std::memory_order_acq_rel) - 1;
        if (count == 0) {
            delete this;
            return webrtc::RefCountReleaseStatus::kDroppedLastRef;
        }
        return webrtc::RefCountReleaseStatus::kOtherRefsRemained;
    }

    // AudioDeviceModule implementation
    int32_t ActiveAudioLayer(AudioLayer* audio_layer) const override {
        *audio_layer = AudioLayer::kDummyAudio;
        return 0;
    }

    int32_t RegisterAudioCallback(webrtc::AudioTransport* callback) override {
        std::lock_guard<std::mutex> lock(audio_transport_mutex_);
        audio_transport_ = callback;
        return 0;
    }

    webrtc::AudioTransport* GetAudioTransport() const {
        std::lock_guard<std::mutex> lock(audio_transport_mutex_);
        return audio_transport_;
    }

    int32_t Init() override {
        initialized_ = true;
        return 0;
    }

    int32_t Terminate() override {
        StopPlayout();
        initialized_ = false;
        return 0;
    }

    bool Initialized() const override { return initialized_; }
    int16_t PlayoutDevices() override { return 1; }
    int16_t RecordingDevices() override { return 0; }
    int32_t PlayoutDeviceName(uint16_t, char* name, char* guid) override {
        if (name) strcpy(name, "DummyPlayout");
        if (guid) strcpy(guid, "dummy-playout-guid");
        return 0;
    }
    int32_t RecordingDeviceName(uint16_t, char*, char*) override { return 0; }
    int32_t SetPlayoutDevice(uint16_t) override { return 0; }
    int32_t SetPlayoutDevice(WindowsDeviceType) override { return 0; }
    int32_t SetRecordingDevice(uint16_t) override { return 0; }
    int32_t SetRecordingDevice(WindowsDeviceType) override { return 0; }

    int32_t PlayoutIsAvailable(bool* available) override {
        *available = true;
        return 0;
    }

    int32_t InitPlayout() override {
        playout_initialized_ = true;
        return 0;
    }

    bool PlayoutIsInitialized() const override {
        return playout_initialized_;
    }

    int32_t RecordingIsAvailable(bool* available) override {
        *available = false;
        return 0;
    }
    int32_t InitRecording() override { return 0; }
    bool RecordingIsInitialized() const override { return false; }

    int32_t StartPlayout() override {
        if (!playout_initialized_) {
            return -1;
        }
        if (playing_) {
            return 0;
        }
        playing_ = true;
        stop_playout_thread_ = false;

        // Start background thread to pull audio data
        playout_thread_ = std::thread([this]() {
            PlayoutThreadFunc();
        });

        return 0;
    }

    int32_t StopPlayout() override {
        if (!playing_) {
            return 0;
        }
        playing_ = false;
        stop_playout_thread_ = true;

        if (playout_thread_.joinable()) {
            playout_thread_.join();
        }
        return 0;
    }

    bool Playing() const override { return playing_; }

    int32_t StartRecording() override { return 0; }
    int32_t StopRecording() override { return 0; }
    bool Recording() const override { return false; }
    int32_t InitSpeaker() override { return 0; }
    bool SpeakerIsInitialized() const override { return true; }
    int32_t InitMicrophone() override { return 0; }
    bool MicrophoneIsInitialized() const override { return false; }
    int32_t SpeakerVolumeIsAvailable(bool* available) override {
        *available = false;
        return 0;
    }
    int32_t SetSpeakerVolume(uint32_t) override { return -1; }
    int32_t SpeakerVolume(uint32_t* volume) const override { return -1; }
    int32_t MaxSpeakerVolume(uint32_t* max) const override { return -1; }
    int32_t MinSpeakerVolume(uint32_t* min) const override { return -1; }
    int32_t MicrophoneVolumeIsAvailable(bool* available) override {
        *available = false;
        return 0;
    }
    int32_t SetMicrophoneVolume(uint32_t) override { return -1; }
    int32_t MicrophoneVolume(uint32_t* volume) const override { return -1; }
    int32_t MaxMicrophoneVolume(uint32_t* max) const override { return -1; }
    int32_t MinMicrophoneVolume(uint32_t* min) const override { return -1; }
    int32_t SpeakerMuteIsAvailable(bool* available) override {
        *available = false;
        return 0;
    }
    int32_t SetSpeakerMute(bool) override { return -1; }
    int32_t SpeakerMute(bool* enabled) const override { return -1; }
    int32_t MicrophoneMuteIsAvailable(bool* available) override {
        *available = false;
        return 0;
    }
    int32_t SetMicrophoneMute(bool) override { return -1; }
    int32_t MicrophoneMute(bool* enabled) const override { return -1; }

    int32_t StereoPlayoutIsAvailable(bool* available) const override {
        *available = true;
        return 0;
    }
    int32_t SetStereoPlayout(bool enable) override {
        stereo_playout_ = enable;
        return 0;
    }
    int32_t StereoPlayout(bool* enabled) const override {
        *enabled = stereo_playout_;
        return 0;
    }
    int32_t StereoRecordingIsAvailable(bool* available) const override {
        *available = false;
        return 0;
    }
    int32_t SetStereoRecording(bool) override { return -1; }
    int32_t StereoRecording(bool* enabled) const override { return -1; }

    int32_t PlayoutDelay(uint16_t* delay_ms) const override {
        *delay_ms = 10;  // 10ms playout delay
        return 0;
    }

    bool BuiltInAECIsAvailable() const override { return false; }
    int32_t EnableBuiltInAEC(bool) override { return -1; }
    bool BuiltInAGCIsAvailable() const override { return false; }
    int32_t EnableBuiltInAGC(bool) override { return -1; }
    bool BuiltInNSIsAvailable() const override { return false; }
    int32_t EnableBuiltInNS(bool) override { return -1; }

protected:
    DummyAudioDeviceModule()
        : ref_count_(0),
          audio_transport_(nullptr),
          initialized_(false),
          playout_initialized_(false),
          playing_(false),
          stereo_playout_(true),
          stop_playout_thread_(false),
          playout_callback_(nullptr),
          playout_callback_user_data_(nullptr) {}

    ~DummyAudioDeviceModule() override {
        StopPlayout();
    }

private:
    void PlayoutThreadFunc() {
        // Audio parameters: 48kHz, stereo, 10ms frames
        const int kSampleRate = 48000;
        const size_t kNumChannels = stereo_playout_ ? 2 : 1;
        const size_t kSamplesPerChannel = kSampleRate / 100;  // 10ms = 480 samples
        const size_t kTotalSamples = kSamplesPerChannel * kNumChannels;
        const auto kFrameDuration = std::chrono::microseconds(10000);  // 10ms in microseconds

        std::vector<int16_t> audio_buffer(kTotalSamples, 0);
        int64_t elapsed_time_ms = 0;
        int64_t ntp_time_ms = 0;

        auto next_frame_time = std::chrono::steady_clock::now();

        while (!stop_playout_thread_) {
            // Wait until next frame time
            next_frame_time += kFrameDuration;
            std::this_thread::sleep_until(next_frame_time);

            webrtc::AudioTransport* transport = nullptr;
            {
                std::lock_guard<std::mutex> lock(audio_transport_mutex_);
                transport = audio_transport_;
            }

            if (transport && playing_) {
                // Pull audio data from the audio pipeline
                size_t samples_out = 0;
                transport->NeedMorePlayData(
                    kSamplesPerChannel,
                    sizeof(int16_t) * 8,  // bits per sample
                    kNumChannels,
                    kSampleRate,
                    audio_buffer.data(),
                    samples_out,
                    &elapsed_time_ms,
                    &ntp_time_ms
                );

                // Deliver audio to registered callback
                PlayoutAudioCallback cb = nullptr;
                void* cb_user_data = nullptr;
                {
                    std::lock_guard<std::mutex> lock(callback_mutex_);
                    cb = playout_callback_;
                    cb_user_data = playout_callback_user_data_;
                }

                if (cb && samples_out > 0) {
                    cb(cb_user_data, kSampleRate, kNumChannels, kSamplesPerChannel, audio_buffer.data());
                }
            }
        }
    }

    mutable std::atomic<int> ref_count_;
    mutable std::mutex audio_transport_mutex_;
    webrtc::AudioTransport* audio_transport_;
    bool initialized_;
    bool playout_initialized_;
    std::atomic<bool> playing_;
    bool stereo_playout_;
    std::atomic<bool> stop_playout_thread_;
    std::thread playout_thread_;

    // Callback for playout audio
    mutable std::mutex callback_mutex_;
    PlayoutAudioCallback playout_callback_;
    void* playout_callback_user_data_;
};

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

// VideoSink implementation for receiving decoded video frames
class VideoSinkImpl : public webrtc::VideoSinkInterface<webrtc::VideoFrame> {
public:
    VideoSinkImpl(VideoFrameCallback callback, void* user_data)
        : callback_(callback), user_data_(user_data) {}

    void OnFrame(const webrtc::VideoFrame& frame) override {
        if (!callback_) {
            return;
        }

        // Get I420 buffer (convert if necessary)
        webrtc::scoped_refptr<webrtc::I420BufferInterface> i420_buffer =
            frame.video_frame_buffer()->ToI420();

        if (!i420_buffer) {
            return;
        }

        // Call the callback with frame data
        callback_(
            user_data_,
            i420_buffer->width(),
            i420_buffer->height(),
            frame.timestamp_us(),
            i420_buffer->DataY(),
            i420_buffer->StrideY(),
            i420_buffer->DataU(),
            i420_buffer->StrideU(),
            i420_buffer->DataV(),
            i420_buffer->StrideV()
        );
    }

private:
    VideoFrameCallback callback_;
    void* user_data_;
};

// AudioSink implementation for receiving decoded audio frames
// Note: For audio, we use AudioTrackSinkInterface
class AudioSinkImpl : public webrtc::AudioTrackSinkInterface {
public:
    AudioSinkImpl(AudioFrameCallback callback, void* user_data)
        : callback_(callback), user_data_(user_data) {}

    void OnData(const void* audio_data,
                int bits_per_sample,
                int sample_rate,
                size_t number_of_channels,
                size_t number_of_frames) override {
        if (!callback_) {
            return;
        }

        // libwebrtc delivers audio as interleaved 16-bit PCM
        if (bits_per_sample != 16) {
            return;
        }

        callback_(
            user_data_,
            sample_rate,
            number_of_channels,
            number_of_frames,
            static_cast<const int16_t*>(audio_data)
        );
    }

private:
    AudioFrameCallback callback_;
    void* user_data_;
};

}  // namespace

// Opaque struct definitions
struct WebrtcPeerConnectionFactory {
    webrtc::scoped_refptr<webrtc::PeerConnectionFactoryInterface> factory;
    webrtc::scoped_refptr<DummyAudioDeviceModule> adm;
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
    void OnIceGatheringChange(webrtc::PeerConnectionInterface::IceGatheringState new_state) override;
    void OnIceCandidate(const webrtc::IceCandidateInterface* candidate) override;
    void OnTrack(webrtc::scoped_refptr<webrtc::RtpTransceiverInterface> transceiver) override;

private:
    WebrtcPeerConnection* pc_;
};

struct WebrtcPeerConnection {
    webrtc::scoped_refptr<webrtc::PeerConnectionInterface> pc;
    std::unique_ptr<PeerConnectionObserverImpl> observer;
    WebrtcPeerConnectionFactory* factory = nullptr;  // Reference to factory for ADM access

    OnTrackCallback on_track_callback = nullptr;
    void* on_track_user_data = nullptr;

    OnIceConnectionStateChangeCallback on_ice_state_callback = nullptr;
    void* on_ice_state_user_data = nullptr;

    OnIceCandidateCallback on_ice_candidate_callback = nullptr;
    void* on_ice_candidate_user_data = nullptr;

    OnIceGatheringStateChangeCallback on_ice_gathering_state_callback = nullptr;
    void* on_ice_gathering_state_user_data = nullptr;

    // Frame callbacks
    VideoFrameCallback video_frame_callback = nullptr;
    void* video_frame_user_data = nullptr;

    AudioFrameCallback audio_frame_callback = nullptr;
    void* audio_frame_user_data = nullptr;

    // Track sinks (owned by PeerConnection, registered to tracks)
    std::vector<std::unique_ptr<VideoSinkImpl>> video_sinks;
    std::vector<std::unique_ptr<AudioSinkImpl>> audio_sinks;

    // Keep track of registered tracks for cleanup
    std::vector<webrtc::scoped_refptr<webrtc::VideoTrackInterface>> video_tracks;
    std::vector<webrtc::scoped_refptr<webrtc::AudioTrackInterface>> audio_tracks;
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

void PeerConnectionObserverImpl::OnIceGatheringChange(
    webrtc::PeerConnectionInterface::IceGatheringState new_state) {
    if (pc_->on_ice_gathering_state_callback) {
        pc_->on_ice_gathering_state_callback(
            pc_->on_ice_gathering_state_user_data,
            static_cast<int>(new_state));
    }
}

void PeerConnectionObserverImpl::OnIceCandidate(
    const webrtc::IceCandidateInterface* candidate) {
    if (pc_->on_ice_candidate_callback && candidate) {
        std::string sdp;
        if (candidate->ToString(&sdp)) {
            pc_->on_ice_candidate_callback(
                pc_->on_ice_candidate_user_data,
                sdp.c_str(),
                candidate->sdp_mid().c_str(),
                candidate->sdp_mline_index());
        }
    }
}

void PeerConnectionObserverImpl::OnTrack(
    webrtc::scoped_refptr<webrtc::RtpTransceiverInterface> transceiver) {
    if (!transceiver->receiver()) {
        return;
    }

    auto track = transceiver->receiver()->track();
    if (!track) {
        return;
    }

    std::string track_id = track->id();
    bool is_video = (track->kind() == webrtc::MediaStreamTrackInterface::kVideoKind);

    // Call the on_track callback first
    if (pc_->on_track_callback) {
        pc_->on_track_callback(pc_->on_track_user_data, track_id.c_str(), is_video ? 1 : 0);
    }

    // Register sinks for frame callbacks
    if (is_video) {
        auto video_track = static_cast<webrtc::VideoTrackInterface*>(track.get());

        // If video frame callback is set, register a sink
        if (pc_->video_frame_callback) {
            auto sink = std::make_unique<VideoSinkImpl>(
                pc_->video_frame_callback,
                pc_->video_frame_user_data
            );
            video_track->AddOrUpdateSink(sink.get(), webrtc::VideoSinkWants());
            pc_->video_sinks.push_back(std::move(sink));
            pc_->video_tracks.push_back(
                webrtc::scoped_refptr<webrtc::VideoTrackInterface>(video_track)
            );
        }
    } else {
        // Audio is handled via DummyAudioDeviceModule's playout callback
        // AudioTrackSinkInterface::OnData delivers empty data, so we don't use it
        // The ADM's NeedMorePlayData pulls the actual decoded audio
        (void)track;  // Suppress unused variable warning
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

    // Use DummyAudioDeviceModule to prevent audio from being played through speakers
    auto dummy_adm = DummyAudioDeviceModule::Create();

    auto factory = webrtc::CreatePeerConnectionFactory(
        g_network_thread.get(),
        g_worker_thread.get(),
        g_signaling_thread.get(),
        dummy_adm,  // Use dummy ADM to disable speaker output
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
    wrapper->adm = dummy_adm;  // Save ADM reference for audio callback
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
    wrapper->factory = factory;  // Save factory reference for ADM access

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
        // Remove video sinks from tracks
        for (size_t i = 0; i < pc->video_tracks.size() && i < pc->video_sinks.size(); ++i) {
            if (pc->video_tracks[i]) {
                pc->video_tracks[i]->RemoveSink(pc->video_sinks[i].get());
            }
        }

        // Remove audio sinks from tracks
        for (size_t i = 0; i < pc->audio_tracks.size() && i < pc->audio_sinks.size(); ++i) {
            if (pc->audio_tracks[i]) {
                pc->audio_tracks[i]->RemoveSink(pc->audio_sinks[i].get());
            }
        }

        // Clear vectors
        pc->video_sinks.clear();
        pc->audio_sinks.clear();
        pc->video_tracks.clear();
        pc->audio_tracks.clear();

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

void webrtc_pc_set_on_ice_candidate_callback(
    WebrtcPeerConnection* pc,
    OnIceCandidateCallback callback,
    void* user_data) {
    if (pc) {
        pc->on_ice_candidate_callback = callback;
        pc->on_ice_candidate_user_data = user_data;
    }
}

void webrtc_pc_set_on_ice_gathering_state_change_callback(
    WebrtcPeerConnection* pc,
    OnIceGatheringStateChangeCallback callback,
    void* user_data) {
    if (pc) {
        pc->on_ice_gathering_state_callback = callback;
        pc->on_ice_gathering_state_user_data = user_data;
    }
}

void webrtc_video_track_set_frame_callback(
    WebrtcVideoTrack* track,
    VideoFrameCallback callback,
    void* user_data) {
    // Deprecated: Use webrtc_pc_set_video_frame_callback instead
    // This function is kept for API compatibility but does nothing
}

void webrtc_audio_track_set_frame_callback(
    WebrtcAudioTrack* track,
    AudioFrameCallback callback,
    void* user_data) {
    // Deprecated: Use webrtc_pc_set_audio_frame_callback instead
    // This function is kept for API compatibility but does nothing
}

void webrtc_pc_set_video_frame_callback(
    WebrtcPeerConnection* pc,
    VideoFrameCallback callback,
    void* user_data) {
    if (pc) {
        pc->video_frame_callback = callback;
        pc->video_frame_user_data = user_data;
    }
}

void webrtc_pc_set_audio_frame_callback(
    WebrtcPeerConnection* pc,
    AudioFrameCallback callback,
    void* user_data) {
    if (pc) {
        pc->audio_frame_callback = callback;
        pc->audio_frame_user_data = user_data;

        // Also register callback with ADM for playout audio
        if (pc->factory && pc->factory->adm) {
            // Cast AudioFrameCallback to PlayoutAudioCallback (same signature)
            pc->factory->adm->SetPlayoutCallback(
                reinterpret_cast<PlayoutAudioCallback>(callback),
                user_data
            );
        }
    }
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
