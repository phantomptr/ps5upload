/* Copyright (C) 2024 John Törnblom

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
#include "klog.h"
#include "payload.h"
#include "syscall.h"


/**
 * Patch syscall permissions of the currently running process.
 **/
static int
patch_syscall_permissions(void) {
  int pid = __crt_syscall(SYS_getpid);
  unsigned long kproc;
  unsigned long kaddr;
  unsigned long uaddr;

  if(!(kproc=kernel_get_proc(pid))) {
    return -1;
  }

  if(kernel_copyout(kproc + 0x3e8, &kaddr, sizeof(kaddr)) < 0) {
    return -1;
  }

  // set lowest address allowed to make syscalls
  uaddr = 0;
  if(kernel_copyin(&uaddr, kaddr + 0xf0, sizeof(uaddr)) < 0) {
    return -1;
  }

  // set highest address allowed to make syscalls
  uaddr = -1;
  if(kernel_copyin(&uaddr, kaddr + 0xf8, sizeof(uaddr)) < 0) {
    return -1;
  }

  return 0;
}


/**
 * Patch the credentials of the currently running process.
 **/
static int
patch_kernel_ucred(void) {
  int pid = __crt_syscall(SYS_getpid);
  unsigned char attrs[32];
  unsigned char caps[16];

  if(kernel_get_ucred_caps(pid, caps)) {
    return -1;
  }
  if(kernel_get_ucred_attrs(pid, attrs)) {
    return -1;
  }

  caps[5]  = 0x1c; // ??
  caps[7]  = 0x40; // jail related?
  caps[15] |= 0x40; // jitshm
  attrs[3] |= 0x80; // ptrace

  if(kernel_set_ucred_caps(pid, caps)) {
    return -1;
  }
  if(kernel_set_ucred_attrs(pid, attrs)) {
    return -1;
  }

  return 0;
}


int
__patch_init(void) {
  int error;

  if((error=patch_kernel_ucred())) {
    return error;
  }
  if((error=patch_syscall_permissions())) {
      return error;
  }

  return 0;
}
