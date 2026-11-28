// PS5Upload Play - microphone capture.
// libSceAudioIn is imported at link time (build-in-container.sh makes its stub): loading it at
// run time with sceKernelLoadStartModule/sceKernelDlsym does not work for an app (public
// reports: 0x80020002 from /system/common/lib, no symbols from the sandbox path). Signatures
// as public PS4 emulator sources and PS5 homebrew give them; the results are in docs/MIC.md.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "mic.hpp"

extern "C"
{
    int sceAudioInInit();
    int sceAudioInOpen(std::int32_t user, std::uint32_t type, std::uint32_t index, std::uint32_t len,
                       std::uint32_t freq, std::uint32_t param);
    int sceAudioInHqOpen(std::int32_t user, std::uint32_t type, std::uint32_t index, std::uint32_t len,
                         std::uint32_t freq, std::uint32_t param);
    int sceAudioInInput(std::int32_t handle, void *dest);
    int sceAudioInGetSilentState(std::int32_t handle);
    int sceAudioInClose(std::int32_t handle);
}

namespace play::ps5::mic
{
int init() noexcept
{
    const int rc = sceAudioInInit();
    // 0x8026000c / 0x8026000e: already initialized
    return rc == static_cast<int>(0x8026000c) || rc == static_cast<int>(0x8026000e) ? 0 : rc;
}

int open(int user, const Format &f) noexcept
{
    const std::uint32_t param = f.stereo ? 2 : 0; // S16 stereo / S16 mono
    return f.hq ? sceAudioInHqOpen(user, static_cast<std::uint32_t>(f.type), 0, f.grain, f.rate, param)
                : sceAudioInOpen(user, static_cast<std::uint32_t>(f.type), 0, f.grain, f.rate, param);
}

int read(int handle, std::int16_t *out) noexcept
{
    return sceAudioInInput(handle, out);
}

int silent(int handle) noexcept
{
    return sceAudioInGetSilentState(handle);
}

int close(int handle) noexcept
{
    return sceAudioInClose(handle);
}
} // namespace play::ps5::mic
