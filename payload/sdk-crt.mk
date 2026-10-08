# ps5upload's build of the PS5 Payload SDK's startup code (crt1.o).
#
# The SDK's own crt1.o (v0.43) stops every payload before main() when either of
# two kernel searches it added comes up empty (see third_party/sdk-crt/
# ps5upload.patch): reported on FW 5.50, where 6.x helpers never started while
# 5.x (built on v0.42) did. The same sources, with those searches made
# best-effort, are built here and linked into every payload with -nostartfiles.
#
# Include after prospero.mk. Link with:  $(CC) ... -nostartfiles $(SDK_CRT1) ...

# Leave the includer's default goal alone (restored at the end of this file).
SDK_CRT_SAVED_GOAL := $(.DEFAULT_GOAL)

SDK_CRT_DIR   := $(patsubst %/,%,$(dir $(lastword $(MAKEFILE_LIST))))/third_party/sdk-crt
SDK_CRT_BUILD := $(SDK_CRT_DIR)/build
SDK_CRT1      := $(SDK_CRT_BUILD)/crt1.o

SDK_CRT_CC := $(PS5_PAYLOAD_SDK)/bin/clang
# lld, found the way the SDK's prospero-lld finds it: LLVM's bindir (Debian's lld-22 is there),
# else PATH, else Homebrew's lld formula (a separate formula from llvm since lld 19). Not
# prospero-lld itself: it adds the payload linker script, which a -r link must not get.
SDK_CRT_LLVM_BINDIR := $(shell $(PS5_PAYLOAD_SDK)/bin/prospero-llvm-config --bindir 2>/dev/null)
SDK_CRT_LD := $(firstword $(wildcard $(SDK_CRT_LLVM_BINDIR)/ld.lld) \
                          $(shell command -v ld.lld 2>/dev/null) \
                          $(shell brew --prefix lld 2>/dev/null)/bin/ld.lld)

# Upstream's crt/Makefile flags, without -Werror: vendored code is not held to our warning
# policy, and a newer clang's new warning must not break the build.
SDK_CRT_CFLAGS := -ffreestanding -fno-builtin -nostdlib -fPIC \
                  -target x86_64-sie-ps5 -fno-plt -fno-stack-protector \
                  -fvisibility-nodllstorageclass=default -Wall \
                  -fms-extensions -Wno-microsoft-anon-tag -O1

SDK_CRT_NAMES := crt syscall klog nid kernel rtld rtld_so rtld_sprx rtld_payload \
                 rtld_dlfcn mdbg patch kernel_procio kernel_iommu _start
SDK_CRT_OBJS  := $(addprefix $(SDK_CRT_BUILD)/,$(addsuffix .o,$(SDK_CRT_NAMES)))
SDK_CRT_HDRS  := $(wildcard $(SDK_CRT_DIR)/*.h)

$(SDK_CRT1): $(SDK_CRT_OBJS)
	$(SDK_CRT_LD) -m elf_x86_64 -r -o $@ $^

$(SDK_CRT_BUILD)/%.o: $(SDK_CRT_DIR)/%.c $(SDK_CRT_HDRS)
	@mkdir -p $(dir $@)
	$(SDK_CRT_CC) -c $(SDK_CRT_CFLAGS) -o $@ $<

$(SDK_CRT_BUILD)/%.o: $(SDK_CRT_DIR)/%.S
	@mkdir -p $(dir $@)
	$(SDK_CRT_CC) -c $(SDK_CRT_CFLAGS) -masm=intel -o $@ $<

.DEFAULT_GOAL := $(SDK_CRT_SAVED_GOAL)
