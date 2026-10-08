/* Copyright (C) 2026 John Törnblom

This program is free software; you can redistribute it and/or modify it
under the terms of the GNU General Public License as published by the
Free Software Foundation; either version 3, or (at your option) any
later version.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program; see the file COPYING. If not, see
<http://www.gnu.org/licenses/>.  */

#include "kernel.h"
#include "kernel_iommu.h"


/**
 * Physical base address of the AMD IOMMU MMIO region.
 **/
#define IOMMU_MMIO_BASE 0xfdd80000UL


/**
 * MMIO register offsets for the IOMMU command buffer.
 **/
#define IOMMU_MMIO_CB3_HEAD 0xe000
#define IOMMU_MMIO_CB3_TAIL 0xe008


/**
 * IOMMU command buffer ring constants.
 **/
#define IOMMU_CB_SIZE  0x2000
#define IOMMU_CB_MASK  (IOMMU_CB_SIZE - 1)
#define IOMMU_CMD_SIZE 0x10


/**
 * Field offsets within the IOMMU softc data structure.
 **/
#define IOMMU_SC_CB3_PTR   0x80
#define IOMMU_SC_CB3_INDEX 0x88


/**
 * Global state that is initialised on first call.
 **/
static unsigned long iommu_mmio_kaddr = 0;
static unsigned long iommu_cb_base    = 0;
static unsigned long iommu_cb_index   = 0;


/**
 * Initialize the global state.
 **/
static int
iommu_init(void) {
  if(iommu_mmio_kaddr) {
    return 0;
  }
  if(!KERNEL_ADDRESS_IOMMU_SOFTC) {
    return -1;
  }

  iommu_mmio_kaddr = KERNEL_ADDRESS_DMAP_BASE + IOMMU_MMIO_BASE;

  if(kernel_copyout(KERNEL_ADDRESS_IOMMU_SOFTC + IOMMU_SC_CB3_PTR,
		    &iommu_cb_base, sizeof(iommu_cb_base))) {
    iommu_mmio_kaddr = 0;
    return -1;
  }
  iommu_cb_index = KERNEL_ADDRESS_IOMMU_SOFTC + IOMMU_SC_CB3_INDEX;

  if(!iommu_cb_base) {
    iommu_mmio_kaddr = 0;
    return -1;
  }

  return 0;
}


/**
 * Submit a command to the IOMMU command queue.
 **/
static int
iommu_submit_cmd(const void *cmd) {
  unsigned long curr_tail;
  unsigned long next_tail;
  unsigned long head;
  unsigned long tail;

  if(kernel_copyout(iommu_mmio_kaddr + IOMMU_MMIO_CB3_TAIL,
		    &curr_tail, sizeof(curr_tail))) {
    return -1;
  }
  next_tail = (curr_tail + IOMMU_CMD_SIZE) & IOMMU_CB_MASK;

  if(kernel_copyin(cmd, iommu_cb_base + curr_tail, IOMMU_CMD_SIZE)) {
    return -1;
  }
  if(kernel_copyin(&next_tail, iommu_mmio_kaddr + IOMMU_MMIO_CB3_TAIL,
		   sizeof(next_tail))) {
    return -1;
  }
  if(kernel_copyin(&next_tail, iommu_cb_index, sizeof(next_tail))) {
    return -1;
  }

  while(1) {
    if(kernel_copyout(iommu_mmio_kaddr + IOMMU_MMIO_CB3_HEAD, &head, sizeof(head))) {
      return -1;
    }
    if(kernel_copyout(iommu_mmio_kaddr + IOMMU_MMIO_CB3_TAIL, &tail, sizeof(tail))) {
      return -1;
    }
    if(head == tail) {
      return 0;
    }
  }
}


/**
 * Submit a write command to IOMMU command queue.
 **/
static int
iommu_write8(unsigned long paddr, unsigned long val) {
  unsigned int cmd[4];

  cmd[0] = (unsigned int)(paddr & 0xFFFFFFF8) | 0x05;
  cmd[1] = (unsigned int)((paddr >> 32) & 0xFFFFF) | 0x10000000;
  cmd[2] = (unsigned int)(val);
  cmd[3] = (unsigned int)(val >> 32);

  return iommu_submit_cmd(cmd);
}


/**
 * Copy byte values into a word.
 **/
static unsigned long
iommu_patch_word(unsigned long word, const unsigned char *src,
		 unsigned int off, unsigned int n) {
  unsigned char *b = (unsigned char*)&word;

  for(unsigned int i=0; i<n; i++) {
    b[off+i] = src[i];
  }

  return word;
}


int
kernel_iommu_copyin(const void *uaddr, unsigned long kaddr, unsigned long len) {
  const unsigned char *src = (const unsigned char*)uaddr;
  unsigned long paddr;
  unsigned long chunk;
  unsigned long curr;
  unsigned long plen;
  unsigned long coff;
  unsigned int head;
  unsigned long off;
  unsigned long val;

  if(!uaddr || !kaddr || !len) {
    return -1;
  }
  if(iommu_init()) {
    return -1;
  }

  for(off=0; off<len; off+=chunk) {
    if(kernel_proc_getpaddr(0, kaddr+off, &paddr, &plen)) {
      return -1;
    }
    chunk = len - off;
    if(chunk > plen) {
      chunk = plen;
    }

    coff = 0;
    if(paddr & 7) {
      head = (unsigned int)(8 - (paddr & 7));
      if(head > (unsigned int)chunk)  {
	head = (unsigned int)chunk;
      }

      if(kernel_copyout(KERNEL_ADDRESS_DMAP_BASE + (paddr & ~7UL),
			&curr, sizeof(curr))) {
	return -1;
      }
      curr = iommu_patch_word(curr, src+off, (unsigned int)(paddr & 7), head);
      if(iommu_write8(paddr & ~7UL, curr)) {
	return -1;
      }
      coff += head;
    }

    while(coff+8 <= chunk) {
      val = 0;
      for(int i=0; i<8; i++) {
	val |= (unsigned long)src[off+coff+i] << (i*8);
      }
      iommu_write8(paddr+coff, val);
      coff += 8;
    }

    if(coff < chunk) {
      if(kernel_copyout(KERNEL_ADDRESS_DMAP_BASE+paddr+coff, &curr,
			sizeof(curr))) {
	return -1;
      }
      curr = iommu_patch_word(curr, src+off+coff, 0, (unsigned int)(chunk-coff));
      if(iommu_write8(paddr+coff, curr)) {
	return -1;
      }
    }
  }

  return 0;
}


int
kernel_iommu_setlong(unsigned long kaddr, unsigned long val) {
  return kernel_iommu_copyin(&val, kaddr, sizeof(val));
}


int
kernel_iommu_setint(unsigned long kaddr, unsigned int val) {
  return kernel_iommu_copyin(&val, kaddr, sizeof(val));
}


int
kernel_iommu_setshort(unsigned long kaddr, unsigned short val) {
  return kernel_iommu_copyin(&val, kaddr, sizeof(val));
}


int
kernel_iommu_setchar(unsigned long kaddr, unsigned char val) {
  return kernel_iommu_copyin(&val, kaddr, sizeof(val));
}
