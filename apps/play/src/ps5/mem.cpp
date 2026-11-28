// PS5Upload Play - direct and flexible memory.
// SPDX-License-Identifier: GPL-3.0-or-later
#include "../app/log.hpp"
#include "sys.hpp"

namespace play::ps5
{
bool mem_direct(Mem &m, std::size_t len, std::size_t align, int type, int prot) noexcept
{
    if (align < k16K)
        align = k16K;
    m.len = round_up(len, k16K);
    int rc = sceKernelAllocateDirectMemory(0, static_cast<std::int64_t>(sceKernelGetDirectMemorySize()), m.len,
                                           align, type, &m.phys);
    if (rc != 0)
    {
        log("memory: direct %zu KiB type %d: allocate %#x", m.len >> 10, type, rc);
        m = {};
        return false;
    }
    rc = sceKernelMapDirectMemory(&m.addr, m.len, prot, 0, m.phys, align);
    if (rc != 0)
    {
        log("memory: direct %zu KiB type %d: map %#x", m.len >> 10, type, rc);
        sceKernelReleaseDirectMemory(m.phys, m.len);
        m = {};
        return false;
    }
    return true;
}

void mem_free(Mem &m) noexcept
{
    if (m.addr && m.phys >= 0)
    {
        sceKernelMunmap(m.addr, m.len);
        sceKernelReleaseDirectMemory(m.phys, m.len);
    }
    m = {};
}

std::uint8_t *big_alloc(std::size_t size) noexcept
{
    void *addr = nullptr;
    const int rc = sceKernelMapFlexibleMemory(&addr, round_up(size, k16K), 0x03, 0);
    if (rc != 0)
    {
        log("memory: flexible %zu KiB: %#x", size >> 10, rc);
        return nullptr;
    }
    return static_cast<std::uint8_t *>(addr);
}
} // namespace play::ps5
