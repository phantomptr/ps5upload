# PS5 Upload Payload Makefile

ifndef PS5_PAYLOAD_SDK
$(error PS5_PAYLOAD_SDK is not set. Please export PS5_PAYLOAD_SDK=/path/to/sdk)
endif

include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk

TARGET := ps5upload.elf

# C sources
C_SRCS := main.c storage.c protocol.c extract.c notify.c transfer.c lz4.c sha256.c unrar_handler.c \
	third_party/zstd/zstd.c \
	third_party/lzma_sdk/Alloc.c \
	third_party/lzma_sdk/CpuArch.c \
	third_party/lzma_sdk/LzmaDec.c \
	third_party/lzma_sdk/LzmaEnc.c \
	third_party/lzma_sdk/LzmaLib.c \
	third_party/lzma_sdk/LzFind.c \
	third_party/lzma_sdk/Sort.c

C_OBJS := main.o storage.o protocol.o extract.o notify.o transfer.o lz4.o sha256.o unrar_handler.o \
	third_party/zstd/zstd.o \
	third_party/lzma_sdk/Alloc.o \
	third_party/lzma_sdk/CpuArch.o \
	third_party/lzma_sdk/LzmaDec.o \
	third_party/lzma_sdk/LzmaEnc.o \
	third_party/lzma_sdk/LzmaLib.o \
	third_party/lzma_sdk/LzFind.o \
	third_party/lzma_sdk/Sort.o

# UnRAR C++ sources (library mode - RARDLL)
# Note: Many .cpp files are #included by others and should NOT be compiled separately
# Based on official unrar makefile: OBJECTS + LIB_OBJ for lib target
UNRAR_DIR := third_party/unrar
UNRAR_SRCS := \
	$(UNRAR_DIR)/rar.cpp \
	$(UNRAR_DIR)/strlist.cpp \
	$(UNRAR_DIR)/strfn.cpp \
	$(UNRAR_DIR)/pathfn.cpp \
	$(UNRAR_DIR)/smallfn.cpp \
	$(UNRAR_DIR)/global.cpp \
	$(UNRAR_DIR)/file.cpp \
	$(UNRAR_DIR)/filefn.cpp \
	$(UNRAR_DIR)/filcreat.cpp \
	$(UNRAR_DIR)/archive.cpp \
	$(UNRAR_DIR)/arcread.cpp \
	$(UNRAR_DIR)/unicode.cpp \
	$(UNRAR_DIR)/system.cpp \
	$(UNRAR_DIR)/crypt.cpp \
	$(UNRAR_DIR)/crc.cpp \
	$(UNRAR_DIR)/rawread.cpp \
	$(UNRAR_DIR)/encname.cpp \
	$(UNRAR_DIR)/resource.cpp \
	$(UNRAR_DIR)/match.cpp \
	$(UNRAR_DIR)/timefn.cpp \
	$(UNRAR_DIR)/rdwrfn.cpp \
	$(UNRAR_DIR)/consio.cpp \
	$(UNRAR_DIR)/options.cpp \
	$(UNRAR_DIR)/errhnd.cpp \
	$(UNRAR_DIR)/rarvm.cpp \
	$(UNRAR_DIR)/secpassword.cpp \
	$(UNRAR_DIR)/rijndael.cpp \
	$(UNRAR_DIR)/getbits.cpp \
	$(UNRAR_DIR)/sha1.cpp \
	$(UNRAR_DIR)/sha256.cpp \
	$(UNRAR_DIR)/blake2s.cpp \
	$(UNRAR_DIR)/hash.cpp \
	$(UNRAR_DIR)/extinfo.cpp \
	$(UNRAR_DIR)/extract.cpp \
	$(UNRAR_DIR)/volume.cpp \
	$(UNRAR_DIR)/list.cpp \
	$(UNRAR_DIR)/find.cpp \
	$(UNRAR_DIR)/unpack.cpp \
	$(UNRAR_DIR)/headers.cpp \
	$(UNRAR_DIR)/threadpool.cpp \
	$(UNRAR_DIR)/rs16.cpp \
	$(UNRAR_DIR)/cmddata.cpp \
	$(UNRAR_DIR)/ui.cpp \
	$(UNRAR_DIR)/filestr.cpp \
	$(UNRAR_DIR)/scantree.cpp \
	$(UNRAR_DIR)/dll.cpp \
	$(UNRAR_DIR)/qopen.cpp \
	$(UNRAR_DIR)/largepage.cpp \
	$(UNRAR_DIR)/unrar_wrapper.cpp

UNRAR_OBJS := $(UNRAR_SRCS:.cpp=.o)

# All objects
OBJS := $(C_OBJS) $(UNRAR_OBJS)

# Read version from VERSION file
VERSION := $(shell cat ../VERSION)

# Common flags
COMMON_FLAGS := -O2 -DPS5_UPLOAD_VERSION=\"$(VERSION)\" -DSERVER_PORT_STR=\"9113\" -D_7ZIP_ST \
	-Ithird_party/zstd -Ithird_party/lzma_sdk -Ithird_party/unrar

# C flags
CFLAGS := -Wall $(COMMON_FLAGS)

# C++ flags for unrar
# Note: Disable USE_SSE (x86 only), disable RAR_SMP (threading), disable large pages
CXXFLAGS := -std=c++11 $(COMMON_FLAGS) \
	-D_UNIX -DSILENT -DRARDLL -DLITTLE_ENDIAN -DALLOW_MISALIGNED \
	-D_FILE_OFFSET_BITS=64 -D_LARGEFILE_SOURCE \
	-UUSE_SSE -URAR_SMP \
	-Wno-logical-op-parentheses -Wno-switch -Wno-dangling-else \
	-Wno-delete-non-virtual-dtor -Wno-unused-variable -fno-rtti

THIRD_PARTY_CFLAGS := -Wno-unneeded-internal-declaration -Wno-empty-body
LDADD := -lkernel -lpthread -lc++

.PHONY: all clean test info help

all: $(TARGET)
	@echo "✓ Payload built: $(TARGET)"
	@echo "  Load this file on your PS5 using your payload loader"

$(TARGET): $(OBJS)
	@echo "Linking $(TARGET) with C++..."
	$(CXX) $(CXXFLAGS) -o $@ $(OBJS) $(LDADD)

# C sources
%.o: %.c
	@echo "Compiling C: $<..."
	$(CC) $(CFLAGS) -c $< -o $@

# C third-party sources
third_party/zstd/%.o: third_party/zstd/%.c
	@echo "Compiling C: $<..."
	$(CC) $(CFLAGS) $(THIRD_PARTY_CFLAGS) -c $< -o $@

third_party/lzma_sdk/%.o: third_party/lzma_sdk/%.c
	@echo "Compiling C: $<..."
	$(CC) $(CFLAGS) $(THIRD_PARTY_CFLAGS) -c $< -o $@

# C++ unrar sources
$(UNRAR_DIR)/%.o: $(UNRAR_DIR)/%.cpp
	@echo "Compiling C++: $<..."
	$(CXX) $(CXXFLAGS) -c $< -o $@

notify.o: notify.c notify.h config.h
	$(CC) $(CFLAGS) -c notify.c -o notify.o

transfer.o: transfer.c transfer.h protocol_defs.h notify.h
	$(CC) $(CFLAGS) -c transfer.c -o transfer.o

unrar_handler.o: unrar_handler.c unrar_handler.h third_party/unrar/unrar_wrapper.h
	$(CC) $(CFLAGS) -c unrar_handler.c -o unrar_handler.o

clean:
	@echo "Cleaning build artifacts..."
	rm -f $(TARGET) $(C_OBJS) $(UNRAR_OBJS)

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
	@echo "  CXX: $(CXX)"
	@echo "  Target: $(TARGET)"
	@echo "  C Sources: $(C_SRCS)"
	@echo "  C++ Sources: $(UNRAR_SRCS)"

help:
	@echo "PS5 Upload Payload Build"
	@echo ""
	@echo "Targets:"
	@echo "  make          - Build ps5upload.elf (with unrar support)"
	@echo "  make clean    - Remove build artifacts"
	@echo "  make test     - Verify build"
	@echo "  make info     - Show build configuration"
	@echo ""
