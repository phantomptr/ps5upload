PS5_HOST ?= 192.168.137.2
PS5_PORT ?= 9021

ifdef PS5_PAYLOAD_SDK
    include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
else
    $(error PS5_PAYLOAD_SDK is undefined)
endif

# Our own build of the SDK's startup code; see ../sdk-crt.mk for why.
include ../sdk-crt.mk

# The PS5Upload SMB daemon: network shares for the console's Files page. The helper embeds this
# ELF and launches it through the console's :9021 loader on demand (include/smbd_proto.h); it is
# never sent on its own. Listens on 127.0.0.1:9183 only.
ELF := ps5upload-smb.elf

# libsmb2 (LGPL-2.1-or-later; ../third_party/libsmb2/README.md), compiled as its own objects:
# third-party code is not held to our -Werror policy. -Os and section GC keep the ELF small (the
# library's server half is never referenced and is dropped at link time).
SMB2_DIR := ../third_party/libsmb2
SMB2_SRCS := $(wildcard $(SMB2_DIR)/lib/*.c)
SMB2_OBJS := $(patsubst $(SMB2_DIR)/lib/%.c,build/libsmb2/%.o,$(SMB2_SRCS))
SMB2_CFLAGS := -Os -g0 -ffunction-sections -fdata-sections -DHAVE_CONFIG_H \
               -I$(SMB2_DIR) -I$(SMB2_DIR)/include -I$(SMB2_DIR)/include/smb2

CFLAGS := -Wall -Wextra -Werror -g -Os -ffunction-sections -fdata-sections \
          -I. -I../include -I$(SMB2_DIR)/include
LDFLAGS := -Wl,--gc-sections
LIBS := -lpthread

SRCS := smbd_main.c smbd_jobs.c smbd_copy.c smbd_path.c smbd_json.c smbd_smbfs.c smbd_localfs.c smbd_library.c smbd_image.c \
        ../src/pkg_info.c
HEADERS := smbd_copy.h smbd_fs.h smbd_jobs.h smbd_json.h smbd_library.h smbd_image.h smbd_log.h smbd_path.h \
           ../include/smbd_proto.h ../include/config.h ../include/pkg_info.h

all: $(ELF)

build/libsmb2/%.o: $(SMB2_DIR)/lib/%.c $(SMB2_DIR)/config.h Makefile
	@mkdir -p $(dir $@)
	$(CC) $(SMB2_CFLAGS) -c -o $@ $<

# The Makefile is a prerequisite: LIBS/CFLAGS live here.
$(ELF): $(SRCS) $(HEADERS) $(SMB2_OBJS) $(SDK_CRT1) Makefile
	$(CC) $(CFLAGS) $(LDFLAGS) -nostartfiles -o $@ $(SRCS) $(SMB2_OBJS) $(SDK_CRT1) $(LIBS)

clean:
	rm -f $(ELF)
	rm -rf build

.PHONY: all clean
