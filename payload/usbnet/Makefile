PS5_HOST ?= 192.168.137.2
PS5_PORT ?= 9021

ifdef PS5_PAYLOAD_SDK
    include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
else
    $(error PS5_PAYLOAD_SDK is undefined)
endif

# Our own build of the SDK's startup code; see ../sdk-crt.mk for why.
include ../sdk-crt.mk

# ps5u-usbnet: the USB Ethernet adapter daemon (README.md, ../include/usbnet_proto.h). EXPERIMENTAL.
# The helper embeds this ELF (stripped) and launches it through the console's :9021 loader while a
# supported adapter is plugged in; it is never sent on its own. Its control port is 127.0.0.1:9185.
ELF := ps5u-usbnet.elf

# lwIP (BSD-style licence, ../../third_party/lwip/COPYING), the same submodule the tailnet core uses,
# built here with this daemon's own options (lwipopts.h): a separate process, a separate lwIP.
# Third-party code is not held to our -Werror policy.
LWIP_DIR := ../../third_party/lwip
LWIP_SRCS := $(wildcard $(LWIP_DIR)/src/core/*.c) $(wildcard $(LWIP_DIR)/src/core/ipv4/*.c) \
             $(wildcard $(LWIP_DIR)/src/api/*.c) $(LWIP_DIR)/src/netif/ethernet.c
LWIP_OBJS := $(patsubst $(LWIP_DIR)/src/%.c,build/lwip/%.o,$(LWIP_SRCS))
# The console's libc: realtime condition variables (no condattr import), see sys_arch.c.
PORT_FLAGS := -DUN_COND_REALTIME -I. -I$(LWIP_DIR)/src/include
LWIP_CFLAGS := -O2 -g0 -ffunction-sections -fdata-sections $(PORT_FLAGS)

# -O2: the receive thread and the splice carry every byte of a transfer.
CFLAGS := -Wall -Wextra -Werror -g -O2 -ffunction-sections -fdata-sections $(PORT_FLAGS) -I../include
LDFLAGS := -Wl,--gc-sections
LIBS := -lpthread

SRCS := main.c usbn_log.c adapter.c ncm.c rtl.c vendor.c vendor_regs.c net.c fwd.c fwd_lwip.c link.c sys_arch.c \
        ../smb/smbd_json.c
HEADERS := adapter.h ncm.h rtl.h vendor.h frame.h net.h fwd.h fwd_lwip.h link.h usbn_log.h lwipopts.h arch/cc.h \
           arch/sys_arch.h ../smb/smbd_json.h ../include/usbnet_proto.h

all: $(ELF)

build/lwip/%.o: $(LWIP_DIR)/src/%.c lwipopts.h arch/cc.h arch/sys_arch.h Makefile
	@mkdir -p $(dir $@)
	$(CC) $(LWIP_CFLAGS) -c -o $@ $<

# The Makefile is a prerequisite: LIBS/CFLAGS live here.
$(ELF): $(SRCS) $(HEADERS) $(LWIP_OBJS) $(SDK_CRT1) Makefile
	$(CC) $(CFLAGS) $(LDFLAGS) -nostartfiles -o $@ $(SRCS) $(LWIP_OBJS) $(SDK_CRT1) $(LIBS)

clean:
	rm -f $(ELF)
	rm -rf build

.PHONY: all clean
