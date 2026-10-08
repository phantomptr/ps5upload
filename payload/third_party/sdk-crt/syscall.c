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

#include "payload.h"


static __attribute__ ((used)) long ptr_syscall = 0;

void
__crt_syscall() {
  asm(".intel_syntax noprefix\n"
      "  mov rax, rdi\n"
      "  mov rdi, rsi\n"
      "  mov rsi, rdx\n"
      "  mov rdx, rcx\n"
      "  mov r10, r8\n"
      "  mov r8,  r9\n"
      "  mov r9,  qword ptr [rsp + 8]\n"
      "  call qword ptr [rip + ptr_syscall]\n"
      "  ret\n");
}


int
__crt_syscall_init(payload_args_t *args) {
  int (*sceKernelDlsym)(int, const char*, void*) = 0;

  if(args->sys_dynlib_dlsym(0x1, "sceKernelDlsym", &sceKernelDlsym)) {
    args->sys_dynlib_dlsym(0x2001, "sceKernelDlsym", &sceKernelDlsym);
  }
  if(sceKernelDlsym == args->sys_dynlib_dlsym) {
    if(args->sys_dynlib_dlsym(0x1, "getpid", &ptr_syscall)) {
      args->sys_dynlib_dlsym(0x2001, "getpid", &ptr_syscall);
    }
  } else {
    ptr_syscall = (long)args->sys_dynlib_dlsym;
  }

  if(!ptr_syscall) {
    return -1;
  }

  ptr_syscall += 0xa; // jump directly to the syscall instruction

  return 0;
}
