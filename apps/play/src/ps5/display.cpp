// PS5Upload Play - the screen.
// Phase 0 (docs/PHASE0.md): sceVideoOut takes RGBA8 sRGB only in the tiled layout; NV12 to RGBA
// on the CPU costs about 3 ms per 1080p picture on 4 threads. The tiled address function is
// PS5Term's (src/demo_renderer.cpp, GPL-3.0-or-later), split into its row and column parts.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "display.hpp"

#include "../app/log.hpp"
#include "../net/net.hpp"
#include "sys.hpp"

#include <atomic>
#include <cstring>
#include <pthread.h>

namespace play::ps5::display
{
namespace
{
constexpr unsigned kW = 1920, kH = 1080, kBuffers = 3, kThreads = 4; // kThreads: workers + caller
constexpr std::size_t kFrameBytes = 0x1000000;
constexpr std::uint64_t kFormatRgba8Srgb = UINT64_C(0x8000000022000000);

int video = -1;
Mem fb_mem;
std::uint8_t *fbs[kBuffers];
unsigned next_buf = 0;
std::uint64_t flip_arg = 0;
bool status_ok = true;
pthread_mutex_t present_lock = PTHREAD_MUTEX_INITIALIZER;
std::atomic<std::uint64_t> last_video_us{0};
Stats st;
float convert_avg = 0;

// The tiled layout: address = row part + column part (PS5Term's function, separated).
std::uint32_t col_xor[kW], col_block[kW];
std::uint32_t row_xor[kH], row_block[kH];

void tables() noexcept
{
    for (unsigned x = 0; x < kW; ++x)
    {
        col_xor[x] = ((x << 2) & 0xcU) ^ ((x << 5) & 0x380U) ^ ((x << 4) & 0x400U) ^ ((x << 6) & 0x800U) ^
                     ((x << 9) & 0xa000U);
        col_block[x] = (x >> 7) << 16;
    }
    const std::uint32_t blocks_per_row = (kW + 127U) >> 7;
    for (unsigned y = 0; y < kH; ++y)
    {
        row_xor[y] = ((y << 4) & 0x70U) ^ ((y << 5) & 0xf00U) ^ ((y << 9) & 0x1000U) ^ ((y << 8) & 0x4000U);
        row_block[y] = ((y >> 7) * blocks_per_row) << 16;
    }
}

inline std::uint32_t *at(std::uint8_t *fb, unsigned x, unsigned y) noexcept
{
    return reinterpret_cast<std::uint32_t *>(fb + row_block[y] + col_block[x] + (row_xor[y] ^ col_xor[x]));
}

void flush(void *address, std::size_t length) noexcept
{
    auto *p = static_cast<std::uint8_t *>(address);
    for (auto *end = p + length; p < end; p += 64)
        __asm__ volatile("clflush (%0)" : : "r"(p) : "memory");
    __asm__ volatile("mfence" ::: "memory");
}

// ---- the overlay, kept here so a picture never reads a half-drawn one ----------------------
std::uint32_t *ov = nullptr; // full-size layer; only the bands are used
int ov_top = 0, ov_bottom = 0;
pthread_mutex_t ov_lock = PTHREAD_MUTEX_INITIALIZER;

// ---- worker threads -------------------------------------------------------------------------
struct Job
{
    enum Kind
    {
        copy_ui,
        nv12,
    } kind;
    std::uint8_t *dst;
    const std::uint32_t *ui;
    const std::uint8_t *y, *uv;
    std::size_t pitch;
    unsigned w, h;
};
Job job;
unsigned generation = 0, done = 0;
pthread_mutex_t job_lock = PTHREAD_MUTEX_INITIALIZER;
pthread_cond_t job_start = PTHREAD_COND_INITIALIZER, job_end = PTHREAD_COND_INITIALIZER;
// scaling tables for the current picture size
unsigned map_w = 0, map_h = 0;
std::uint16_t map_x[kW], map_y[kH];

inline std::uint8_t clamp8(int v) noexcept
{
    return static_cast<std::uint8_t>(v < 0 ? 0 : v > 255 ? 255 : v);
}

void rows(unsigned part, unsigned parts) noexcept
{
    const unsigned y0 = kH * part / parts, y1 = kH * (part + 1) / parts;
    if (job.kind == Job::copy_ui)
    {
        for (unsigned y = y0; y < y1; ++y)
        {
            const std::uint32_t *src = job.ui + static_cast<std::size_t>(y) * kW;
            for (unsigned x = 0; x < kW; ++x)
                *at(job.dst, x, y) = src[x];
        }
        return;
    }
    const bool scale = job.w != kW || job.h != kH;
    for (unsigned y = y0; y < y1; ++y)
    {
        const unsigned sy = scale ? map_y[y] : y;
        const std::uint8_t *yr = job.y + static_cast<std::size_t>(sy) * job.pitch;
        const std::uint8_t *uvr = job.uv + static_cast<std::size_t>(sy / 2) * job.pitch;
        const bool blend = static_cast<int>(y) < ov_top || static_cast<int>(y) >= static_cast<int>(kH) - ov_bottom;
        const std::uint32_t *orow = ov + static_cast<std::size_t>(y) * kW;
        for (unsigned x = 0; x < kW; ++x)
        {
            const unsigned sx = scale ? map_x[x] : x;
            // BT.709, limited range (the console's Remote Play stream)
            const int c = 298 * (yr[sx] - 16) + 128;
            const int d = uvr[sx & ~1u] - 128, e = uvr[sx | 1u] - 128;
            std::uint32_t r = clamp8((c + 459 * e) >> 8);
            std::uint32_t g = clamp8((c - 55 * d - 136 * e) >> 8);
            std::uint32_t b = clamp8((c + 541 * d) >> 8);
            if (blend)
            {
                const std::uint32_t o = orow[x];
                const std::uint32_t a = o >> 24;
                if (a)
                {
                    const std::uint32_t ia = 255 - a;
                    r = ((o & 0xff) * a + r * ia) / 255;
                    g = (((o >> 8) & 0xff) * a + g * ia) / 255;
                    b = (((o >> 16) & 0xff) * a + b * ia) / 255;
                }
            }
            *at(job.dst, x, y) = 0xff000000u | (b << 16) | (g << 8) | r;
        }
    }
}

void *worker(void *arg) noexcept
{
    const unsigned index = static_cast<unsigned>(reinterpret_cast<std::uintptr_t>(arg));
    unsigned seen = 0;
    for (;;)
    {
        pthread_mutex_lock(&job_lock);
        while (generation == seen)
            pthread_cond_wait(&job_start, &job_lock);
        seen = generation;
        pthread_mutex_unlock(&job_lock);
        rows(index + 1, kThreads);
        pthread_mutex_lock(&job_lock);
        ++done;
        pthread_cond_signal(&job_end);
        pthread_mutex_unlock(&job_lock);
    }
    return nullptr;
}

void run_job() noexcept
{
    pthread_mutex_lock(&job_lock);
    done = 0;
    ++generation;
    pthread_cond_broadcast(&job_start);
    pthread_mutex_unlock(&job_lock);
    rows(0, kThreads);
    pthread_mutex_lock(&job_lock);
    while (done < kThreads - 1)
        pthread_cond_wait(&job_end, &job_lock);
    pthread_mutex_unlock(&job_lock);
}

// The buffer to draw next: the one after the last shown, once no flip is still waiting.
std::uint8_t *acquire() noexcept
{
    if (status_ok)
    {
        for (int i = 0; i < 40; ++i)
        {
            VideoFlipStatus s{};
            const int rc = sceVideoOutGetFlipStatus(video, &s);
            if (rc < 0)
            {
                log("display: sceVideoOutGetFlipStatus %#x: pacing by vblank instead", rc);
                status_ok = false;
                break;
            }
            // Three buffers: one on screen, one waiting for its vblank, one being drawn.
            if (s.flip_pending_num <= 1)
                break;
            ++st.waits;
            sceKernelUsleep(1000);
        }
    }
    return fbs[next_buf];
}

void flip() noexcept
{
    flush(fbs[next_buf], kFrameBytes);
    sceVideoOutSubmitFlip(video, static_cast<int>(next_buf), 1, static_cast<std::int64_t>(++flip_arg));
    ++st.flips;
    next_buf = (next_buf + 1) % kBuffers;
    if (!status_ok)
        sceVideoOutWaitVblank(video);
}
} // namespace

bool open() noexcept
{
    tables();
    video = sceVideoOutOpen(0xff, 0, 0, nullptr);
    log("display: sceVideoOutOpen %#x", video);
    if (video < 0)
        return false;
    // Write-combined memory, as PS5Term's renderer uses for its framebuffers.
    if (!mem_direct(fb_mem, kFrameBytes * kBuffers, 0x200000, 3, kProtCpuGpuRw))
        return false;
    VideoBuffer bufs[kBuffers];
    for (unsigned i = 0; i < kBuffers; ++i)
    {
        fbs[i] = static_cast<std::uint8_t *>(fb_mem.addr) + kFrameBytes * i;
        std::memset(fbs[i], 0, kFrameBytes);
        bufs[i] = {fbs[i], nullptr, nullptr, nullptr};
    }
    flush(fb_mem.addr, fb_mem.len);
    sceVideoOutSetFlipRate(video, 0);
    VideoAttribute attr{};
    sceVideoOutSetBufferAttribute2(&attr, kFormatRgba8Srgb, 0 /* tiled */, kW, kH, 0, 0, 0);
    const int rc = sceVideoOutRegisterBuffers2(video, 0, 0, bufs, kBuffers, &attr, 0, nullptr);
    log("display: register %u tiled RGBA8 buffers: %#x", kBuffers, rc);
    if (rc < 0)
        return false;
    ov = reinterpret_cast<std::uint32_t *>(big_alloc(kW * kH * 4));
    if (ov)
        std::memset(ov, 0, kW * kH * 4);
    for (unsigned i = 0; i + 1 < kThreads; ++i)
    {
        pthread_t t;
        if (pthread_create(&t, nullptr, worker, reinterpret_cast<void *>(static_cast<std::uintptr_t>(i))) != 0)
            log("display: worker %u failed", i);
    }
    return true;
}

void present_ui(const std::uint32_t *ui) noexcept
{
    pthread_mutex_lock(&present_lock);
    job.kind = Job::copy_ui;
    job.dst = acquire();
    job.ui = ui;
    run_job();
    flip();
    pthread_mutex_unlock(&present_lock);
}

void present_nv12(const std::uint8_t *y, std::size_t pitch, unsigned w, unsigned h, unsigned luma_rows) noexcept
{
    if (!w || !h || w > 4096 || h > 2304)
        return;
    // Decoders round the height up to whole macroblocks: 1088 holds a 1080 picture.
    if (h == 1088)
        h = 1080;
    else if (h == 544)
        h = 540;
    pthread_mutex_lock(&present_lock);
    if (w != map_w || h != map_h)
    {
        for (unsigned x = 0; x < kW; ++x)
            map_x[x] = static_cast<std::uint16_t>(x * w / kW);
        for (unsigned yy = 0; yy < kH; ++yy)
            map_y[yy] = static_cast<std::uint16_t>(yy * h / kH);
        map_w = w;
        map_h = h;
        log("display: pictures are %ux%u (pitch %zu)", w, h, pitch);
    }
    job.kind = Job::nv12;
    job.dst = acquire();
    const std::uint64_t t0 = net::now_us();
    job.y = y;
    job.uv = y + pitch * luma_rows;
    job.pitch = pitch;
    job.w = w;
    job.h = h;
    pthread_mutex_lock(&ov_lock);
    if (!ov)
        ov_top = ov_bottom = 0;
    run_job();
    pthread_mutex_unlock(&ov_lock);
    flip();
    const float ms = static_cast<float>(net::now_us() - t0) / 1000.0f;
    pthread_mutex_unlock(&present_lock);
    last_video_us = net::now_us();
    convert_avg = convert_avg * 0.95f + ms * 0.05f;
}

void set_overlay(const std::uint32_t *layer, int top, int bottom) noexcept
{
    if (!ov)
        return;
    top = top < 0 ? 0 : top > static_cast<int>(kH) / 2 ? static_cast<int>(kH) / 2 : top;
    bottom = bottom < 0 ? 0 : bottom > static_cast<int>(kH) / 2 ? static_cast<int>(kH) / 2 : bottom;
    pthread_mutex_lock(&ov_lock);
    if (top)
        std::memcpy(ov, layer, static_cast<std::size_t>(top) * kW * 4);
    if (bottom)
    {
        const std::size_t from = static_cast<std::size_t>(kH - static_cast<unsigned>(bottom)) * kW;
        std::memcpy(ov + from, layer + from, static_cast<std::size_t>(bottom) * kW * 4);
    }
    ov_top = top;
    ov_bottom = bottom;
    pthread_mutex_unlock(&ov_lock);
}

std::uint64_t since_video_us() noexcept
{
    const std::uint64_t t = last_video_us.load();
    return t ? net::now_us() - t : ~0ull;
}

Stats stats() noexcept
{
    Stats s = st;
    s.convert_ms = convert_avg;
    return s;
}
} // namespace play::ps5::display
