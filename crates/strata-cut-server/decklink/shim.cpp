// Plain C ABI over the Blackmagic DeckLink SDK (COM), for the Rust side
// (src/recorder/sdk.rs). Built by build.rs against the headers in
// ../decklink-sdk/linux/ (the same approach as the CG server's shim).
//
// The DeckLink runtime (Blackmagic Desktop Video, libDeckLinkAPI.so) is loaded
// at run time by DeckLinkAPIDispatch.cpp: on machines without it every entry
// point fails gracefully instead of the server failing to start.
//
// Capture: every frame the card gives (with its sound) goes into a queue the
// recorder empties; the queue is bounded, its oldest frames are dropped when
// the recorder falls behind. The input follows the signal's format when the
// card can detect it.

#include "DeckLinkAPI.h"
#include "DeckLinkAPIConfiguration.h"

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <deque>
#include <mutex>
#include <thread>
#include <vector>

typedef bool dl_bool;
typedef const char *dl_string;

// What identifies a device and where it sits (same layout as `RawDevice`).
// `has_*` say whether the matching attribute answered.
struct dl_device_info {
    int64_t persistent_id;
    int64_t group_id;
    int64_t topological_id;
    int64_t sub_device_index;
    int64_t sub_devices;
    int32_t has_persistent_id;
    int32_t has_group_id;
    int32_t has_topological_id;
    int32_t has_sub_device_index;
    int32_t has_sub_devices;
    int32_t has_reference_input;
    // 1 locked, 0 not locked, -1 unknown.
    int32_t reference_locked;
    // BMDDisplayMode of the reference signal (0: none or unknown).
    uint32_t reference_mode;
    // BMDProfileID of the active profile (0: unknown).
    uint32_t profile;
    // The profiles the device can switch to.
    uint32_t profiles[8];
    int32_t profile_count;
    // Bit 0 output, 1 input, 4 input format detection.
    int32_t caps;
    char display_name[128];
    char model_name[128];
};

// One display mode (same layout as `RawMode`).
struct dl_mode {
    uint32_t id;
    int32_t width;
    int32_t height;
    // BMDFieldDominance: 'uppr', 'lowr', 'prog', 'psf '.
    uint32_t field_dominance;
    int64_t duration;
    int64_t timescale;
    char name[64];
};

// What dl_capture_next took (same layout as `RawPacket`).
struct dl_packet {
    int64_t video_bytes;
    int64_t audio_bytes;
    int32_t width;
    int32_t height;
    int32_t samples;
    // 1: the card had no input signal for this frame.
    int32_t no_signal;
};

namespace {

void set_err(char *err, int len, const char *msg) {
    if (err && len > 0) {
        std::snprintf(err, (size_t)len, "%s", msg);
    }
}

template <class T> void release(T *&p) {
    if (p) {
        p->Release();
        p = NULL;
    }
}

// Copies a DeckLink string to UTF-8 and frees it.
void take_string(dl_string s, char *buf, int len) {
    if (!buf || len <= 0) {
        return;
    }
    buf[0] = 0;
    if (!s) {
        return;
    }
    std::snprintf(buf, (size_t)len, "%s", s);
    std::free((void *)s);
}

const char *NO_RUNTIME = "Blackmagic Desktop Video is not installed (no DeckLink driver)";

// The device at `index` (a new reference), NULL with `err` set otherwise.
IDeckLink *device_at(int index, char *err, int errlen) {
    IDeckLinkIterator *it = CreateDeckLinkIteratorInstance();
    if (!it) {
        set_err(err, errlen, NO_RUNTIME);
        return NULL;
    }
    IDeckLink *dev = NULL;
    IDeckLink *found = NULL;
    int i = 0;
    while (it->Next(&dev) == S_OK) {
        if (i++ == index) {
            found = dev;
            break;
        }
        release(dev);
    }
    release(it);
    if (!found) {
        char msg[96];
        std::snprintf(msg, sizeof msg, "no DeckLink device #%d (%d found)", index + 1, i);
        set_err(err, errlen, msg);
    }
    return found;
}

bool same_iid(const void *a, const void *b) { return std::memcmp(a, b, 16) == 0; }

// IUnknown {00000000-0000-0000-C000-000000000046}.
const unsigned char IID_UNKNOWN_BYTES[16] = {0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46};

bool flag_attribute(IDeckLink *dev, BMDDeckLinkAttributeID id) {
    IDeckLinkProfileAttributes *attrs = NULL;
    if (dev->QueryInterface(IID_IDeckLinkProfileAttributes, (void **)&attrs) != S_OK) {
        return false;
    }
    dl_bool v = false;
    bool ok = attrs->GetFlag(id, &v) == S_OK && v;
    release(attrs);
    return ok;
}

int64_t int_attribute(IDeckLinkProfileAttributes *attrs, BMDDeckLinkAttributeID id, int32_t *ok) {
    int64_t v = 0;
    *ok = attrs && attrs->GetInt(id, &v) == S_OK ? 1 : 0;
    return *ok ? v : 0;
}

int caps_of(IDeckLink *dev) {
    int caps = 0;
    IDeckLinkOutput *out = NULL;
    if (dev->QueryInterface(IID_IDeckLinkOutput, (void **)&out) == S_OK) {
        caps |= 1;
        release(out);
    }
    IDeckLinkInput *in = NULL;
    if (dev->QueryInterface(IID_IDeckLinkInput, (void **)&in) == S_OK) {
        caps |= 2;
        release(in);
    }
    if (flag_attribute(dev, BMDDeckLinkSupportsInputFormatDetection)) {
        caps |= 16;
    }
    return caps;
}

// Identity, place, reference and profiles of a device.
void describe(IDeckLink *dev, dl_device_info *o) {
    std::memset(o, 0, sizeof *o);
    o->reference_locked = -1;
    dl_string s = NULL;
    if (dev->GetDisplayName(&s) == S_OK) {
        take_string(s, o->display_name, (int)sizeof o->display_name);
    }
    s = NULL;
    if (dev->GetModelName(&s) == S_OK) {
        take_string(s, o->model_name, (int)sizeof o->model_name);
    }
    IDeckLinkProfileAttributes *attrs = NULL;
    if (dev->QueryInterface(IID_IDeckLinkProfileAttributes, (void **)&attrs) != S_OK) {
        attrs = NULL;
    }
    if (attrs) {
        o->persistent_id = int_attribute(attrs, BMDDeckLinkPersistentID, &o->has_persistent_id);
        o->group_id = int_attribute(attrs, BMDDeckLinkDeviceGroupID, &o->has_group_id);
        o->topological_id = int_attribute(attrs, BMDDeckLinkTopologicalID, &o->has_topological_id);
        o->sub_device_index = int_attribute(attrs, BMDDeckLinkSubDeviceIndex, &o->has_sub_device_index);
        o->sub_devices = int_attribute(attrs, BMDDeckLinkNumberOfSubDevices, &o->has_sub_devices);
        int32_t ok = 0;
        o->profile = (uint32_t)int_attribute(attrs, BMDDeckLinkProfileID, &ok);
        dl_bool v = false;
        o->has_reference_input = attrs->GetFlag(BMDDeckLinkHasReferenceInput, &v) == S_OK && v ? 1 : 0;
        release(attrs);
    }
    IDeckLinkStatus *status = NULL;
    if (dev->QueryInterface(IID_IDeckLinkStatus, (void **)&status) == S_OK && status) {
        dl_bool v = false;
        if (status->GetFlag(bmdDeckLinkStatusReferenceSignalLocked, &v) == S_OK) {
            o->reference_locked = v ? 1 : 0;
        }
        int64_t mode = 0;
        if (status->GetInt(bmdDeckLinkStatusReferenceSignalMode, &mode) == S_OK) {
            o->reference_mode = (uint32_t)mode;
        }
        release(status);
    }
    IDeckLinkProfileManager *pm = NULL;
    if (dev->QueryInterface(IID_IDeckLinkProfileManager, (void **)&pm) == S_OK && pm) {
        IDeckLinkProfileIterator *it = NULL;
        if (pm->GetProfiles(&it) == S_OK && it) {
            IDeckLinkProfile *p = NULL;
            while (it->Next(&p) == S_OK && p) {
                IDeckLinkProfileAttributes *pa = NULL;
                if (o->profile_count < 8 &&
                    p->QueryInterface(IID_IDeckLinkProfileAttributes, (void **)&pa) == S_OK && pa) {
                    int32_t ok = 0;
                    uint32_t id = (uint32_t)int_attribute(pa, BMDDeckLinkProfileID, &ok);
                    if (ok) {
                        o->profiles[o->profile_count++] = id;
                    }
                    release(pa);
                }
                release(p);
            }
            release(it);
        }
        release(pm);
    }
    o->caps = caps_of(dev);
}

void fill_mode(IDeckLinkDisplayMode *mode, dl_mode *d) {
    std::memset(d, 0, sizeof *d);
    d->id = (uint32_t)mode->GetDisplayMode();
    d->width = (int32_t)mode->GetWidth();
    d->height = (int32_t)mode->GetHeight();
    d->field_dominance = (uint32_t)mode->GetFieldDominance();
    BMDTimeValue dur = 0;
    BMDTimeScale ts = 0;
    mode->GetFrameRate(&dur, &ts);
    d->duration = dur;
    d->timescale = ts;
    dl_string name = NULL;
    if (mode->GetName(&name) == S_OK) {
        take_string(name, d->name, (int)sizeof d->name);
    }
}

// One frame of the card and the sound that came with it.
struct Packet {
    std::vector<uint8_t> video; // UYVY, rows packed
    std::vector<uint8_t> audio; // signed 32-bit, channels interleaved
    int width = 0, height = 0, samples = 0;
    int no_signal = 0;
};

class Capture final : public IDeckLinkInputCallback {
  public:
    IDeckLink *dev = NULL;
    IDeckLinkInput *in = NULL;
    bool detection = false;
    int channels = 0;
    // Frames kept when the recorder falls behind, and their bytes at most.
    size_t max_frames = 50;
    size_t max_bytes = (size_t)400 << 20;

    std::mutex m;
    std::condition_variable arrived;
    std::deque<Packet> queue;
    size_t bytes = 0;
    std::vector<uint8_t> pending_audio; // sound that came without a frame
    int pending_samples = 0;
    int signal = -1;   // -1 unknown, 0 no input source, 1 a picture
    uint32_t mode = 0; // the BMDDisplayMode captured (the detected one once the signal is known)
    uint64_t mode_changes = 0;
    uint64_t dropped = 0;

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, LPVOID *ppv) override {
        if (!ppv) {
            return E_POINTER;
        }
        if (same_iid(&iid, IID_UNKNOWN_BYTES) || same_iid(&iid, &IID_IDeckLinkInputCallback)) {
            *ppv = static_cast<IDeckLinkInputCallback *>(this);
            AddRef();
            return S_OK;
        }
        *ppv = NULL;
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return ++refs; }
    ULONG STDMETHODCALLTYPE Release() override {
        ULONG n = --refs;
        if (n == 0) {
            delete this;
        }
        return n;
    }

    HRESULT STDMETHODCALLTYPE VideoInputFormatChanged(BMDVideoInputFormatChangedEvents events,
                                                      IDeckLinkDisplayMode *newMode,
                                                      BMDDetectedVideoInputFormatFlags flags) override {
        (void)flags;
        if (!newMode || !(events & (bmdVideoInputDisplayModeChanged | bmdVideoInputFieldDominanceChanged))) {
            return S_OK;
        }
        // Follow the incoming signal; frames stay 8-bit 4:2:2 (the card converts an RGB signal).
        {
            std::lock_guard<std::mutex> lk(m);
            mode = (uint32_t)newMode->GetDisplayMode();
            mode_changes++;
        }
        in->PauseStreams();
        in->EnableVideoInput(newMode->GetDisplayMode(), bmdFormat8BitYUV,
                             detection ? bmdVideoInputEnableFormatDetection : bmdVideoInputFlagDefault);
        in->FlushStreams();
        in->StartStreams();
        arrived.notify_all();
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE VideoInputFrameArrived(IDeckLinkVideoInputFrame *f,
                                                     IDeckLinkAudioInputPacket *audio) override {
        std::vector<uint8_t> sound;
        int samples = 0;
        if (audio && channels > 0) {
            void *pcm = NULL;
            long n = audio->GetSampleFrameCount();
            if (n > 0 && audio->GetBytes(&pcm) == S_OK && pcm) {
                size_t size = (size_t)n * (size_t)channels * 4;
                sound.assign((const uint8_t *)pcm, (const uint8_t *)pcm + size);
                samples = (int)n;
            }
        }
        std::lock_guard<std::mutex> lk(m);
        if (!f) {
            // Sound alone: it goes with the next frame.
            pending_audio.insert(pending_audio.end(), sound.begin(), sound.end());
            pending_samples += samples;
            return S_OK;
        }
        Packet p;
        p.no_signal = (f->GetFlags() & bmdFrameHasNoInputSource) ? 1 : 0;
        signal = p.no_signal ? 0 : 1;
        void *pixels = NULL;
        long w = f->GetWidth(), h = f->GetHeight(), row = f->GetRowBytes();
        if (f->GetPixelFormat() == bmdFormat8BitYUV && f->GetBytes(&pixels) == S_OK && pixels && w > 0 && h > 0 &&
            row >= w * 2) {
            size_t line = (size_t)w * 2;
            p.video.resize(line * (size_t)h);
            const uint8_t *src = (const uint8_t *)pixels;
            for (long y = 0; y < h; y++) {
                std::memcpy(&p.video[(size_t)y * line], src + (size_t)y * (size_t)row, line);
            }
            p.width = (int)w;
            p.height = (int)h;
        }
        if (!pending_audio.empty()) {
            p.audio.swap(pending_audio);
            p.samples = pending_samples;
            pending_samples = 0;
            p.audio.insert(p.audio.end(), sound.begin(), sound.end());
            p.samples += samples;
        } else {
            p.audio.swap(sound);
            p.samples = samples;
        }
        bytes_add(p);
        queue.push_back(std::move(p));
        // The recorder fell behind: the oldest frames go (with their sound).
        while (queue.size() > 1 && (queue.size() > max_frames || bytes > max_bytes)) {
            bytes_sub(queue.front());
            queue.pop_front();
            dropped++;
        }
        arrived.notify_all();
        return S_OK;
    }

    void bytes_add(const Packet &p) { bytes += p.video.size() + p.audio.size(); }
    void bytes_sub(const Packet &p) { bytes -= p.video.size() + p.audio.size(); }

    // Stops the capture; the interfaces go with the object.
    void close() {
        if (in) {
            in->StopStreams();
            in->SetCallback(NULL);
            in->DisableVideoInput();
            if (channels > 0) {
                in->DisableAudioInput();
            }
        }
    }

  private:
    std::atomic<ULONG> refs{1};
    virtual ~Capture() {
        release(in);
        release(dev);
    }
};

} // namespace

extern "C" {

// 1 when the DeckLink runtime (Desktop Video) is installed.
int dl_runtime_present(void) {
    IDeckLinkIterator *it = CreateDeckLinkIteratorInstance();
    if (!it) {
        return 0;
    }
    release(it);
    return 1;
}

// Every device in Desktop Video order: writes at most `cap` and returns how
// many there are, or -1 when the runtime is missing.
int dl_devices(dl_device_info *out, int cap) {
    IDeckLinkIterator *it = CreateDeckLinkIteratorInstance();
    if (!it) {
        return -1;
    }
    int n = 0;
    IDeckLink *dev = NULL;
    while (it->Next(&dev) == S_OK) {
        if (n < cap && out) {
            describe(dev, &out[n]);
        }
        n++;
        release(dev);
    }
    release(it);
    return n;
}

// The version of the DeckLink runtime (Desktop Video), e.g. "12.4"; 0 on success.
int dl_api_version(char *buf, int len) {
    set_err(buf, len, "");
    IDeckLinkAPIInformation *info = CreateDeckLinkAPIInformationInstance();
    if (!info) {
        return -1;
    }
    dl_string s = NULL;
    int r = -1;
    if (info->GetString(BMDDeckLinkAPIVersion, &s) == S_OK) {
        take_string(s, buf, len);
        r = 0;
    }
    release(info);
    return r;
}

// Activates a profile (BMDProfileID) of a device, waiting up to 3 s for it to
// take effect. 0 on success, -1 with `err` set.
int dl_set_profile(int index, uint32_t profile, char *err, int errlen) {
    set_err(err, errlen, "");
    IDeckLink *dev = device_at(index, err, errlen);
    if (!dev) {
        return -1;
    }
    int r = -1;
    IDeckLinkProfileManager *pm = NULL;
    IDeckLinkProfile *p = NULL;
    if (dev->QueryInterface(IID_IDeckLinkProfileManager, (void **)&pm) != S_OK || !pm) {
        set_err(err, errlen, "this DeckLink device has a single profile");
    } else if (pm->GetProfile((BMDProfileID)profile, &p) != S_OK || !p) {
        set_err(err, errlen, "this DeckLink device has no such profile");
    } else if (p->SetActive() != S_OK) {
        set_err(err, errlen, "cannot activate the profile (device in use?)");
    } else {
        for (int i = 0; i < 30; i++) {
            dl_bool active = false;
            if (p->IsActive(&active) == S_OK && active) {
                r = 0;
                break;
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(100));
        }
        if (r != 0) {
            set_err(err, errlen, "the profile did not become active");
        }
    }
    release(p);
    release(pm);
    release(dev);
    return r;
}

// Display modes of a device's input. Returns how many there are (may exceed
// `cap`), or -1 with `err` set.
int dl_input_modes(int index, dl_mode *out, int cap, char *err, int errlen) {
    IDeckLink *dev = device_at(index, err, errlen);
    if (!dev) {
        return -1;
    }
    int n = -1;
    IDeckLinkInput *in = NULL;
    IDeckLinkDisplayModeIterator *it = NULL;
    if (dev->QueryInterface(IID_IDeckLinkInput, (void **)&in) != S_OK) {
        set_err(err, errlen, "this DeckLink device has no input");
    } else if (in->GetDisplayModeIterator(&it) != S_OK || !it) {
        set_err(err, errlen, "cannot list the display modes");
    } else {
        n = 0;
        IDeckLinkDisplayMode *mode = NULL;
        while (it->Next(&mode) == S_OK) {
            if (out && n < cap) {
                fill_mode(mode, &out[n]);
            }
            n++;
            release(mode);
        }
    }
    release(it);
    release(in);
    release(dev);
    return n;
}

// Opens a device's input in `mode` (a BMDDisplayMode), capturing 8-bit 4:2:2
// (UYVY) and, when `channels` > 0, 48 kHz 32-bit sound. `detect`: follow the
// signal's format (when the card can detect it). `connection` /
// `audio_connection`: a BMDVideoConnection / BMDAudioConnection, 0 = as the
// card is set. `max_frames`: frames kept when the reader falls behind.
void *dl_capture_open(int index, uint32_t mode, int detect, int64_t connection, int64_t audio_connection,
                      int channels, int max_frames, char *err, int errlen) {
    set_err(err, errlen, "");
    IDeckLink *dev = device_at(index, err, errlen);
    if (!dev) {
        return NULL;
    }
    Capture *c = new Capture();
    c->dev = dev;
    c->channels = channels;
    if (max_frames > 0) {
        c->max_frames = (size_t)max_frames;
    }
    BMDDisplayMode m = (BMDDisplayMode)mode;
    BMDVideoInputFlags flags = bmdVideoInputFlagDefault;
    if (dev->QueryInterface(IID_IDeckLinkInput, (void **)&c->in) != S_OK) {
        c->in = NULL;
        set_err(err, errlen, "this DeckLink device has no input");
        goto fail;
    }
    if (connection || audio_connection) {
        IDeckLinkConfiguration *cfg = NULL;
        if (dev->QueryInterface(IID_IDeckLinkConfiguration, (void **)&cfg) == S_OK && cfg) {
            if (connection && cfg->SetInt(bmdDeckLinkConfigVideoInputConnection, connection) != S_OK) {
                release(cfg);
                set_err(err, errlen, "this DeckLink device has no such video input");
                goto fail;
            }
            if (audio_connection && cfg->SetInt(bmdDeckLinkConfigAudioInputConnection, audio_connection) != S_OK) {
                release(cfg);
                set_err(err, errlen, "this DeckLink device has no such audio input");
                goto fail;
            }
            release(cfg);
        }
    }
    c->detection = detect && flag_attribute(dev, BMDDeckLinkSupportsInputFormatDetection);
    {
        BMDDisplayMode actual = m;
        dl_bool supported = false;
        if (c->in->DoesSupportVideoMode(bmdVideoConnectionUnspecified, m, bmdFormat8BitYUV, bmdNoVideoInputConversion,
                                        bmdSupportedVideoModeDefault, &actual, &supported) != S_OK ||
            !supported) {
            set_err(err, errlen, "the device cannot capture this video format");
            goto fail;
        }
    }
    if (c->detection) {
        flags = (BMDVideoInputFlags)(flags | bmdVideoInputEnableFormatDetection);
    }
    if (c->in->SetCallback(c) != S_OK) {
        set_err(err, errlen, "cannot set the input callback");
        goto fail;
    }
    if (c->in->EnableVideoInput(m, bmdFormat8BitYUV, flags) != S_OK) {
        set_err(err, errlen, "cannot enable the video input (in use by another program?)");
        goto fail;
    }
    if (channels > 0 &&
        c->in->EnableAudioInput(bmdAudioSampleRate48kHz, bmdAudioSampleType32bitInteger, (uint32_t)channels) != S_OK) {
        set_err(err, errlen, "cannot enable the audio input (number of channels?)");
        goto fail;
    }
    {
        std::lock_guard<std::mutex> lk(c->m);
        c->mode = (uint32_t)m;
    }
    if (c->in->StartStreams() != S_OK) {
        set_err(err, errlen, "cannot start the capture");
        goto fail;
    }
    return c;
fail:
    c->close();
    c->Release();
    return NULL;
}

// Takes the oldest frame captured and its sound, waiting up to timeout_ms.
// 1 taken, 0 nothing came, -1 a buffer is too small (`info` says the sizes
// needed; the frame stays). `video` / `audio` may be NULL to drop them.
int dl_capture_next(void *h, int timeout_ms, uint8_t *video, int64_t video_cap, uint8_t *audio, int64_t audio_cap,
                    dl_packet *info) {
    Capture *c = (Capture *)h;
    std::unique_lock<std::mutex> lk(c->m);
    if (!c->arrived.wait_for(lk, std::chrono::milliseconds(timeout_ms), [&] { return !c->queue.empty(); })) {
        return 0;
    }
    Packet &p = c->queue.front();
    info->video_bytes = (int64_t)p.video.size();
    info->audio_bytes = (int64_t)p.audio.size();
    info->width = p.width;
    info->height = p.height;
    info->samples = p.samples;
    info->no_signal = p.no_signal;
    if ((video && video_cap < info->video_bytes) || (audio && audio_cap < info->audio_bytes)) {
        return -1;
    }
    if (video && !p.video.empty()) {
        std::memcpy(video, p.video.data(), p.video.size());
    }
    if (audio && !p.audio.empty()) {
        std::memcpy(audio, p.audio.data(), p.audio.size());
    }
    c->bytes_sub(p);
    c->queue.pop_front();
    return 1;
}

// The display mode captured now (the detected one once the card saw the
// signal), and how many times it changed. 0 on success.
int dl_capture_mode(void *h, dl_mode *out, uint64_t *changes) {
    Capture *c = (Capture *)h;
    uint32_t id = 0;
    {
        std::lock_guard<std::mutex> lk(c->m);
        id = c->mode;
        *changes = c->mode_changes;
    }
    IDeckLinkDisplayMode *mode = NULL;
    if (c->in->GetDisplayMode((BMDDisplayMode)id, &mode) != S_OK || !mode) {
        return -1;
    }
    fill_mode(mode, out);
    release(mode);
    return 0;
}

// 1 a picture, 0 no input source, -1 unknown (nothing received yet).
int dl_capture_signal(void *h) {
    Capture *c = (Capture *)h;
    std::lock_guard<std::mutex> lk(c->m);
    return c->signal;
}

// Frames dropped because the reader fell behind.
uint64_t dl_capture_dropped(void *h) {
    Capture *c = (Capture *)h;
    std::lock_guard<std::mutex> lk(c->m);
    return c->dropped;
}

void dl_capture_close(void *h) {
    Capture *c = (Capture *)h;
    c->close();
    c->Release();
}

} // extern "C"
