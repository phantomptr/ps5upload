# tailnet: a small gaming-only Tailscale node in C11.
#
#   make              host library + tnctl (clang/cc, macOS or Linux)
#   make test         unit tests built with ASan + UBSan, then run
#   make ps5          build/ps5/libtailnet.a with the PS5 payload toolchain
#                     (needs PS5_PAYLOAD_SDK; Monocypher comes from the payload)
#   make ps5-link     link-check: tnctl.elf for the PS5 with its own Monocypher
#                     (proves every libc/libkernel symbol resolves; not for running)
#   make size         code size of our own objects
#   make clean
#
# Third-party code (Monocypher, BearSSL) is built separately and, like in
# payload/, is not held to -Werror.

TARGET ?= host
MONO_DIR ?= ../third_party/ava1/c/third_party/monocypher
BEARSSL_DIR ?= ../third_party/bearssl
LWIP_DIR ?= ../third_party/lwip

ifeq ($(TARGET),ps5)
  ifndef PS5_PAYLOAD_SDK
    $(error PS5_PAYLOAD_SDK is undefined)
  endif
  include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
  BUILD ?= build/ps5
  # The payload already links Monocypher (payload/Makefile MONO_OBJ): do not duplicate it.
  WITH_MONO ?= 0
  OPT ?= -O2
  # No /dev/urandom or getentropy assumptions on the console: we inject entropy
  # from kern.arandom into every TLS engine (src/net.c).
  BR_DEFS := -DBR_USE_URANDOM=0 -DBR_USE_GETENTROPY=0
  # lwIP's asserts abort(); on the console a bad assert must not kill the helper.
  LWIP_DEFS := -DLWIP_NOASSERT
  SIZE ?= $(firstword $(wildcard /opt/homebrew/opt/llvm/bin/llvm-size) llvm-size)
else ifeq ($(TARGET),win)
  # Compile check against real Windows headers (MinGW-w64); CI builds with MSVC via cc-rs.
  CC := x86_64-w64-mingw32-gcc
  AR := x86_64-w64-mingw32-ar
  BUILD ?= build/win
  OPT ?= -O2
  WITH_MONO ?= 1
  BR_DEFS :=
  SIZE ?= size
else
  CC ?= cc
  ifeq ($(SAN),1)
    BUILD ?= build/asan
    SANFLAGS := -fsanitize=address,undefined -fno-omit-frame-pointer -fno-sanitize-recover=undefined
    OPT ?= -O1 -g
  else
    BUILD ?= build/host
    OPT ?= -O2 -g
  endif
  WITH_MONO ?= 1
  BR_DEFS :=
  SIZE ?= size
endif

# Same warning policy as payload/Makefile (-Wall -Wextra -Werror).
WARN := -Wall -Wextra -Werror
LWIP_INC := -Ilwip-port -I$(LWIP_DIR)/src/include
CFLAGS_OURS := -std=c11 -D_DEFAULT_SOURCE -D_DARWIN_C_SOURCE $(WARN) $(OPT) $(SANFLAGS) $(LWIP_DEFS) \
	-Iinclude -Isrc -I$(MONO_DIR) -I$(BEARSSL_DIR)/inc $(LWIP_INC)
CFLAGS_3P := -std=c11 -D_DEFAULT_SOURCE -D_DARWIN_C_SOURCE -w -O2 $(SANFLAGS) $(BR_DEFS) $(LWIP_DEFS) \
	-I$(BEARSSL_DIR)/inc -I$(BEARSSL_DIR)/src -I$(MONO_DIR) $(LWIP_INC)
TN_LDFLAGS := $(SANFLAGS) -lpthread

SRCS := src/crypto.c src/json.c src/util.c src/keys.c src/net.c src/noise.c src/hpack.c src/wg.c src/disco.c src/ip.c src/derp.c src/dp.c src/fwd.c src/os.c \
	src/h2.c src/netmap.c src/control.c src/tailnet.c
OBJS := $(patsubst src/%.c,$(BUILD)/%.o,$(SRCS))
MONO_OBJ := $(if $(filter 1,$(WITH_MONO)),$(BUILD)/3p/monocypher.o)
BR_SRCS := $(sort $(wildcard $(BEARSSL_DIR)/src/*/*.c))
BR_OBJS := $(patsubst $(BEARSSL_DIR)/src/%.c,$(BUILD)/3p/bearssl/%.o,$(BR_SRCS))
# lwIP 2.2.1 (BSD-3-Clause): IPv4 + TCP only, NO_SYS (lwip-port/lwipopts.h).
LWIP_SRCS := $(addprefix $(LWIP_DIR)/src/core/,init.c def.c inet_chksum.c ip.c mem.c memp.c netif.c pbuf.c \
	tcp.c tcp_in.c tcp_out.c timeouts.c ipv4/ip4.c ipv4/ip4_addr.c)
LWIP_OBJS := $(patsubst $(LWIP_DIR)/src/core/%.c,$(BUILD)/3p/lwip/%.o,$(LWIP_SRCS))

LIB := $(BUILD)/libtailnet.a
BRLIB := $(BUILD)/libbearssl.a
LWLIB := $(BUILD)/liblwip.a
TNCTL := $(BUILD)/tnctl
TESTS := test_crypto test_hpack test_noise test_netmap test_dp test_netdown
TEST_BINS := $(addprefix $(BUILD)/,$(TESTS))

.PHONY: all lib test ps5 ps5-link win size clean check-bearssl

ifneq ($(filter ps5 win,$(TARGET)),)
all: lib
else
all: lib $(TNCTL)
endif
lib: check-bearssl $(LIB) $(BRLIB) $(LWLIB)

check-bearssl:
	@test -f $(BEARSSL_DIR)/inc/bearssl.h || { echo "BearSSL missing: git submodule update --init third_party/bearssl"; exit 1; }
	@test -f $(MONO_DIR)/monocypher.h || { echo "Monocypher missing: git submodule update --init third_party/ava1"; exit 1; }
	@test -f $(LWIP_DIR)/src/include/lwip/tcp.h || { echo "lwIP missing: git submodule update --init third_party/lwip"; exit 1; }

$(BUILD)/%.o: src/%.c include/tailnet.h src/tn.h src/os.h src/crypto.h src/json.h lwip-port/lwipopts.h Makefile
	@mkdir -p $(dir $@)
	$(CC) $(CFLAGS_OURS) -c -o $@ $<

$(BUILD)/3p/monocypher.o: $(MONO_DIR)/monocypher.c
	@mkdir -p $(dir $@)
	$(CC) $(CFLAGS_3P) -c -o $@ $<

$(BUILD)/3p/bearssl/%.o: $(BEARSSL_DIR)/src/%.c
	@mkdir -p $(dir $@)
	@$(CC) $(CFLAGS_3P) -c -o $@ $<

$(LIB): $(OBJS) $(MONO_OBJ)
	rm -f $@
	$(AR) rcs $@ $^

$(BRLIB): $(BR_OBJS)
	rm -f $@
	$(AR) rcs $@ $^

$(BUILD)/3p/lwip/%.o: $(LWIP_DIR)/src/core/%.c lwip-port/lwipopts.h
	@mkdir -p $(dir $@)
	@$(CC) $(CFLAGS_3P) -c -o $@ $<

$(LWLIB): $(LWIP_OBJS)
	rm -f $@
	$(AR) rcs $@ $^

$(TNCTL): tools/tnctl.c $(LIB) $(BRLIB) $(LWLIB)
	$(CC) $(CFLAGS_OURS) -o $@ $< $(LIB) $(LWLIB) $(BRLIB) $(TN_LDFLAGS)

$(BUILD)/test_%: tests/test_%.c tests/tt.h tests/vectors_go.h $(LIB) $(BRLIB) $(LWLIB)
	$(CC) $(CFLAGS_OURS) -Itests -o $@ $< $(LIB) $(LWLIB) $(BRLIB) $(TN_LDFLAGS)

ifeq ($(SAN),1)
run-tests: $(TEST_BINS)
	@set -e; for t in $(TEST_BINS); do echo "== $$t"; (cd tests && ../$$t); done
endif

test:
	$(MAKE) SAN=1 run-tests

ps5:
	$(MAKE) TARGET=ps5 lib

win:
	$(MAKE) TARGET=win lib

ps5-link:
	$(MAKE) TARGET=ps5 WITH_MONO=1 BUILD=build/ps5-link lib
	$(MAKE) TARGET=ps5 WITH_MONO=1 BUILD=build/ps5-link build/ps5-link/tnctl.elf

ifeq ($(TARGET),ps5)
$(BUILD)/tnctl.elf: tools/tnctl.c $(LIB) $(BRLIB) $(LWLIB)
	$(CC) $(CFLAGS_OURS) -o $@ $< $(LIB) $(LWLIB) $(BRLIB) -lpthread
endif

size: lib
	@$(SIZE) $(OBJS)
	@$(SIZE) $(OBJS) | awk 'NR>1 {t+=$$1; d+=$$2} END {print "our objects: text " t ", data " d " bytes"}'

clean:
	rm -f $(OBJS) $(LIB) $(BRLIB) $(LWLIB) $(TNCTL) $(TEST_BINS)
	@echo "third-party objects kept under $(BUILD)/3p (remove the build/ directory by hand if needed)"
