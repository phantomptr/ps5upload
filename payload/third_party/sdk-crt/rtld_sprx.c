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
#include "klog.h"
#include "kernel.h"
#include "nid.h"
#include "rtld_sprx.h"
#include "syscall.h"


/**
 * Dependencies to standard libraries.
 **/
static char* (*strcpy)(char*, const char*) = 0;
static int (*strcmp)(const char*, const char*) = 0;
static int (*strncmp)(const char*, const char*, unsigned long) = 0;

static void* (*calloc)(unsigned long, unsigned long) = 0;
static void* (*malloc)(unsigned long) = 0;
static void (*free)(void*) = 0;

static int   (*sceKernelLoadStartModule)(const char*, unsigned long, const void*,
					 unsigned int, void*, int*) = 0;
static int   (*sceKernelStopUnloadModule)(int, unsigned long, const void*, unsigned int,
					  const void*, void*) = 0;
static int   (*sceSysmoduleLoadModuleInternal)(unsigned int) = 0;


/**
 * Extend the rtld_lib_t type with auxiliary parameters needed by .sprx loader.
 * This requires -fms-extensions.
 **/
typedef struct rtld_sprx_lib {
  struct rtld_lib;

  int handle;
  char* strtab;
  Elf64_Sym* symtab;
  unsigned long symtab_size;

  int unload_on_close;
} rtld_sprx_lib_t;


/**
 * Lookup table for sceSysmoduleLoadModuleInternal().
 **/
static struct sysmodtab {
  const char* name;
  unsigned int handle;
} sysmodtab[] = {
  {"libSceAbstractLocal.sprx", 0x8000005f},
  {"libSceAbstractStorage.sprx", 0x80000058},
  {"libSceAbstractTcs.sprx", 0x800000a1},
  {"libSceAbstractTwitter.sprx", 0x80000062},
  {"libSceAbstractYoutube.sprx", 0x80000061},
  {"libSceAc3Enc.sprx", 0x80000013},
  {"libSceAgc.sprx", 0x80000094},
  {"libSceAgcDriver.sprx", 0x80000080},
  {"libSceAgcResourceRegistration.sprx", 0x80000093},
  {"libSceAgcVsh.sprx", 0x80000086},
  {"libSceAgcVshDebug.sprx", 0x80000087},
  {"libSceAjm.native.sprx", 0x80000023},
  {"libSceAjmi.sprx", 0x8000007e},
  {"libSceAmpr.sprx", 0x800000b7},
  {"libSceAppChecker.sprx", 0x80000032},
  {"libSceAppDbShellCoreClient.sprx", 0x800000a7},
  {"libSceAppInstUtil.sprx", 0x80000014},
  {"libSceAsyncStorageInternal.sprx", 0x80000077},
  {"libSceAudioIn.sprx", 0x80000002},
  {"libSceAudioOut.sprx", 0x80000001},
  {"libSceAudioSystem.sprx", 0x80000083},
  {"libSceAudiodecCpuDtsHdMa.sprx", 0x8000002d},
  {"libSceAudiodecCpuLpcm.sprx", 0x8000002e},
  {"libSceAudiodecCpuTrhd.sprx", 0x80000082},
  {"libSceAvSetting.sprx", 0x80000021},
  {"libSceAvcap2.sprx", 0x80000085},
  {"libSceBackupRestoreUtil.sprx", 0x8000003f},
  {"libSceBgft.sprx", 0x8000002a},
  {"libSceBgsStorage.sprx", 0x800000a3},
  {"libSceCamera.sprx", 0x8000001a},
  {"libSceCdlgUtilServer.sprx", 0x80000007},
  {"libSceCommonDialog.sprx", 0x80000018},
  {"libSceComposite.sprx", 0x8000008a},
  {"libSceCompositeExt.sprx", 0x8000008b},
  {"libSceContentListController.sprx", 0x800000ad},
  {"libSceDataTransfer.sprx", 0x80000057},
  {"libSceDbg.sprx", 0x80000025},
  {"libSceDipsw.sprx", 0x80000029},
  {"libSceDseehx.sprx", 0x80000056},
  {"libSceDtsEnc.sprx", 0x80000028},
  {"libSceEmbeddedTts.sprx", 0x8000009c},
  {"libSceEmbeddedTtsCoreG3.sprx", 0x8000009b},
  {"libSceFsInternalForVsh.sprx", 0x80000066},
  {"libSceGLSlimVSH.sprx", 0x800000a9},
  {"libSceGifParser.sprx", 0x8000005e},
  {"libSceGpuCapture.sprx", 0x8000007f},
  {"libSceGpuTrace.sprx", 0x8000007b},
  {"libSceGvMp4Parser.sprx", 0x8000005c},
  {"libSceHidControl.sprx", 0x80000017},
  {"libSceHttp.sprx", 0x8000000a},
  {"libSceHttp2.sprx", 0x8000008c},
  {"libSceHttpCache.sprx", 0x80000078},
  {"libSceIcu.sprx", 0x800000a8},
  {"libSceIdu.sprx", 0x800000a6},
  {"libSceImageUtil.sprx", 0x80000059},
  {"libSceIpmi.sprx", 0x8000001d},
  {"libSceJemspace.sprx", 0x8000009e},
  {"libSceJitBridge.sprx", 0x8000006f},
  {"libSceJpegParser.sprx", 0x8000005b},
  {"libSceJsc.sprx", 0x800000b0},
  {"libSceJscCompiler.sprx", 0x80000070},
  {"libSceJxr.sprx", 0x800000b4},
  {"libSceJxrParser.sprx", 0x800000b5},
  {"libSceKbEmulate.sprx", 0x80000031},
  {"libSceLibreSsl.sprx", 0x80000065},
  {"libSceLibreSsl3.sprx", 0x800000b8},
  {"libSceLoginMgrServer.sprx", 0x80000045},
  {"libSceMarlin.sprx", 0x80000027},
  {"libSceMat.sprx", 0x80000048},
  {"libSceMbus.sprx", 0x8000001e},
  {"libSceMediaFrameworkInterface.sprx", 0x80000095},
  {"libSceMediaFrameworkUtil.sprx", 0x800000b6},
  {"libSceMetadataReaderWriter.sprx", 0x8000005a},
  {"libSceNKWeb.sprx", 0x80000079},
  {"libSceNKWebKit.sprx", 0x8000007a},
  {"libSceNet.sprx", 0x8000001c},
  {"libSceNetCtl.sprx", 0x80000009},
  {"libSceNgs2.sprx", 0x80000090},
  {"libSceNpCommon.sprx", 0x8000000c},
  {"libSceNpGameIntent.sprx", 0x8000008d},
  {"libSceNpManager.sprx", 0x8000000d},
  {"libSceNpRemotePlaySessionSignaling.sprx", 0x8000009a},
  {"libSceNpSns.sprx", 0x8000001b},
  {"libSceNpTcs.sprx", 0x800000a0},
  {"libSceNpWebApi.sprx", 0x8000000e},
  {"libSceNpWebApi2.sprx", 0x8000008f},
  {"libSceOpusCeltDec.sprx", 0x80000044},
  {"libSceOpusCeltEnc.sprx", 0x80000043},
  {"libSceOpusDec.sprx", 0x80000069},
  {"libSceOpusSilkEnc.sprx", 0x80000068},
  {"libSceOrbisCompat.sprx", 0x80000071},
  {"libScePad.sprx", 0x80000024},
  {"libScePngParser.sprx", 0x8000005d},
  {"libScePosixForWebKit.sprx", 0x80000098},
  {"libScePsm.sprx", 0x80000030},
  {"libSceRazorCpu_debug.sprx", 0x80000075},
  {"libSceRegMgr.sprx", 0x8000001f},
  {"libSceRemotePlayClientIpc.sprx", 0x800000b9},
  {"libSceResourceArbitrator.sprx", 0x80000092},
  {"libSceRnpsAppMgr.sprx", 0x80000076},
  {"libSceRtc.sprx", 0x80000020},
  {"libSceSaveData.sprx", 0x8000000f},
  {"libSceShareInternal.native.sprx", 0x8000008e},
  {"libSceSocialScreen.sprx", 0x800000ae},
  {"libSceSsl.sprx", 0x8000000b},
  {"libSceSulphaDrv.sprx", 0x8000003b},
  {"libSceSysCore.sprx", 0x80000004},
  {"libSceSysUtil.sprx", 0x80000026},
  {"libSceSystemLogger2.sprx", 0x800000b3},
  {"libSceSystemLogger2Delivery.sprx", 0x80000089},
  {"libSceSystemLogger2Game.sprx", 0x8000009f},
  {"libSceSystemLogger2NativeQueueClient.sprx", 0x80000088},
  {"libSceSystemService.sprx", 0x80000010},
  {"libSceSystemTts.sprx", 0x80000097},
  {"libSceTEEClient.sprx", 0x800000a2},
  {"libSceUserService.sprx", 0x80000011},
  {"libSceVcodec.sprx", 0x80000091},
  {"libSceVdecCore.native.sprx", 0x80000015},
  {"libSceVdecSavc2.native.sprx", 0x80000036},
  {"libSceVdecShevc.native.sprx", 0x8000003c},
  {"libSceVdecSvp9.native.sprx", 0x800000af},
  {"libSceVenc.sprx", 0x80000084},
  {"libSceVideoOut.sprx", 0x80000022},
  {"libSceVideoOutSecondary.sprx", 0x80000046},
  {"libSceVideoStreamingEngine_sys.sprx", 0x800000b2},
  {"libSceVisionManager.sprx", 0x80000012},
  {"libSceVnaInternal.sprx", 0x8000007c},
  {"libSceVnaWebsocket.sprx", 0x8000007d},
  {"libSceVoiceCommand.sprx", 0x80000099},
  {"libSceWeb.sprx", 0x80000072},
  {"libSceWebKit2.sprx", 0x80000073},
  {"libSceWebKit2Secure.sprx", 0x80000074},
  {"libSceWebmParserMdrw.sprx", 0x800000a4},
  {"libcairo.sprx", 0x800000ac},
  {"libcurl.sprx", 0x800000b1},
  {"libicu.sprx", 0x800000aa},
  {"libpng16.sprx", 0x800000ab}
};


/**
 * Open up a new sprx object and initialize its members.
 **/
static int
sprx_open(rtld_lib_t* ctx) {
  rtld_sprx_lib_t* lib = (rtld_sprx_lib_t*)ctx;
  unsigned int handle = 0;
  dynlib_dynsec_t dynsec;
  dynlib_obj_t obj;
  char path[1024];
  int unload = 0;
  int error = 0;

  // check if the lib is an alias for libkernel
  if(!strcmp(lib->soname, "libkernel.sprx") ||
     !strcmp(lib->soname, "libkernel_web.sprx") ||
     !strcmp(lib->soname, "libkernel_sys.sprx")) {
    if(kernel_dynlib_dlsym(-1, 0x1, "sceKernelDlsym")) {
      handle = 1;
    } else {
      handle = 0x2001;
    }
  }
  // check if lib is already opened
  else if(kernel_dynlib_handle(-1, lib->soname, &handle) < 0) {
    if(__rtld_find_file(lib->soname, path)) {
      return -1;
    }

    for(int i=0; i<sizeof(sysmodtab)/sizeof(sysmodtab[0]); i++) {
      if(!strcmp(lib->soname, sysmodtab[i].name)) {
        if((error=sceSysmoduleLoadModuleInternal(sysmodtab[i].handle))) {
          klog_printf("sceSysmoduleLoadModuleInternal: 0x%x\n", error);
          return -1;
        }
      }
    }

    if((handle=sceKernelLoadStartModule(path, 0, 0, 0, 0, 0)) < 0) {
      klog_printf("sceKernelLoadStartModule: 0x%x\n", handle);
      return -1;
    }
    unload = 1;
  }

  if(kernel_dynlib_obj(-1, handle, &obj) < 0) {
    error = 1;
  }
  else if(kernel_copyout(obj.path, lib->soname, 1024) < 0) {
    error = 1;
  }
  else if(kernel_copyout(obj.dynsec, &dynsec, sizeof(dynsec)) < 0) {
    error = 1;
  }
  else if(!(lib->strtab=malloc(dynsec.strtabsize))) {
    klog_perror("malloc");
    return -1;
  }
  else if(kernel_copyout(dynsec.strtab, lib->strtab, dynsec.strtabsize) < 0) {
    error = 1;
  }
  else if(!(lib->symtab=malloc(dynsec.symtabsize))) {
    klog_perror("malloc");
    return -1;
  }
  else if(kernel_copyout(dynsec.symtab, lib->symtab, dynsec.symtabsize) < 0) {
    error = 1;
  }

  if(error) {
    klog_puts("Unknown kernel I/O error");
    return -1;
  }

  lib->handle          = handle;
  lib->unload_on_close = unload;
  lib->mapbase         = (void*)obj.mapbase;
  lib->mapsize         = obj.mapsize;
  lib->symtab_size     = dynsec.symtabsize;

  return 0;
}


/**
 * TODO: sprx_init
 **/
static int
sprx_init(rtld_lib_t* ctx, int argc, char** argv, char** envp) {
  return 0;
}


/**
 * TODO: sprx_fini
 **/
static int
sprx_fini(rtld_lib_t* ctx) {
  return 0;
}


/**
 * Resolve the address of a symbol provided by the given sprx object-
 **/
static void*
sprx_sym2addr(rtld_lib_t* ctx, const char* name) {
  rtld_sprx_lib_t* lib = (rtld_sprx_lib_t*)ctx;
  char nid[12];

  if(!lib->symtab || !lib->strtab || !lib->mapbase) {
    return 0;
  }

  nid_encode(name, nid);

  for(unsigned long i=0; i<lib->symtab_size/sizeof(Elf64_Sym); i++) {
    if(!lib->symtab[i].st_size) {
      continue;
    }

    if(!strncmp(nid, lib->strtab + lib->symtab[i].st_name, 11) ||
       !strncmp(name, lib->strtab + lib->symtab[i].st_name, 11)) {
      return lib->mapbase + lib->symtab[i].st_value;
    }
  }

  return 0;
}


/**
 * Figure out the symbol name associated with the given address.
 **/
static const char*
sprx_addr2sym(rtld_lib_t* ctx, void* addr) {
  rtld_sprx_lib_t* lib = (rtld_sprx_lib_t*)ctx;

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


/**
 * Close the given sprx object and free memory associated with its members.
 **/
static int
sprx_close(rtld_lib_t* ctx) {
  rtld_sprx_lib_t* lib = (rtld_sprx_lib_t*)ctx;
  int err = 0;

  if(lib->unload_on_close) {
    if((err=sceKernelStopUnloadModule(lib->handle, 0, 0, 0, 0, 0))) {
      return err;
    }
  }

  if(lib->strtab) {
    free(lib->strtab);
  }
  if(lib->symtab) {
    free(lib->symtab);
  }

  lib->handle          = -1;
  lib->strtab          = 0;
  lib->symtab          = 0;
  lib->symtab_size     = 0;
  lib->mapbase         = 0;
  lib->mapsize         = 0;
  lib->unload_on_close = 0;

  return 0;
}


/**
 * Free all memory associated the given sprx object.
 **/
static void
sprx_destroy(rtld_lib_t* ctx) {
  rtld_sprx_lib_t* lib = (rtld_sprx_lib_t*)ctx;

  if(sprx_close(ctx)) {
    lib->unload_on_close = 0;
    sprx_close(ctx);
  }

  free(lib);
}


rtld_lib_t*
__rtld_sprx_new(rtld_lib_t* parent, const char *soname) {
  rtld_sprx_lib_t* lib = calloc(1, sizeof(rtld_sprx_lib_t));

  lib->parent   = parent;
  lib->open     = sprx_open;
  lib->init     = sprx_init;
  lib->sym2addr = sprx_sym2addr;
  lib->addr2sym = sprx_addr2sym;
  lib->fini     = sprx_fini;
  lib->close    = sprx_close;
  lib->destroy  = sprx_destroy;
  lib->refcnt   = 0;

  strcpy(lib->soname, soname);

  return (rtld_lib_t*)lib;
}


int
__rtld_sprx_init(void) {
  unsigned int libSceSysmodule;
  unsigned int libkernel = 0x1;
  unsigned int libc = 0x2;

  if(!kernel_dynlib_dlsym(-1, libkernel, "sceKernelDlsym")) {
    libkernel = 0x2001;
  }
  if(!KERNEL_DLSYM(libkernel, sceKernelLoadStartModule)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libkernel, sceKernelStopUnloadModule)) {
    return -1;
  }

  if(!KERNEL_DLSYM(libc, strcpy)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libc, strcmp)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libc, strncmp)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libc, calloc)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libc, malloc)) {
    return -1;
  }
  if(!KERNEL_DLSYM(libc, free)) {
    return -1;
  }

  if(kernel_dynlib_handle(-1, "libSceSysmodule.sprx", &libSceSysmodule)) {
    if((libSceSysmodule=sceKernelLoadStartModule("/system/common/lib/libSceSysmodule.sprx",
                                                 0, 0, 0, 0, 0)) <= 0) {
      return -1;
    }
  }
  if(!KERNEL_DLSYM(libSceSysmodule, sceSysmoduleLoadModuleInternal)) {
    return -1;
  }

  return 0;
}
