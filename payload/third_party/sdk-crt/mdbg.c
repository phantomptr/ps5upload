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
#include "mdbg.h"
#include "syscall.h"


/**
 *
 **/
#define SCE_AUTHID_COREDUMP 0x4800000000000006l


/**
 *
 **/
#define MDBG_MEMOP_READ  0x12
#define MDBG_MEMOP_WRITE 0x13


/**
 *
 **/
typedef struct mdbg_cmd {
  unsigned long type;
  unsigned long cmd;
} mdbg_cmd_t;


/**
 *
 **/
typedef struct mdbg_memop_args {
  int pid;
  unsigned long src;
  unsigned long dst;
  unsigned long len;
} mdbg_memop_args_t;


/**
 *
 **/
typedef struct mdbg_res {
  int status;
  unsigned long len;
} mdbg_res_t;



/**
 *
 **/
static int
mdbg_memop(int memop, mdbg_memop_args_t *args) {
  static const unsigned char privcaps[16] = {0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff,
					     0xff,0xff,0xff,0xff,0xff,0xff,0xff,0xff};
  int pid = (int)__crt_syscall(SYS_getpid);
  mdbg_cmd_t cmd = {1, memop};
  unsigned char caps[16];
  unsigned long authid;
  mdbg_res_t res;
  long err;

  if(args->pid < 0) {
    args->pid = pid;
  }

  if(kernel_get_ucred_caps(pid, caps)) {
    return -1;
  }

  if(!(authid=kernel_get_ucred_authid(pid))) {
    return -1;
  }

  if(kernel_set_ucred_authid(pid, SCE_AUTHID_COREDUMP)) {
    return -1;
  }

  if(kernel_set_ucred_caps(pid, privcaps)) {
    return -1;
  }

  do {
    res.len = res.status = 0;
    if((err=__crt_syscall(SYS_mdbg_call, &cmd, args, &res)) == -1) {
      break;
    }
    args->src += res.len;
    args->dst += res.len;
    args->len -= res.len;
  } while(res.status != 0 && args->len && res.len);

  if(kernel_set_ucred_authid(pid, authid)) {
    return -1;
  }
  if(kernel_set_ucred_caps(pid, caps)) {
    return -1;
  }

  return err;
}


int
mdbg_copyout(int pid, unsigned long addr, void *buf, unsigned long len) {
  mdbg_memop_args_t args = {pid, addr, (unsigned long)buf, len};

  return mdbg_memop(MDBG_MEMOP_READ, &args);
}


int
mdbg_copyin(int pid, const void* buf, unsigned long addr, unsigned long len) {
  mdbg_memop_args_t args = {pid, addr, (unsigned long)buf, len};

  return mdbg_memop(MDBG_MEMOP_WRITE, &args);
}


int
mdbg_setchar(int pid, unsigned long addr, char val) {
  return mdbg_copyin(pid, &val, addr, sizeof(val));
}

int
mdbg_setshort(int pid, unsigned long addr, short val) {
  return mdbg_copyin(pid, &val, addr, sizeof(val));
}

int
mdbg_setint(int pid, unsigned long addr, int val) {
  return mdbg_copyin(pid, &val, addr, sizeof(val));
}

int
mdbg_setlong(int pid, unsigned long addr, long val) {
  return mdbg_copyin(pid, &val, addr, sizeof(val));
}

long
mdbg_getlong(int pid, unsigned long addr) {
  long val = 0;

  mdbg_copyout(pid, addr, &val, sizeof(val));

  return val;
}

int
mdbg_getint(int pid, unsigned long addr) {
  int val = 0;

  mdbg_copyout(pid, addr, &val, sizeof(val));

  return val;
}

short
mdbg_getshort(int pid, unsigned long addr) {
  short val = 0;

  mdbg_copyout(pid, addr, &val, sizeof(val));

  return val;
}

char
mdbg_getchar(int pid, unsigned long addr) {
  char val = 0;

  mdbg_copyout(pid, addr, &val, sizeof(val));

  return val;
}
