/* Copyright (C) 2026 John Törnblom, idlesauce, and sleirsgoevy

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
#include "mdbg.h"


#define X86_PG_V  0x01
#define X86_PG_PS 0x80

#define PG_FRAME (0xffffffffff000ul)


/**
 * Walk the page table hierarchy (9 bit indices in 4 levels) to find the
 * entry of the physical address mapped to the vaddr of the given process.
 **/
int
kernel_proc_getpaddr(int pid, unsigned long vaddr, unsigned long* paddr,
		     unsigned long* plen) {
  unsigned long table_offset;
  unsigned long table_index;
  unsigned long vmspace;
  unsigned long pml4u;
  unsigned long proc;
  unsigned long cr3;
  unsigned long pte;

  if(!KERNEL_OFFSET_VMSPACE_VM_PMAP) {
    return -1;
  }
  if(!(proc=kernel_get_proc(pid))) {
    return -1;
  }

  if(kernel_copyout(proc + KERNEL_OFFSET_PROC_P_VMSPACE,
		    &vmspace, sizeof(vmspace))) {
    return -1;
  }
  if(kernel_copyout(vmspace + KERNEL_OFFSET_VMSPACE_VM_PMAP + 0x20,
		    &pml4u, sizeof(pml4u))) {
    return -1;
  }
  if(kernel_copyout(vmspace + KERNEL_OFFSET_VMSPACE_VM_PMAP + 0x28,
		    &cr3, sizeof(cr3))) {
    return -1;
  }

  pte = cr3;
  for(long bitpos=39; bitpos>=12; bitpos-=9) {
    table_index = (vaddr >> bitpos) & ((1ull << 9) - 1);
    table_offset = table_index * 8;
    pte &= PG_FRAME;

    if(kernel_copyout(pml4u-cr3 + table_offset + pte, &pte, sizeof(pte))) {
      return -1;
    }

    // check entry validity
    if(!(pte & X86_PG_V)) {
      return -1;
    }

    // check if the entry is flagged as large or if we reached the last level
    if((pte & X86_PG_PS) || bitpos == 12) {
      *paddr = pte;
      *paddr &= (1ul << 52) - (1ul << bitpos);
      *paddr |= vaddr & ((1ul << bitpos) - 1);
      *plen = (*paddr | ((1ul << bitpos) - 1)) + 1 - *paddr;
      return 0;
    }
  }

  return -1;
}


int
kernel_proc_copyin(int pid, const void *buf, unsigned long addr,
		   unsigned long len) {
  unsigned long paddr;
  unsigned long plen;

  // lets see if we can just use mdbg
  if(!mdbg_copyin(pid, buf, addr, len)) {
    return 0;
  }

  /* ps5upload: no DMAP base was found (see kernel.c), so no physical-address fallback. */
  if(!KERNEL_ADDRESS_DMAP_BASE) {
    return -1;
  }

  for(unsigned long i=0; i<len; i+=plen) {
    if(kernel_proc_getpaddr(pid, addr + i, &paddr, &plen)) {
      return -1;
    }

    if(plen > len) {
      plen = len;
    }

    if(kernel_copyin(((unsigned char*)buf) + i,
		     KERNEL_ADDRESS_DMAP_BASE + paddr, plen)) {
      return -1;
    }
  }

  return 0;
}


int
kernel_proc_copyout(int pid, unsigned long addr, void *buf,
		    unsigned long len) {
  unsigned long paddr;
  unsigned long plen;

  // lets see if we can just use mdbg
  if(!mdbg_copyout(pid, addr, buf, len)) {
    return 0;
  }

  /* ps5upload: no DMAP base was found (see kernel.c), so no physical-address fallback. */
  if(!KERNEL_ADDRESS_DMAP_BASE) {
    return -1;
  }

  for(unsigned long i=0; i<len; i+=plen) {
    if(kernel_proc_getpaddr(pid, addr + i, &paddr, &plen)) {
      return -1;
    }

    if(plen > len) {
      plen = len;
    }

    if(kernel_copyout(KERNEL_ADDRESS_DMAP_BASE + paddr,
		      ((unsigned char*)buf) + i, plen)) {
      return -1;
    }
  }

  return 0;
}


int
kernel_proc_setchar(int pid, unsigned long addr, char val) {
  return kernel_proc_copyin(pid, &val, addr, sizeof(val));
}


int
kernel_proc_setshort(int pid, unsigned long addr, short val) {
  return kernel_proc_copyin(pid, &val, addr, sizeof(val));
}


int
kernel_proc_setint(int pid, unsigned long addr, int val) {
  return kernel_proc_copyin(pid, &val, addr, sizeof(val));
}


int
kernel_proc_setlong(int pid, unsigned long addr, long val) {
  return kernel_proc_copyin(pid, &val, addr, sizeof(val));
}


char
kernel_proc_getchar(int pid, unsigned long addr) {
  char val = 0;

  kernel_proc_copyout(pid, addr, &val, sizeof(val));

  return val;
}


short
kernel_proc_getshort(int pid, unsigned long addr) {
  short val = 0;

  kernel_proc_copyout(pid, addr, &val, sizeof(val));

  return val;
}


int
kernel_proc_getint(int pid, unsigned long addr) {
  int val = 0;

  kernel_proc_copyout(pid, addr, &val, sizeof(val));

  return val;
}


long
kernel_proc_getlong(int pid, unsigned long addr) {
  long val = 0;

  kernel_proc_copyout(pid, addr, &val, sizeof(val));

  return val;
}
