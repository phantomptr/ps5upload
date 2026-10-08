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
#include "syscall.h"
#include "rtld.h"
#include "rtld_dlfcn.h"
#include "rtld_payload.h"
#include "rtld_so.h"
#include "rtld_sprx.h"


/**
 * Dependencies to standard libraries.
 **/
static char* (*strcpy)(char*, const char*) = 0;
static int (*strcmp)(const char*, const char*) = 0;
static int (*strncmp)(const char*, const char*, unsigned long) = 0;
static unsigned long (*strlen)(const char*) = 0;
static char* (*strcat)(char*, const char*) = 0;
static int (*sprintf)(char*, const char*, ...) = 0;

static char* (*getenv)(const char*) = 0;

static void* (*calloc)(unsigned long, unsigned long) = 0;
static void (*free)(void*) = 0;


/**
 * Extend the rtld_lib_t type with auxiliary parameters for shallow
 * copies of a lib. This requires -fms-extensions.
 **/
typedef struct rtld_ref_lib {
  struct rtld_lib;

  rtld_lib_t* ref;
} rtld_ref_lib_t;


/**
 * Check if a string ends with a given suffix.
 **/
static int
endswith(const char *string, const char* suffix) {
  unsigned long suffix_len = strlen(suffix);
  unsigned long string_len = strlen(string);

  if(string_len < suffix_len) {
    return 0;
  }

  return strncmp(string + string_len - suffix_len,
                 suffix, suffix_len) == 0;
}


/**
 *
 **/
static int
ref_open(rtld_lib_t* ctx) {
  rtld_ref_lib_t* lib = (rtld_ref_lib_t*)ctx;
  return __rtld_lib_open(lib->ref);
}


/**
 *
 **/
static int
ref_init(rtld_lib_t* ctx, int argc, char** argv, char** envp) {
  return 0;
}


/**
 *
 **/
static void*
ref_sym2addr(rtld_lib_t* ctx, const char* name) {
  rtld_ref_lib_t* lib = (rtld_ref_lib_t*)ctx;
  return __rtld_lib_sym2addr(lib->ref, name);
}


/**
 *
 **/
static const char*
ref_addr2sym(rtld_lib_t* ctx, void* addr) {
  rtld_ref_lib_t* lib = (rtld_ref_lib_t*)ctx;
  return __rtld_lib_addr2sym(lib->ref, addr);
}


/**
 *
 **/
static int
ref_fini(rtld_lib_t* ctx) {
  return 0;
}


/**
 *
 **/
static int
ref_close(rtld_lib_t* ctx) {
  rtld_ref_lib_t* lib = (rtld_ref_lib_t*)ctx;
  return __rtld_lib_close(lib->ref);
}


/**
 *
 **/
static void
ref_destroy(rtld_lib_t* ctx) {
  free(ctx);
}


static rtld_lib_t*
__rtld_lib_soname2lib(rtld_lib_t* ctx, const char *soname) {
  rtld_lib_t* lib;

  if(!ctx) {
    return 0;
  }

  if(!strcmp(ctx->soname, soname)) {
    return ctx;
  }

  for(rtld_lib_seq_t* it=ctx->children.head; it; it=it->next) {
    if((lib=__rtld_lib_soname2lib(it->lib, soname))) {
      return lib;
    }
  }

  return 0;
}


int
__rtld_find_file(const char *name, char* path) {
  unsigned long random_word_len = 0xff;
  char random_word[0xff];
  unsigned long off = 0;
  const char *ldpaths;
  char buf[0x100];
  char cwd[1024];
  int err = 0;

  if(!name || !*name || !path) {
    return -1;
  }

  path[0] = 0;
  if(name[0] == '/') {
    if(!(err=__crt_syscall(SYS_stat, name, buf))) {
      strcpy(path, name);
    }
    return err;
  }

  sprintf(path, "/system/priv/lib/%s", name);
  if(!__crt_syscall(SYS_stat, path, buf)) {
    return 0;
  }

  sprintf(path, "/system/common/lib/%s", name);
  if(!__crt_syscall(SYS_stat, path, buf)) {
    return 0;
  }

  sprintf(path, "/system_ex/priv_ex/lib/%s", name);
  if(!__crt_syscall(SYS_stat, path, buf)) {
    return 0;
  }

  sprintf(path, "/system_ex/common_ex/lib/%s", name);
  if(!__crt_syscall(SYS_stat, path, buf)) {
    return 0;
  }

  if(!__crt_syscall(SYS_randomized_path, 0, &random_word, &random_word_len)) {
    sprintf(path, "/%s/priv/lib/%s", random_word, name);
    if(!__crt_syscall(SYS_stat, path, buf)) {
      return 0;
    }

    sprintf(path, "/%s/common/lib/%s", random_word, name);
    if(!__crt_syscall(SYS_stat, path, buf)) {
      return 0;
    }

    sprintf(path, "/%s/priv_ex/lib/%s", random_word, name);
    if(!__crt_syscall(SYS_stat, path, buf)) {
      return 0;
    }

    sprintf(path, "/%s/common_ex/lib/%s", random_word, name);
    if(!__crt_syscall(SYS_stat, path, buf)) {
      return 0;
    }
  }

  if((ldpaths=getenv("LD_LIBRARY_PATH"))) {
    for(int i=0; ldpaths[i]; i++) {
      path[off++] = ldpaths[i];
      if(path[off] == ':' || ldpaths[i+1] == 0) {
	path[off] = '/';
	path[off+1] = 0;
	strcat(path, name);
	off = 0;
	if(!__crt_syscall(SYS_stat, path, buf)) {
	  return 0;
	}
      }
    }
  }

  sprintf(path, "/user/homebrew/lib/%s", name);
  if(!__crt_syscall(SYS_stat, path, buf)) {
    return 0;
  }

  if(! __crt_syscall(SYS___getcwd, cwd, sizeof(cwd))) {
    sprintf(path, "%s/%s", cwd, name);
    if(!__crt_syscall(SYS_stat, path, buf)) {
      return 0;
    }
  }

  path[0] = 0;
  return -1;
}


rtld_lib_t*
__rtld_lib_new(rtld_lib_t* parent, const char* soname) {
  rtld_lib_t* root = parent;
  rtld_ref_lib_t* lib = 0;
  rtld_lib_t* ref = 0;
  char path[1024];

  // find the root node
  while(root && root->parent) {
    root = root->parent;
  }

  if(__rtld_find_file(soname, path)) {
    strcpy(path, soname);
  }

  // check if the lib is already loaded
  if((ref=__rtld_lib_soname2lib(root, path))) {
    lib = calloc(1, sizeof(rtld_ref_lib_t));
    lib->parent   = parent;
    lib->ref      = ref;
    lib->open     = ref_open;
    lib->init     = ref_init;
    lib->sym2addr = ref_sym2addr;
    lib->addr2sym = ref_addr2sym;
    lib->fini     = ref_fini;
    lib->close    = ref_close;
    lib->destroy  = ref_destroy;
    lib->refcnt   = 0;
    lib->mapbase  = ref->mapbase;
    lib->mapsize  = ref->mapsize;

    strcpy(lib->soname, ref->soname);

    return (rtld_lib_t*)lib;
  }

  // .sprx and .so files have different loaders
  if(endswith(soname, ".sprx")) {
    return __rtld_sprx_new(parent, soname);
  } else {
    return __rtld_so_new(parent, soname);
  }
}


int
__rtld_lib_open(rtld_lib_t* ctx) {
  if(!ctx) {
    return -1;
  }

  ctx->refcnt++;
  if(ctx->refcnt == 1) {
    return ctx->open(ctx);
  }

  return 0;
}


int
__rtld_lib_append_dep(rtld_lib_t* ctx, rtld_lib_t* lib) {
  rtld_lib_seq_t* seq;

  if(!ctx || !lib) {
    return -1;
  }

  if(!(seq=calloc(1, sizeof(rtld_lib_seq_t)))) {
    return -1;
  }

  seq->lib = lib;

  if(!ctx->children.head) {
    ctx->children.head = seq;
  }

  if(!ctx->children.tail) {
    ctx->children.tail = seq;
  } else {
    seq->prev = ctx->children.tail;
    ctx->children.tail->next = seq;
    ctx->children.tail = seq;
  }

  return 0;
}


int
__rtld_lib_remove_dep(rtld_lib_t* ctx, rtld_lib_t* lib) {
  if(!ctx || !lib) {
    return -1;
  }

  for(rtld_lib_seq_t* it=ctx->children.head; it; it=it->next) {
    if(it->lib != lib) {
      continue;
    }

    if(it->prev) {
      it->prev->next = it->next;
    }
    if(it->next) {
      it->next->prev = it->prev;
    }

    free(it);

    return 0;
  }

  return -1;
}


int
__rtld_lib_init(rtld_lib_t* ctx, int argc, char** argv, char** envp) {
  int err;

  if(!ctx) {
    return -1;
  }

  // init in reverse order
  for(rtld_lib_seq_t* it=ctx->children.tail; it; it=it->prev) {
    if((err=__rtld_lib_init(it->lib, argc, argv, envp))) {
      return err;
    }
  }

  return ctx->init(ctx, argc, argv, envp);
}


rtld_lib_t*
__rtld_lib_sym2lib(rtld_lib_t* ctx, const char* name) {
  rtld_lib_t* lib;

  if(!ctx || !name) {
    return 0;
  }

  if(ctx->sym2addr(ctx, name)) {
    return ctx;
  }

  // TODO: breadth-first search
  for(rtld_lib_seq_t* it=ctx->children.head; it; it=it->next) {
      if((lib=__rtld_lib_sym2lib(it->lib, name))) {
      return lib;
    }
  }

  return 0;
}


rtld_lib_t*
__rtld_lib_addr2lib(rtld_lib_t* ctx, void* addr) {
  rtld_lib_t* lib;

  if(!ctx || !addr) {
    return 0;
  }

  if(addr >= ctx->mapbase &&
     addr < ctx->mapbase + ctx->mapsize) {
      return ctx;
    }

  for(rtld_lib_seq_t* it=ctx->children.head; it; it=it->next) {
      if((lib=__rtld_lib_addr2lib(it->lib, addr))) {
      return lib;
    }
  }

  return 0;
}


void*
__rtld_lib_sym2addr(rtld_lib_t* ctx, const char* name) {
  if(ctx && name) {
    return ctx->sym2addr(ctx, name);
  } else {
    return 0;
  }
}


const char*
__rtld_lib_addr2sym(rtld_lib_t* ctx, void* addr) {
  if(ctx && addr) {
    return ctx->addr2sym(ctx, addr);
  } else {
    return 0;
  }
}


int
__rtld_lib_fini(rtld_lib_t* ctx) {
  int err;

  if(!ctx) {
    return -1;
  }

  if((err=ctx->fini(ctx))) {
    return err;
  }

  for(rtld_lib_seq_t* it=ctx->children.head; it; it=it->next) {
    if((err=__rtld_lib_fini(it->lib))) {
      return err;
    }
  }

  return 0;
}


int
__rtld_lib_close(rtld_lib_t* ctx) {
  int err;

  if(!ctx) {
    return -1;
  }

  ctx->refcnt--;
  if(ctx->refcnt > 0) {
    return 0;
  }

  for(rtld_lib_seq_t* it=ctx->children.tail; it; it=it->prev) {
    if((err=__rtld_lib_close(it->lib))) {
      return err;
    }
  }

  if((err=ctx->close(ctx))) {
    return err;
  }

  if(ctx->parent) {
    return __rtld_lib_remove_dep(ctx->parent, ctx);
  } else {
    return 0;
  }
}


void
__rtld_lib_destroy(rtld_lib_t* ctx) {
  if(ctx) {
    ctx->destroy(ctx);
  }
}


int
__rtld_init(void) {
  int err;

  if(!KERNEL_DLSYM(0x2, strcpy)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, strcat)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, strcmp)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, strncmp)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, strlen)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, sprintf)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, calloc)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, free)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, getenv)) {
    return -1;
  }

  if((err=__rtld_sprx_init())) {
    return err;
  }
  if((err=__rtld_so_init())) {
    return err;
  }
  if((err=__rtld_payload_init())) {
    return err;
  }
  if((err=__rtld_dlfcn_init())) {
    return err;
  }

  return 0;
}

