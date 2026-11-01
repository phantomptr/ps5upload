PS5_HOST ?= 192.168.137.2
PS5_PORT ?= 9021

ifdef PS5_PAYLOAD_SDK
    include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
else
    $(error PS5_PAYLOAD_SDK is undefined)
endif

# Our own build of the SDK's startup code; see sdk-crt.mk for why.
include sdk-crt.mk

ELF := ps5upload.elf

# AVA1 (github.com/phantomptr/ava1) is the ../third_party/ava1 submodule; BLAKE3 and Monocypher
# come with it, so the payload and the protocol library use one copy of each.
AVA1 := ../third_party/ava1/c
BLAKE3_DIR := $(AVA1)/third_party/blake3
# Use the SIMD dispatcher + all x86-64 assembly backends (SSE2/SSE4.1/AVX2/AVX512).
# Zen 2 (PS5) supports through AVX2; dispatcher picks best at runtime.
# Portable falls back on unsupported hardware.
BLAKE3_C_SRCS := $(BLAKE3_DIR)/blake3.c \
                 $(BLAKE3_DIR)/blake3_portable.c \
                 $(BLAKE3_DIR)/blake3_dispatch.c
# The C files are built as separate objects at -O2 (review P5): the rest of the payload is
# compiled without -O, which left the BLAKE3 dispatcher, chunk state and parent merges at
# -O0 on every hashed byte (the assembly kernels were unaffected). Third-party code is
# not held to -Werror, like SQLite and Monocypher. The assembly stays in SRCS.
BLAKE3_OBJS := $(patsubst $(BLAKE3_DIR)/%.c,build/blake3/%.o,$(BLAKE3_C_SRCS))
BLAKE3_SRCS := $(BLAKE3_DIR)/blake3_sse2_x86-64_unix.S \
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
# Monocypher (X25519 + constant-time helpers for AVA1), compiled like SQLite: third-party
# code is not held to our -Werror policy.
MONO_DIR := $(AVA1)/third_party/monocypher
MONO_OBJ := monocypher.o
AVA1_LIB := ava1_wire.c ava1_frame.c ava1_keys.c ava1_noise.c ava1_conn.c \
            ava1_store.c ava1_pairlimit.c ava1_server.c ava1_trust.c platform_ps5.c \
            ava1_aead.c ava1_chacha_avx2.c ava1_b3.c ava1_ranges.c \
            ava1_journal.c ava1_tune.c ava1_thread.c ava1_manifest.c \
            ava1_job.c ava1_data.c ava1_apply.c ava1_recv.c ava1_send.c \
            ava1_copy.c ava1_calibrate.c ava1_op.c ava1_events.c gen/ava1_gen.c
# ps5upload's own side of AVA1: the glue, and the codecs of its application schema
# (protocol/ps5upload-ava1/app.toml, `make ava1-app-gen`).
# AVA1 is built as objects at -O2: it seals every byte of a transfer, and the rest of the
# payload is compiled without -O (i.e. -O0). The AVX2 ChaCha20 alone gets -mavx2;
# ava1_aead.c calls it only once CPUID has confirmed AVX2, so the ELF stays safe on any
# x86-64. payload/build/ is git-ignored.
AVA1_OBJS := $(patsubst %.c,build/ava1/lib/%.o,$(AVA1_LIB)) build/ava1/src/ava1_glue.o build/ava1/gen/ava1_app_gen.o
AVA1_CFLAGS = $(CFLAGS) -O2 \
    -DAVA1_TIMING_FLAG='"/data/ps5upload/debug/ava1-timing"' \
    -DAVA1_LOG_SMALL_OFF_FLAG='"/data/ps5upload/debug/ava1-log-small-off"'

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

INCLUDES := -Iinclude -Ismb -I$(BLAKE3_DIR) -I$(SQLITE_DIR) -Ithird_party/tiny_aes -I$(AVA1) -I$(AVA1)/gen -I$(MONO_DIR) -Igen
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
# libSceIpmi is linked BEFORE libSceAppInstUtil on upstream's explicit
# instruction: ps5-payload-dev/sdk#26 (still open) — "you need to explicitly
# link with -lSceIpmi ... put -lSceIpmi before -lSceAppInstUtil, the order is
# important". The maintainer treats it as a bug in Sony's rtld: AppInstUtil
# depends on Ipmi, and without Ipmi loaded first the executable "does not work
# correctly". The SDK's own samples/install_app links it exactly this way.
#
# Why it is easy to miss: when the HOST process a loader injects into already
# has libSceIpmi loaded, the missing dependency is invisible — so a payload can
# work under one loader and break under another. Several popular payloads omit
# it and work, so this is hardening that follows upstream, not a proven cause
# of any specific report.
LIBS := -lSceSystemService -lSceUserService -lSceIpmi -lSceAppInstUtil \
        -lSceNotification -lSceRegMgr -lSceFsInternalForVsh -lkernel_sys

# The home-screen tile's icon0.png (512x512, the app icon client/src-tauri/icons/icon.png),
# embedded as a C array by xxd: webui_tile_icon0_png[] + webui_tile_icon0_png_len, which
# webui_tile.c writes into /data/ps5upload/tile/PSUP00001/sce_sys/. Generated under build/
# (git-ignored); `cd assets` keeps xxd's symbol names independent of the path.
WEBUI_TILE_ICON_PNG := assets/webui_tile_icon0.png
WEBUI_TILE_ICON_C := build/webui_tile_icon.c

# The console web page, built from the client (`npm run build:console`: one self-contained
# client/dist-console/index.html), gzip'd and embedded by xxd as webui_page_gz[] +
# webui_page_gz_len (include/webui_page.h), which the helper serves at GET /. Needs the client's
# node_modules (`npm ci` in client/). Rebuilt when the page's sources, the locale files (its
# strings), the PC app's tokens (src/index.css) or the build config change.
WEBUI_PAGE_C := build/webui_page.c
# The same build writes client/dist-console/strings/<lang>.json (every language but English, which
# is inside the page); scripts/embed-console-strings.mjs gzips each into build/webui_strings.c
# (include/webui_strings.h), served at GET /strings/<lang>.json.
WEBUI_STRINGS_C := build/webui_strings.c
# And its offline copy (client/src/console/offline/offlineCopy.ts): dist-console/sw.js and
# dist-console/cache.appcache, embedded by xxd as webui_sw_js[] and webui_cache_appcache[]
# (include/webui_offline.h), served at GET /sw.js and GET /cache.appcache.
WEBUI_OFFLINE_C := build/webui_offline.c
# The installer daemon the console page's package install brings up (webui_pkg.c), embedded by xxd
# as webui_installer_elf[] + webui_installer_elf_len plus its INST_VERSION (include/webui_installer.h).
# The same checked-in ELF the engine embeds (the root Makefile builds it before this payload).
WEBUI_INSTALLER_ELF := installer/ps5upload-installer.elf
WEBUI_INSTALLER_C := build/webui_installer.c

# The SMB daemon (payload/smb, include/smbd_proto.h): built by its own Makefile, stripped, and
# embedded by xxd as smbd_elf[] + smbd_elf_len (include/smb_client.h), which the helper sends to the
# console's ELF loader when the Files page first needs a network share. libsmb2 is never linked into
# the helper itself; only the daemon's small JSON and path rules are shared (smb/smbd_json.c,
# smb/smbd_path.c).
SMBD_DIR := smb
SMBD_ELF := $(SMBD_DIR)/ps5upload-smb.elf
SMBD_ELF_C := build/smbd_elf.c
SMBD_DEPS := $(wildcard $(SMBD_DIR)/*.c $(SMBD_DIR)/*.h) $(SMBD_DIR)/Makefile \
        $(wildcard third_party/libsmb2/lib/*.c third_party/libsmb2/lib/*.h third_party/libsmb2/include/*.h \
                   third_party/libsmb2/include/smb2/*.h) third_party/libsmb2/config.h include/smbd_proto.h include/config.h \
        src/pkg_info.c include/pkg_info.h
CLIENT_DIR := ../client
# src/lib too: the page imports lib/format, lib/fanTargets, lib/archiveName, lib/filePermissions,
# lib/sdkVersionHex, lib/cleanupModel and lib/cleanupText, and an edit there left a stale page.
WEBUI_PAGE_DEPS := $(shell find $(CLIENT_DIR)/src/console $(CLIENT_DIR)/src/lib -type f ! -name '*.test.ts' 2>/dev/null) \
        $(wildcard $(CLIENT_DIR)/src/i18n/locales/*.ts) $(CLIENT_DIR)/src/index.css \
        $(CLIENT_DIR)/public/logo-square.png $(CLIENT_DIR)/vite.console.config.ts \
        $(CLIENT_DIR)/package.json $(CLIENT_DIR)/package-lock.json ../scripts/build-console-strings.mjs

# The on-console archive extractor (extractor/, its own payload: decompression code linked into the
# helper crashed it, see extractor/README.md), embedded by xxd as ps5upload_extractor_elf[] +
# ps5upload_extractor_elf_len, which src/archive_extract.c sends to the console's ELF loader for each
# extraction. Built by its own Makefile (7-Zip and libarchive, C++); `cd extractor` keeps xxd's names.
EXTRACTOR_DIR := extractor
EXTRACTOR_ELF := $(EXTRACTOR_DIR)/ps5upload-extractor.elf
EXTRACTOR_ELF_C := build/extractor_elf.c

SRCS := src/main.c src/runtime.c src/shell_builtin.c src/takeover.c src/takeover_flag.c src/ownership_record.c src/ava1_stop.c src/register.c src/hw_info.c \
        src/drive_sensors.c \
        src/proc_list.c src/ptrace_remote.c src/shellui_rpc.c \
        src/sony_api_lock.c src/kernel_rw_lock.c src/sys_time.c \
        src/sys_registry.c src/smp_meta.c src/profile.c \
        src/backup.c src/remoteplay.c src/rp_pair.c src/fan_curve.c src/fan_map.c src/fan_servo.c src/mp1_clock.c src/cpu_measure.c src/indicator.c src/notif.c \
        src/wake_watchdog.c src/icon_keeper.c src/ffpfsc_unpack.c src/cheats.c src/cheats_plan.c src/cheats_guard.c src/cheats_list.c src/game_patch.c src/xml_encoding.c src/activity.c src/sdk_changer.c src/fakelib_overlay.c \
        src/tmdb.c src/fw_spoof.c src/focus_probe.c \
        src/content_db.c src/mgmt_rpc.c src/fs_jobs.c src/net_probe.c src/net_ports.c src/mgmt_fs.c src/path_policy.c \
        src/webui_http.c src/webui_auth.c src/webui_server.c src/webui_codec.c src/webui_api.c \
        src/webui_settings.c src/webui_inputlog.c src/webui_files.c src/webui_cheats_store.c src/webui_tile.c src/webui_strings.c src/webui_offline.c src/files_prefs.c src/files_handoff.c src/vinput.c src/mesh.c src/storage_cleanup.c src/cleanup_rules.c src/archive_extract.c \
        src/app_dump.c src/app_dump_core.c src/app_dump_pkg.c src/app_dump_fself.c src/app_dump_self.c src/app_dump_sha256.c $(WEBUI_TILE_ICON_C) $(EXTRACTOR_ELF_C) \
        src/pkg_info.c src/webui_pkg.c $(WEBUI_INSTALLER_C) \
        src/webui_smb.c src/smb_shares.c src/smb_client.c smb/smbd_json.c smb/smbd_path.c $(SMBD_ELF_C) \
        $(WEBUI_PAGE_C) $(WEBUI_STRINGS_C) $(WEBUI_OFFLINE_C) \
        third_party/tiny_aes/aes.c \
        $(BLAKE3_SRCS)

# Headers as Make prerequisites so editing a .h triggers a rebuild. This
# is belt-and-braces given the recipe below re-compiles every .c every
# time, but it guards against the one case where the build system
# otherwise misses: `touch include/foo.h` alone, with no .c change.
HEADERS := $(wildcard include/*.h) smb/smbd_json.h smb/smbd_path.h $(wildcard $(BLAKE3_DIR)/*.h) $(wildcard $(AVA1)/*.h) $(wildcard $(AVA1)/gen/*.h) $(wildcard gen/*.h)

all: $(ELF) $(ELF).gz

# Third-party code is not held to our warning policy: sqlite3.c is a
# generated 9 MB translation unit and -Wall -Wextra -Werror on it would make
# every upstream SQLite release a build break for us. Compiled separately so
# our own sources keep -Werror.
$(SQLITE_OBJ): $(SQLITE_DIR)/sqlite3.c $(SQLITE_DIR)/sqlite3.h
	$(CC) $(SQLITE_CFLAGS) -c -o $@ $<

$(MONO_OBJ): $(MONO_DIR)/monocypher.c $(MONO_DIR)/monocypher.h
	$(CC) -O2 -g0 -I$(MONO_DIR) -c -o $@ $<

$(SQLITE_DIR)/sqlite3.c $(SQLITE_DIR)/sqlite3.h:
	@echo "ERROR: SQLite sources are missing from $(SQLITE_DIR)." >&2
	@echo "       Run scripts/install-ps5-sdk.sh to fetch them." >&2
	@false

# The Makefile itself is a prerequisite: LIBS and CFLAGS live here, and
# without this a change to either printed "Built" while leaving the previous
# link in place. Adding -lSceIpmi did exactly that — the stale ELF had no
# libSceIpmi in DT_NEEDED until the sources were touched.
$(ELF): $(SRCS) $(HEADERS) $(SQLITE_OBJ) $(MONO_OBJ) $(BLAKE3_OBJS) $(AVA1_OBJS) $(SDK_CRT1) Makefile
	$(CC) $(CFLAGS) -nostartfiles -o $@ $(SRCS) $(BLAKE3_OBJS) $(AVA1_OBJS) $(SQLITE_OBJ) $(MONO_OBJ) $(SDK_CRT1) $(LIBS)

build/blake3/%.o: $(BLAKE3_DIR)/%.c $(HEADERS) Makefile
	@mkdir -p $(dir $@)
	$(CC) $(filter-out -Werror,$(CFLAGS)) -O2 -c -o $@ $<

build/ava1/lib/%.o: $(AVA1)/%.c $(HEADERS) Makefile
	@mkdir -p $(dir $@)
	$(CC) $(AVA1_CFLAGS) -c -o $@ $<

build/ava1/src/%.o: src/%.c $(HEADERS) Makefile
	@mkdir -p $(dir $@)
	$(CC) $(AVA1_CFLAGS) -c -o $@ $<

build/ava1/gen/%.o: gen/%.c $(HEADERS) Makefile
	@mkdir -p $(dir $@)
	$(CC) $(AVA1_CFLAGS) -c -o $@ $<

build/ava1/lib/ava1_chacha_avx2.o: $(AVA1)/ava1_chacha_avx2.c $(HEADERS) Makefile
	@mkdir -p $(dir $@)
	$(CC) $(AVA1_CFLAGS) -mavx2 -c -o $@ $<

# Always asks the extractor's Makefile (it knows its own sources); the C array is rewritten only when
# the ELF changed.
$(EXTRACTOR_ELF): FORCE
	$(MAKE) -C $(EXTRACTOR_DIR)

$(EXTRACTOR_ELF_C): $(EXTRACTOR_ELF) Makefile
	@mkdir -p $(dir $@)
	(cd $(EXTRACTOR_DIR) && xxd -i ps5upload-extractor.elf) > $@.tmp && mv $@.tmp $@

FORCE:

$(WEBUI_TILE_ICON_C): $(WEBUI_TILE_ICON_PNG) Makefile
	@mkdir -p $(dir $@)
	(cd assets && xxd -i webui_tile_icon0.png) > $@.tmp && mv $@.tmp $@

# `cd build` keeps xxd's symbol names webui_page_gz / webui_page_gz_len.
$(WEBUI_PAGE_C): $(WEBUI_PAGE_DEPS) Makefile
	@mkdir -p $(dir $@)
	npm --prefix $(CLIENT_DIR) run build:console
	gzip -9n -c $(CLIENT_DIR)/dist-console/index.html > build/webui_page.gz.tmp
	mv build/webui_page.gz.tmp build/webui_page.gz
	(cd build && xxd -i webui_page.gz) > $@.tmp && mv $@.tmp $@

# Copied under build/ first so xxd names the array webui_installer_elf.
$(WEBUI_INSTALLER_C): $(WEBUI_INSTALLER_ELF) installer/main.c Makefile
	@mkdir -p $(dir $@)
	cp $(WEBUI_INSTALLER_ELF) build/webui_installer.elf
	(cd build && xxd -i webui_installer.elf) > $@.tmp
	sed -n 's/^#define INST_VERSION *"\([0-9.]*\)".*/const char webui_installer_version[] = "\1";/p' installer/main.c >> $@.tmp
	grep -q webui_installer_version $@.tmp && mv $@.tmp $@

# The daemon ELF, stripped (its symbols are not needed on the console), then embedded. `cd build`
# keeps xxd's symbol names smbd_elf / smbd_elf_len.
$(SMBD_ELF): $(SMBD_DEPS)
	$(MAKE) -C $(SMBD_DIR)

$(SMBD_ELF_C): $(SMBD_ELF) Makefile
	@mkdir -p $(dir $@)
	$(PS5_PAYLOAD_SDK)/bin/prospero-strip -o build/smbd.elf $(SMBD_ELF)
	(cd build && xxd -i smbd.elf) > $@.tmp && mv $@.tmp $@

# Written from the dist-console/strings the page rule's build just produced.
$(WEBUI_STRINGS_C): $(WEBUI_PAGE_C) ../scripts/embed-console-strings.mjs Makefile
	@mkdir -p $(dir $@)
	node ../scripts/embed-console-strings.mjs $(CLIENT_DIR)/dist-console/strings $@

# Copied under build/ first so xxd names the arrays webui_sw_js / webui_cache_appcache.
$(WEBUI_OFFLINE_C): $(WEBUI_PAGE_C) Makefile
	@mkdir -p $(dir $@)
	cp $(CLIENT_DIR)/dist-console/sw.js build/webui_sw.js
	cp $(CLIENT_DIR)/dist-console/cache.appcache build/webui_cache.appcache
	(cd build && xxd -i webui_sw.js && xxd -i webui_cache.appcache) > $@.tmp && mv $@.tmp $@

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
	rm -f $(ELF) $(ELF).gz $(SQLITE_OBJ) $(MONO_OBJ)
	rm -rf build/ava1 build/blake3 $(SDK_CRT_BUILD)
	$(MAKE) -C $(EXTRACTOR_DIR) clean
	rm -f $(EXTRACTOR_ELF_C)
	rm -f $(WEBUI_TILE_ICON_C) $(WEBUI_PAGE_C) $(WEBUI_STRINGS_C) $(WEBUI_OFFLINE_C) build/webui_page.gz \
		build/webui_sw.js build/webui_cache.appcache $(WEBUI_INSTALLER_C) build/webui_installer.elf $(SMBD_ELF_C) build/smbd.elf
	$(MAKE) -C $(SMBD_DIR) clean

send: $(ELF)
	@echo "Sending $(ELF) to $(PS5_HOST):$(PS5_PORT) ..."
	nc -w 1 $(PS5_HOST) $(PS5_PORT) < $(ELF)
	@echo "Sent — wait for PS5 notification before testing"

.PHONY: all clean send FORCE
