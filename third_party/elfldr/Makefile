#   Copyright (C) 2024 John Törnblom
#
# This file is free software; you can redistribute it and/or modify it
# under the terms of the GNU General Public License as published by
# the Free Software Foundation; either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful, but
# WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
# General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program; see the file COPYING. If not see
# <http://www.gnu.org/licenses/>.

PS5_HOST ?= ps5
PS5_PORT ?= 9020

ifdef PS5_PAYLOAD_SDK
    include $(PS5_PAYLOAD_SDK)/toolchain/prospero.mk
else
    $(error PS5_PAYLOAD_SDK is undefined)
endif

CFLAGS := -Wall -Werror -O1
LDADD  := -lSceSsl -lSceHttp

all: elfldr-ps5.elf

bootstrap.o: socksrv.elf
main.o: bootstrap.elf

bootstrap.elf: bootstrap.o elfldr.o pt.o notify.o
	$(CC) -o $@ $^
	$(STRIP) $@

socksrv.elf: socksrv.o selfldr.o elfldr.o pt.o notify.o uri.o
	$(CC) -o $@ $^ $(LDADD)
	$(STRIP) $@

elfldr-ps5.elf: main.o elfldr.o pt.o notify.o
	$(CC) -o $@ $^
	$(STRIP) $@

clean:
	rm -f *.o *.elf

test: elfldr-ps5.elf
	$(PS5_DEPLOY) -h $(PS5_HOST) -p $(PS5_PORT) $^

.INTERMEDIATE: socksrv.elf bootstrap.elf
