// PS5Upload Play - phase 0 spike: controller, hardware video decode, display, sound.
//
// Runs a fixed test sequence at start, then plays the H.264 test stream in a
// loop with the pad state drawn on top, and takes commands from
// /data/ps5upload-play-spike/cmd (avc, hevc, play-avc, play-hevc, tone,
// rumble, probe, tiled, linear, exit). Every measurement goes to
// /data/ps5upload-play-spike/log.txt.
//
// The libSceVideodec2 structure layouts and call order follow the PS5 VLC
// port's ps5/modules/ps5vdec.c (GPL-2.0-or-later, https://github.com/ZiZc3/VLC);
// this file is our own code. The tiled framebuffer address function is from
// PS5Term's src/demo_renderer.cpp (GPL-3.0-or-later).
//
// SPDX-License-Identifier: GPL-3.0-or-later

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <new>
#include <pthread.h>
#include <stdarg.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

extern "C"
{
    // kernel
    std::size_t sceKernelGetDirectMemorySize();
    int sceKernelAllocateDirectMemory(std::int64_t start, std::int64_t end, std::size_t len,
                                      std::size_t align, int type, std::int64_t *phys);
    int sceKernelMapDirectMemory(void **addr, std::size_t len, int prot, int flags,
                                 std::int64_t phys, std::size_t align);
    int sceKernelReleaseDirectMemory(std::int64_t phys, std::size_t len);
    int sceKernelMapFlexibleMemory(void **addr, std::size_t len, int prot, int flags);
    int sceKernelAvailableFlexibleMemorySize(std::size_t *size);
    int sceKernelAvailableDirectMemorySize(std::int64_t start, std::int64_t end, std::size_t align,
                                           std::int64_t *phys, std::size_t *size);
    int sceKernelMunmap(void *addr, std::size_t len);
    int sceKernelUsleep(std::uint32_t us);
    int sceKernelSendNotificationRequest(int device, void *request, std::size_t size, int blocking);
    // system
    int sceSysmoduleLoadModule(std::uint16_t id);
    int sceSystemServiceHideSplashScreen();
    int sceSystemServiceLoadExec(const char *path, const char *const *argv);
    int sceSystemServicePowerTick();
    int sceShellCoreUtilGetAutoPowerDownRemainingSeconds(int *seconds);
    int sceUserServiceInitialize(const void *params);
    int sceUserServiceGetInitialUser(std::int32_t *user);
    // video out
    int sceVideoOutOpen(std::int32_t user, std::int32_t bus, std::int32_t index, const void *param);
    int sceVideoOutClose(std::int32_t handle);
    int sceVideoOutSetFlipRate(std::int32_t handle, std::int32_t rate);
    int sceVideoOutSubmitFlip(std::int32_t handle, std::int32_t index, std::uint32_t mode,
                              std::int64_t arg);
    int sceVideoOutWaitVblank(std::int32_t handle);
    int sceVideoOutUnregisterBuffers(std::int32_t handle, std::int32_t set_index);
    // pad
    int scePadInit();
    int scePadOpen(std::int32_t user, std::int32_t type, std::int32_t index, const void *param);
    int scePadRead(std::int32_t handle, void *samples, std::int32_t count);
    int scePadSetVibrationMode(std::int32_t handle, std::int32_t mode);
    int scePadSetVibration(std::int32_t handle, const void *vibration);
    int scePadSetLightBar(std::int32_t handle, const void *color);
    // audio
    int sceAudioOutInit();
    int sceAudioOutOpen(std::int32_t user, std::int32_t type, std::int32_t index, std::uint32_t len,
                        std::uint32_t freq, std::uint32_t param);
    int sceAudioOutOutput(std::int32_t handle, const void *buffer);
    int sceAudioOutClose(std::int32_t handle);
}

// ---- video out structures (PS5Term) -------------------------------------------------------
struct VideoBuffer
{
    void *data;
    void *metadata;
    void *reserved0;
    void *reserved1;
};
struct VideoAttribute
{
    std::uint8_t reserved[80];
};
extern "C" void sceVideoOutSetBufferAttribute2(VideoAttribute *attribute, std::uint64_t format,
                                               std::uint32_t tiling, std::uint32_t width,
                                               std::uint32_t height, std::uint64_t option,
                                               std::uint32_t dcc_control,
                                               std::uint64_t dcc_clear_color);
extern "C" int sceVideoOutRegisterBuffers2(std::int32_t handle, std::int32_t set_index,
                                           std::int32_t start, VideoBuffer *buffers,
                                           std::int32_t count, VideoAttribute *attribute,
                                           std::int32_t category, void *option);

// ---- libSceVideodec2 (layouts as the VLC port found them) ---------------------------------
enum : std::uint32_t
{
    kCodecAvc = 1,
    kCodecHevc = 974921,
};
struct VdecConfig
{
    std::uint64_t this_size;
    std::uint32_t resource_type;
    std::uint32_t codec_type;
    std::uint32_t profile;
    std::uint32_t max_level;
    std::int32_t max_frame_width;
    std::int32_t max_frame_height;
    std::int32_t max_dpb_frame_count;
    std::uint32_t decode_pipeline_depth;
    void *compute_queue;
    std::uint64_t cpu_affinity_mask;
    std::int32_t cpu_thread_priority;
    bool optimize_progressive_video;
    bool check_memory_type;
    std::uint8_t reserved0;
    std::uint8_t reserved1;
    void *extra_config_info;
};
struct VdecMemory
{
    std::uint64_t this_size;
    std::uint64_t cpu_memory_size;
    void *cpu_memory;
    std::uint64_t gpu_memory_size;
    void *gpu_memory;
    std::uint64_t cpu_gpu_memory_size;
    void *cpu_gpu_memory;
    std::uint64_t max_frame_buffer_size;
    std::uint32_t frame_buffer_alignment;
    std::uint32_t reserved0;
};
struct VdecInput
{
    std::uint64_t this_size;
    void *au_data;
    std::uint64_t au_size;
    std::uint64_t pts_data;
    std::uint64_t dts_data;
    std::uint64_t attached_data;
};
struct VdecOutput
{
    std::uint64_t this_size;
    bool is_valid;
    bool is_error_frame;
    std::uint8_t picture_count;
    std::uint32_t codec_type;
    std::uint32_t frame_width;
    std::uint32_t frame_pitch;
    std::uint32_t frame_height;
    void *frame_buffer;
    std::uint64_t frame_buffer_size;
    std::uint32_t frame_format;
    std::uint32_t frame_pitch_in_bytes;
};
struct VdecFrameBuffer
{
    std::uint64_t this_size;
    void *frame_buffer;
    std::uint64_t frame_buffer_size;
    bool is_accepted;
};
struct VdecComputeMemory
{
    std::uint64_t this_size;
    std::uint64_t cpu_gpu_memory_size;
    void *cpu_gpu_memory;
};
struct VdecComputeConfig
{
    std::uint64_t this_size;
    std::uint16_t compute_pipe_id;
    std::uint16_t compute_queue_id;
    bool check_memory_type;
    std::uint8_t reserved0;
    std::uint16_t reserved1;
};
static_assert(sizeof(VdecConfig) == 0x48);
static_assert(sizeof(VdecMemory) == 0x48);
static_assert(sizeof(VdecInput) == 0x30);
static_assert(sizeof(VdecOutput) == 0x38);
static_assert(sizeof(VdecFrameBuffer) == 0x20);
static_assert(sizeof(VdecComputeMemory) == 0x18);
static_assert(sizeof(VdecComputeConfig) == 0x10);
extern "C"
{
    int sceVideodec2QueryComputeMemoryInfo(VdecComputeMemory *info);
    int sceVideodec2AllocateComputeQueue(const VdecComputeConfig *config,
                                         const VdecComputeMemory *memory, void **queue);
    int sceVideodec2QueryDecoderMemoryInfo(const VdecConfig *config, VdecMemory *memory);
    int sceVideodec2CreateDecoder(const VdecConfig *config, const VdecMemory *memory,
                                  void **decoder);
    int sceVideodec2DeleteDecoder(void *decoder);
    int sceVideodec2Decode(void *decoder, const VdecInput *input, VdecFrameBuffer *frame,
                           VdecOutput *output);
    int sceVideodec2Flush(void *decoder, VdecFrameBuffer *frame, VdecOutput *output);
}

// ---- pad sample (120 bytes, PS5Term / VLC port layout) ------------------------------------
struct PadSample
{
    std::uint32_t buttons;
    std::uint8_t lx, ly, rx, ry, l2, r2;
    std::uint8_t reserved[66];
    std::int32_t connected;
    std::uint64_t timestamp;
    std::uint8_t extension[16];
    std::uint8_t connected_count;
    std::uint8_t remaining[15];
};
static_assert(sizeof(PadSample) == 120);
static_assert(offsetof(PadSample, connected) == 0x4c);
constexpr std::uint32_t kPadIntercepted = 0x80000000u;

// ---- helpers ------------------------------------------------------------------------------
namespace
{
constexpr const char *kDir = "/data/ps5upload-play-spike";
constexpr const char *kLog = "/data/ps5upload-play-spike/log.txt";
constexpr const char *kCmd = "/data/ps5upload-play-spike/cmd";
constexpr int kProtCpuGpuRw = 0x33;
constexpr std::size_t k16K = 0x4000;
int log_fd = -1;

std::uint64_t now_us()
{
    timespec ts{};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return static_cast<std::uint64_t>(ts.tv_sec) * 1000000u + ts.tv_nsec / 1000;
}
std::uint64_t start_us;

void logf(const char *format, ...) __attribute__((format(printf, 1, 2)));
void logf(const char *format, ...)
{
    char line[1024];
    const std::uint64_t t = now_us() - start_us;
    int n = snprintf(line, sizeof line, "[%7.3f] ", t / 1e6);
    va_list args;
    va_start(args, format);
    n += vsnprintf(line + n, sizeof line - n - 2, format, args);
    va_end(args);
    if (n > static_cast<int>(sizeof line) - 2)
        n = sizeof line - 2;
    line[n++] = '\n';
    if (log_fd >= 0)
        (void)write(log_fd, line, n);
}

void notify(const char *text)
{
    static std::uint8_t request[0xc30];
    std::memset(request, 0, sizeof request);
    snprintf(reinterpret_cast<char *>(request) + 0x2d, 1024, "%s", text);
    (void)sceKernelSendNotificationRequest(0, request, sizeof request, 0);
}

std::size_t round_up(std::size_t v, std::size_t a)
{
    return (v + a - 1) & ~(a - 1);
}

struct Mem
{
    void *addr = nullptr;
    std::size_t len = 0;
    std::int64_t phys = -1;
};

bool mem_direct(Mem &m, std::size_t len, std::size_t align, int type, int prot)
{
    if (align < k16K)
        align = k16K;
    m.len = round_up(len, k16K);
    int rc = sceKernelAllocateDirectMemory(0, static_cast<std::int64_t>(sceKernelGetDirectMemorySize()),
                                           m.len, align, type, &m.phys);
    if (rc != 0)
    {
        logf("direct memory %zu KiB type %d: allocate %#x", m.len >> 10, type, rc);
        m.phys = -1;
        return false;
    }
    rc = sceKernelMapDirectMemory(&m.addr, m.len, prot, 0, m.phys, align);
    if (rc != 0)
    {
        logf("direct memory %zu KiB type %d: map %#x", m.len >> 10, type, rc);
        sceKernelReleaseDirectMemory(m.phys, m.len);
        m = {};
        return false;
    }
    return true;
}

void mem_free(Mem &m)
{
    if (m.addr && m.phys >= 0)
    {
        sceKernelMunmap(m.addr, m.len);
        sceKernelReleaseDirectMemory(m.phys, m.len);
    }
    m = {};
}

// Big buffers come from flexible memory: the app's malloc heap is small (a second 9 MiB
// new[] failed on the console).
std::uint8_t *big_alloc(std::size_t size)
{
    void *addr = nullptr;
    const int rc = sceKernelMapFlexibleMemory(&addr, round_up(size, k16K), 0x03, 0);
    if (rc != 0)
    {
        logf("flexible memory %zu KiB: %#x", size >> 10, rc);
        return nullptr;
    }
    return static_cast<std::uint8_t *>(addr);
}

std::uint8_t *read_file(const char *path, std::size_t *size)
{
    int fd = open(path, O_RDONLY);
    if (fd < 0)
        return nullptr;
    struct stat st{};
    if (fstat(fd, &st) != 0 || st.st_size <= 0)
    {
        close(fd);
        return nullptr;
    }
    auto *data = big_alloc(st.st_size);
    std::size_t got = 0;
    while (data && got < static_cast<std::size_t>(st.st_size))
    {
        long r = read(fd, data + got, st.st_size - got);
        if (r <= 0)
            break;
        got += r;
    }
    close(fd);
    *size = got;
    return data;
}

// ---- Annex B access units (the test streams carry an AUD before every picture) ------------
struct Stream
{
    std::uint8_t *data = nullptr;
    std::size_t size = 0;
    std::size_t offsets[1024];
    std::size_t sizes[1024];
    unsigned count = 0;
};

bool is_aud(const std::uint8_t *p, std::size_t left, bool hevc)
{
    if (left < 5 || p[0] != 0 || p[1] != 0)
        return false;
    std::size_t at = p[2] == 1 ? 3 : (p[2] == 0 && p[3] == 1 ? 4 : 0);
    if (!at)
        return false;
    return hevc ? ((p[at] >> 1) & 0x3f) == 35 : (p[at] & 0x1f) == 9;
}

bool load_stream(Stream &s, const char *path, bool hevc)
{
    s.data = read_file(path, &s.size);
    if (!s.data)
    {
        logf("%s: cannot read", path);
        return false;
    }
    s.count = 0;
    std::size_t last = 0;
    bool have = false;
    for (std::size_t i = 0; i + 5 < s.size; ++i)
    {
        if (s.data[i] == 0 && is_aud(s.data + i, s.size - i, hevc))
        {
            if (have && s.count < 1024)
            {
                s.offsets[s.count] = last;
                s.sizes[s.count++] = i - last;
            }
            last = i;
            have = true;
            i += 3;
        }
    }
    if (have && s.count < 1024)
    {
        s.offsets[s.count] = last;
        s.sizes[s.count++] = s.size - last;
    }
    std::size_t biggest = 0;
    for (unsigned i = 0; i < s.count; ++i)
        biggest = s.sizes[i] > biggest ? s.sizes[i] : biggest;
    logf("%s: %zu bytes, %u access units, biggest %zu bytes", path, s.size, s.count, biggest);
    return s.count > 0;
}

// ---- display --------------------------------------------------------------------------------
constexpr unsigned kW = 1920, kH = 1080;
constexpr std::size_t kFrameBytes = 0x1000000;
int video = -1;
Mem fb_mem;
std::uint8_t *fbs[4];
bool tiled = false;      // layout of the buffers being shown
bool linear_ok = false;  // set 0 (linear) registered
bool tiled_ok = false;   // set 1 (tiled) registered
unsigned flip_count = 0;

// PS5Term's 1080p tiled framebuffer address (src/demo_renderer.cpp, GPL-3.0-or-later).
constexpr std::size_t tiled_byte_offset(unsigned x, unsigned y)
{
    const std::uint32_t offset = ((y << 4) & 0x70U) ^ ((y << 5) & 0xf00U) ^ ((y << 9) & 0x1000U) ^
                                 ((y << 8) & 0x4000U) ^ ((x << 2) & 0xcU) ^ ((x << 5) & 0x380U) ^
                                 ((x << 4) & 0x400U) ^ ((x << 6) & 0x800U) ^ ((x << 9) & 0xa000U);
    const std::uint32_t blocks_per_row = (kW + 127U) >> 7;
    const std::uint32_t block_index = (y >> 7) * blocks_per_row + (x >> 7);
    return (static_cast<std::size_t>(block_index) << 16) + offset;
}

inline void put(std::uint8_t *fb, unsigned x, unsigned y, std::uint32_t c)
{
    const std::size_t at = tiled ? tiled_byte_offset(x, y) : (static_cast<std::size_t>(y) * kW + x) * 4;
    *reinterpret_cast<std::uint32_t *>(fb + at) = c;
}

void fill(std::uint8_t *fb, unsigned x0, unsigned y0, unsigned w, unsigned h, std::uint32_t c)
{
    for (unsigned y = y0; y < y0 + h && y < kH; ++y)
        for (unsigned x = x0; x < x0 + w && x < kW; ++x)
            put(fb, x, y, c);
}

void flush(void *address, std::size_t length)
{
    auto *at = static_cast<std::uint8_t *>(address);
    for (auto *end = at + length; at < end; at += 64)
        __asm__ volatile("clflush (%0)" : : "r"(at) : "memory");
    __asm__ volatile("mfence" ::: "memory");
}

bool display_open()
{
    video = sceVideoOutOpen(0xff, 0, 0, nullptr);
    logf("sceVideoOutOpen: %#x", video);
    if (video < 0)
        return false;
    // WC "garlic" memory, as PS5Term's renderer uses for its buffers.
    if (!mem_direct(fb_mem, kFrameBytes * 4, 0x200000, 3, kProtCpuGpuRw))
        return false;
    for (int i = 0; i < 4; ++i)
    {
        fbs[i] = static_cast<std::uint8_t *>(fb_mem.addr) + kFrameBytes * i;
        std::memset(fbs[i], 0, kFrameBytes);
    }
    flush(fb_mem.addr, fb_mem.len);
    sceVideoOutSetFlipRate(video, 0);
    // Set 0: linear RGBA8 sRGB (buffers 0, 1). Set 1: tiled (buffers 2, 3), PS5Term's layout.
    VideoAttribute attr{};
    sceVideoOutSetBufferAttribute2(&attr, UINT64_C(0x8000000022000000), 1, kW, kH, 0, 0, 0);
    VideoBuffer lin[2] = {{fbs[0], nullptr, nullptr, nullptr}, {fbs[1], nullptr, nullptr, nullptr}};
    int rc = sceVideoOutRegisterBuffers2(video, 0, 0, lin, 2, &attr, 0, nullptr);
    linear_ok = rc >= 0;
    logf("register linear RGBA8 (tiling 1): %#x", rc);
    VideoAttribute attr2{};
    sceVideoOutSetBufferAttribute2(&attr2, UINT64_C(0x8000000022000000), 0, kW, kH, 0, 0, 0);
    VideoBuffer til[2] = {{fbs[2], nullptr, nullptr, nullptr}, {fbs[3], nullptr, nullptr, nullptr}};
    rc = sceVideoOutRegisterBuffers2(video, 1, 2, til, 2, &attr2, 0, nullptr);
    tiled_ok = rc >= 0;
    logf("register tiled RGBA8 (tiling 0): %#x", rc);
    tiled = !linear_ok;
    return linear_ok || tiled_ok;
}

// The buffer to draw next, and showing it.
std::uint8_t *back_buffer()
{
    const unsigned base = tiled ? 2 : 0;
    return fbs[base + ((flip_count + 1) & 1)];
}

std::uint64_t vblank_wait_us;
void show()
{
    const unsigned base = tiled ? 2 : 0;
    ++flip_count;
    sceVideoOutSubmitFlip(video, static_cast<int>(base + (flip_count & 1)), 1, flip_count);
    const std::uint64_t t = now_us();
    sceVideoOutWaitVblank(video);
    vblank_wait_us = now_us() - t;
}

// ---- NV12 -> RGBA (BT.601 limited range), split over worker threads ------------------------
struct ConvertJob
{
    const std::uint8_t *y;
    const std::uint8_t *uv;
    std::size_t pitch;
    std::uint8_t *dst;
    unsigned row0, row1;
};

inline std::uint8_t clamp8(int v)
{
    return static_cast<std::uint8_t>(v < 0 ? 0 : v > 255 ? 255 : v);
}

void convert_rows(const ConvertJob &j)
{
    for (unsigned y = j.row0; y < j.row1; ++y)
    {
        const std::uint8_t *yr = j.y + y * j.pitch;
        const std::uint8_t *uvr = j.uv + (y / 2) * j.pitch;
        if (!tiled)
        {
            auto *out = reinterpret_cast<std::uint32_t *>(j.dst + static_cast<std::size_t>(y) * kW * 4);
            for (unsigned x = 0; x < kW; ++x)
            {
                const int c = 298 * (yr[x] - 16) + 128;
                const int d = uvr[x & ~1u] - 128, e = uvr[x | 1u] - 128;
                const std::uint32_t r = clamp8((c + 409 * e) >> 8);
                const std::uint32_t g = clamp8((c - 100 * d - 208 * e) >> 8);
                const std::uint32_t b = clamp8((c + 516 * d) >> 8);
                out[x] = 0xff000000u | (b << 16) | (g << 8) | r;
            }
        }
        else
        {
            for (unsigned x = 0; x < kW; ++x)
            {
                const int c = 298 * (yr[x] - 16) + 128;
                const int d = uvr[x & ~1u] - 128, e = uvr[x | 1u] - 128;
                const std::uint32_t r = clamp8((c + 409 * e) >> 8);
                const std::uint32_t g = clamp8((c - 100 * d - 208 * e) >> 8);
                const std::uint32_t b = clamp8((c + 516 * d) >> 8);
                *reinterpret_cast<std::uint32_t *>(j.dst + tiled_byte_offset(x, y)) =
                    0xff000000u | (b << 16) | (g << 8) | r;
            }
        }
    }
}

// Workers sleep on a condition variable between pictures: spinning ones took the cores the
// decoder's own thread needs.
constexpr unsigned kMaxWorkers = 8;
unsigned workers = 1;  // threads besides the caller
ConvertJob jobs[kMaxWorkers + 1];
unsigned job_generation = 0, jobs_done = 0;
pthread_mutex_t job_lock = PTHREAD_MUTEX_INITIALIZER;
pthread_cond_t job_start = PTHREAD_COND_INITIALIZER, job_end = PTHREAD_COND_INITIALIZER;
pthread_t worker_threads[kMaxWorkers];

void *worker_main(void *arg)
{
    const unsigned index = static_cast<unsigned>(reinterpret_cast<std::uintptr_t>(arg));
    unsigned seen = 0;
    for (;;)
    {
        pthread_mutex_lock(&job_lock);
        while (job_generation == seen)
            pthread_cond_wait(&job_start, &job_lock);
        seen = job_generation;
        const bool mine = index < workers;
        pthread_mutex_unlock(&job_lock);
        if (mine)
            convert_rows(jobs[index + 1]);
        pthread_mutex_lock(&job_lock);
        ++jobs_done;
        pthread_cond_signal(&job_end);
        pthread_mutex_unlock(&job_lock);
    }
    return nullptr;
}
unsigned started_workers = 0;

void start_workers(unsigned n)
{
    if (started_workers == 0)
        logf("worker sync init: mutex %d, conds %d %d", pthread_mutex_init(&job_lock, nullptr),
             pthread_cond_init(&job_start, nullptr), pthread_cond_init(&job_end, nullptr));
    for (; started_workers < n && started_workers < kMaxWorkers; ++started_workers)
    {
        int rc = pthread_create(&worker_threads[started_workers], nullptr, worker_main,
                                reinterpret_cast<void *>(static_cast<std::uintptr_t>(started_workers)));
        if (rc != 0)
        {
            logf("pthread_create %u: %d", started_workers, rc);
            break;
        }
    }
}

// Convert one decoded picture into dst using `threads` threads (1 = caller only).
void convert(const std::uint8_t *picture, std::size_t pitch, unsigned luma_rows, std::uint8_t *dst,
             unsigned threads)
{
    const std::uint8_t *uv = picture + pitch * luma_rows;
    if (threads <= 1 || started_workers == 0)
    {
        convert_rows({picture, uv, pitch, dst, 0, kH});
        return;
    }
    if (threads > started_workers + 1)
        threads = started_workers + 1;
    const unsigned per = ((kH / threads) + 1) & ~1u;
    for (unsigned i = 0; i < threads; ++i)
    {
        unsigned a = i * per, b = (i + 1) * per;
        jobs[i] = {picture, uv, pitch, dst, a > kH ? kH : a, b > kH || i + 1 == threads ? kH : b};
    }
    pthread_mutex_lock(&job_lock);
    workers = threads - 1;
    jobs_done = 0;
    ++job_generation;
    pthread_cond_broadcast(&job_start);
    pthread_mutex_unlock(&job_lock);
    convert_rows(jobs[0]);
    pthread_mutex_lock(&job_lock);
    while (jobs_done < started_workers)
        pthread_cond_wait(&job_end, &job_lock);
    pthread_mutex_unlock(&job_lock);
}

// FNV-1a 64 over 8-byte words of the visible NV12 picture (tools/nv12-hashes.c).
std::uint64_t picture_hash(const std::uint8_t *picture, std::size_t pitch, unsigned luma_rows)
{
    std::uint64_t h = 0xcbf29ce484222325ull;
    for (unsigned y = 0; y < kH; ++y)
    {
        const std::uint8_t *row = picture + y * pitch;
        for (unsigned x = 0; x < kW; x += 8)
        {
            std::uint64_t w;
            std::memcpy(&w, row + x, 8);
            h ^= w;
            h *= 0x100000001b3ull;
        }
    }
    const std::uint8_t *uv = picture + pitch * luma_rows;
    for (unsigned y = 0; y < kH / 2; ++y)
    {
        const std::uint8_t *row = uv + y * pitch;
        for (unsigned x = 0; x < kW; x += 8)
        {
            std::uint64_t w;
            std::memcpy(&w, row + x, 8);
            h ^= w;
            h *= 0x100000001b3ull;
        }
    }
    return h;
}

// ---- pad ------------------------------------------------------------------------------------
int user_id = -1;
int pad = -1;
PadSample pad_last{};
bool pad_have = false;
unsigned pad_reads = 0, pad_samples = 0, pad_intercepted_samples = 0;

void pad_open()
{
    int a = sceUserServiceInitialize(nullptr);
    int b = sceUserServiceGetInitialUser(&user_id);
    int c = scePadInit();
    pad = scePadOpen(user_id, 0, 0, nullptr);
    logf("user service init %#x, initial user %#x (rc %#x), scePadInit %#x, scePadOpen %#x", a,
         user_id, b, c, pad);
    if (pad >= 0)
        logf("scePadSetVibrationMode(2): %#x", scePadSetVibrationMode(pad, 2));
}

void pad_poll()
{
    if (pad < 0)
        return;
    PadSample s[16];
    const int n = scePadRead(pad, s, 16);
    ++pad_reads;
    if (n < 0)
    {
        static int errors;
        if (errors++ < 5)
            logf("scePadRead: %#x", n);
        return;
    }
    for (int i = 0; i < n; ++i)
    {
        ++pad_samples;
        if (s[i].buttons & kPadIntercepted)
            ++pad_intercepted_samples;
        const PadSample &p = s[i];
        auto moved = [](int a, int b) { return a - b > 12 || b - a > 12; };
        if (!pad_have || p.buttons != pad_last.buttons || p.connected != pad_last.connected ||
            moved(p.lx, pad_last.lx) || moved(p.ly, pad_last.ly) || moved(p.rx, pad_last.rx) ||
            moved(p.ry, pad_last.ry) || moved(p.l2, pad_last.l2) || moved(p.r2, pad_last.r2))
        {
            logf("pad: buttons %08x%s connected %d sticks L %u,%u R %u,%u triggers %u %u ts %llu",
                 p.buttons, (p.buttons & kPadIntercepted) ? " (INTERCEPTED)" : "", p.connected,
                 p.lx, p.ly, p.rx, p.ry, p.l2, p.r2, static_cast<unsigned long long>(p.timestamp));
            pad_last = p;
            pad_have = true;
        }
    }
}

void rumble(std::uint8_t large, std::uint8_t small)
{
    const std::uint8_t v[2] = {large, small};
    int rc = scePadSetVibration(pad, v);
    logf("scePadSetVibration(%u, %u): %#x", large, small, rc);
}

void light(std::uint8_t r, std::uint8_t g, std::uint8_t b)
{
    const std::uint8_t c[4] = {r, g, b, 0};
    int rc = scePadSetLightBar(pad, c);
    static int logged;
    if (logged++ < 3)
        logf("scePadSetLightBar(%u,%u,%u): %#x", r, g, b, rc);
}

// Pad state over the picture, top right: 16 button squares, stick and trigger bars.
void draw_pad(std::uint8_t *fb)
{
    const std::uint32_t on = 0xff20e0ffu, off = 0xff303030u;
    unsigned x0 = kW - 16 * 26 - 40, y0 = 40;
    fill(fb, x0 - 10, y0 - 10, 16 * 26 + 20, 150, 0xff101010u);
    for (unsigned i = 0; i < 16; ++i)
        fill(fb, x0 + i * 26, y0, 20, 20, (pad_last.buttons >> i) & 1 ? on : off);
    const std::uint8_t v[6] = {pad_last.lx, pad_last.ly, pad_last.rx, pad_last.ry, pad_last.l2, pad_last.r2};
    for (unsigned i = 0; i < 6; ++i)
    {
        fill(fb, x0, y0 + 30 + i * 16, 256, 10, off);
        fill(fb, x0, y0 + 30 + i * 16, v[i], 10, 0xff40ff40u);
    }
    if (pad_last.buttons & kPadIntercepted)
        fill(fb, x0 + 300, y0 + 30, 100, 90, 0xff0000ffu);
}

// ---- decoder --------------------------------------------------------------------------------
Mem queue_mem;
void *compute_queue = nullptr;
void *cpu_block = nullptr;
std::size_t cpu_block_len = 0;

bool compute_queue_open()
{
    VdecComputeMemory info{};
    info.this_size = sizeof info;
    int rc = sceVideodec2QueryComputeMemoryInfo(&info);
    logf("sceVideodec2QueryComputeMemoryInfo: %#x, cpu+gpu %llu KiB", rc,
         static_cast<unsigned long long>(info.cpu_gpu_memory_size >> 10));
    if (rc != 0 || !mem_direct(queue_mem, info.cpu_gpu_memory_size, k16K, 12, kProtCpuGpuRw))
        return false;
    info.cpu_gpu_memory = queue_mem.addr;
    info.cpu_gpu_memory_size = queue_mem.len;
    VdecComputeConfig config{};
    config.this_size = sizeof config;
    rc = sceVideodec2AllocateComputeQueue(&config, &info, &compute_queue);
    logf("sceVideodec2AllocateComputeQueue: %#x", rc);
    return rc == 0;
}

constexpr unsigned kFrames = 16, kAuSlots = 8;
constexpr std::size_t kAuSlotSize = 4u << 20;
struct Decoder
{
    void *dec = nullptr;
    std::uint32_t codec = 0;
    Mem gpu, cpu_gpu, frames, au[kAuSlots];
    std::size_t frame_size = 0;
    bool held[kFrames] = {};
    unsigned au_next = 0;
};

bool decoder_open(Decoder &d, std::uint32_t codec, std::uint32_t depth, int frame_type)
{
    d = {};
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
    c.decode_pipeline_depth = depth;
    c.compute_queue = compute_queue;
    c.cpu_affinity_mask = 0x3f;
    c.cpu_thread_priority = 700;
    c.optimize_progressive_video = true;
    VdecMemory m{};
    m.this_size = sizeof m;
    const std::uint64_t t0 = now_us();
    int rc = sceVideodec2QueryDecoderMemoryInfo(&c, &m);
    logf("%s depth %u: memory query %#x: cpu %llu KiB, gpu %llu KiB, cpu+gpu %llu KiB, frame %llu KiB "
         "align %#x",
         codec == kCodecAvc ? "H.264" : "HEVC", depth, rc,
         static_cast<unsigned long long>(m.cpu_memory_size >> 10),
         static_cast<unsigned long long>(m.gpu_memory_size >> 10),
         static_cast<unsigned long long>(m.cpu_gpu_memory_size >> 10),
         static_cast<unsigned long long>(m.max_frame_buffer_size >> 10), m.frame_buffer_alignment);
    if (rc != 0)
        return false;
    // CPU memory: one flexible block reused by every decoder (flexible memory given back
    // did not come back in the VLC port's logs).
    if (m.cpu_memory_size > cpu_block_len)
    {
        const std::size_t want = round_up(m.cpu_memory_size, k16K);
        void *addr = nullptr;
        rc = sceKernelMapFlexibleMemory(&addr, want, 0x03, 0);
        if (rc != 0)
        {
            logf("flexible memory %zu KiB: %#x", want >> 10, rc);
            return false;
        }
        cpu_block = addr;
        cpu_block_len = want;
    }
    const std::size_t align = m.frame_buffer_alignment > k16K ? m.frame_buffer_alignment : k16K;
    d.frame_size = round_up(m.max_frame_buffer_size, align);
    bool ok = (!m.gpu_memory_size || mem_direct(d.gpu, m.gpu_memory_size, k16K, 12, kProtCpuGpuRw)) &&
              (!m.cpu_gpu_memory_size ||
               mem_direct(d.cpu_gpu, m.cpu_gpu_memory_size, k16K, 12, kProtCpuGpuRw)) &&
              mem_direct(d.frames, d.frame_size * kFrames, align, frame_type, kProtCpuGpuRw);
    for (unsigned i = 0; ok && i < kAuSlots; ++i)
        ok = mem_direct(d.au[i], kAuSlotSize, k16K, 12, kProtCpuGpuRw);
    if (!ok)
        return false;
    std::memset(cpu_block, 0, cpu_block_len);
    if (d.gpu.addr)
        std::memset(d.gpu.addr, 0, d.gpu.len);
    if (d.cpu_gpu.addr)
        std::memset(d.cpu_gpu.addr, 0, d.cpu_gpu.len);
    std::memset(d.frames.addr, 0x80, d.frames.len);
    m.cpu_memory = m.cpu_memory_size ? cpu_block : nullptr;
    m.gpu_memory = d.gpu.addr;
    m.cpu_gpu_memory = d.cpu_gpu.addr;
    rc = sceVideodec2CreateDecoder(&c, &m, &d.dec);
    logf("sceVideodec2CreateDecoder: %#x (setup %.1f ms, picture buffers %u x %zu KiB, memory type %d)",
         rc, (now_us() - t0) / 1000.0, kFrames, d.frame_size >> 10, frame_type);
    return rc == 0;
}

void decoder_close(Decoder &d)
{
    if (d.dec)
        logf("sceVideodec2DeleteDecoder: %#x", sceVideodec2DeleteDecoder(d.dec));
    for (auto &a : d.au)
        mem_free(a);
    mem_free(d.frames);
    mem_free(d.cpu_gpu);
    mem_free(d.gpu);
    d.dec = nullptr;
}

int free_frame(Decoder &d, VdecFrameBuffer &fb)
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

void frame_back(Decoder &d, const void *buffer)
{
    const auto *base = static_cast<const std::uint8_t *>(d.frames.addr);
    const auto *b = static_cast<const std::uint8_t *>(buffer);
    if (!b || b < base)
        return;
    const std::size_t i = (b - base) / d.frame_size;
    if (i < kFrames)
        d.held[i] = false;
}

// One access unit in; returns the picture that came out (valid or not) in `out`.
int decode_one(Decoder &d, const std::uint8_t *au, std::size_t size, std::uint64_t pts, VdecOutput &out,
               std::uint64_t *call_us)
{
    Mem &slot = d.au[d.au_next++ % kAuSlots];
    std::memcpy(slot.addr, au, size);
    VdecInput in{};
    in.this_size = sizeof in;
    in.au_data = slot.addr;
    in.au_size = size;
    in.pts_data = pts;
    in.dts_data = ~0ull;
    VdecFrameBuffer fb;
    const int frame = free_frame(d, fb);
    if (frame < 0)
        return -1;
    out = {};
    out.this_size = sizeof out;
    const std::uint64_t t = now_us();
    const int rc = sceVideodec2Decode(d.dec, &in, &fb, &out);
    *call_us = now_us() - t;
    if (fb.is_accepted)
        d.held[frame] = true;
    return rc;
}

// ---- tests ----------------------------------------------------------------------------------
Stream avc, hevc;

// Decode a whole stream as fast as possible; then again with hashes against the reference.
void test_decode(Stream &s, std::uint32_t codec, std::uint32_t depth, int frame_type, bool hashes)
{
    Decoder d;
    if (!decoder_open(d, codec, depth, frame_type))
    {
        decoder_close(d);
        return;
    }
    const char *name = codec == kCodecAvc ? "H.264" : "HEVC";
    char path[128];
    snprintf(path, sizeof path, "%s/%s-hashes.txt", kDir, codec == kCodecAvc ? "avc" : "hevc");
    int hash_fd = hashes ? open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644) : -1;
    std::uint64_t total_call = 0, max_call = 0, hash_us = 0, copy_us = 0;
    unsigned outputs = 0, errors = 0, first_out_at = 0;
    bool logged_format = false;
    static std::uint8_t *copy_target;
    if (!copy_target)
        copy_target = big_alloc(2048 * 1088 * 3 / 2);
    auto picture = [&](VdecOutput &out) {
        if (out.is_error_frame)
            ++errors;
        if (!logged_format)
        {
            logged_format = true;
            logf("%s picture: %ux%u pitch %u (%u bytes) format %u buffer %llu bytes", name, out.frame_width,
                 out.frame_height, out.frame_pitch, out.frame_pitch_in_bytes, out.frame_format,
                 static_cast<unsigned long long>(out.frame_buffer_size));
        }
        if (hashes && out.frame_buffer)
        {
            const std::uint64_t t = now_us();
            const std::uint64_t h = picture_hash(static_cast<std::uint8_t *>(out.frame_buffer),
                                                 out.frame_pitch_in_bytes, out.frame_height);
            hash_us += now_us() - t;
            char line[24];
            int n = snprintf(line, sizeof line, "%016llx\n", static_cast<unsigned long long>(h));
            if (hash_fd >= 0)
                (void)write(hash_fd, line, n);
            // Cost of reading a picture out of the decoder's memory into ordinary memory.
            const std::uint64_t t2 = now_us();
            std::memcpy(copy_target, out.frame_buffer, out.frame_pitch_in_bytes * out.frame_height * 3 / 2);
            copy_us += now_us() - t2;
        }
        ++outputs;
        frame_back(d, out.frame_buffer);
    };
    const std::uint64_t t0 = now_us();
    for (unsigned i = 0; i < s.count; ++i)
    {
        VdecOutput out;
        std::uint64_t call = 0;
        const int rc = decode_one(d, s.data + s.offsets[i], s.sizes[i], i, out, &call);
        total_call += call;
        max_call = call > max_call ? call : max_call;
        if (rc != 0)
        {
            logf("%s decode AU %u: %#x", name, i, rc);
            if (rc == -1 || ++errors > 5)
                break;
            continue;
        }
        if (out.is_valid)
        {
            if (!outputs)
                first_out_at = i;
            picture(out);
        }
    }
    for (int i = 0; i < 32; ++i)
    {
        VdecFrameBuffer fb;
        const int frame = free_frame(d, fb);
        if (frame < 0)
            break;
        VdecOutput out{};
        out.this_size = sizeof out;
        const int rc = sceVideodec2Flush(d.dec, &fb, &out);
        if (fb.is_accepted)
            d.held[frame] = true;
        if (rc != 0 || !out.is_valid)
            break;
        picture(out);
    }
    const std::uint64_t wall = now_us() - t0;
    if (hash_fd >= 0)
        close(hash_fd);
    logf("%s depth %u%s: %u AUs -> %u pictures (%u error frames), first picture after AU %u; "
         "decode call avg %.2f ms max %.2f ms; wall %.1f ms = %.1f pictures/s%s",
         name, depth, hashes ? " +hash" : "", s.count, outputs, errors, first_out_at,
         total_call / 1000.0 / (s.count ? s.count : 1), max_call / 1000.0, wall / 1000.0,
         outputs * 1e6 / (wall ? wall : 1), hashes ? "" : " (decode only)");
    if (hashes && outputs)
        logf("%s: hash read %.2f ms/picture, memcpy out of decoder memory %.2f ms/picture", name,
             hash_us / 1000.0 / outputs, copy_us / 1000.0 / outputs);
    decoder_close(d);
}

// Play a stream to the screen at the display rate, converting on the CPU.
volatile bool stop_play = false;
void poll_commands(bool in_play);

void play(Stream &s, std::uint32_t codec, unsigned seconds, unsigned threads, bool forever)
{
    Decoder d;
    if (!decoder_open(d, codec, 1, 12))
    {
        decoder_close(d);
        return;
    }
    stop_play = false;
    const char *name = codec == kCodecAvc ? "H.264" : "HEVC";
    std::uint64_t conv_total = 0, conv_max = 0, dec_total = 0, frame_total = 0, frame_max = 0;
    unsigned shown = 0, late = 0, window = 0;
    std::uint64_t last_flip = now_us(), end = now_us() + seconds * 1000000ull;
    std::uint64_t last_beat = now_us(), last_tick = 0;
    unsigned au = 0;
    while (!stop_play && (forever || now_us() < end))
    {
        VdecOutput out;
        std::uint64_t call = 0;
        const int rc = decode_one(d, s.data + s.offsets[au % s.count], s.sizes[au % s.count], au, out, &call);
        ++au;
        dec_total += call;
        if (rc != 0)
        {
            logf("%s play decode: %#x", name, rc);
            break;
        }
        if (!out.is_valid)
            continue;
        std::uint8_t *dst = back_buffer();
        const std::uint64_t t = now_us();
        convert(static_cast<std::uint8_t *>(out.frame_buffer), out.frame_pitch_in_bytes, out.frame_height, dst,
                threads);
        pad_poll();
        draw_pad(dst);
        flush(dst, tiled ? kFrameBytes : kW * kH * 4);
        const std::uint64_t c = now_us() - t;
        conv_total += c;
        conv_max = c > conv_max ? c : conv_max;
        frame_back(d, out.frame_buffer);
        show();
        const std::uint64_t now = now_us(), interval = now - last_flip;
        last_flip = now;
        frame_total += interval;
        frame_max = interval > frame_max ? interval : frame_max;
        if (interval > 25000)
            ++late;
        if (++shown == 1)
            logf("%s play: first picture shown (convert %.2f ms)", name, c / 1000.0);
        ++window;
        if (now - last_beat >= 5000000)
        {
            logf("%s play (%u threads, %s): %u pictures, convert+overlay+flush avg %.2f ms max %.2f ms, "
                 "decode call avg %.2f ms, frame interval avg %.2f ms max %.2f ms, %u over 25 ms, "
                 "pad reads %u samples %u intercepted %u",
                 name, threads, tiled ? "tiled" : "linear", window, conv_total / 1000.0 / window,
                 conv_max / 1000.0, dec_total / 1000.0 / window, frame_total / 1000.0 / window,
                 frame_max / 1000.0, late, pad_reads, pad_samples, pad_intercepted_samples);
            conv_total = conv_max = dec_total = frame_total = frame_max = 0;
            late = window = 0;
            pad_reads = pad_samples = pad_intercepted_samples = 0;
            last_beat = now;
        }
        if (now - last_tick >= 10000000)
        {
            const int rc2 = sceSystemServicePowerTick();
            static int logged;
            if (logged++ < 2)
                logf("sceSystemServicePowerTick: %#x", rc2);
            last_tick = now;
        }
        if (forever && (shown % 30) == 0)
            poll_commands(true);
    }
    logf("%s play stopped after %u pictures", name, shown);
    decoder_close(d);
}

void test_audio(unsigned ms)
{
    static bool init;
    if (!init)
    {
        logf("sceAudioOutInit: %#x", sceAudioOutInit());
        init = true;
    }
    const std::uint32_t grain = 256, rate = 48000;
    const int port = sceAudioOutOpen(0xff, 0, 0, grain, rate, 1);
    logf("sceAudioOutOpen(user 0xff, main, 256, 48000, S16 stereo): %#x", port);
    if (port < 0)
        return;
    static std::int16_t buffer[256 * 2];
    double phase = 0;
    const unsigned calls = ms * rate / 1000 / grain;
    std::uint64_t first[8] = {}, maxc = 0;
    const std::uint64_t t0 = now_us();
    for (unsigned i = 0; i < calls; ++i)
    {
        for (unsigned f = 0; f < grain; ++f)
        {
            // 440 Hz at -12 dB; the integer sine keeps libm out of the app.
            phase += 440.0 / rate;
            if (phase >= 1)
                phase -= 1;
            const double x = phase < 0.5 ? phase * 4 - 1 : 3 - phase * 4;  // triangle -1..1
            const auto v = static_cast<std::int16_t>(x * 8000);
            buffer[f * 2] = buffer[f * 2 + 1] = v;
        }
        const std::uint64_t t = now_us();
        const int rc = sceAudioOutOutput(port, buffer);
        const std::uint64_t took = now_us() - t;
        if (i < 8)
            first[i] = took;
        maxc = took > maxc ? took : maxc;
        if (rc < 0)
        {
            logf("sceAudioOutOutput: %#x", rc);
            break;
        }
    }
    const std::uint64_t wall = now_us() - t0;
    logf("audio: %u blocks of 256 frames in %.1f ms (%.2f ms per block, 5.33 expected), output call "
         "max %.2f ms, first calls %.2f %.2f %.2f %.2f %.2f %.2f %.2f %.2f ms",
         calls, wall / 1000.0, wall / 1000.0 / calls, maxc / 1000.0, first[0] / 1000.0, first[1] / 1000.0,
         first[2] / 1000.0, first[3] / 1000.0, first[4] / 1000.0, first[5] / 1000.0, first[6] / 1000.0,
         first[7] / 1000.0);
    logf("sceAudioOutClose: %#x", sceAudioOutClose(port));
}

void test_rumble()
{
    if (pad < 0)
        return;
    rumble(255, 255);
    light(255, 0, 0);
    sceKernelUsleep(600000);
    rumble(0, 0);
    light(0, 0, 255);
}

void test_conversion()
{
    // Conversion cost alone, from an ordinary-memory picture into each framebuffer layout.
    static std::uint8_t *picture;
    if (!picture)
        picture = big_alloc(2048 * 1088 * 3 / 2);
    if (!picture)
        return;
    for (std::size_t i = 0; i < 2048u * 1088 * 3 / 2; ++i)
        picture[i] = static_cast<std::uint8_t>(i * 7);
    const bool was_tiled = tiled;
    for (int layout = 0; layout < 2; ++layout)
    {
        if ((layout == 0 && !linear_ok) || (layout == 1 && !tiled_ok))
            continue;
        tiled = layout == 1;
        const unsigned options[4] = {1, 2, 4, 6};
        for (unsigned threads : options)
        {
            std::uint64_t total = 0, maxc = 0;
            for (int r = 0; r < 20; ++r)
            {
                const std::uint64_t t = now_us();
                convert(picture, 2048, 1088, fbs[tiled ? 2 : 0], threads);
                const std::uint64_t c = now_us() - t;
                total += c;
                maxc = c > maxc ? c : maxc;
            }
            logf("NV12->RGBA 1080p into %s WC framebuffer, %u thread(s): avg %.2f ms max %.2f ms",
                 tiled ? "tiled" : "linear", threads, total / 20000.0, maxc / 1000.0);
        }
    }
    tiled = was_tiled;
    const std::uint64_t t = now_us();
    flush(fbs[0], kFrameBytes);
    logf("clflush of one 16 MiB framebuffer: %.2f ms", (now_us() - t) / 1000.0);
}

void probe_formats()
{
    // Which pixel formats sceVideoOutRegisterBuffers2 takes (set 2, buffer 4, then unregistered).
    // Only 0x8000000022000000 is known from PS5Term; the rest are guesses built the same way
    // from the PS4 format codes (top 16 bits << 32, low 16 bits << 16).
    const std::uint64_t formats[] = {
        UINT64_C(0x8000000022000000), UINT64_C(0x8000000000000000), UINT64_C(0x8806000000000000),
        UINT64_C(0x8800000000000000), UINT64_C(0xC106000000000000), UINT64_C(0x4400000000000000),
        UINT64_C(0x4401000000000000), UINT64_C(0x4600000000000000),
    };
    for (std::uint64_t format : formats)
        for (std::uint32_t tiling = 0; tiling < 2; ++tiling)
        {
            VideoAttribute attr{};
            sceVideoOutSetBufferAttribute2(&attr, format, tiling, kW, kH, 0, 0, 0);
            VideoBuffer b = {fbs[3], nullptr, nullptr, nullptr};
            const int rc = sceVideoOutRegisterBuffers2(video, 2, 4, &b, 1, &attr, 0, nullptr);
            logf("register format %#018llx tiling %u: %#x", static_cast<unsigned long long>(format), tiling, rc);
            if (rc >= 0)
                sceVideoOutUnregisterBuffers(video, 2);
        }
}

[[noreturn]] void quit()
{
    logf("exit: sceSystemServiceLoadExec(\"exit\")");
    if (pad >= 0)
        rumble(0, 0);
    const int rc = sceSystemServiceLoadExec("exit", nullptr);
    logf("sceSystemServiceLoadExec returned %#x", rc);
    for (;;)
        sceKernelUsleep(1000000);
}

unsigned play_threads = 4;
char *pending_command = nullptr;
void poll_commands(bool in_play)
{
    static std::uint64_t next;
    const std::uint64_t now = now_us();
    if (now < next)
        return;
    next = now + 500000;
    char text[64] = {};
    int fd = open(kCmd, O_RDONLY);
    if (fd < 0)
        return;
    const long n = read(fd, text, sizeof text - 1);
    close(fd);
    unlink(kCmd);
    if (n <= 0)
        return;
    for (long i = 0; i < n; ++i)
        if (text[i] == '\n' || text[i] == '\r' || text[i] == ' ')
            text[i] = 0;
    logf("command: %s", text);
    if (!std::strcmp(text, "exit"))
        quit();
    if (!std::strcmp(text, "tiled") && tiled_ok)
        tiled = true;
    else if (!std::strcmp(text, "linear") && linear_ok)
        tiled = false;
    else if (!std::strcmp(text, "rumble"))
        test_rumble();
    else if (!std::strcmp(text, "tone"))
        test_audio(3000);
    else if (!std::strcmp(text, "probe"))
        probe_formats();
    else if (!std::strcmp(text, "awake"))
    {
        // Only on request: a ShellCoreUtil call an app may not be allowed to make.
        logf("calling sceShellCoreUtilGetAutoPowerDownRemainingSeconds");
        int seconds = -1;
        const int rc = sceShellCoreUtilGetAutoPowerDownRemainingSeconds(&seconds);
        logf("auto power-down remaining: rc %#x, %d s", rc, seconds);
        logf("sceSystemServicePowerTick: %#x", sceSystemServicePowerTick());
        seconds = -1;
        const int rc2 = sceShellCoreUtilGetAutoPowerDownRemainingSeconds(&seconds);
        logf("after a tick: rc %#x, %d s", rc2, seconds);
    }
    else if (!std::strcmp(text, "t1"))
        play_threads = 1;
    else if (!std::strcmp(text, "t4"))
        play_threads = 4;
    else if (in_play)
    {
        // Decoder tests need the decoder the loop holds: stop the loop, main runs them.
        static char pending[64];
        std::memcpy(pending, text, sizeof pending);
        pending_command = pending;
        stop_play = true;
    }
}
} // namespace

int main()
{
    start_us = now_us();
    mkdir(kDir, 0755);
    log_fd = open(kLog, O_WRONLY | O_CREAT | O_APPEND, 0644);
    logf("==== PS5Upload Play spike start (pid %d)", getpid());
    // The decoder library first, as the VLC port does (before anything else is set up).
    std::uint64_t t = now_us();
    int rc = sceSysmoduleLoadModule(207);
    logf("sceSysmoduleLoadModule(207 libSceVideodec2): %#x in %.1f ms", rc, (now_us() - t) / 1000.0);
    std::size_t flex = 0;
    sceKernelAvailableFlexibleMemorySize(&flex);
    logf("direct memory size %zu MiB, flexible available %zu MiB", sceKernelGetDirectMemorySize() >> 20,
         flex >> 20);
    pad_open();
    light(0, 0, 255);
    if (!display_open())
        logf("display failed");
    // First picture: dark blue, then hide the splash.
    fill(back_buffer(), 0, 0, kW, kH, 0xff401008u);
    flush(back_buffer(), kFrameBytes);
    show();
    logf("sceSystemServiceHideSplashScreen: %#x", sceSystemServiceHideSplashScreen());
    notify("PS5Upload Play spike running");
    start_workers(6);

    const bool vq = compute_queue_open();
    const bool have_avc = load_stream(avc, "/app0/assets/avc.264", false);
    const bool have_hevc = load_stream(hevc, "/app0/assets/hevc.265", true);
    if (vq && have_avc)
    {
        test_decode(avc, kCodecAvc, 1, 12, false);
        test_decode(avc, kCodecAvc, 4, 12, false);
        test_decode(avc, kCodecAvc, 1, 12, true);
    }
    if (vq && have_hevc)
    {
        test_decode(hevc, kCodecHevc, 1, 12, false);
        test_decode(hevc, kCodecHevc, 4, 12, false);
        test_decode(hevc, kCodecHevc, 1, 12, true);
    }
    test_conversion();
    test_audio(2000);
    test_rumble();
    logf("start-up tests done; playing H.264 in a loop (commands in %s)", kCmd);
    for (;;)
    {
        if (vq && have_avc)
            play(avc, kCodecAvc, 0, play_threads, true);
        else
        {
            pad_poll();
            draw_pad(back_buffer());
            show();
            poll_commands(false);
        }
        if (pending_command)
        {
            char *cmd = pending_command;
            pending_command = nullptr;
            if (!std::strcmp(cmd, "avc"))
            {
                test_decode(avc, kCodecAvc, 1, 12, false);
                test_decode(avc, kCodecAvc, 1, 12, true);
            }
            else if (!std::strcmp(cmd, "hevc"))
            {
                test_decode(hevc, kCodecHevc, 1, 12, false);
                test_decode(hevc, kCodecHevc, 1, 12, true);
            }
            else if (!std::strcmp(cmd, "avc-wb"))
                test_decode(avc, kCodecAvc, 1, 0, true);
            else if (!std::strcmp(cmd, "play-hevc"))
                play(hevc, kCodecHevc, 20, play_threads, false);
        }
    }
}
