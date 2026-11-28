# ps5u-usbnet: third-party code and credits

ps5u-usbnet is part of PS5Upload (GPL-3.0, see ../../LICENSE). It is **experimental**: tried on
little or no real hardware, with no warranty, used at your own risk (see ../../DISCLAIMER.md).
It builds on:

## tom-ps5-usbnet — GPL-3.0-or-later

https://github.com/wujiezero/tom-ps5-usbnet (wujiezero, and the testers it thanks).
The USB adapter driver is adapted from it, file by file; each file names its origin in its header:

| Here | From |
|---|---|
| `adapter.c`, `adapter.h` | `src/adapter.c`, `include/adapter.h` (finding the adapter, configurations, ECM pass-through, MAC string) |
| `vendor.c`, `vendor.h`, `vendor_regs.c` | `src/vendor.c`, `include/vendor.h`, `src/vendor_regs.c` (bulk transfers, the chip bring-up after Linux `r8152`) |
| `rtl.c`, `rtl.h`, `ncm.c`, `ncm.h`, `frame.h` | the same names (descriptor and aggregate codecs) |
| `net.c`, `net.h` | `src/net.c`, `include/net.h` (lwIP on the bulk link) |
| `fwd.c`, `fwd.h`, `fwd_lwip.c`, `fwd_lwip.h` | `src/forward.c`, `src/forward_lwip.c`, `include/forward*.h` (joining lwIP connections to 127.0.0.1) |
| `sys_arch.c`, `arch/cc.h`, `arch/sys_arch.h` | `port/` (lwIP on pthreads) |
| `lwipopts.h`, `link.c`, `main.c` | values and logic from `include/lwipopts.h` and `src/main.c` |
| `payload/tests/usbnet_selftest.c` | the captured RTL8159 descriptors of `tests/mock_descriptors.h` |

tom-ps5-usbnet in turn credits the Linux `r8152` driver (GPL-2.0) as the reference for the chip's
initialization sequence; the register tables follow it.

## lwIP — BSD-3-Clause

https://savannah.nongnu.org/projects/lwip/ (Adam Dunkels and contributors). The git submodule
`../../third_party/lwip` (COPYING there), shared with the tailnet core and built here with this
daemon's own options (`lwipopts.h`).
