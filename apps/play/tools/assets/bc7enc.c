/*
 * bc7enc - a small BC7 (mode 6) encoder and decoder for PS5Upload Play's
 * pic0.dds / pic1.dds backgrounds. Our own work.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 *
 * Usage:
 *   bc7enc -e WIDTH HEIGHT in.rgba out.dds   encode raw RGBA8 to a BC7 DDS
 *   bc7enc -d in.dds out.rgba                decode a mode 6 BC7 DDS to raw RGBA8
 *   bc7enc -t in.dds ref.rgba MIN_PSNR       decode, print RGB PSNR vs ref,
 *                                            fail if it is below MIN_PSNR
 *
 * Output: one 2D surface, DX10 header, DXGI_FORMAT_BC7_UNORM, no mipmaps.
 * Every block uses mode 6 (7-bit RGBA endpoints + p-bit, 4-bit indices).
 * Alpha is always 255, so both p-bits are 1 and colour endpoints are odd.
 * Endpoint fit: principal axis + min/max, a few least-squares passes over
 * the chosen indices, then a +/-1 local search on each endpoint channel.
 */
#include <math.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static const int W4[16] = {0, 4, 9, 13, 17, 21, 26, 30, 34, 38, 43, 47, 51, 55, 60, 64};

/* ---------- encoder ---------- */

/* 7-bit endpoint values, 3 channels, for endpoints 0 and 1 */
typedef struct { int q[2][3]; } Ends;

static int expand(int q) { return (q << 1) | 1; } /* p-bit 1 */

static int quant(float v) {
    int q = (int)lrintf((v - 1.0f) * 0.5f);
    return q < 0 ? 0 : q > 127 ? 127 : q;
}

/* Best index per pixel and total squared RGB error for these endpoints. */
static long eval(const Ends *e, const int px[16][3], int idx[16]) {
    int pal[16][3];
    for (int c = 0; c < 3; c++) {
        int a = expand(e->q[0][c]), b = expand(e->q[1][c]);
        for (int i = 0; i < 16; i++)
            pal[i][c] = ((64 - W4[i]) * a + W4[i] * b + 32) >> 6;
    }
    long total = 0;
    for (int p = 0; p < 16; p++) {
        int best = 1 << 30, bi = 0;
        for (int i = 0; i < 16; i++) {
            int dr = pal[i][0] - px[p][0], dg = pal[i][1] - px[p][1], db = pal[i][2] - px[p][2];
            int d = dr * dr + dg * dg + db * db;
            if (d < best) { best = d; bi = i; }
        }
        idx[p] = bi;
        total += best;
    }
    return total;
}

static long fit_block(const int px[16][3], Ends *out, int idx[16]) {
    float mean[3] = {0, 0, 0}, cov[6] = {0, 0, 0, 0, 0, 0};
    for (int p = 0; p < 16; p++)
        for (int c = 0; c < 3; c++) mean[c] += px[p][c] / 16.0f;
    for (int p = 0; p < 16; p++) {
        float d[3] = {px[p][0] - mean[0], px[p][1] - mean[1], px[p][2] - mean[2]};
        cov[0] += d[0] * d[0]; cov[1] += d[0] * d[1]; cov[2] += d[0] * d[2];
        cov[3] += d[1] * d[1]; cov[4] += d[1] * d[2]; cov[5] += d[2] * d[2];
    }
    float ax[3] = {0.577f, 0.577f, 0.577f};
    for (int it = 0; it < 8; it++) { /* power iteration for the principal axis */
        float n[3] = {cov[0] * ax[0] + cov[1] * ax[1] + cov[2] * ax[2],
                      cov[1] * ax[0] + cov[3] * ax[1] + cov[4] * ax[2],
                      cov[2] * ax[0] + cov[4] * ax[1] + cov[5] * ax[2]};
        float len = sqrtf(n[0] * n[0] + n[1] * n[1] + n[2] * n[2]);
        if (len < 1e-6f) break;
        for (int c = 0; c < 3; c++) ax[c] = n[c] / len;
    }
    float lo = 1e9f, hi = -1e9f;
    for (int p = 0; p < 16; p++) {
        float t = (px[p][0] - mean[0]) * ax[0] + (px[p][1] - mean[1]) * ax[1] + (px[p][2] - mean[2]) * ax[2];
        if (t < lo) lo = t;
        if (t > hi) hi = t;
    }
    Ends best;
    for (int c = 0; c < 3; c++) {
        best.q[0][c] = quant(mean[c] + lo * ax[c]);
        best.q[1][c] = quant(mean[c] + hi * ax[c]);
    }
    int bidx[16], tidx[16];
    long berr = eval(&best, px, bidx);

    for (int it = 0; it < 3 && berr > 0; it++) { /* least squares on the indices */
        float A = 0, B = 0, C = 0, r0[3] = {0, 0, 0}, r1[3] = {0, 0, 0};
        for (int p = 0; p < 16; p++) {
            float w = W4[bidx[p]] / 64.0f, v = 1.0f - w;
            A += v * v; B += v * w; C += w * w;
            for (int c = 0; c < 3; c++) { r0[c] += v * px[p][c]; r1[c] += w * px[p][c]; }
        }
        float det = A * C - B * B;
        if (fabsf(det) < 1e-6f) break;
        Ends t;
        for (int c = 0; c < 3; c++) {
            t.q[0][c] = quant((C * r0[c] - B * r1[c]) / det);
            t.q[1][c] = quant((A * r1[c] - B * r0[c]) / det);
        }
        long err = eval(&t, px, tidx);
        if (err >= berr) break;
        best = t; berr = err; memcpy(bidx, tidx, sizeof bidx);
    }

    for (int pass = 0; pass < 4 && berr > 0; pass++) { /* +/-1 local search */
        int improved = 0;
        for (int e = 0; e < 2; e++)
            for (int c = 0; c < 3; c++)
                for (int d = -1; d <= 1; d += 2) {
                    Ends t = best;
                    t.q[e][c] += d;
                    if (t.q[e][c] < 0 || t.q[e][c] > 127) continue;
                    long err = eval(&t, px, tidx);
                    if (err < berr) { best = t; berr = err; memcpy(bidx, tidx, sizeof bidx); improved = 1; }
                }
        if (!improved) break;
    }
    *out = best;
    memcpy(idx, bidx, sizeof bidx);
    return berr;
}

typedef struct { uint64_t lo, hi; int pos; } Bits;

static void put(Bits *b, uint64_t v, int n) {
    for (int i = 0; i < n; i++, b->pos++) {
        uint64_t bit = (v >> i) & 1;
        if (b->pos < 64) b->lo |= bit << b->pos;
        else b->hi |= bit << (b->pos - 64);
    }
}

static void pack_mode6(Ends e, int idx[16], uint8_t out[16]) {
    if (idx[0] & 8) { /* anchor index MSB must be 0: swap endpoints */
        for (int c = 0; c < 3; c++) { int t = e.q[0][c]; e.q[0][c] = e.q[1][c]; e.q[1][c] = t; }
        for (int p = 0; p < 16; p++) idx[p] = 15 - idx[p];
    }
    Bits b = {0, 0, 0};
    put(&b, 1u << 6, 7);
    for (int c = 0; c < 3; c++) { put(&b, e.q[0][c], 7); put(&b, e.q[1][c], 7); }
    put(&b, 127, 7); put(&b, 127, 7); /* alpha endpoints */
    put(&b, 1, 1); put(&b, 1, 1);     /* p-bits */
    put(&b, idx[0], 3);
    for (int p = 1; p < 16; p++) put(&b, idx[p], 4);
    for (int i = 0; i < 8; i++) { out[i] = (uint8_t)(b.lo >> (8 * i)); out[8 + i] = (uint8_t)(b.hi >> (8 * i)); }
}

typedef struct {
    const uint8_t *rgba; uint8_t *blocks; int w, h, tid, nthreads; double sse;
} Job;

static void *encode_rows(void *arg) {
    Job *j = arg;
    int bw = j->w / 4, bh = j->h / 4;
    for (int by = j->tid; by < bh; by += j->nthreads)
        for (int bx = 0; bx < bw; bx++) {
            int px[16][3], idx[16];
            for (int p = 0; p < 16; p++) {
                const uint8_t *s = j->rgba + ((size_t)(by * 4 + p / 4) * j->w + bx * 4 + p % 4) * 4;
                px[p][0] = s[0]; px[p][1] = s[1]; px[p][2] = s[2];
            }
            Ends e;
            j->sse += fit_block(px, &e, idx);
            pack_mode6(e, idx, j->blocks + ((size_t)by * bw + bx) * 16);
        }
    return NULL;
}

/* ---------- DDS container ---------- */

static void le32(uint8_t *p, uint32_t v) { for (int i = 0; i < 4; i++) p[i] = (uint8_t)(v >> (8 * i)); }
static uint32_t rd32(const uint8_t *p) { return p[0] | p[1] << 8 | p[2] << 16 | (uint32_t)p[3] << 24; }

static void dds_header(uint8_t h[148], int w, int hgt) {
    memset(h, 0, 148);
    memcpy(h, "DDS ", 4);
    le32(h + 4, 124);                       /* header size */
    le32(h + 8, 0x1 | 0x2 | 0x4 | 0x1000 | 0x80000); /* caps|height|width|pixelformat|linearsize */
    le32(h + 12, hgt);
    le32(h + 16, w);
    le32(h + 20, (uint32_t)(w / 4) * (hgt / 4) * 16);
    le32(h + 28, 1);                        /* mipMapCount */
    le32(h + 76, 32);                       /* pixel format size */
    le32(h + 80, 0x4);                      /* DDPF_FOURCC */
    memcpy(h + 84, "DX10", 4);
    le32(h + 108, 0x1000);                  /* DDSCAPS_TEXTURE */
    le32(h + 128, 98);                      /* DXGI_FORMAT_BC7_UNORM */
    le32(h + 132, 3);                       /* D3D10_RESOURCE_DIMENSION_TEXTURE2D */
    le32(h + 140, 1);                       /* arraySize */
}

static uint8_t *read_file(const char *path, size_t *len) {
    FILE *f = fopen(path, "rb");
    if (!f) { perror(path); exit(1); }
    fseek(f, 0, SEEK_END);
    long n = ftell(f);
    fseek(f, 0, SEEK_SET);
    uint8_t *buf = malloc(n > 0 ? (size_t)n : 1);
    if (!buf || fread(buf, 1, (size_t)n, f) != (size_t)n) { fprintf(stderr, "%s: read failed\n", path); exit(1); }
    fclose(f);
    *len = (size_t)n;
    return buf;
}

static void write_file(const char *path, const void *a, size_t alen, const void *b, size_t blen) {
    FILE *f = fopen(path, "wb");
    if (!f || fwrite(a, 1, alen, f) != alen || (blen && fwrite(b, 1, blen, f) != blen) || fclose(f)) {
        fprintf(stderr, "%s: write failed\n", path);
        exit(1);
    }
}

/* ---------- decoder (mode 6 only) ---------- */

static uint32_t get(const uint8_t *blk, int *pos, int n) {
    uint32_t v = 0;
    for (int i = 0; i < n; i++, (*pos)++) v |= (uint32_t)((blk[*pos >> 3] >> (*pos & 7)) & 1) << i;
    return v;
}

static uint8_t *decode_dds(const char *path, int *w, int *h) {
    size_t len;
    uint8_t *d = read_file(path, &len);
    if (len < 148 || memcmp(d, "DDS ", 4) || memcmp(d + 84, "DX10", 4) || rd32(d + 128) != 98) {
        fprintf(stderr, "%s: not a BC7 DX10 DDS\n", path);
        exit(1);
    }
    *h = (int)rd32(d + 12); *w = (int)rd32(d + 16);
    int bw = *w / 4, bh = *h / 4;
    if (len != 148 + (size_t)bw * bh * 16) { fprintf(stderr, "%s: wrong size\n", path); exit(1); }
    uint8_t *out = malloc((size_t)*w * *h * 4);
    for (int by = 0; by < bh; by++)
        for (int bx = 0; bx < bw; bx++) {
            const uint8_t *blk = d + 148 + ((size_t)by * bw + bx) * 16;
            int pos = 0;
            if (get(blk, &pos, 7) != 0x40) { fprintf(stderr, "%s: block %d,%d is not mode 6\n", path, bx, by); exit(1); }
            int e[2][4];
            for (int c = 0; c < 4; c++) { e[0][c] = get(blk, &pos, 7); e[1][c] = get(blk, &pos, 7); }
            int p0 = get(blk, &pos, 1), p1 = get(blk, &pos, 1);
            for (int c = 0; c < 4; c++) { e[0][c] = e[0][c] << 1 | p0; e[1][c] = e[1][c] << 1 | p1; }
            for (int p = 0; p < 16; p++) {
                int i = get(blk, &pos, p ? 4 : 3);
                uint8_t *o = out + ((size_t)(by * 4 + p / 4) * *w + bx * 4 + p % 4) * 4;
                for (int c = 0; c < 4; c++) o[c] = (uint8_t)(((64 - W4[i]) * e[0][c] + W4[i] * e[1][c] + 32) >> 6);
            }
        }
    free(d);
    return out;
}

static int usage(void) {
    fprintf(stderr, "usage: bc7enc -e W H in.rgba out.dds | -d in.dds out.rgba | -t in.dds ref.rgba MIN_PSNR\n");
    return 2;
}

int main(int argc, char **argv) {
    if (argc == 6 && !strcmp(argv[1], "-e")) {
        int w = atoi(argv[2]), h = atoi(argv[3]);
        if (w <= 0 || h <= 0 || w % 4 || h % 4) { fprintf(stderr, "size must be a positive multiple of 4\n"); return 1; }
        size_t len;
        uint8_t *rgba = read_file(argv[4], &len);
        if (len != (size_t)w * h * 4) { fprintf(stderr, "%s: expected %dx%d RGBA8\n", argv[4], w, h); return 1; }
        size_t nblocks = (size_t)(w / 4) * (h / 4);
        uint8_t *blocks = calloc(nblocks, 16), hdr[148];
        long n = sysconf(_SC_NPROCESSORS_ONLN);
        int nt = n < 1 ? 1 : n > 64 ? 64 : (int)n;
        pthread_t th[64];
        Job jobs[64];
        double sse = 0;
        for (int t = 0; t < nt; t++) {
            jobs[t] = (Job){rgba, blocks, w, h, t, nt, 0};
            pthread_create(&th[t], NULL, encode_rows, &jobs[t]);
        }
        for (int t = 0; t < nt; t++) { pthread_join(th[t], NULL); sse += jobs[t].sse; }
        dds_header(hdr, w, h);
        write_file(argv[5], hdr, sizeof hdr, blocks, nblocks * 16);
        double mse = sse / ((double)w * h * 3);
        printf("bc7enc: %s %dx%d mode 6, encoder PSNR %.2f dB\n", argv[5], w, h, mse > 0 ? 10 * log10(255.0 * 255.0 / mse) : 99.0);
        return 0;
    }
    if (argc == 4 && !strcmp(argv[1], "-d")) {
        int w, h;
        uint8_t *px = decode_dds(argv[2], &w, &h);
        write_file(argv[3], px, (size_t)w * h * 4, NULL, 0);
        return 0;
    }
    if (argc == 5 && !strcmp(argv[1], "-t")) {
        int w, h;
        size_t len;
        uint8_t *px = decode_dds(argv[2], &w, &h), *ref = read_file(argv[3], &len);
        if (len != (size_t)w * h * 4) { fprintf(stderr, "%s: size differs from %s\n", argv[3], argv[2]); return 1; }
        double sse = 0;
        for (size_t i = 0; i < len; i++) {
            if (i % 4 == 3) {
                if (px[i] != 255) { fprintf(stderr, "%s: alpha is not 255\n", argv[2]); return 1; }
                continue;
            }
            double d = (double)px[i] - ref[i];
            sse += d * d;
        }
        double mse = sse / ((double)w * h * 3), psnr = mse > 0 ? 10 * log10(255.0 * 255.0 / mse) : 99.0;
        double min = atof(argv[4]);
        printf("bc7enc: %s decodes at PSNR %.2f dB (minimum %.1f)\n", argv[2], psnr, min);
        return psnr >= min ? 0 : 1;
    }
    return usage();
}
