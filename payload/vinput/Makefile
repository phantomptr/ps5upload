PS5_HOST ?= 192.168.137.2
PS5_PORT ?= 9021

ifdef PS5_PAYLOAD_SDK
    include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
else
    $(error PS5_PAYLOAD_SDK is undefined)
endif

# Our own build of the SDK's startup code; see ../sdk-crt.mk for why.
include ../sdk-crt.mk

# ps5u-vinput: the console's virtual keyboard and mouse for the helper (README.md). The helper
# embeds this ELF (stripped) and launches it through the console's :9021 loader when a keyboard &
# mouse session opens; it is never sent on its own. Listens on 127.0.0.1:9184 only.
ELF := ps5u-vinput.elf

CFLAGS := -Wall -Wextra -Werror -g -Os -I../include
LIBS := -lkernel_sys -lSceUserService -lSceSystemService -lSceSysmodule

SRCS := main.c ../src/vinput_companion.c ../src/vinput_wire.c ../src/vinput_dev.c
HEADERS := ../include/vinput.h ../include/vinput_wire.h ../include/vinput_companion.h ../include/vinput_dev.h

all: $(ELF)

# The Makefile is a prerequisite: LIBS/CFLAGS live here.
$(ELF): $(SRCS) $(HEADERS) $(SDK_CRT1) Makefile
	$(CC) $(CFLAGS) -nostartfiles -o $@ $(SRCS) $(SDK_CRT1) $(LIBS)

clean:
	rm -f $(ELF)

.PHONY: all clean
