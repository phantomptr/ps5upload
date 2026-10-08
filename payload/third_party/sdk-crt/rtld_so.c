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
#include "rtld_so.h"
#include "syscall.h"


/**
 * Dependencies to standard libraries.
 **/
static int (*strcmp)(const char*, const char*) = 0;
static char* (*strcpy)(char*, const char*) = 0;

static void* (*malloc)(unsigned long) = 0;
static void* (*calloc)(unsigned long, unsigned long) = 0;
static void* (*memcpy)(void*, const void*, unsigned long) = 0;
static void (*free)(void*) = 0;


/**
 * Standard posix macros.
 **/
#define PROT_NONE  0x00
#define PROT_READ  0x01
#define PROT_WRITE 0x02
#define PROT_EXEC  0x04

#define MAP_SHARED  0x0001
#define MAP_PRIVATE 0x0002
#define MAP_FIXED   0x0010
#define MAP_ANON    0x1000
#define MAP_FAILED  ((void*)-1)

#define SEEK_SET 0
#define SEEK_END 2


/**
 * Convenient macros.
 **/
#define PAGE_SIZE 0x4000
#define ROUND_PG(x) (((x) + (PAGE_SIZE - 1)) & ~(PAGE_SIZE - 1))
#define TRUNC_PG(x) ((x) & ~(PAGE_SIZE - 1))
#define PFLAGS(x)   ((((x) & PF_R) ? PROT_READ  : 0) |  \
		     (((x) & PF_W) ? PROT_WRITE : 0) |  \
		     (((x) & PF_X) ? PROT_EXEC  : 0))


/**
 * Extend the rtld_lib_t type with auxiliary parameters needed by .so loader.
 * This requires -fms-extensions.
 **/
typedef struct rtld_so_lib {
  struct rtld_lib;

  unsigned char* image;

  Elf64_Ehdr *ehdr;
  Elf64_Phdr *phdr;
  Elf64_Shdr *shdr;

  char* strtab;
  Elf64_Sym* symtab;
  unsigned long symtab_size;
  Elf64_Rela* plt;
  unsigned long plt_size;

  void (**init_array)(int, char**, char**);
  unsigned long init_array_size;

  void (**fini_array)(void);
  unsigned long fini_array_size;
} rtld_so_lib_t;


/**
 * Convenient function for the standard mmap syscall.
 **/
static void*
mmap(void* addr, unsigned long len, int prot, int flags, int fd,
     unsigned long offset) {
  return (void*)__crt_syscall(SYS_mmap, addr, len, prot, flags, fd, offset);
}


/**
 * Convenient function for the standard mprotect syscall.
 **/
static int
mprotect(void* addr, unsigned long size, int prot) {
  if((prot & PROT_EXEC)) {
    return kernel_mprotect(-1, (unsigned long)addr, size, prot);
  } else {
    return (int)__crt_syscall(SYS_mprotect, addr, size, prot);
  }
}


/**
 * Convenient function for the standard munmap syscall.
 **/
static int
munmap(void* addr, unsigned long len) {
  return (int)__crt_syscall(SYS_munmap, addr, len);
}


/**
 * Read a file from disk at the given path.
 **/
static unsigned char*
readfile(const char* path) {
  unsigned char* buf;
  long len;
  int fd;

  if((fd=(int)__crt_syscall(SYS_open, path, 0)) < 0) {
    klog_perror("open");
    return 0;
  }

  if((len=__crt_syscall(SYS_lseek, fd, 0, SEEK_END)) < 0) {
    klog_perror("lseek");
    __crt_syscall(SYS_close, fd);
    return 0;
  }

  if(__crt_syscall(SYS_lseek, fd, 0, SEEK_SET) < 0) {
    klog_perror("lseek");
    __crt_syscall(SYS_close, fd);
    return 0;
  }

  if(!(buf=malloc(len))) {
    klog_perror("malloc");
    __crt_syscall(SYS_close, fd);
    return 0;
  }

  if(__crt_syscall(SYS_read, fd, buf, len) != len) {
    klog_perror("read");
    free(buf);
    __crt_syscall(SYS_close, fd);
    return 0;
  }

  if(__crt_syscall(SYS_close, fd) < 0) {
    klog_perror("close");
    free(buf);
    return 0;
  }

  return buf;
}


/**
 * Process the R_X86_64_GLOB_DAT relocation type.
 **/
static int
r_glob_dat(rtld_so_lib_t* ctx, Elf64_Rela* rela) {
  Elf64_Sym* sym = ctx->symtab + ELF64_R_SYM(rela->r_info);
  const char* name = ctx->strtab + sym->st_name;
  void* loc = ctx->mapbase + rela->r_offset;
  rtld_lib_t* lib = (rtld_lib_t*)ctx;
  void* val = 0;

  while(lib->parent) {
    lib = lib->parent;
  }

  if((lib=__rtld_lib_sym2lib(lib, name))) {
    if((val=__rtld_lib_sym2addr(lib, name))) {
      memcpy(loc, &val, sizeof(val));
      return 0;
    }
  }

  lib = (rtld_lib_t*)ctx;
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

  klog_printf("%s: unable to resolve '%s'\n", ctx->soname, name);

  return -1;
}


/**
 * Process the R_X86_64_GLOB_DAT relocation type.
 **/
static int
r_jmp_slot(rtld_so_lib_t* lib, Elf64_Rela* rela) {
  return r_glob_dat(lib, rela);
}


/**
 * Process the R_X86_64_64 relocation type.
 **/
static int
r_direct_64(rtld_so_lib_t* ctx, Elf64_Rela* rela) {
  Elf64_Sym* sym = ctx->symtab + ELF64_R_SYM(rela->r_info);
  const char* name = ctx->strtab + sym->st_name;
  void* loc = ctx->mapbase + rela->r_offset;
  rtld_lib_t* lib = (rtld_lib_t*)ctx;
  void* val = 0;

  while(lib->parent) {
    lib = lib->parent;
  }

  if((lib=__rtld_lib_sym2lib(lib, name))) {
    if((val=__rtld_lib_sym2addr(lib, name))) {
      val += rela->r_addend;
      memcpy(loc, &val, sizeof(val));
      return 0;
    }
  }

  lib = (rtld_lib_t*)ctx;
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

  klog_printf("%s: unable to resolve '%s'\n", ctx->soname, name);

  return -1;
}


/**
 * Process the R_X86_64_RELATIVE relocation type.
 **/
static int
r_relative(rtld_so_lib_t *lib, Elf64_Rela* rela) {
  void* loc = lib->mapbase + rela->r_offset;
  void* val = lib->mapbase + rela->r_addend;

  memcpy(loc, &val, sizeof(val));

  return 0;
}


/**
 * Process the DT_NEEDED entry in the .dynamic section.
 **/
static int
dt_needed(rtld_so_lib_t *ctx, const char* soname) {
  rtld_lib_t* lib = (rtld_lib_t*)ctx;
  rtld_lib_t *needed;

  if(!(needed=__rtld_lib_new(lib, soname))) {
    return -1;
  }

  if(__rtld_lib_open(needed)) {
    klog_printf("%s: unable to load '%s'\n", ctx->soname, soname);
    __rtld_lib_destroy(needed);
    return -1;
  }

  return __rtld_lib_append_dep(lib, needed);
}


/**
 * Process a program header with the entry type PT_LOAD.
 **/
static int
pt_load(rtld_so_lib_t *lib, Elf64_Phdr *phdr) {
  if(!phdr->p_memsz) {
    return 0;
  }

  if(!phdr->p_filesz) {
    return 0;
  }

  memcpy(lib->mapbase + phdr->p_vaddr,
	 lib->image + phdr->p_offset,
	 phdr->p_filesz);

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


/**
 * Process a program header with the entry type PT_DYNAMIC.
 **/
static int
pt_dynamic(rtld_so_lib_t *lib, Elf64_Phdr *phdr) {
  Elf64_Dyn *dyn = (Elf64_Dyn *)(lib->image + phdr->p_offset);
  unsigned int *gnu_hash = 0;

  // find lookup tables
  for(unsigned long i=0; dyn[i].d_tag!=DT_NULL; i++) {
    switch (dyn[i].d_tag) {
    case DT_SYMTAB:
      lib->symtab = (Elf64_Sym*)(lib->image + lib->phdr->p_offset +
                                 dyn[i].d_un.d_ptr);
      break;

    case DT_GNU_HASH:
      gnu_hash = (unsigned int*)(lib->image + lib->phdr->p_offset +
                                 dyn[i].d_un.d_ptr);
      break;

    case DT_STRTAB:
      lib->strtab = (char*)(lib->image + lib->phdr->p_offset +
                            dyn[i].d_un.d_ptr);
      break;

    case DT_JMPREL:
      lib->plt = (Elf64_Rela*)(lib->image + lib->phdr->p_offset +
                               dyn[i].d_un.d_ptr);
      break;

    case DT_PLTRELSZ:
      lib->plt_size = dyn[i].d_un.d_val;
      break;

    case DT_INIT_ARRAY:
      lib->init_array = (void*)(lib->mapbase + dyn[i].d_un.d_ptr);
      break;

    case DT_INIT_ARRAYSZ:
      lib->init_array_size = dyn[i].d_un.d_val;
      break;

    case DT_FINI_ARRAY:
      lib->fini_array = (void*)(lib->mapbase + dyn[i].d_un.d_ptr);
      break;

    case DT_FINI_ARRAYSZ:
      lib->fini_array_size = dyn[i].d_un.d_val;
      break;
    }
  }

  // symtab size is determined using DT_GNU_HASH
  if(gnu_hash) {
    lib->symtab_size = dynsym_count(gnu_hash) * sizeof(Elf64_Sym);
  }

  // load needed libraries
  for(unsigned long i=0; dyn[i].d_tag!=DT_NULL; i++) {
    switch (dyn[i].d_tag) {
    case DT_NEEDED:
      if(dt_needed(lib, lib->strtab + dyn[i].d_un.d_ptr)) {
	return -1;
      }
      break;
    }
  }

  return 0;
}


/**
 * Load a shared object into memory, and parse its attributes.
 **/
static int
so_load(rtld_so_lib_t* lib, const char* path) {
  unsigned long min_vaddr = -1;
  unsigned long max_vaddr = 0;
  Elf64_Rela* rela = 0;
  int error = 0;

  if(!(lib->image=readfile(path))) {
    return -1;
  }

  lib->ehdr = (Elf64_Ehdr*)lib->image;
  lib->phdr = (Elf64_Phdr*)(lib->image + lib->ehdr->e_phoff);
  lib->shdr = (Elf64_Shdr*)(lib->image + lib->ehdr->e_shoff);

  // compute size of virtual memory region
  for(unsigned long i=0; i<lib->ehdr->e_phnum; i++) {
    if(lib->phdr[i].p_vaddr < min_vaddr) {
      min_vaddr = lib->phdr[i].p_vaddr;
    }

    if(max_vaddr < lib->phdr[i].p_vaddr + lib->phdr[i].p_memsz) {
      max_vaddr = lib->phdr[i].p_vaddr + lib->phdr[i].p_memsz;
    }
  }

  min_vaddr = TRUNC_PG(min_vaddr);
  max_vaddr = ROUND_PG(max_vaddr);
  lib->mapsize = max_vaddr - min_vaddr;

  int flags = MAP_PRIVATE | MAP_ANON;
  int prot = PROT_READ | PROT_WRITE;
  if(lib->ehdr->e_type != ET_DYN) {
    klog_printf("%s: Not a shared object\n", lib->soname);
    return -1;
  }

  // reserve an address space of sufficient size
  if((lib->mapbase=mmap(0, lib->mapsize, prot, flags, -1, 0)) == MAP_FAILED) {
    return -1;
  }

  for(unsigned long i=0; i<lib->ehdr->e_phnum && !error; i++) {
    switch(lib->phdr[i].p_type) {
    case PT_LOAD:
      error = pt_load(lib, &lib->phdr[i]);
      break;

    case PT_DYNAMIC:
      error = pt_dynamic(lib, &lib->phdr[i]);
      break;
    }
  }

  for(unsigned long i=0; i<lib->ehdr->e_shnum && !error; i++) {
    if(lib->shdr[i].sh_type != SHT_RELA) {
      continue;
    }

    rela = (Elf64_Rela*)(lib->image + lib->shdr[i].sh_offset);
    for(int j=0; j<lib->shdr[i].sh_size/sizeof(Elf64_Rela) && !error; j++) {
      switch(rela[j].r_info & 0xffffffffl) {

      case R_X86_64_GLOB_DAT:
	error = r_glob_dat(lib, &rela[j]);
	break;

      case R_X86_64_64:
        error = r_direct_64(lib, &rela[j]);
        break;

      case R_X86_64_JMP_SLOT:
        error = r_jmp_slot(lib, &rela[j]);
        break;

      case R_X86_64_RELATIVE:
	error = r_relative(lib, &rela[j]);
	break;

      default:
        klog_printf("Unsupported relocation type %x\n",
                    rela[i].r_info);
        return -1;
      }
    }
  }

  for(int i=0; i<lib->plt_size/sizeof(Elf64_Rela); i++) {
    switch(lib->plt[i].r_info & 0xffffffffl) {
    case R_X86_64_JMP_SLOT:
      if(r_jmp_slot(lib, &lib->plt[i])) {
	return -1;
      }
      break;

    default:
      klog_printf("Unsupported plt relocation type %x\n",
                  lib->plt[i].r_info);
      break;
    }
  }

  // set protection bits on mapped memory
  for(int i=0; i<lib->ehdr->e_phnum && !error; i++) {
    if(lib->phdr[i].p_type != PT_LOAD || lib->phdr[i].p_memsz == 0) {
      continue;
    }
    if(mprotect(lib->mapbase + lib->phdr[i].p_vaddr,
                ROUND_PG(lib->phdr[i].p_memsz),
                PFLAGS(lib->phdr[i].p_flags))) {
      klog_perror("mprotect");
      error = 1;
    }
  }

  if(error) {
    return -1;
  }

  strcpy(lib->soname, path);

  return 0;
}


static int
so_open(rtld_lib_t* ctx) {
  rtld_so_lib_t* lib = (rtld_so_lib_t*)ctx;
  char path[1024];
  int err;

  if((err=__rtld_find_file(lib->soname, path))) {
    return err;
  }

  return so_load(lib, path);
}


static int
so_init(rtld_lib_t* ctx, int argc, char** argv, char** envp) {
  rtld_so_lib_t* lib = (rtld_so_lib_t*)ctx;

  for(unsigned long i=0; i<lib->init_array_size/sizeof(void*); i++) {
    lib->init_array[i](argc, argv, envp);
  }

  return 0;
}


static void*
so_sym2addr(rtld_lib_t* ctx, const char* name) {
  rtld_so_lib_t* lib = (rtld_so_lib_t*)ctx;

  if(!lib->symtab || !lib->strtab || !lib->mapbase) {
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
so_addr2sym(rtld_lib_t* ctx, void* addr) {
  rtld_so_lib_t* lib = (rtld_so_lib_t*)ctx;

  if(!lib->symtab || !lib->strtab || !lib->mapbase) {
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
so_fini(rtld_lib_t* ctx) {
  rtld_so_lib_t* lib = (rtld_so_lib_t*)ctx;

  for(unsigned long i=0; i<lib->fini_array_size/sizeof(void*); i++) {
    lib->fini_array[i]();
  }

  return 0;
}


static int
so_close(rtld_lib_t* ctx) {
  rtld_so_lib_t* lib = (rtld_so_lib_t*)ctx;

  if(lib->image) {
    free(lib->image);
  }
  if(lib->mapbase) {
    munmap(lib->mapbase, lib->mapsize);
  }

  lib->image = 0;
  lib->ehdr = 0;
  lib->phdr = 0;
  lib->shdr = 0;
  lib->strtab = 0;
  lib->symtab = 0;
  lib->symtab_size = 0;
  lib->mapbase = 0;
  lib->mapsize = 0;

  return 0;
}


static void
so_destroy(rtld_lib_t* ctx) {
  rtld_so_lib_t* lib = (rtld_so_lib_t*)ctx;

  so_close(ctx);

  free(lib);
}


rtld_lib_t*
__rtld_so_new(rtld_lib_t* parent, const char *soname) {
  rtld_so_lib_t* lib = calloc(1, sizeof(rtld_so_lib_t));

  lib->parent   = parent;
  lib->open     = so_open;
  lib->init     = so_init;
  lib->sym2addr = so_sym2addr;
  lib->addr2sym = so_addr2sym;
  lib->fini     = so_fini;
  lib->close    = so_close;
  lib->destroy  = so_destroy;
  lib->refcnt   = 0;

  strcpy(lib->soname, soname);

  return (rtld_lib_t*)lib;
}


int
__rtld_so_init(void) {
  unsigned int libc = 0x2;

  if(!KERNEL_DLSYM(libc, strcmp)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libc, strcpy)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libc, malloc)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libc, calloc)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libc, memcpy)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libc, free)) {
    return -1;
  }

  return 0;
}
