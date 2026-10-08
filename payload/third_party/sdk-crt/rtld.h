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

#pragma once

#include "payload.h"


/**
 * Double linked list for keeping track of deps.
 **/
typedef struct rtld_lib_seq {
  struct rtld_lib *lib;
  struct rtld_lib_seq *next;
  struct rtld_lib_seq *prev;
} rtld_lib_seq_t;


/**
 * Prototypes for different kinds of libs (.sprx or .so).
 **/
typedef struct rtld_lib {
  int (*open)(struct rtld_lib* ctx);
  int (*init)(struct rtld_lib* ctx, int, char**, char**);
  void* (*sym2addr)(struct rtld_lib* ctx, const char* name);
  const char* (*addr2sym)(struct rtld_lib* ctx, void* addr);
  int (*fini)(struct rtld_lib* ctx);
  int (*close)(struct rtld_lib* ctx);
  void (*destroy)(struct rtld_lib* ctx);

  char soname[1024];
  void *mapbase;
  unsigned long mapsize;
  int refcnt;
  struct rtld_lib* parent;
  struct {
    rtld_lib_seq_t* head;
    rtld_lib_seq_t* tail;
  } children;
} rtld_lib_t;


/**
 * Allocate a new lib object with the given soname.
 **/
rtld_lib_t* __rtld_lib_new(rtld_lib_t* prev, const char* soname);


/**
 * Open a lib.
 **/
int __rtld_lib_open(rtld_lib_t* ctx);


/**
 * Append a dependency.
 **/
int __rtld_lib_append_dep(rtld_lib_t* ctx, rtld_lib_t* lib);


/**
 * Remove a dependency.
 **/
int __rtld_lib_remove_dep(rtld_lib_t* ctx, rtld_lib_t* lib);


/**
 * Run lib constructors.
 **/
int __rtld_lib_init(rtld_lib_t* ctx, int argc, char** argv, char** envp);


/**
 * Recursively search for a lib defining a symbol with the given name.
 **/
rtld_lib_t* __rtld_lib_sym2lib(rtld_lib_t* ctx, const char* name);


/**
 * Recursively search for the lib defining the given address.
 **/
rtld_lib_t* __rtld_lib_addr2lib(rtld_lib_t* ctx, void* addr);


/**
 * Get the address of a symbol with the given name.
 **/
void* __rtld_lib_sym2addr(rtld_lib_t* ctx, const char* name);


/**
 * Find the symbol name closest to the given address.
 **/
const char* __rtld_lib_addr2sym(rtld_lib_t* ctx, void* addr);


/**
 * Run lib destructors.
 **/
int __rtld_lib_fini(rtld_lib_t* ctx);


/**
 * Close the lib.
 **/
int __rtld_lib_close(rtld_lib_t* ctx);


/**
 * Free up all memory associated with the lib.
 **/
void __rtld_lib_destroy(rtld_lib_t* ctx);


/**
 * Initialize the lib loader.
 **/
int __rtld_init(void);


/**
 * Find the path to a file with the given name.
 *
 * The search order is as follows:
 *   1 - absolute path
 *   2 - /system/priv/lib/
 *   3 - /system/common/lib/
 *   4 - /system_ex/priv_ex/lib/
 *   5 - /system_ex/common_ex/lib/
 *   6 - paths in the env var LD_LIBRARY_PATH
 *   7 - /user/homebrew/lib/
 *   8 - current work dir
 **/
int __rtld_find_file(const char *name, char* path);

