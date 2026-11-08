PS5_HOST ?= 192.168.137.2
PS5_PORT ?= 9021

ifdef PS5_PAYLOAD_SDK
    include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
else
    $(error PS5_PAYLOAD_SDK is undefined)
endif

# Our own build of the SDK's startup code; see ../../sdk-crt.mk for why.
include ../../sdk-crt.mk

# ps5u-mbusprobe: a standalone probe of libSceMbus's device binding (README.md). Never embedded in
# or linked into the helper; sent on its own to the console's :9021 loader.
ELF := ps5u-mbusprobe.elf

CFLAGS := -Wall -Wextra -Werror -g -Os
LIBS := -lkernel_sys -lSceUserService -lSceSysmodule

all: $(ELF)

# The Makefile is a prerequisite: LIBS/CFLAGS live here.
$(ELF): main.c $(SDK_CRT1) Makefile
	$(CC) $(CFLAGS) -nostartfiles -o $@ main.c $(SDK_CRT1) $(LIBS)

# Sends it to the console's loader, as payload/Makefile's `send` does: make send PS5_HOST=<ip>
send: $(ELF)
	@echo "Sending $(ELF) to $(PS5_HOST):$(PS5_PORT) ..."
	nc -w 1 $(PS5_HOST) $(PS5_PORT) < $(ELF)

clean:
	rm -f $(ELF)

.PHONY: all clean send
