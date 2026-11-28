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

CORE := src/core/json.cpp src/core/ws.cpp src/core/proto.cpp src/core/jitter.cpp src/core/settings.cpp
NET := src/net/net.cpp src/app/guest.cpp src/app/companion.cpp src/app/helper.cpp src/app/log.cpp
UI := src/app/app.cpp src/ui/canvas.cpp src/ui/strings.cpp

.PHONY: all app release test unit e2e shots clean

all: app

app:
	./build.sh

release:
	./build.sh release

test: unit e2e

unit: $(T)/test_core
	$(T)/test_core

e2e: $(T)/play-cli
	$(PYTHON) -I tests/e2e_host.py $(T)/play-cli $(T)/e2e

$(T)/test_core: tests/test_core.cpp $(CORE) $(wildcard src/core/*.hpp)
	@mkdir -p $(T)
	$(HOST_CXX) $(HOST_FLAGS) $(SAN) -o $@ tests/test_core.cpp $(CORE)

$(T)/play-cli: tests/play_cli.cpp $(CORE) $(NET) $(wildcard src/*/*.hpp)
	@mkdir -p $(T)
	$(HOST_CXX) $(HOST_FLAGS) -pthread -o $@ tests/play_cli.cpp $(CORE) $(NET)

$(T)/qrcodegen.o: third_party/qrcodegen/qrcodegen.c
	@mkdir -p $(T)
	$(HOST_CC) -O2 -c -o $@ $<

$(T)/ui-shot: tests/ui_shot.cpp $(CORE) $(NET) $(UI) $(T)/qrcodegen.o $(wildcard src/*/*.hpp)
	$(HOST_CXX) $(HOST_FLAGS) -pthread -o $@ tests/ui_shot.cpp $(CORE) $(NET) $(UI) $(T)/qrcodegen.o

shots: $(T)/ui-shot
	@mkdir -p $(T)/shots
	$(T)/ui-shot build/assets $(T)/shots

clean:
	rm -rf build dist
