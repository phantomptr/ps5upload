#!/usr/bin/env node
'use strict';

function percentile(values, p) {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  const idx = Math.min(sorted.length - 1, Math.max(0, Math.ceil((p / 100) * sorted.length) - 1));
  return sorted[idx];
}

function mean(values) {
  if (!values.length) return 0;
  let sum = 0;
  for (const value of values) sum += value;
  return sum / values.length;
}

class ConcatReader {
  constructor() {
    this.buffer = Buffer.alloc(0);
  }
  push(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
  }
  available() {
    return this.buffer.length;
  }
  readExact(len) {
    const out = this.buffer.subarray(0, len);
    this.buffer = this.buffer.subarray(len);
    return out;
  }
}

class QueueReader {
  constructor() {
    this.chunks = [];
    this.total = 0;
  }
  push(chunk) {
    this.chunks.push(chunk);
    this.total += chunk.length;
  }
  available() {
    return this.total;
  }
  readExact(len) {
    const out = Buffer.allocUnsafe(len);
    let copied = 0;
    while (copied < len && this.chunks.length > 0) {
      const head = this.chunks[0];
      const take = Math.min(head.length, len - copied);
      head.copy(out, copied, 0, take);
      copied += take;
      this.total -= take;
      if (take === head.length) this.chunks.shift();
      else this.chunks[0] = head.subarray(take);
    }
    return out;
  }
}

function runBench(ReaderCtor, chunks, frameLen) {
  const reader = new ReaderCtor();
  const start = process.hrtime.bigint();
  let consumed = 0;
  for (const chunk of chunks) {
    reader.push(chunk);
    while (reader.available() >= frameLen) {
      reader.readExact(frameLen);
      consumed += frameLen;
    }
  }
  if (reader.available() > 0) {
    const tail = reader.available();
    reader.readExact(tail);
    consumed += tail;
  }
  const sec = Number(process.hrtime.bigint() - start) / 1e9;
  return consumed / (1024 * 1024) / sec;
}

let seed = (Date.now() & 0xffffffff) >>> 0;
function rnd() {
  seed = (1664525 * seed + 1013904223) >>> 0;
  return seed / 0x100000000;
}

function buildChunks(totalBytes, minChunk, maxChunk) {
  const chunks = [];
  let remaining = totalBytes;
  while (remaining > 0) {
    const want = Math.min(remaining, Math.floor(minChunk + rnd() * (maxChunk - minChunk + 1)));
    const buf = Buffer.allocUnsafe(want);
    buf.fill(0xaa);
    chunks.push(buf);
    remaining -= want;
  }
  return chunks;
}

const iterations = 5;
const warmup = 1;
const totalBytes = 128 * 1024 * 1024;
const minChunk = 1024;
const maxChunk = 64 * 1024;
const frameLen = 4096;

const concatResults = [];
const queueResults = [];
for (let i = 0; i < warmup + iterations; i += 1) {
  const chunks = buildChunks(totalBytes, minChunk, maxChunk);
  const concat = runBench(ConcatReader, chunks, frameLen);
  const queue = runBench(QueueReader, chunks, frameLen);
  if (i < warmup) continue;
  concatResults.push(concat);
  queueResults.push(queue);
}

const concatMean = mean(concatResults);
const queueMean = mean(queueResults);
const concatMedian = percentile(concatResults, 50);
const queueMedian = percentile(queueResults, 50);

console.log(
  `perf guard: concat(mean=${concatMean.toFixed(2)} MB/s median=${concatMedian.toFixed(2)} MB/s), ` +
  `queue(mean=${queueMean.toFixed(2)} MB/s median=${queueMedian.toFixed(2)} MB/s)`
);

if (queueMedian < 500) {
  console.error(`Performance guard failed: queue median too low (${queueMedian.toFixed(2)} MB/s)`);
  process.exit(1);
}
if (queueMean < concatMean * 0.5) {
  console.error(
    `Performance guard failed: queue mean regressed too far (queue=${queueMean.toFixed(2)} MB/s concat=${concatMean.toFixed(2)} MB/s)`
  );
  process.exit(1);
}
