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

#include "elf.h"
#include "kernel.h"
#include "klog.h"
#include "syscall.h"
#include "rtld_payload.h"


/**
 * Extend rtld_lib_t anonymously with auxiliary parameters needed
 * for resolving symbols in the payload needed by shared objects.
 **/
typedef struct rtld_payload_lib {
  struct rtld_lib;

  Elf64_Sym* symtab;
  char* strtab;
  unsigned long symtab_size;
} rtld_payload_lib_t;


/**
 * Dependencies to standard libraries.
 **/
static int (*strcmp)(const char*, const char*) = 0;
static int (*strcpy)(char*, const char*) = 0;

static void* (*calloc)(unsigned long, unsigned long) = 0;
static void* (*memcpy)(void*, const void*, unsigned long) = 0;
static void (*free)(void*) = 0;


/**
 * Dependencies provided by the ELF linker.
 **/
extern void (*__init_array_start[])(int, char**, char**) __attribute__((weak));
extern void (*__init_array_end[])(int, char**, char**) __attribute__((weak));

extern void (*__fini_array_start[])(void) __attribute__((weak));
extern void (*__fini_array_end[])(void) __attribute__((weak));

extern unsigned char __image_start[] __attribute__((weak));
extern unsigned char __image_end[] __attribute__((weak));

extern Elf64_Dyn _DYNAMIC[];


/**
 * Process the DT_NEEDED entry in the .dynamic section.
 **/
static int
dt_needed(rtld_payload_lib_t* ctx, const char* soname) {
  rtld_lib_t *lib = (rtld_lib_t*)ctx;
  rtld_lib_t *needed;

  if(!(needed=__rtld_lib_new(lib, soname))) {
    klog_printf("Unable to load %s\n", soname);
    return -1;

  } else if(__rtld_lib_open(needed) < 0) {
    klog_printf("unable to load '%s'\n", soname);
    __rtld_lib_destroy(needed);
    return -1;
  }

  return __rtld_lib_append_dep(lib, needed);
}


/**
 * Process the R_X86_64_GLOB_DAT relocation type.
 **/
static int
r_glob_dat(rtld_payload_lib_t* ctx, Elf64_Rela* rela) {
  Elf64_Sym* sym = ctx->symtab + ELF64_R_SYM(rela->r_info);
  const char* name = ctx->strtab + sym->st_name;
  void* loc = ctx->mapbase + rela->r_offset;
  rtld_lib_t* lib = (rtld_lib_t*)ctx;
  void* val = 0;

  if((lib=__rtld_lib_sym2lib(lib, name))) {
    if((val=__rtld_lib_sym2addr(lib, name))) {
      memcpy(loc, &val, sizeof(val));
      return 0;
    }
  }

  // ignore unresolved weak symbols
  if(ELF64_ST_BIND(sym->st_info) == STB_WEAK) {
    return 0;
  }

  klog_printf("Unable to resolve '%s'\n", name);

  return -1;
}


/**
 * Process the R_X86_64_JMP_SLOT relocation type.
 **/
static int
r_jmp_slot(rtld_payload_lib_t* ctx, Elf64_Rela* rela) {
  return r_glob_dat(ctx, rela);
}


/**
 * Process the R_X86_64_64 relocation type.
 **/
static int
r_direct_64(rtld_payload_lib_t* ctx, Elf64_Rela* rela) {
  Elf64_Sym* sym = ctx->symtab + ELF64_R_SYM(rela->r_info);
  const char* name = ctx->strtab + sym->st_name;
  void* loc = ctx->mapbase + rela->r_offset;
  rtld_lib_t* lib = (rtld_lib_t*)ctx;
  void* val = 0;

  if((lib=__rtld_lib_sym2lib(lib, name))) {
    if((val=__rtld_lib_sym2addr(lib, name))) {
      val += rela->r_addend;
      memcpy(loc, &val, sizeof(val));
      return 0;
    }
  }

  // ignore unresolved weak symbols
  if(ELF64_ST_BIND(sym->st_info) == STB_WEAK) {
    return 0;
  }

  klog_printf("Unable to resolve '%s'\n", name);

  return -1;
}


/**
 * Process the R_X86_64_RELATIVE relocation type.
 **/
static int
r_relative(rtld_payload_lib_t* ctx, Elf64_Rela* rela) {
  unsigned long* loc = (unsigned long*)(ctx->mapbase + rela->r_offset);
  unsigned long val = (unsigned long)(ctx->mapbase + rela->r_addend);

  memcpy(loc, &val, sizeof(val));

  return 0;
}


/**
 *  Figure out the symtab size.
 **/
static unsigned int
dynsym_count(unsigned int *gnu_hash) {
  unsigned int nbuckets = gnu_hash[0];
  unsigned int symoffset = gnu_hash[1];
  unsigned int bloom_size = gnu_hash[2];
  unsigned long *bloom = (unsigned long *)(gnu_hash + 4);
  unsigned int *buckets = (unsigned int *)(bloom + bloom_size);
  unsigned int *chain = buckets + nbuckets;
  unsigned int max_index = 0;
  unsigned int index = 0;

  for(unsigned int i=0; i<nbuckets; i++) {
    if(buckets[i] == 0) {
      continue;
    }

    index = buckets[i];
    while(1) {
      if(chain[index-symoffset] & 1) {
        break;
      }
      index++;
    }

    if(index > max_index) {
      max_index = index;
    }
  }

  return max_index + 1;
}


static int
payload_open(rtld_lib_t* ctx) {
  rtld_payload_lib_t* lib = (rtld_payload_lib_t*)ctx;
  unsigned int *gnu_hash = 0;
  unsigned long relasz = 0;
  unsigned long pltsz = 0;
  Elf64_Rela* rela = 0;
  Elf64_Rela* plt = 0;

  // find lookup tables
  for(int i=0; _DYNAMIC[i].d_tag!=DT_NULL; i++) {
    switch(_DYNAMIC[i].d_tag) {
    case DT_SYMTAB:
      lib->symtab = (Elf64_Sym*)(lib->mapbase + _DYNAMIC[i].d_un.d_ptr);
      break;

    case DT_STRTAB:
      lib->strtab = (char*)(lib->mapbase + _DYNAMIC[i].d_un.d_ptr);
      break;

    case DT_GNU_HASH:
      gnu_hash = (unsigned int*)(lib->mapbase + _DYNAMIC[i].d_un.d_ptr);
      break;

    case DT_RELA:
      rela = (Elf64_Rela*)(lib->mapbase + _DYNAMIC[i].d_un.d_ptr);
      break;

    case DT_RELASZ:
      relasz = _DYNAMIC[i].d_un.d_val;
      break;

    case DT_JMPREL:
      plt = (Elf64_Rela*)(lib->mapbase + _DYNAMIC[i].d_un.d_ptr);
      break;

    case DT_PLTRELSZ:
      pltsz = _DYNAMIC[i].d_un.d_val;
      break;
    }
  }

  // symtab size is determined using DT_GNU_HASH
  if(gnu_hash) {
    lib->symtab_size = dynsym_count(gnu_hash) * sizeof(Elf64_Sym);
  }

  // load needed libraries
  for(int i=0; _DYNAMIC[i].d_tag!=DT_NULL; i++) {
    switch(_DYNAMIC[i].d_tag) {
    case DT_NEEDED:
      if(dt_needed(lib, lib->strtab + _DYNAMIC[i].d_un.d_val)) {
	return -1;
      }
      break;
    }
  }

  // apply relocations
  for(int i=0; i<relasz/sizeof(Elf64_Rela); i++) {
    switch(rela[i].r_info & 0xffffffffl) {
    case R_X86_64_JMP_SLOT:
      if(r_jmp_slot(lib, &rela[i])) {
	return -1;
      }
      break;

    case R_X86_64_64:
      if(r_direct_64(lib, &rela[i])) {
	return -1;
      }
      break;

    case R_X86_64_GLOB_DAT:
      if(r_glob_dat(lib, &rela[i])) {
	return -1;
      }
      break;

    case R_X86_64_RELATIVE:
      if(r_relative(lib, &rela[i])) {
	return -1;
      }
      break;

    default:
      klog_printf("Unsupported relocation type %x\n",
                  rela[i].r_info);
      return -1;
    }
  }

  for(int i=0; i<pltsz/sizeof(Elf64_Rela); i++) {
    switch(plt[i].r_info & 0xffffffffl) {
    case R_X86_64_JMP_SLOT:
      if(r_jmp_slot(lib, &plt[i])) {
	return -1;
      }
      break;

    default:
      klog_printf("Unsupported plt type %x\n",
                  plt[i].r_info);
      break;
    }
  }

  return 0;
}


static int
payload_init(rtld_lib_t* ctx, int argc, char** argv, char** envp) {
  unsigned long count = __init_array_end - __init_array_start;

  for(unsigned long i=0; i<count; i++) {
    __init_array_start[i](argc, argv, envp);
  }

  return 0;
}


static int
payload_fini(rtld_lib_t* ctx) {
  unsigned long count = __fini_array_end - __fini_array_start;

  for(unsigned long i=0; i<count; i++) {
    __fini_array_start[count-i-1]();
  }

  return 0;
}


static void*
payload_sym2addr(rtld_lib_t* ctx, const char* name) {
  rtld_payload_lib_t* lib = (rtld_payload_lib_t*)ctx;

  if(!lib->symtab || !lib->strtab) {
    return 0;
  }

  for(unsigned long i=0; i<lib->symtab_size/sizeof(Elf64_Sym); i++) {
    if(!lib->symtab[i].st_size) {
      continue;
    }

    if(!strcmp(name, lib->strtab + lib->symtab[i].st_name)) {
      return lib->mapbase + lib->symtab[i].st_value;
    }
  }

  return 0;
}


static const char*
payload_addr2sym(rtld_lib_t* ctx, void* addr) {
  rtld_payload_lib_t* lib = (rtld_payload_lib_t*)ctx;

  if(!lib->symtab || !lib->strtab) {
    return 0;
  }

  if(addr < lib->mapbase ||
     addr > lib->mapbase + lib->mapsize) {
    return 0;
  }

  for(unsigned long i=0; i<lib->symtab_size/sizeof(Elf64_Sym); i++) {
    if(!lib->symtab[i].st_size) {
      continue;
    }

    if(addr >= lib->mapbase + lib->symtab[i].st_value &&
       addr <= lib->mapbase + lib->symtab[i].st_value + lib->symtab[i].st_size) {
      return lib->strtab + lib->symtab[i].st_name;
    }
  }

  return 0;
}


static int
payload_close(rtld_lib_t* ctx) {
  return 0;
}


static void
payload_destroy(rtld_lib_t* ctx) {
  free(ctx);
}


rtld_lib_t*
__rtld_payload_new(const char *soname) {
  rtld_payload_lib_t* lib = calloc(1, sizeof(rtld_payload_lib_t));
  int pid = __crt_syscall(SYS_getpid);

  lib->open     = payload_open;
  lib->init     = payload_init;
  lib->sym2addr = payload_sym2addr;
  lib->addr2sym = payload_addr2sym;
  lib->fini     = payload_fini;
  lib->close    = payload_close;
  lib->destroy  = payload_destroy;
  lib->refcnt   = 0;
  lib->mapbase  = __image_start;
  lib->mapsize  = __image_end - __image_start;

  strcpy(lib->soname, soname);
  __crt_syscall(0x268, pid, lib->soname, 1024);

  return (rtld_lib_t*)lib;
}


int
__rtld_payload_init(void) {
  if(!KERNEL_DLSYM(0x2, calloc)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, memcpy)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, free)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, strcpy)) {
    return -1;
  }
  if(!KERNEL_DLSYM(0x2, strcmp)) {
    return -1;
  }

  return 0;
}

