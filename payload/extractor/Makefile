# ps5upload-extractor: the on-console archive extractor (see README.md).
#
#   make            the PS5 ELF (ps5upload-extractor.elf), which the helper embeds and starts
#                   through the console's ELF loader when an extraction is asked for
#   make host       the same code as a host program, for the tests
#   make host-test  builds it and runs tests/run.py (needs python3; 7zz or 7z for the 7z/AES cases)
#
# Objects go under build/ (git-ignored). Third-party code is built without our -Werror.

PS5_HOST ?= 192.168.137.2
PS5_PORT ?= 9021

ELF := ps5upload-extractor.elf
HOST_BIN := build/host/ps5upload-extractor-host

SEVENZ := ../third_party/7zip
LIBARCHIVE := ../third_party/libarchive

# 7-Zip, extract-only and single-threaded: 7z, zip, tar, gz, bz2, xz, zstd, lzma, split sets, with
# the codecs and the AES/ZipCrypto decryption they use. No RAR (xrar.c, libarchive) and no
# encoders. Paths relative to $(SEVENZ)/CPP.
SEVENZ_CPP := \
  Common/CRC.cpp Common/CrcReg.cpp Common/IntToString.cpp Common/StringToInt.cpp Common/MyString.cpp \
  Common/MyVector.cpp Common/NewHandler.cpp Common/Sha1Prepare.cpp Common/Sha1Reg.cpp Common/Sha256Prepare.cpp \
  Common/Sha256Reg.cpp Common/StringConvert.cpp Common/UTFConvert.cpp Common/Xxh64Reg.cpp Common/XzCrc64Init.cpp \
  Common/XzCrc64Reg.cpp Common/MyWindows.cpp \
  Windows/PropVariant.cpp Windows/PropVariantConv.cpp Windows/PropVariantUtils.cpp Windows/System.cpp Windows/TimeUtils.cpp \
  7zip/Common/CreateCoder.cpp 7zip/Common/CWrappers.cpp 7zip/Common/InBuffer.cpp 7zip/Common/FilterCoder.cpp \
  7zip/Common/LimitedStreams.cpp 7zip/Common/LockedStream.cpp 7zip/Common/MethodId.cpp 7zip/Common/MethodProps.cpp \
  7zip/Common/OffsetStream.cpp 7zip/Common/OutBuffer.cpp 7zip/Common/ProgressUtils.cpp 7zip/Common/PropId.cpp \
  7zip/Common/StreamObjects.cpp 7zip/Common/StreamUtils.cpp 7zip/Common/UniqBlocks.cpp \
  7zip/Archive/Common/CoderMixer2.cpp 7zip/Archive/Common/DummyOutStream.cpp 7zip/Archive/Common/FindSignature.cpp \
  7zip/Archive/Common/InStreamWithCRC.cpp 7zip/Archive/Common/ItemNameUtils.cpp 7zip/Archive/Common/MultiStream.cpp \
  7zip/Archive/Common/OutStreamWithCRC.cpp 7zip/Archive/Common/OutStreamWithSha1.cpp 7zip/Archive/Common/HandlerOut.cpp \
  7zip/Archive/Common/ParseProperties.cpp \
  7zip/Archive/7z/7zDecode.cpp 7zip/Archive/7z/7zExtract.cpp 7zip/Archive/7z/7zHandler.cpp 7zip/Archive/7z/7zHeader.cpp \
  7zip/Archive/7z/7zIn.cpp 7zip/Archive/7z/7zProperties.cpp 7zip/Archive/7z/7zSpecStream.cpp 7zip/Archive/7z/7zRegister.cpp \
  7zip/Archive/Bz2Handler.cpp 7zip/Archive/GzHandler.cpp 7zip/Archive/LzmaHandler.cpp 7zip/Archive/SplitHandler.cpp \
  7zip/Archive/XzHandler.cpp 7zip/Archive/ZstdHandler.cpp \
  7zip/Archive/Tar/TarHandler.cpp 7zip/Archive/Tar/TarHeader.cpp 7zip/Archive/Tar/TarIn.cpp 7zip/Archive/Tar/TarRegister.cpp \
  7zip/Archive/Zip/ZipHandler.cpp 7zip/Archive/Zip/ZipIn.cpp 7zip/Archive/Zip/ZipItem.cpp 7zip/Archive/Zip/ZipRegister.cpp \
  7zip/Compress/Bcj2Coder.cpp 7zip/Compress/Bcj2Register.cpp 7zip/Compress/BcjCoder.cpp 7zip/Compress/BcjRegister.cpp \
  7zip/Compress/BitlDecoder.cpp 7zip/Compress/BranchMisc.cpp 7zip/Compress/BranchRegister.cpp 7zip/Compress/ByteSwap.cpp \
  7zip/Compress/BZip2Crc.cpp 7zip/Compress/BZip2Decoder.cpp 7zip/Compress/BZip2Register.cpp 7zip/Compress/CopyCoder.cpp \
  7zip/Compress/CopyRegister.cpp 7zip/Compress/Deflate64Register.cpp 7zip/Compress/DeflateDecoder.cpp \
  7zip/Compress/DeflateRegister.cpp 7zip/Compress/DeltaFilter.cpp 7zip/Compress/ImplodeDecoder.cpp 7zip/Compress/LzfseDecoder.cpp \
  7zip/Compress/LzOutWindow.cpp 7zip/Compress/Lzma2Decoder.cpp 7zip/Compress/Lzma2Register.cpp \
  7zip/Compress/LzmaDecoder.cpp 7zip/Compress/LzmaRegister.cpp 7zip/Compress/PpmdDecoder.cpp \
  7zip/Compress/PpmdRegister.cpp 7zip/Compress/PpmdZip.cpp 7zip/Compress/ShrinkDecoder.cpp \
  7zip/Compress/XzDecoder.cpp 7zip/Compress/ZstdDecoder.cpp \
  7zip/Crypto/7zAes.cpp 7zip/Crypto/7zAesRegister.cpp 7zip/Crypto/HmacSha1.cpp 7zip/Crypto/HmacSha256.cpp \
  7zip/Crypto/MyAes.cpp 7zip/Crypto/MyAesReg.cpp 7zip/Crypto/Pbkdf2HmacSha1.cpp 7zip/Crypto/WzAes.cpp \
  7zip/Crypto/ZipCrypto.cpp 7zip/Crypto/ZipStrong.cpp 7zip/Crypto/RandGen.cpp
# Paths relative to $(SEVENZ)/C.
SEVENZ_C := 7zBuf2.c 7zCrc.c 7zCrcOpt.c 7zStream.c Aes.c AesOpt.c Alloc.c Bcj2.c Bra.c Bra86.c BraIA64.c CpuArch.c \
  Delta.c Lzma2Dec.c Lzma2DecMt.c LzmaDec.c Ppmd7.c Ppmd7Dec.c Ppmd7aDec.c Ppmd8.c Ppmd8Dec.c Ppmd8Enc.c Sha1.c Sha1Opt.c Sha256.c \
  Sha256Opt.c SwapBytes.c Xxh64.c Xz.c XzDec.c XzIn.c XzCrc64.c XzCrc64Opt.c ZstdDec.c

LIBARCHIVE_C := archive_acl.c archive_entry_sparse.c archive_entry_xattr.c archive_random.c archive_blake2s_ref.c archive_blake2sp_ref.c archive_check_magic.c archive_crc32.c archive_entry.c \
  archive_options.c archive_ppmd7.c archive_read.c archive_read_open_filename.c archive_read_set_options.c \
  archive_read_support_format_rar.c archive_read_support_format_rar5.c archive_string.c archive_string_sprintf.c \
  archive_time.c archive_util.c archive_virtual.c

OWN_C := src/xmain.c src/xsink.c src/xrun.c src/xreq.c src/xrar.c
OWN_CPP := src/x7z.cpp src/xguids.cpp

# Z7_ST: single-threaded (bounded memory, no MT decoders). Z7_EXTRACT_ONLY: no update side.
SEVENZ_DEFS := -DZ7_ST -DZ7_EXTRACT_ONLY -DZ7_NO_LONG_PATH -D_FILE_OFFSET_BITS=64
SEVENZ_INC := -I$(SEVENZ)/CPP
# The largest RAR5 window (dictionary) allocated, bytes (#801): see third_party/libarchive/README.md.
RAR5_MAX_WINDOW ?= 4294967296ULL
LIBARCHIVE_DEFS := -DHAVE_CONFIG_H -D__LIBARCHIVE_ENABLE_VISIBILITY=0 -DPS5U_RAR5_MAX_WINDOW=$(RAR5_MAX_WINDOW)
LIBARCHIVE_INC := -I$(LIBARCHIVE)
OWN_INC := -Isrc -I../include $(SEVENZ_INC) $(LIBARCHIVE_INC)

ifeq ($(MAKECMDGOALS),)
PS5_BUILD := 1
endif
ifneq ($(filter all $(ELF) send,$(MAKECMDGOALS)),)
PS5_BUILD := 1
endif

ifdef PS5_BUILD
ifndef PS5_PAYLOAD_SDK
$(error PS5_PAYLOAD_SDK is undefined)
endif
include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
# Our own build of the SDK's startup code; see ../sdk-crt.mk.
include ../sdk-crt.mk
endif

PS5_OBJ := build/ps5
HOST_OBJ := build/host

ps5_objs = $(addprefix $(PS5_OBJ)/7z/,$(SEVENZ_CPP:.cpp=.o)) $(addprefix $(PS5_OBJ)/7zc/,$(SEVENZ_C:.c=.o)) \
  $(addprefix $(PS5_OBJ)/la/,$(LIBARCHIVE_C:.c=.o)) $(addprefix $(PS5_OBJ)/,$(OWN_C:.c=.o) $(OWN_CPP:.cpp=.o))
host_objs = $(addprefix $(HOST_OBJ)/7z/,$(SEVENZ_CPP:.cpp=.o)) $(addprefix $(HOST_OBJ)/7zc/,$(SEVENZ_C:.c=.o)) \
  $(addprefix $(HOST_OBJ)/la/,$(LIBARCHIVE_C:.c=.o)) $(addprefix $(HOST_OBJ)/,$(OWN_C:.c=.o) $(OWN_CPP:.cpp=.o))

# -Os: decoding speed comes from 7-Zip's hand-tuned C (LzmaDec, the CRCs, AES-NI), which -Os keeps;
# the size matters because the helper carries this ELF.
PS5_TP_FLAGS := -Os -ffunction-sections -fdata-sections -w
PS5_OWN_FLAGS := -Os -ffunction-sections -fdata-sections -Wall -Wextra -Werror
# PS5 (Zen 2) has AES-NI and SHA-NI but no VAES: 7-Zip picks at run time, but its VAES and SHA-512
# paths need compiler support the SDK's clang target may lack, so they are left out.
PS5_SEVENZ_DEFS := $(SEVENZ_DEFS)
HOST_CC ?= cc
HOST_CXX ?= c++
HOST_TP_FLAGS := -O2 -g -w
HOST_OWN_FLAGS := -O1 -g -Wall -Wextra -Werror

all: $(ELF)

$(ELF): $(ps5_objs) $(SDK_CRT1) Makefile
	$(CXX) -nostartfiles -Wl,--gc-sections -o $@ $(ps5_objs) $(SDK_CRT1) -lpthread
	$(STRIP) $@

$(PS5_OBJ)/7z/%.o: $(SEVENZ)/CPP/%.cpp Makefile
	@mkdir -p $(dir $@)
	$(CXX) $(PS5_TP_FLAGS) $(PS5_SEVENZ_DEFS) $(SEVENZ_INC) -c -o $@ $<
$(PS5_OBJ)/7zc/%.o: $(SEVENZ)/C/%.c Makefile
	@mkdir -p $(dir $@)
	$(CC) $(PS5_TP_FLAGS) $(PS5_SEVENZ_DEFS) -c -o $@ $<
$(PS5_OBJ)/la/%.o: $(LIBARCHIVE)/%.c $(LIBARCHIVE)/config.h Makefile
	@mkdir -p $(dir $@)
	$(CC) $(PS5_TP_FLAGS) $(LIBARCHIVE_DEFS) $(LIBARCHIVE_INC) -c -o $@ $<
$(PS5_OBJ)/src/%.o: src/%.c $(wildcard src/*.h) ../include/archive_name.h Makefile
	@mkdir -p $(dir $@)
	$(CC) $(PS5_OWN_FLAGS) $(OWN_INC) -c -o $@ $<
$(PS5_OBJ)/src/%.o: src/%.cpp $(wildcard src/*.h) Makefile
	@mkdir -p $(dir $@)
	$(CXX) $(PS5_OWN_FLAGS) $(PS5_SEVENZ_DEFS) $(OWN_INC) -c -o $@ $<

host: $(HOST_BIN)

$(HOST_BIN): $(host_objs) Makefile
	$(HOST_CXX) -o $@ $(host_objs) -lpthread

$(HOST_OBJ)/7z/%.o: $(SEVENZ)/CPP/%.cpp Makefile
	@mkdir -p $(dir $@)
	$(HOST_CXX) $(HOST_TP_FLAGS) $(SEVENZ_DEFS) $(SEVENZ_INC) -c -o $@ $<
$(HOST_OBJ)/7zc/%.o: $(SEVENZ)/C/%.c Makefile
	@mkdir -p $(dir $@)
	$(HOST_CC) $(HOST_TP_FLAGS) $(SEVENZ_DEFS) -c -o $@ $<
$(HOST_OBJ)/la/%.o: $(LIBARCHIVE)/%.c $(LIBARCHIVE)/config.h Makefile
	@mkdir -p $(dir $@)
	$(HOST_CC) $(HOST_TP_FLAGS) $(LIBARCHIVE_DEFS) $(LIBARCHIVE_INC) -c -o $@ $<
$(HOST_OBJ)/src/%.o: src/%.c $(wildcard src/*.h) ../include/archive_name.h Makefile
	@mkdir -p $(dir $@)
	$(HOST_CC) $(HOST_OWN_FLAGS) $(OWN_INC) -c -o $@ $<
$(HOST_OBJ)/src/%.o: src/%.cpp $(wildcard src/*.h) Makefile
	@mkdir -p $(dir $@)
	$(HOST_CXX) $(HOST_OWN_FLAGS) $(SEVENZ_DEFS) $(OWN_INC) -c -o $@ $<

host-test: $(HOST_BIN)
	# Under a time limit: a hang fails by name with a stack sample (../../scripts/run-limited.pl).
	perl ../../scripts/run-limited.pl 300 python3 tests/run.py $(HOST_BIN)

send: $(ELF)
	nc -w 1 $(PS5_HOST) $(PS5_PORT) < $(ELF)

clean:
	rm -rf build $(ELF)

.PHONY: all host host-test send clean
