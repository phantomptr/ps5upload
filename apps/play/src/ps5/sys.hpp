// PS5Upload Play - the console's system calls the app uses, and their structures.
// The libSceVideodec2 layouts and call order follow the PS5 VLC port's ps5/modules/ps5vdec.c
// (GPL-2.0-or-later, https://github.com/ZiZc3/VLC); the video-out and pad structures are
// PS5Term's (GPL-3.0-or-later). Measured on the console in phase 0 (docs/PHASE0.md).
// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>

extern "C"
{
    // kernel
    std::size_t sceKernelGetDirectMemorySize();
    int sceKernelAllocateDirectMemory(std::int64_t start, std::int64_t end, std::size_t len, std::size_t align,
                                      int type, std::int64_t *phys);
    int sceKernelMapDirectMemory(void **addr, std::size_t len, int prot, int flags, std::int64_t phys,
                                 std::size_t align);
    int sceKernelReleaseDirectMemory(std::int64_t phys, std::size_t len);
    int sceKernelMapFlexibleMemory(void **addr, std::size_t len, int prot, int flags);
    int sceKernelAvailableFlexibleMemorySize(std::size_t *size);
    int sceKernelMunmap(void *addr, std::size_t len);
    int sceKernelUsleep(std::uint32_t us);
    int sceKernelSendNotificationRequest(int device, void *request, std::size_t size, int blocking);
    // system
    int sceSysmoduleLoadModule(std::uint16_t id);
    int sceSystemServiceHideSplashScreen();
    int sceSystemServiceLoadExec(const char *path, const char *const *argv);
    int sceSystemServicePowerTick();
    int sceUserServiceInitialize(const void *params);
    int sceUserServiceGetInitialUser(std::int32_t *user);
    int sceUserServiceGetUserName(std::int32_t user, char *name, std::size_t size);
    // video out
    int sceVideoOutOpen(std::int32_t user, std::int32_t bus, std::int32_t index, const void *param);
    int sceVideoOutSetFlipRate(std::int32_t handle, std::int32_t rate);
    int sceVideoOutSubmitFlip(std::int32_t handle, std::int32_t index, std::uint32_t mode, std::int64_t arg);
    int sceVideoOutWaitVblank(std::int32_t handle);
    // pad
    int scePadInit();
    int scePadOpen(std::int32_t user, std::int32_t type, std::int32_t index, const void *param);
    int scePadRead(std::int32_t handle, void *samples, std::int32_t count);
    int scePadSetVibrationMode(std::int32_t handle, std::int32_t mode);
    int scePadSetVibration(std::int32_t handle, const void *vibration);
    int scePadSetLightBar(std::int32_t handle, const void *color);
    int scePadResetLightBar(std::int32_t handle);
    int scePadSetMotionSensorState(std::int32_t handle, bool enable);
    int scePadGetControllerInformation(std::int32_t handle, void *info);
    // audio
    int sceAudioOutInit();
    int sceAudioOutOpen(std::int32_t user, std::int32_t type, std::int32_t index, std::uint32_t len,
                        std::uint32_t freq, std::uint32_t param);
    int sceAudioOutOutput(std::int32_t handle, const void *buffer);
}

// ---- video out (PS5Term) --------------------------------------------------------------------
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
struct VideoFlipStatus
{
    std::uint64_t count;
    std::uint64_t process_time;
    std::uint64_t tsc;
    std::int64_t flip_arg;
    std::uint64_t submit_tsc;
    std::uint64_t reserved0;
    std::int32_t gc_queue_num;
    std::int32_t flip_pending_num;
    std::int32_t current_buffer;
    std::uint32_t reserved1;
    std::uint8_t spare[64]; // room in case the console's structure is larger
};
extern "C" void sceVideoOutSetBufferAttribute2(VideoAttribute *attribute, std::uint64_t format, std::uint32_t tiling,
                                               std::uint32_t width, std::uint32_t height, std::uint64_t option,
                                               std::uint32_t dcc_control, std::uint64_t dcc_clear_color);
extern "C" int sceVideoOutRegisterBuffers2(std::int32_t handle, std::int32_t set_index, std::int32_t start,
                                           VideoBuffer *buffers, std::int32_t count, VideoAttribute *attribute,
                                           std::int32_t category, void *option);
extern "C" int sceVideoOutGetFlipStatus(std::int32_t handle, VideoFlipStatus *status);

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
    int sceVideodec2AllocateComputeQueue(const VdecComputeConfig *config, const VdecComputeMemory *memory,
                                         void **queue);
    int sceVideodec2QueryDecoderMemoryInfo(const VdecConfig *config, VdecMemory *memory);
    int sceVideodec2CreateDecoder(const VdecConfig *config, const VdecMemory *memory, void **decoder);
    int sceVideodec2DeleteDecoder(void *decoder);
    int sceVideodec2Decode(void *decoder, const VdecInput *input, VdecFrameBuffer *frame, VdecOutput *output);
    int sceVideodec2Flush(void *decoder, VdecFrameBuffer *frame, VdecOutput *output);
    int sceVideodec2Reset(void *decoder);
}

// ---- pad sample (120 bytes, ScePadData) ---------------------------------------------------
struct PadTouch
{
    std::uint16_t x, y;
    std::uint8_t id;
    std::uint8_t reserved[3];
};
struct PadSample
{
    std::uint32_t buttons;
    std::uint8_t lx, ly, rx, ry, l2, r2;
    std::uint8_t pad0[2];
    float orientation[4];
    float accel[3];   // g
    float gyro[3];    // rad/s
    std::uint8_t touch_count;
    std::uint8_t pad1[3];
    std::uint32_t pad2;
    PadTouch touch[2];
    std::uint8_t connected; // a byte (reading 4 bytes gave 0xB0000001)
    std::uint8_t pad3[3];
    std::uint64_t timestamp;
    std::uint8_t extension[16];
    std::uint8_t connected_count;
    std::uint8_t remaining[15];
};
static_assert(sizeof(PadSample) == 120);
static_assert(offsetof(PadSample, connected) == 0x4c);
static_assert(offsetof(PadSample, touch_count) == 0x34);

namespace play::ps5
{
constexpr int kProtCpuGpuRw = 0x33;
constexpr std::size_t k16K = 0x4000;
struct Mem
{
    void *addr = nullptr;
    std::size_t len = 0;
    std::int64_t phys = -1;
};
inline std::size_t round_up(std::size_t v, std::size_t a) noexcept
{
    return (v + a - 1) & ~(a - 1);
}
bool mem_direct(Mem &m, std::size_t len, std::size_t align, int type, int prot) noexcept;
void mem_free(Mem &m) noexcept;
// Big buffers come from flexible memory: the app's malloc heap is small.
std::uint8_t *big_alloc(std::size_t size) noexcept;
} // namespace play::ps5
