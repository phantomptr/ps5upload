/* Copyright (C) 2026 John Törnblom

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

#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>


/**
 *
 **/
typedef struct http_ctx {
  int libnetMemId;
  int libsslCtxId;
  int libhttpCtxId;
  int tmplId;
  int connId;
  int reqId;
} http_ctx_t;


int sceNetInit();
int sceNetPoolCreate(const char*, int, int);
int sceNetPoolDestroy(int);

int sceSslInit(size_t);
int sceSslTerm(int);

int sceHttpInit(int, int, size_t);
int sceHttpTerm(int);

int sceHttpCreateTemplate(int, const char*, int, int);
int sceHttpsSetSslCallback(int, void*, void*);
int sceHttpSetResponseHeaderMaxSize(int, size_t);
int sceHttpDeleteTemplate(int);

int sceHttpCreateConnectionWithURL(int, const char*, int);
int sceHttpDeleteConnection(int);

int sceHttpCreateRequestWithURL(int, int, const char*, uint64_t);
int sceHttpSendRequest(int, const void*, size_t);
int sceHttpGetResponseContentLength(int, int*, uint64_t*);
int sceHttpGetStatusCode(int, int*);
int sceHttpReadData(int, void *, size_t);
int sceHttpDeleteRequest(int);


/**
 *
 **/
static int
http_ssl_cb(void) {
  return 0;
}


/**
 *
 **/
static int
http_init(http_ctx_t* ctx, const char* agent, const char* url) {
  int err;

  ctx->libnetMemId  = -1;
  ctx->libsslCtxId  = -1;
  ctx->libhttpCtxId = -1;
  ctx->tmplId       = -1;
  ctx->reqId        = -1;

  if((err=sceNetInit()) < 0) {
    return err;
  }

  if((ctx->libnetMemId=sceNetPoolCreate("http_get", 16*1024, 0)) < 0) {
    return ctx->libnetMemId;
  }

  if((ctx->libsslCtxId=sceSslInit(128*1024)) < 0) {
    return ctx->libsslCtxId;
  }

  if((ctx->libhttpCtxId=sceHttpInit(ctx->libnetMemId, ctx->libsslCtxId,
				    32*1024)) < 0) {
    return ctx->libhttpCtxId;
  }

  if((ctx->tmplId=sceHttpCreateTemplate(ctx->libhttpCtxId, agent, 2, 1)) < 0) {
    return ctx->tmplId;
  }

  if((err=sceHttpSetResponseHeaderMaxSize(ctx->tmplId, 0x2000)) < 0) {
    return err;
  }

  if((err=sceHttpsSetSslCallback(ctx->tmplId, http_ssl_cb, 0))) {
    return err;
  }

  if((ctx->connId=sceHttpCreateConnectionWithURL(ctx->tmplId, url, 0)) < 0) {
    return ctx->connId;
  }

  if((ctx->reqId=sceHttpCreateRequestWithURL(ctx->connId, 0, url, 0)) < 0) {
    return ctx->reqId;
  }

  return 0;
}


/**
 *
 **/
static int
http_request(http_ctx_t* ctx, uint8_t** data, size_t* len) {
  int status = -1;
  int err;
  int n;

  if((err=sceHttpSendRequest(ctx->reqId, 0, 0))) {
    return err;
  }
  if((err=sceHttpGetStatusCode(ctx->reqId, &status))) {
    return err;
  }
  if((err=sceHttpGetResponseContentLength(ctx->reqId, &err, len))) {
    return err;
  }

  if(!len || status != 200) {
    // TODO: support resources without a content length
    return status;
  }

  if(!(*data=malloc(*len))) {
    return -1;
  }

  if((n=sceHttpReadData(ctx->reqId, *data, *len) < 0)) {
    return n;
  }

  return status;
}


/**
 *
 **/
static void
http_fini(http_ctx_t* ctx) {
  if(ctx->reqId >= 0) {
    sceHttpDeleteRequest(ctx->reqId);
  }
  if(ctx->connId >= 0) {
    sceHttpDeleteConnection(ctx->connId);
  }
  if(ctx->tmplId >= 0) {
    sceHttpDeleteTemplate(ctx->tmplId);
  }
  if(ctx->libhttpCtxId >= 0) {
    sceHttpTerm(ctx->libhttpCtxId);
  }
  if(ctx->libsslCtxId >= 0) {
    sceSslTerm(ctx->libsslCtxId);
  }
  if(ctx->libnetMemId >= 0) {
    sceNetPoolDestroy(ctx->libnetMemId);
  }
}


/**
 *
 **/
static uint8_t*
http_get(const char* url, size_t* len) {
  http_ctx_t ctx;
  size_t size = 0;
  uint8_t* data;

  if((http_init(&ctx, "elfldr.elf", url))) {
    http_fini(&ctx);
    return 0;
  }

  if(http_request(&ctx, &data, &size) != 200) {
    http_fini(&ctx);
    return 0;
  }

  http_fini(&ctx);;
  if(len) {
    *len = size;
  }
  return data;
}


/**
 * Read a payload from he given URL.
 **/
static int
url_read(const char* url, uint8_t** content, size_t* content_size) {
  if((*content=http_get(url, content_size))) {
    return 0;
  }

  return -1;
}


/**
 * Read a payload from given path.
 **/
static int
path_read(const char* path, uint8_t** content, size_t* content_size) {
  uint8_t* buf;
  ssize_t size;
  FILE* f;

  if(!(f=fopen(path, "rb"))) {
    return -1;
  }
  if(fseek(f, 0, SEEK_END)) {
    return -1;
  }
  if((size=ftell(f)) < 0) {
    return -1;
  }
  if(fseek(f, 0, SEEK_SET)) {
    return -1;
  }
  if(!(buf=malloc(size))) {
    return -1;
  }
  if(fread(buf, 1, size, f) != size) {
    free(buf);
    return -1;
  }
  if(fclose(f)) {
    free(buf);
    return -1;
  }

  *content = buf;
  *content_size = size;

  return 0;
}


static int
hexval(char c) {
  if(c >= '0' && c <= '9') {
    return c - '0';
  }
  if(c >= 'a' && c <= 'f') {
    return c - 'a' + 10;
  }
  if(c >= 'A' && c <= 'F') {
    return c - 'A' + 10;
  }
  return -1;
}


static char*
uri_decode(const char* src, size_t len) {
  char* out;
  char* dst;
  int lo;
  int hi;

  if(!(out=malloc(len+1))) {
    return 0;
  }

  dst = out;
  for(size_t i=0; i<len; i++) {
    if(src[i] == '%' && i+2 < len) {
      hi = hexval(src[i+1]);
      lo = hexval(src[i+2]);
      if(hi >= 0 && lo >= 0) {
        *dst++ = (char)((hi << 4) | lo);
        i += 2;
      } else {
        *dst++ = src[i];
      }
    }
    else {
      *dst++ = src[i];
    }
  }

  *dst = 0;

  return out;
}


static char*
uri_get_path(const char* uri) {
  size_t len = strlen(uri);
  const char* begin = 0;
  const char* end = 0;

  end = uri + len;
  for(int i=0; i<len; i++) {
    if(!begin && uri[i] == ':' && uri[i+1] == '/') {
      begin = uri + i + 1;
    }
    else if(uri[i] == '?' || uri[i] == '#' || uri[i] == '\0') {
      end = uri + i;
      break;
    }
  }

  return uri_decode(begin, end-begin);
}


char*
uri_get_filename(const char* uri) {
  const char* begin = 0;
  const char* end = 0;
  size_t len;

  if(!(len=strlen(uri))) {
    return 0;
  }

  end = uri + len;
  for(int i=0; i<len; i++) {
    if(uri[i] == '/') {
      begin = uri + i + 1;
    }
    else if(uri[i] == '?' || uri[i] == '#' || uri[i] == '\0') {
      end = uri + i;
      break;
    }
  }

  return uri_decode(begin, end-begin);
}


int
uri_get_content(const char* uri, uint8_t** content, size_t* content_size) {
  int err = -1;
  char *path;

  if(!strncmp(uri, "file:/", 6)) {
    if((path=uri_get_path(uri))) {
      err = path_read(path, content, content_size);
      free(path);
    }
  } else if(!strncmp(uri, "http:/", 6) || !strncmp(uri, "https:/", 7)) {
    err = url_read(uri, content, content_size);
  }

  return err;
}


char*
uri_get_param(const char* uri, const char* name) {
  const char* begin;
  const char* end;
  const char* p;
  size_t len;

  if(!(p=strchr(uri, '?'))) {
    return 0;
  }

  p++;
  len = strlen(name);

  while(*p && *p != '#') {
    if(!strncmp(p, name, len) && p[len] == '=') {
      begin = p + len + 1;

      if(!(end=strchr(begin, '&'))) {
	if(!(end=strchr(begin, '#'))) {
          end = begin + strlen(begin);
	}
      }

      return uri_decode(begin, end-begin);
    }

    while(*p && *p != '&' && *p != '#') {
      p++;
    }

    if(*p == '&') {
      p++;
    }
  }

  return 0;
}
