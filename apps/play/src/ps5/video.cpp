// PS5Upload Play - the game's pictures.
// Decoder setup as measured in phase 0 (docs/PHASE0.md), after the PS5 VLC port's
// ps5vdec.c: the library (module 207) and one compute queue first, then per decoder its CPU
// memory (flexible), GPU and CPU+GPU memory (direct, type 12), 16 picture buffers and 8
// access-unit buffers. HEVC at depth 1 gives each picture back from the call that took it.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "video.hpp"

#include "../app/log.hpp"
#include "../core/hdr.hpp"
#include "../core/proto.hpp"
#include "../net/net.hpp"
#include "display.hpp"
#include "sys.hpp"

#include <cstring>
#include <pthread.h>

namespace play::ps5::video
{
namespace
{
constexpr unsigned kFrames = 16, kAuSlots = 8, kQueue = 16; // a burst after a network stall fits
constexpr std::size_t kAuMax = 4u << 20;

// ---- the decoder ----------------------------------------------------------------------------
Mem queue_mem;
void *compute_queue = nullptr;
void *cpu_block = nullptr;
std::size_t cpu_block_len = 0;

struct Decoder
{
    void *dec = nullptr;
    std::uint32_t codec = 0;
    Mem gpu, cpu_gpu, frames, au[kAuSlots];
    std::size_t frame_size = 0;
    bool held[kFrames] = {};
    unsigned au_next = 0;
};
Decoder dec;

void close_decoder(Decoder &d) noexcept
{
    if (d.dec)
        log("video: delete decoder %#x", sceVideodec2DeleteDecoder(d.dec));
    for (auto &a : d.au)
        mem_free(a);
    mem_free(d.frames);
    mem_free(d.cpu_gpu);
    mem_free(d.gpu);
    d = Decoder{};
}

bool open_decoder(Decoder &d, std::uint32_t codec) noexcept
{
    close_decoder(d);
    d.codec = codec;
    VdecConfig c{};
    c.this_size = sizeof c;
    c.resource_type = 1;
    c.codec_type = codec;
    c.profile = codec == kCodecAvc ? 100 : 2;
    c.max_level = codec == kCodecAvc ? 52 : 156;
    c.max_frame_width = 1920;
    c.max_frame_height = 1088;
    c.max_dpb_frame_count = -1;
    c.decode_pipeline_depth = 1;
    c.compute_queue = compute_queue;
    c.cpu_affinity_mask = 0x3f;
    c.cpu_thread_priority = 700;
    c.optimize_progressive_video = true;
    VdecMemory m{};
    m.this_size = sizeof m;
    const std::uint64_t t0 = net::now_us();
    int rc = sceVideodec2QueryDecoderMemoryInfo(&c, &m);
    if (rc != 0)
    {
        log("video: memory query %#x", rc);
        return false;
    }
    if (m.cpu_memory_size > cpu_block_len)
    {
        // One flexible block reused by every decoder.
        void *addr = big_alloc(m.cpu_memory_size);
        if (!addr)
            return false;
        cpu_block = addr;
        cpu_block_len = round_up(m.cpu_memory_size, k16K);
    }
    const std::size_t align = m.frame_buffer_alignment > k16K ? m.frame_buffer_alignment : k16K;
    d.frame_size = round_up(m.max_frame_buffer_size, align);
    bool ok = (!m.gpu_memory_size || mem_direct(d.gpu, m.gpu_memory_size, k16K, 12, kProtCpuGpuRw)) &&
              (!m.cpu_gpu_memory_size || mem_direct(d.cpu_gpu, m.cpu_gpu_memory_size, k16K, 12, kProtCpuGpuRw)) &&
              mem_direct(d.frames, d.frame_size * kFrames, align, 12, kProtCpuGpuRw);
    for (unsigned i = 0; ok && i < kAuSlots; ++i)
        ok = mem_direct(d.au[i], kAuMax, k16K, 12, kProtCpuGpuRw);
    if (!ok)
    {
        close_decoder(d);
        return false;
    }
    std::memset(cpu_block, 0, cpu_block_len);
    m.cpu_memory = m.cpu_memory_size ? cpu_block : nullptr;
    m.gpu_memory = d.gpu.addr;
    m.cpu_gpu_memory = d.cpu_gpu.addr;
    rc = sceVideodec2CreateDecoder(&c, &m, &d.dec);
    log("video: %s decoder %#x in %.1f ms", codec == kCodecAvc ? "H.264" : "HEVC", rc,
        static_cast<double>(net::now_us() - t0) / 1000.0);
    if (rc != 0)
    {
        d.dec = nullptr;
        close_decoder(d);
        return false;
    }
    d.codec = codec;
    return true;
}

int free_frame(Decoder &d, VdecFrameBuffer &fb) noexcept
{
    for (unsigned i = 0; i < kFrames; ++i)
        if (!d.held[i])
        {
            fb = {};
            fb.this_size = sizeof fb;
            fb.frame_buffer = static_cast<std::uint8_t *>(d.frames.addr) + i * d.frame_size;
            fb.frame_buffer_size = d.frame_size;
            return static_cast<int>(i);
        }
    return -1;
}

void frame_back(Decoder &d, const void *buffer) noexcept
{
    const auto *base = static_cast<const std::uint8_t *>(d.frames.addr);
    const auto *b = static_cast<const std::uint8_t *>(buffer);
    if (!b || b < base)
        return;
    const std::size_t i = static_cast<std::size_t>(b - base) / d.frame_size;
    if (i < kFrames)
        d.held[i] = false;
}

// ---- the queue between the network thread and the decoder ----------------------------------
struct Slot
{
    std::uint8_t *data = nullptr;
    std::size_t len = 0;
};
Slot slots[kQueue];
unsigned q_head = 0, q_count = 0;
pthread_mutex_t q_lock = PTHREAD_MUTEX_INITIALIZER;
pthread_cond_t q_cond = PTHREAD_COND_INITIALIZER;
bool active = false, hevc = false, waiting_key = true;
unsigned generation = 0;
std::uint8_t header_buf[16384];
std::size_t header_len = 0;
bool header_new = false;
Stats st;
float decode_avg = 0;

void *thread_main(void *) noexcept
{
    unsigned seen_gen = 0;
    unsigned errors_in_row = 0;
    int shift = -1; // where a 10-bit picture keeps its bits: found on the first one of a session
    for (;;)
    {
        pthread_mutex_lock(&q_lock);
        while (q_count == 0 && seen_gen == generation)
            pthread_cond_wait(&q_cond, &q_lock);
        const bool want_hevc = hevc, is_active = active;
        const unsigned gen = generation;
        // Take the next access unit straight into the decoder's own buffer, under the lock:
        // the network thread may reuse its queue slot as soon as it is popped.
        Mem *au = nullptr;
        std::size_t n = 0;
        bool more = false;
        if (q_count && seen_gen == gen)
        {
            const Slot &item = slots[q_head];
            if (dec.dec && item.len <= kAuMax)
            {
                au = &dec.au[dec.au_next++ % kAuSlots];
                n = item.len;
                std::memcpy(au->addr, item.data, n);
            }
            q_head = (q_head + 1) % kQueue;
            --q_count;
            more = q_count > 0;
        }
        pthread_mutex_unlock(&q_lock);
        if (seen_gen != gen)
        {
            seen_gen = gen;
            errors_in_row = 0;
            shift = -1;
            const std::uint32_t codec = want_hevc ? kCodecHevc : kCodecAvc;
            if (is_active && (!dec.dec || dec.codec != codec))
                open_decoder(dec, codec);
            else if (is_active && dec.dec)
            {
                // A new session on the same codec: start clean.
                close_decoder(dec);
                open_decoder(dec, codec);
            }
            continue;
        }
        if (au)
        {
            VdecInput in{};
            in.this_size = sizeof in;
            in.au_data = au->addr;
            in.au_size = n;
            in.pts_data = st.decoded;
            in.dts_data = ~0ull;
            VdecFrameBuffer fb;
            const int frame = free_frame(dec, fb);
            VdecOutput out{};
            out.this_size = sizeof out;
            const std::uint64_t t0 = net::now_us();
            const int rc = frame >= 0 ? sceVideodec2Decode(dec.dec, &in, &fb, &out) : -1;
            const float ms = static_cast<float>(net::now_us() - t0) / 1000.0f;
            decode_avg = decode_avg * 0.95f + ms * 0.05f;
            if (frame >= 0 && fb.is_accepted)
                dec.held[frame] = true;
            if (rc != 0)
            {
                ++st.errors;
                if (st.errors < 10)
                    log("video: decode %#x (%zu bytes)", rc, n);
                pthread_mutex_lock(&q_lock);
                waiting_key = true;
                pthread_mutex_unlock(&q_lock);
                if (++errors_in_row >= 5)
                {
                    log("video: decoder reset after errors");
                    open_decoder(dec, dec.codec);
                    errors_in_row = 0;
                }
            }
            else
            {
                errors_in_row = 0;
                ++st.decoded;
                if (out.is_valid && out.frame_buffer)
                {
                    if (out.is_error_frame)
                        ++st.errors;
                    // Behind (more waiting): decode on, show only the newest.
                    if (more)
                        ++st.skipped;
                    else
                    {
                        const auto *pic = static_cast<const std::uint8_t *>(out.frame_buffer);
                        // HEVC Main10 (the console's HDR): 16-bit samples, so twice the bytes
                        // per row; 8-bit pictures are NV12.
                        const bool ten = out.frame_pitch && out.frame_pitch_in_bytes == out.frame_pitch * 2;
                        if (ten != st.ten_bit || !st.shown)
                            log("video: pictures %ux%u, format %u, pitch %u (%u bytes), %s", out.frame_width,
                                out.frame_height, out.frame_format, out.frame_pitch, out.frame_pitch_in_bytes,
                                ten ? "10-bit (HDR, shown in SDR)" : "8-bit");
                        st.ten_bit = ten;
                        if (ten)
                        {
                            // The chroma plane starts after the coded height (32-row blocks).
                            const unsigned rows = (out.frame_height + 31) & ~31u;
                            if (shift < 0)
                                shift = hdr::shift_of(pic, out.frame_pitch_in_bytes, out.frame_width,
                                                      out.frame_height);
                            display::present_hdr10(pic, out.frame_pitch_in_bytes, out.frame_width,
                                                   out.frame_height, rows, shift);
                        }
                        else
                            display::present_nv12(pic, out.frame_pitch_in_bytes, out.frame_width, out.frame_height,
                                                  out.frame_height);
                        ++st.shown;
                    }
                    frame_back(dec, out.frame_buffer);
                }
            }
        }
    }
    return nullptr;
}
} // namespace

bool init() noexcept
{
    const std::uint64_t t0 = net::now_us();
    int rc = sceSysmoduleLoadModule(207);
    log("video: load libSceVideodec2 %#x in %.1f ms", rc, static_cast<double>(net::now_us() - t0) / 1000.0);
    VdecComputeMemory info{};
    info.this_size = sizeof info;
    rc = sceVideodec2QueryComputeMemoryInfo(&info);
    if (rc != 0 || !mem_direct(queue_mem, info.cpu_gpu_memory_size, k16K, 12, kProtCpuGpuRw))
    {
        log("video: compute memory query %#x", rc);
        return false;
    }
    info.cpu_gpu_memory = queue_mem.addr;
    info.cpu_gpu_memory_size = queue_mem.len;
    VdecComputeConfig config{};
    config.this_size = sizeof config;
    rc = sceVideodec2AllocateComputeQueue(&config, &info, &compute_queue);
    log("video: compute queue %#x", rc);
    if (rc != 0)
        return false;
    for (auto &s : slots)
    {
        s.data = big_alloc(kAuMax);
        if (!s.data)
            return false;
    }
    pthread_t t;
    return pthread_create(&t, nullptr, thread_main, nullptr) == 0;
}

void begin(bool h) noexcept
{
    pthread_mutex_lock(&q_lock);
    active = true;
    hevc = h;
    st.hevc = h;
    waiting_key = true;
    header_len = 0;
    header_new = false;
    q_count = 0;
    ++generation;
    pthread_cond_signal(&q_cond);
    pthread_mutex_unlock(&q_lock);
}

void end() noexcept
{
    pthread_mutex_lock(&q_lock);
    active = false;
    q_count = 0;
    pthread_mutex_unlock(&q_lock);
}

void header(const std::uint8_t *d, std::size_t n) noexcept
{
    if (n > sizeof header_buf)
        return;
    pthread_mutex_lock(&q_lock);
    std::memcpy(header_buf, d, n);
    header_len = n;
    header_new = true;
    waiting_key = true; // new parameter sets: start again at a keyframe
    pthread_mutex_unlock(&q_lock);
}

void access_unit(const std::uint8_t *d, std::size_t n) noexcept
{
    pthread_mutex_lock(&q_lock);
    if (!active)
    {
        pthread_mutex_unlock(&q_lock);
        return;
    }
    const bool key = proto::is_keyframe(d, n, hevc);
    if (waiting_key && !key)
    {
        ++st.dropped;
        pthread_mutex_unlock(&q_lock);
        return;
    }
    if (q_count == kQueue)
    {
        // Far behind: drop what waits and start again at a keyframe.
        st.dropped += q_count;
        q_count = 0;
        ++st.key_waits;
        if (!key)
        {
            waiting_key = true;
            ++st.dropped;
            pthread_mutex_unlock(&q_lock);
            return;
        }
    }
    Slot &s = slots[(q_head + q_count) % kQueue];
    std::size_t at = 0;
    if (key && header_len && header_len + n <= kAuMax)
    {
        // The parameter sets go in front of the keyframe, in the same access unit.
        std::memcpy(s.data, header_buf, header_len);
        at = header_len;
        header_new = false;
    }
    if (at + n > kAuMax)
    {
        ++st.dropped;
        pthread_mutex_unlock(&q_lock);
        return;
    }
    std::memcpy(s.data + at, d, n);
    s.len = at + n;
    ++q_count;
    if (key)
        waiting_key = false;
    pthread_cond_signal(&q_cond);
    pthread_mutex_unlock(&q_lock);
}

bool wants_keyframe() noexcept
{
    pthread_mutex_lock(&q_lock);
    const bool w = active && waiting_key;
    pthread_mutex_unlock(&q_lock);
    return w;
}

Stats stats() noexcept
{
    pthread_mutex_lock(&q_lock);
    Stats s = st;
    pthread_mutex_unlock(&q_lock);
    s.decode_ms = decode_avg;
    return s;
}
} // namespace play::ps5::video
