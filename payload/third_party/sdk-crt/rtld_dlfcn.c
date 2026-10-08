/* Copyright (C) 2025 John Törnblom

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
#include "rtld.h"
#include "rtld_dlfcn.h"


/**
 * Standard posix macros.
 **/
#define ENOENT 2
#define ENOMEM 12
#define EINVAL 22
#define ENOSYS 78

#define RTLD_NEXT    ((void*)-1)
#define RTLD_DEFAULT ((void*)-2)
#define RTLD_SELF    ((void*)-3)

#define RTLD_LAZY     0x0001
#define RTLD_NOW      0x0002
#define RTLD_MODEMASK 0x0003
#define RTLD_GLOBAL   0x0100
#define RTLD_LOCAL    0x0000
#define RTLD_TRACE    0x0200
#define RTLD_NODELETE 0x1000
#define RTLD_NOLOAD   0x2000


/**
 * Dependencies to standard libraries.
 **/
static char* (*_Strerror)(int, char*) = 0;
static void* (*calloc)(unsigned long, unsigned long) = 0;
static void (*free)(void*) = 0;
static char** (*getargv)(void) = 0;
static int (*getargc)(void) = 0;
static char** environ = 0;


/**
 * Global variables.
 **/
static int g_dlerrno = 0;
static rtld_lib_t* g_root = 0;


void*
__dlopen(const char *filename, int flags) {
  rtld_lib_t* lib;

  if(!(flags & RTLD_MODEMASK)) {
    g_dlerrno = EINVAL;
    return 0;
  }
  if(flags & RTLD_TRACE) {
    g_dlerrno = ENOSYS;
    return 0;
  }
  if(flags & RTLD_NOLOAD) {
    g_dlerrno = ENOSYS;
    return 0;
  }

  /*
  if(flags & RTLD_LAZY) {
    g_dlerrno = ENOSYS;
    return 0;
  }
  */

  if(!filename) {
    return g_root;
  }

  if(!(lib=__rtld_lib_new(g_root, filename))) {
    g_dlerrno = ENOMEM;
    return 0;
  }

  if((g_dlerrno=__rtld_lib_open(lib))) {
    __rtld_lib_destroy(lib);
    return 0;
  }

  if(flags & RTLD_GLOBAL) {
    __rtld_lib_append_dep(g_root, lib);
  }

  __rtld_lib_init(lib, getargc(), getargv(), environ);

  g_dlerrno = 0;

  return lib;
}


int
__dladdr(void *addr, Dl_info *info) {
  rtld_lib_t* lib = __rtld_lib_addr2lib(g_root, addr);

  if(lib) {
    info->dli_fname = lib->soname;
    info->dli_fbase = lib->mapbase;
    info->dli_sname = __rtld_lib_addr2sym(lib, addr);
    info->dli_saddr = __rtld_lib_sym2addr(lib, info->dli_sname);
    return 1;
  }

  g_dlerrno = EINVAL;

  return 0;
}


void*
__dlsym(void *handle, const char *symbol) {
  rtld_lib_t* lib = handle;
  void* addr = 0;

  if(handle == 0) {
    g_dlerrno = ENOSYS;
    return 0;
  }
  if(handle == RTLD_NEXT) {
    g_dlerrno = ENOSYS;
    return 0;
  }
  if(handle == RTLD_SELF) {
    g_dlerrno = ENOSYS;
    return 0;
  }
  if(handle == RTLD_DEFAULT) {
    lib = g_root;
  }

  if((lib=__rtld_lib_sym2lib(lib, symbol))) {
    if((addr=__rtld_lib_sym2addr(lib, symbol))) {
      return addr;
    }
  }

  g_dlerrno = EINVAL;
  return 0;
}


int
__dlclose(void *handle) {
  rtld_lib_t* lib = handle;

  if(g_root == lib || !lib) {
    g_dlerrno = -1;
    return g_dlerrno;
  }

  __rtld_lib_fini(lib);

  g_dlerrno = __rtld_lib_close(lib);
  __rtld_lib_destroy(lib);

  return g_dlerrno;
}


char*
__dlerror(void) {
  if(g_dlerrno) {
    return _Strerror(g_dlerrno, 0);
  }

  return 0;
}


void __rtld_dlfcn_setroot(rtld_lib_t *lib) {
  g_root = lib;
}


int
__rtld_dlfcn_init(void) {
  if(!KERNEL_DLSYM(0x2, calloc)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, free)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, _Strerror)) {
    return -1;
  }

  if(!KERNEL_DLSYM(0x1, getargc)) {
    if(!KERNEL_DLSYM(0x2001, getargc)) {
      return -1;
    }
  }
  if(!KERNEL_DLSYM(0x1, getargv)) {
    if(!KERNEL_DLSYM(0x2001, getargv)) {
      return -1;
    }
  }
  if(!KERNEL_DLSYM(0x1, environ)) {
    if(!KERNEL_DLSYM(0x2001, environ)) {
      return -1;
    }
  }

  return 0;
}

