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


/* F, G, H and I are basic SHA1 functions.
 */
#define F(x, y, z) ((z) ^ ((x) & ((y) ^ (z))))
#define G(x, y, z) ((x) ^ (y) ^ (z))
#define H(x, y, z) (((x) & (y)) | ((z) & ((x) | (y))))
#define I(x, y, z) ((x) ^ (y) ^ (z))

/* ROTATE_LEFT rotates x left n bits.
 */
#define ROTATE_LEFT(x, n) (((x) << (n)) | ((x) >> (32-(n))))

/* W[i]
 */
#define W(i) ( tmp=x[(i-3)&15]^x[(i-8)&15]^x[(i-14)&15]^x[i&15], \
	(x[i&15]=ROTATE_LEFT(tmp, 1)) )

/* FF, GG, HH, and II transformations for rounds 1, 2, 3, and 4.
 */
#define FF(a, b, c, d, e, w) { \
 (e) += F ((b), (c), (d)) + (w) + (unsigned int)(0x5A827999); \
 (e) += ROTATE_LEFT ((a), 5); \
 (b) = ROTATE_LEFT((b), 30); \
  }
#define GG(a, b, c, d, e, w) { \
 (e) += G ((b), (c), (d)) + (w) + (unsigned int)(0x6ED9EBA1); \
 (e) += ROTATE_LEFT ((a), 5); \
 (b) = ROTATE_LEFT((b), 30); \
  }
#define HH(a, b, c, d, e, w) { \
 (e) += H ((b), (c), (d)) + (w) + (unsigned int)(0x8F1BBCDC); \
 (e) += ROTATE_LEFT ((a), 5); \
 (b) = ROTATE_LEFT((b), 30); \
  }
#define II(a, b, c, d, e, w) { \
 (e) += I ((b), (c), (d)) + (w) + (unsigned int)(0xCA62C1D6); \
 (e) += ROTATE_LEFT ((a), 5); \
 (b) = ROTATE_LEFT((b), 30); \
  }


typedef struct {
  unsigned int state[5];
  unsigned int count[2];
  unsigned char buffer[64];
} SHA1_CTX;


/**
 * we need strlen() before we can resolve symbols.
 **/
static unsigned long
strlen(const char *str) {
  const char *start = str;

  while(*str) {
    str++;
  }

  return str - start;
}


/**
 * we need memcpy() before we can resolve symbols.
 **/
static void*
memcpy(void *__restrict dst0, const void *__restrict src0,
       unsigned long len0) {
  char *dst = (char*)dst0;
  char *src = (char*)src0;
  void *save = dst0;

  while(len0--) {
    *dst++ = *src++;
  }

  return save;
}


/**
 * we need memset() before we can resolve symbols.
 **/
static void*
memset(void *m, int c, unsigned long n) {
  char *s = (char*)m;

  while(n--) {
    *s++ = (char)c;
  }

  return m;
}


/**
 *
 **/
static void
SHA1Decode(unsigned int *output, const unsigned char *input, unsigned int len) {
  unsigned int i, j;

  for (i = 0, j = 0; j < len; i++, j += 4) {
    output[i] = ((unsigned int) input[j + 3]) | (((unsigned int) input[j + 2]) << 8) |
      (((unsigned int) input[j + 1]) << 16) | (((unsigned int) input[j]) << 24);
  }
}


/**
 * Hash a single 512-bit block. This is the core of the algorithm.
 **/
static void
SHA1Transform(unsigned int *restrict state, const unsigned char *restrict block) {
  unsigned int a = state[0], b = state[1], c = state[2];
  unsigned int d = state[3], e = state[4], x[16], tmp;

  SHA1Decode(x, block, 64);

  /* Round 1 */
  FF(a, b, c, d, e, x[0]);   /* 1 */
  FF(e, a, b, c, d, x[1]);   /* 2 */
  FF(d, e, a, b, c, x[2]);   /* 3 */
  FF(c, d, e, a, b, x[3]);   /* 4 */
  FF(b, c, d, e, a, x[4]);   /* 5 */
  FF(a, b, c, d, e, x[5]);   /* 6 */
  FF(e, a, b, c, d, x[6]);   /* 7 */
  FF(d, e, a, b, c, x[7]);   /* 8 */
  FF(c, d, e, a, b, x[8]);   /* 9 */
  FF(b, c, d, e, a, x[9]);   /* 10 */
  FF(a, b, c, d, e, x[10]);  /* 11 */
  FF(e, a, b, c, d, x[11]);  /* 12 */
  FF(d, e, a, b, c, x[12]);  /* 13 */
  FF(c, d, e, a, b, x[13]);  /* 14 */
  FF(b, c, d, e, a, x[14]);  /* 15 */
  FF(a, b, c, d, e, x[15]);  /* 16 */
  FF(e, a, b, c, d, W(16));  /* 17 */
  FF(d, e, a, b, c, W(17));  /* 18 */
  FF(c, d, e, a, b, W(18));  /* 19 */
  FF(b, c, d, e, a, W(19));  /* 20 */

  /* Round 2 */
  GG(a, b, c, d, e, W(20));  /* 21 */
  GG(e, a, b, c, d, W(21));  /* 22 */
  GG(d, e, a, b, c, W(22));  /* 23 */
  GG(c, d, e, a, b, W(23));  /* 24 */
  GG(b, c, d, e, a, W(24));  /* 25 */
  GG(a, b, c, d, e, W(25));  /* 26 */
  GG(e, a, b, c, d, W(26));  /* 27 */
  GG(d, e, a, b, c, W(27));  /* 28 */
  GG(c, d, e, a, b, W(28));  /* 29 */
  GG(b, c, d, e, a, W(29));  /* 30 */
  GG(a, b, c, d, e, W(30));  /* 31 */
  GG(e, a, b, c, d, W(31));  /* 32 */
  GG(d, e, a, b, c, W(32));  /* 33 */
  GG(c, d, e, a, b, W(33));  /* 34 */
  GG(b, c, d, e, a, W(34));  /* 35 */
  GG(a, b, c, d, e, W(35));  /* 36 */
  GG(e, a, b, c, d, W(36));  /* 37 */
  GG(d, e, a, b, c, W(37));  /* 38 */
  GG(c, d, e, a, b, W(38));  /* 39 */
  GG(b, c, d, e, a, W(39));  /* 40 */

  /* Round 3 */
  HH(a, b, c, d, e, W(40));  /* 41 */
  HH(e, a, b, c, d, W(41));  /* 42 */
  HH(d, e, a, b, c, W(42));  /* 43 */
  HH(c, d, e, a, b, W(43));  /* 44 */
  HH(b, c, d, e, a, W(44));  /* 45 */
  HH(a, b, c, d, e, W(45));  /* 46 */
  HH(e, a, b, c, d, W(46));  /* 47 */
  HH(d, e, a, b, c, W(47));  /* 48 */
  HH(c, d, e, a, b, W(48));  /* 49 */
  HH(b, c, d, e, a, W(49));  /* 50 */
  HH(a, b, c, d, e, W(50));  /* 51 */
  HH(e, a, b, c, d, W(51));  /* 52 */
  HH(d, e, a, b, c, W(52));  /* 53 */
  HH(c, d, e, a, b, W(53));  /* 54 */
  HH(b, c, d, e, a, W(54));  /* 55 */
  HH(a, b, c, d, e, W(55));  /* 56 */
  HH(e, a, b, c, d, W(56));  /* 57 */
  HH(d, e, a, b, c, W(57));  /* 58 */
  HH(c, d, e, a, b, W(58));  /* 59 */
  HH(b, c, d, e, a, W(59));  /* 60 */

  /* Round 4 */
  II(a, b, c, d, e, W(60));  /* 61 */
  II(e, a, b, c, d, W(61));  /* 62 */
  II(d, e, a, b, c, W(62));  /* 63 */
  II(c, d, e, a, b, W(63));  /* 64 */
  II(b, c, d, e, a, W(64));  /* 65 */
  II(a, b, c, d, e, W(65));  /* 66 */
  II(e, a, b, c, d, W(66));  /* 67 */
  II(d, e, a, b, c, W(67));  /* 68 */
  II(c, d, e, a, b, W(68));  /* 69 */
  II(b, c, d, e, a, W(69));  /* 70 */
  II(a, b, c, d, e, W(70));  /* 71 */
  II(e, a, b, c, d, W(71));  /* 72 */
  II(d, e, a, b, c, W(72));  /* 73 */
  II(c, d, e, a, b, W(73));  /* 74 */
  II(b, c, d, e, a, W(74));  /* 75 */
  II(a, b, c, d, e, W(75));  /* 76 */
  II(e, a, b, c, d, W(76));  /* 77 */
  II(d, e, a, b, c, W(77));  /* 78 */
  II(c, d, e, a, b, W(78));  /* 79 */
  II(b, c, d, e, a, W(79));  /* 80 */

  state[0] += a;
  state[1] += b;
  state[2] += c;
  state[3] += d;
  state[4] += e;
}


/**
 * SHA1Init - Initialize new context
 **/
static inline void
SHA1Init(SHA1_CTX *restrict context) {
  context->state[0] = 0x67452301;
  context->state[1] = 0xEFCDAB89;
  context->state[2] = 0x98BADCFE;
  context->state[3] = 0x10325476;
  context->state[4] = 0xC3D2E1F0;
  context->count[0] = context->count[1] = 0;
}


/**
   Run your data through this.
**/
static inline void
SHA1Update(SHA1_CTX *restrict context, const unsigned char *restrict data,
           unsigned long len) {
  unsigned int i;
  unsigned int j;

  j = context->count[0];
  context->count[0] += len << 3;
  if(context->count[0] < j) {
    context->count[1]++;
  }

  context->count[1] += (len >> 29);
  j = (j >> 3) & 63;
  if((j + len) > 63) {
    memcpy(&context->buffer[j], data, (i = 64 - j));
    SHA1Transform(context->state, context->buffer);
    for(; i + 63 < len; i += 64) {
      SHA1Transform(context->state, &data[i]);
    }
    j = 0;
  } else {
    i = 0;
  }

  memcpy(&context->buffer[j], &data[i], len - i);
}


/**
 * Add padding and return the message digest.
 **/
static inline void
SHA1Final(unsigned char *digest, SHA1_CTX *context) {
  unsigned i;
  unsigned char finalcount[8];
  unsigned char c = 0200;

  for(i=0; i<8; i++) {
    /* Endian independent */
    finalcount[i] = (unsigned char)((context->count[(i >= 4 ? 0 : 1)] >> ((3 - (i & 3)) * 8)) & 255);
  }

  SHA1Update(context, &c, 1);
  while((context->count[0] & 504) != 448) {
    c = 0000;
    SHA1Update(context, &c, 1);
  }

  SHA1Update(context, finalcount, 8); /* Should cause a SHA1Transform() */
  for (i=0; i<20; i++) {
    digest[i] = (unsigned char) ((context->state[i >> 2] >> ((3 - (i & 3)) * 8)) & 255);
  }
}


char*
nid_encode(const char *sym, char nid[12]) {
  const unsigned char salt[16] = "\x51\x8d\x64\xa6\x35\xde\xd8\xc1\xe6\xb0\x39\xb1\xc3\xe5\x52\x30";
  const char b64charset[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+-";
  unsigned char digest[20];
  SHA1_CTX ctx;

  SHA1Init(&ctx);
  SHA1Update(&ctx, (unsigned char *)sym, strlen(sym));
  SHA1Update(&ctx, salt, sizeof(salt));
  SHA1Final(digest, &ctx);
  *(unsigned long *)digest = __builtin_bswap64(*(unsigned long *)(digest));
  memset(digest+8, 0, 8);

  for(int i=0, j=0; j<11;) {
    const unsigned int a = digest[i++];
    const unsigned int b = digest[i++];
    const unsigned int c = digest[i++];
    const unsigned int abc = (a << 16) | (b << 8) | c;

    nid[j++] = b64charset[(abc >> 18) & 0x3F];
    nid[j++] = b64charset[(abc >> 12) & 0x3F];
    nid[j++] = b64charset[(abc >> 6) & 0x3F];
    nid[j++] = b64charset[abc & 0x3F];
  }

  nid[11] = 0;

  return nid;
}
