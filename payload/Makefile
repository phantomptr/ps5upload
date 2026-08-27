PS5_HOST ?= 192.168.137.2
PS5_PORT ?= 9021

ifdef PS5_PAYLOAD_SDK
    include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
else
    $(error PS5_PAYLOAD_SDK is undefined)
endif

ELF := ps5upload.elf

BLAKE3_DIR := third_party/blake3
# Use the SIMD dispatcher + all x86-64 assembly backends (SSE2/SSE4.1/AVX2/AVX512).
# Zen 2 (PS5) supports through AVX2; dispatcher picks best at runtime.
# Portable falls back on unsupported hardware.
BLAKE3_SRCS := $(BLAKE3_DIR)/blake3.c \
               $(BLAKE3_DIR)/blake3_portable.c \
               $(BLAKE3_DIR)/blake3_dispatch.c \
               $(BLAKE3_DIR)/blake3_sse2_x86-64_unix.S \
               $(BLAKE3_DIR)/blake3_sse41_x86-64_unix.S \
               $(BLAKE3_DIR)/blake3_avx2_x86-64_unix.S \
               $(BLAKE3_DIR)/blake3_avx512_x86-64_unix.S

# SQLite, compiled into the payload from the amalgamation that
# scripts/install-ps5-sdk.sh fetches and checksum-verifies.
#
# Why the payload carries its own copy: no PS5 firmware ships
# libSceSqlite.sprx anywhere under /system/common/lib, /system/priv/lib or
# /system_ex/common/lib, so dlsym(RTLD_DEFAULT, "sqlite3_open_v2") can never
# resolve on a console. That is a statement about the *platform*, not about
# what we can link — a static library is just more of our own .text.
# ps5-payload-dev/pacbrew-repo builds this exact version for prospero, which
# is where the approach came from.
#
# Costs ~900 KB of .text (~480 KB more in the gzipped bundle copy). Paid
# because the alternative is parsing SQLite's on-disk b-tree format by hand,
# which can only ever read — and repairing a console that lists a title it
# refuses to delete means writing.
SQLITE_DIR := third_party/sqlite3
# The object lands here, NOT in $(SQLITE_DIR): CI installs the SDK (and with
# it these sources) under sudo, so the source directory ends up root-owned
# while the build step runs as the normal user. Writing the object beside
# the sources would fail with EACCES there.
SQLITE_OBJ := sqlite3.o

# Only the compile-time options that are safe with the *prebuilt*
# amalgamation. SQLite's grammar-affecting SQLITE_OMIT_* flags (OMIT_VACUUM,
# OMIT_WINDOWFUNC, OMIT_CTE, …) require regenerating the parser from
# canonical sources; set against the amalgamation they leave the
# pre-generated parser calling functions that no longer exist and the link
# fails. Verified one at a time — do not add an OMIT flag here without
# rebuilding.
#
#   TEMP_STORE=2     temp b-trees in memory; /system_data is not reliably
#                    writable and a spill there would fail mid-query.
#   THREADSAFE=1     serialised. Several connection threads reach the
#                    content-database handlers concurrently.
#   OMIT_LOAD_EXTENSION  no dlopen of arbitrary .so from a database file.
#   DQS=0            double-quoted string literals are an error, not a
#                    silent fallback to a string when an identifier is
#                    misspelled.
#
# UTF-16 is deliberately NOT omitted: it saves 18 KB and would break
# outright on a database written with a UTF-16 encoding.
SQLITE_CFLAGS := -Os -g0 \
                 -DSQLITE_OS_UNIX=1 \
                 -DSQLITE_THREADSAFE=1 \
                 -DSQLITE_TEMP_STORE=2 \
                 -DSQLITE_OMIT_LOAD_EXTENSION=1 \
                 -DSQLITE_OMIT_DEPRECATED=1 \
                 -DSQLITE_OMIT_JSON=1 \
                 -DSQLITE_DEFAULT_MEMSTATUS=0 \
                 -DSQLITE_DQS=0 \
                 -DSQLITE_UNTESTABLE=1

INCLUDES := -Iinclude -I$(BLAKE3_DIR) -I$(SQLITE_DIR)
CFLAGS := -Wall -Wextra -Werror -g $(INCLUDES)

# Sony sprx linkage. The payload calls sceLncUtilLaunchApp +
# sceKernelGetCpuTemperature etc., and rtld must run those
# libraries' init functions before main() invokes them. With
# compile-time linkage (DT_NEEDED entries here), the rtld walks
# init order correctly. An earlier dlopen-at-runtime path wedged
# on FW 9.60 because some Sony APIs reach into per-library
# internal state that gets initialised lazily — by the time we
# called the API, the state hadn't been built yet.
#
# register.c keeps its dlsym-via-RTLD_DEFAULT resolution as a
# backstop: on a firmware where one of these libraries fails to
# load, dlsym returns NULL for the missing symbols and the
# affected handler returns a clear "service_unavailable" error
# instead of crashing. Belt-and-braces.
# Compile-time linkage: ONLY libraries we know are present on every
# supported PS5 firmware. A missing DT_NEEDED entry makes rtld refuse
# to load the binary at all (no toast, no port bind, loader silently
# rejects the ELF — exact symptom we hit when the previous build
# added libSceFsInternalForVsh / libSceNet / libSceSysCore here).
# Optional features (sceFsMountGamePkg, sceNetGetIfList, etc.)
# dlopen their backing SPRX at runtime — missing SPRX → handler
# returns "service_unavailable" instead of bricking the payload.
#
# `-lkernel_sys` enables compile-time NID resolution for sceKernel* symbols
# (sceKernelGetCpuTemperature / sceKernelGetSocSensorTemperature /
# sceKernelGetCpuFrequency / sceKernelGetSocPowerConsumption). The older path
# used `dlsym(RTLD_DEFAULT, "sceKernelGetCpuTemperature")` which fails on FW
# points where libkernel exports NID-only, then fell back to ptrace-into-
# SceShellUI — which destabilises the console (screen blank, occasional
# power-off). Direct compile-time linkage gives stable per-call sensor reads
# with no ptrace dance. libkernel_sys.sprx is part of every supported PS5
# firmware so adding the DT_NEEDED entry is safe.
#
# libSceRegMgr is linked for the same reason. sys_registry.c resolves the
# sceRegMgr* symbols with dlsym(RTLD_DEFAULT), and on a payload that does
# NOT link the library those lookups all return NULL — every registry read
# and write failed with SYS_REGISTRY_ERR_NO_SYMBOL (0xE0003002), silently,
# because the callers passed NULL for the error code. Verified on hardware:
# the user-slot table read back empty and Remote Play could never find the
# logged-in user. The registry is core to the system (unlike the optional
# libSceRemoteplay), so the DT_NEEDED entry is safe.
LIBS := -lSceSystemService -lSceUserService -lSceAppInstUtil -lSceNotification \
        -lSceRegMgr -lkernel_sys

SRCS := src/main.c src/runtime.c src/shell_builtin.c src/takeover.c src/register.c src/hw_info.c \
        src/drive_sensors.c \
        src/proc_list.c src/ptrace_remote.c src/shellui_rpc.c src/bgft.c \
        src/sony_api_lock.c src/kernel_rw_lock.c src/sys_time.c \
        src/sys_registry.c src/smp_meta.c src/profile.c \
        src/backup.c src/remoteplay.c src/fan_curve.c src/notif.c \
        src/wake_watchdog.c src/cheats.c src/activity.c src/sdk_changer.c \
        src/tmdb.c src/fw_spoof.c src/ftp_server.c src/focus_probe.c \
        src/content_db.c \
        $(BLAKE3_SRCS)

# Headers as Make prerequisites so editing a .h triggers a rebuild. This
# is belt-and-braces given the recipe below re-compiles every .c every
# time, but it guards against the one case where the build system
# otherwise misses: `touch include/foo.h` alone, with no .c change.
HEADERS := $(wildcard include/*.h) $(wildcard $(BLAKE3_DIR)/*.h)

all: $(ELF) $(ELF).gz

# Third-party code is not held to our warning policy: sqlite3.c is a
# generated 9 MB translation unit and -Wall -Wextra -Werror on it would make
# every upstream SQLite release a build break for us. Compiled separately so
# our own sources keep -Werror.
$(SQLITE_OBJ): $(SQLITE_DIR)/sqlite3.c $(SQLITE_DIR)/sqlite3.h
	$(CC) $(SQLITE_CFLAGS) -c -o $@ $<

$(SQLITE_DIR)/sqlite3.c $(SQLITE_DIR)/sqlite3.h:
	@echo "ERROR: SQLite sources are missing from $(SQLITE_DIR)." >&2
	@echo "       Run scripts/install-ps5-sdk.sh to fetch them." >&2
	@false

$(ELF): $(SRCS) $(HEADERS) $(SQLITE_OBJ)
	$(CC) $(CFLAGS) -o $@ $(SRCS) $(SQLITE_OBJ) $(LIBS)

# Gzipped copy for the Tauri bundle resource. linuxdeploy (Linux
# AppImage bundler) walks every ELF in the AppDir and tries to
# resolve its .so deps via `patchelf --print-needed`. The PS5
# payload's FreeBSD ELF depends on PS5 sprx modules that can never
# exist on Linux, so linuxdeploy aborts the build. The .gz form
# shows gzip magic (`\x1f\x8b`) instead of ELF magic — bundler sees
# a regular data file and ships it as a resource. The host decompresses
# on first use (probes.rs::find_bundled_payload).
$(ELF).gz: $(ELF)
	gzip -kf $<

clean:
	rm -f $(ELF) $(ELF).gz $(SQLITE_OBJ)

send: $(ELF)
	@echo "Sending $(ELF) to $(PS5_HOST):$(PS5_PORT) ..."
	nc -w 1 $(PS5_HOST) $(PS5_PORT) < $(ELF)
	@echo "Sent — wait for PS5 notification before testing"

.PHONY: all clean send
