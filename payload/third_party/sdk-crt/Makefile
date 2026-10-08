#   Copyright (C) 2023 John Törnblom
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

include ../Makefile.inc

CFLAGS += -ffreestanding -fno-builtin -nostdlib -fPIC
CFLAGS += -target x86_64-sie-ps5 -fno-plt -fno-stack-protector
CFLAGS += -fvisibility-nodllstorageclass=default -Wall -Werror
CFLAGS += -fms-extensions -Wno-microsoft-anon-tag
CFLAGS += -O1

OBJ := crt.o syscall.o klog.o nid.o kernel.o rtld.o rtld_so.o rtld_sprx.o \
       rtld_payload.o rtld_dlfcn.o mdbg.o patch.o kernel_procio.o \
       kernel_iommu.o _start.o

.PHONY: all clean install

all: crt1.o crti.o crtn.o crtbegin.o crtend.o crtbeginS.o crtendS.o

crtbeginS.o:
	$(AR) -rsc $@

crtendS.o:
	$(AR) -rsc $@

crtbegin.o:
	$(AR) -rsc $@

crtend.o:
	$(AR) -rsc $@

crti.o:
	$(AR) -rsc $@

crtn.o:
	$(AR) -rsc $@

crt1.o: $(OBJ)
	$(LD) -m elf_x86_64 -r -o $@ $^

%.o: %.c
	$(CC) -c $(CFLAGS) -o $@ $^

%.o: %.S
	$(CC) -c $(CFLAGS) -masm=intel -o $@ $^

clean:
	rm -f *.o *.a

install: crt1.o crti.o crtn.o crtbegin.o crtend.o crtbeginS.o crtendS.o
	install -d $(DESTDIR)/target/lib
	install $^ $(DESTDIR)/target/lib
