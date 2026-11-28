# PS5Upload Play - native PS5 app.
#   make -C apps/play           build dist/FAKE99261/ (Docker on macOS/Linux)
#   make -C apps/play release   also dist/PS5Upload-Play-FAKE99261-<version>.zip
#   make -C apps/play test      host unit tests + the app's network code against a stand-in
#                               engine (host compiler and python3 only, no Docker)
#   make -C apps/play shots     every screen as a picture in build/host-tests/shots (after a build)
# SPDX-License-Identifier: GPL-3.0-or-later

HOST_CXX ?= clang++
HOST_CC ?= clang
PYTHON ?= python3
HOST_FLAGS ?= -std=c++20 -O1 -g -Wall -Wextra -Werror
SAN ?= -fsanitize=address,undefined
T := build/host-tests

CORE := src/core/json.cpp src/core/ws.cpp src/core/proto.cpp src/core/jitter.cpp src/core/settings.cpp \
        src/core/invite.cpp src/core/friends.cpp src/core/hdr.cpp \
        src/core/rooms.cpp src/core/chat.cpp src/core/voice.cpp src/core/textkb.cpp
NET := src/net/net.cpp src/net/hub.cpp src/net/tls.cpp src/net/seal.cpp src/net/route.cpp src/net/route_probe.cpp \
       src/app/guest.cpp src/app/guest_route.cpp src/app/route_pick.cpp src/app/companion.cpp src/app/helper.cpp \
       src/app/log.cpp src/app/waiting_room.cpp src/app/rooms_net.cpp
UI := src/app/app.cpp src/app/easy.cpp src/app/route_ui.cpp src/ui/canvas.cpp src/ui/strings.cpp src/ui/i18n.cpp
# TLS for the hub (docs/EASY_JOIN.md): BearSSL and the ISRG roots the helper's tailnet uses.
BEARSSL := ../../third_party/bearssl
TLS_INC := -I$(BEARSSL)/inc -I../../tailnet/src
BR_SRCS := $(sort $(wildcard $(BEARSSL)/src/*/*.c))
BR_LIB := $(T)/libbearssl.a

.PHONY: all app release test unit e2e shots clean

all: app

app:
	./build.sh

release:
	./build.sh release

test: unit e2e

unit: $(T)/test_core $(T)/test_route $(T)/test_rooms
	$(T)/test_core
	$(T)/test_route
	$(T)/test_rooms
	$(PYTHON) -I tests/seal_check.py build/host-tests/seal-vector.txt
	$(PYTHON) -I tools/i18n.py check

e2e: $(T)/play-cli
	$(PYTHON) -I tests/e2e_host.py $(T)/play-cli $(T)/e2e

$(T)/test_core: tests/test_core.cpp $(CORE) src/ui/i18n.cpp src/ui/strings.cpp $(wildcard src/core/*.hpp src/ui/*.hpp)
	@mkdir -p $(T)
	$(HOST_CXX) $(HOST_FLAGS) $(SAN) -o $@ tests/test_core.cpp $(CORE) src/ui/i18n.cpp src/ui/strings.cpp

$(BR_LIB): $(BR_SRCS) src/net/tls_roots.c
	@mkdir -p $(T)/bearssl
	@for s in $(BR_SRCS); do o=$(T)/bearssl/$$(echo $${s#$(BEARSSL)/src/} | tr / _).o; \
	    [ $$o -nt $$s ] || $(HOST_CC) -O2 -w -I$(BEARSSL)/inc -I$(BEARSSL)/src -c $$s -o $$o || exit 1; done
	$(HOST_CC) -O2 -w $(TLS_INC) -c src/net/tls_roots.c -o $(T)/bearssl/tls_roots.o
	rm -f $@ && ar rcs $@ $(T)/bearssl/*.o

# Play Rooms (docs/ROOMS.md): lists, chat, voice, seats, the sealed knock (BearSSL).
$(T)/test_rooms: tests/test_rooms.cpp $(CORE) $(NET) $(BR_LIB) $(wildcard src/*/*.hpp)
	@mkdir -p $(T)
	$(HOST_CXX) $(HOST_FLAGS) $(SAN) $(TLS_INC) -DPLAY_TEST_LOOPBACK -pthread -o $@ tests/test_rooms.cpp $(CORE) $(NET) $(BR_LIB)

# The way to the engine (src/net/route.cpp): tiers, meters, switch rules, the backups' budget.
$(T)/test_route: tests/test_route.cpp src/net/route.cpp src/net/route.hpp
	@mkdir -p $(T)
	$(HOST_CXX) $(HOST_FLAGS) $(SAN) -o $@ tests/test_route.cpp src/net/route.cpp

$(T)/play-cli: tests/play_cli.cpp $(CORE) $(NET) $(BR_LIB) $(wildcard src/*/*.hpp)
	@mkdir -p $(T)
	$(HOST_CXX) $(HOST_FLAGS) $(TLS_INC) -DPLAY_TEST_LOOPBACK -pthread -o $@ tests/play_cli.cpp $(CORE) $(NET) $(BR_LIB)

$(T)/qrcodegen.o: third_party/qrcodegen/qrcodegen.c
	@mkdir -p $(T)
	$(HOST_CC) -O2 -c -o $@ $<

$(T)/ui-shot: tests/ui_shot.cpp $(CORE) $(NET) $(UI) $(T)/qrcodegen.o $(BR_LIB) $(wildcard src/*/*.hpp)
	$(HOST_CXX) $(HOST_FLAGS) $(TLS_INC) -pthread -o $@ tests/ui_shot.cpp $(CORE) $(NET) $(UI) $(T)/qrcodegen.o $(BR_LIB)

# LANG_CODE=ja make shots: the screens in that language
shots: $(T)/ui-shot
	@mkdir -p $(T)/shots
	$(T)/ui-shot build/assets $(T)/shots $(LANG_CODE)

clean:
	rm -rf build dist
