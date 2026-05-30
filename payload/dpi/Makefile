PS5_HOST ?= 192.168.137.2
PS5_PORT ?= 9021

ifdef PS5_PAYLOAD_SDK
    include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
else
    $(error PS5_PAYLOAD_SDK is undefined)
endif

# Standalone DPI install daemon (see ezremote_dpi.c for why it's a
# separate payload). The engine sends this ELF to the loader port when
# :9040 isn't already listening, then POSTs an http(s):// .pkg URL to it.
ELF := ezremote-dpi.elf

CFLAGS := -Wall -Wextra -Werror -g

# sceAppInstUtilInstallByPackage lives in libSceAppInstUtil; the toast
# helper (sceKernelSendNotificationRequest) is in libkernel, linked by
# the prospero crt by default.
LIBS := -lSceAppInstUtil

SRCS := ezremote_dpi.c
HEADERS := sceAppInstUtil.h

all: $(ELF) $(ELF).gz

$(ELF): $(SRCS) $(HEADERS)
	$(CC) $(CFLAGS) -o $@ $(SRCS) $(LIBS)

# Gzipped copy for the host bundle (same rationale as the main payload:
# linuxdeploy chokes on the FreeBSD/sprx ELF, so we ship the .gz and
# decompress on the host before streaming to the loader).
$(ELF).gz: $(ELF)
	gzip -kf $<

clean:
	rm -f $(ELF) $(ELF).gz

send: $(ELF)
	@echo "Sending $(ELF) to $(PS5_HOST):$(PS5_PORT) ..."
	nc -w 1 $(PS5_HOST) $(PS5_PORT) < $(ELF)

.PHONY: all clean send
