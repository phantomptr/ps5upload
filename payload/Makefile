# PS5 Upload Payload Makefile

ifndef PS5_PAYLOAD_SDK
$(error PS5_PAYLOAD_SDK is not set. Please export PS5_PAYLOAD_SDK=/path/to/sdk)
endif

include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk

TARGET := ps5upload.elf
SRCS := main.c storage.c protocol.c extract.c notify.c transfer.c lz4.c
OBJS := main.o storage.o protocol.o extract.o notify.o transfer.o lz4.o

# Read version from VERSION file
VERSION := $(shell cat ../VERSION)
CFLAGS := -Wall -O2 -DPS5_UPLOAD_VERSION=\"$(VERSION)\" -DSERVER_PORT_STR=\"9113\"
LDADD := -lkernel -lpthread

.PHONY: all clean test info help

all: $(TARGET)
	@echo "✓ Payload built: $(TARGET)"
	@echo "  Load this file on your PS5 using your payload loader"

$(TARGET): $(OBJS)
	@echo "Linking $(TARGET)..."
	$(CC) $(CFLAGS) -o $@ $(OBJS) $(LDADD)

%.o: %.c
	@echo "Compiling $<..."
	$(CC) $(CFLAGS) -c $< -o $@

notify.o: notify.c notify.h config.h
	$(CC) $(CFLAGS) -c notify.c -o notify.o

transfer.o: transfer.c transfer.h protocol_defs.h notify.h
	$(CC) $(CFLAGS) -c transfer.c -o transfer.o

clean:
	@echo "Cleaning build artifacts..."
	rm -f $(TARGET) $(OBJS)

test: info
	@echo ""
	@echo "Build test:"
	@if [ -f "$(TARGET)" ]; then \
		echo "  ✓ $(TARGET) exists"; \
		file $(TARGET) | grep -q "ELF" && echo "  ✓ Valid ELF binary" || echo "  ✗ Not an ELF file!"; \
	else \
		echo "  ✗ $(TARGET) not found (run: make)"; \
	fi

info:
	@echo "Build configuration:"
	@echo "  SDK: $(PS5_PAYLOAD_SDK)"
	@echo "  CC: $(CC)"
	@echo "  Target: $(TARGET)"
	@echo "  Sources: $(SRCS)"

help:
	@echo "PS5 Upload Payload Build"
	@echo ""
	@echo "Targets:"
	@echo "  make          - Build ps5upload.elf"
	@echo "  make clean    - Remove build artifacts"
	@echo "  make test     - Verify build"
	@echo "  make info     - Show build configuration"
	@echo ""
