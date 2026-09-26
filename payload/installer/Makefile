PS5_HOST ?= 192.168.137.2
PS5_PORT ?= 9021

ifdef PS5_PAYLOAD_SDK
    include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
else
    $(error PS5_PAYLOAD_SDK is undefined)
endif

# The PS5Upload installer daemon. Loaded by the console's :9021 ELF loader as
# a companion image; binds TCP :9115 and installs via sceAppInstUtilInstallByPackage.
ELF := ps5upload-installer.elf

CFLAGS := -Wall -Wextra -Werror -g -I. -I../include

# -lSceIpmi MUST precede -lSceAppInstUtil (sdk#26): libSceAppInstUtil's
# DT_NEEDED graph resolves only with this ordering, or rtld silently refuses
# the ELF (no run, no :9115 bind). -lpthread for the worker/monitor threads.
LIBS := -lSceSystemService -lSceUserService -lSceIpmi -lSceAppInstUtil \
        -lSceNotification -lkernel_sys -lpthread

SRCS := main.c protocol.c pathsafe.c jobs.c http_range.c loopback.c \
        hints.c sony.c escalate.c
HEADERS := protocol.h pathsafe.h jobs.h http_range.h loopback.h hints.h \
           sony.h escalate.h ../include/authid.h ../include/timed_init.h \
           ../include/sceAppInstUtil.h

all: $(ELF) $(ELF).gz

# The Makefile is a prerequisite: LIBS/CFLAGS live here, so a lib-order change
# forces a relink (the stale-ELF trap the old dpi Makefile documented).
$(ELF): $(SRCS) $(HEADERS) Makefile
	$(CC) $(CFLAGS) -o $@ $(SRCS) $(LIBS)

$(ELF).gz: $(ELF)
	gzip -kf $<

clean:
	rm -f $(ELF) $(ELF).gz

send: $(ELF)
	@echo "Sending $(ELF) to $(PS5_HOST):$(PS5_PORT) ..."
	nc -w 1 $(PS5_HOST) $(PS5_PORT) < $(ELF)

.PHONY: all clean send
