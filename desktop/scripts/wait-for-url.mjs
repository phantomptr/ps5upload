#!/usr/bin/env node
import http from "node:http";
import https from "node:https";

const target = process.argv[2];
const timeoutMs = Number(process.env.WAIT_FOR_URL_TIMEOUT_MS || 60000);
const intervalMs = Number(process.env.WAIT_FOR_URL_INTERVAL_MS || 250);

if (!target) {
  console.error("Usage: node scripts/wait-for-url.mjs <url>");
  process.exit(1);
}

const startedAt = Date.now();

const tryOnce = () =>
  new Promise((resolve) => {
    let url;
    try {
      url = new URL(target);
    } catch {
      resolve(false);
      return;
    }
    const client = url.protocol === "https:" ? https : http;
    const req = client.request(
      url,
      {
        method: "GET",
        timeout: 2000
      },
      (res) => {
        res.resume();
        resolve((res.statusCode || 0) > 0);
      }
    );
    req.on("timeout", () => {
      req.destroy();
      resolve(false);
    });
    req.on("error", () => resolve(false));
    req.end();
  });

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

while (Date.now() - startedAt < timeoutMs) {
  // eslint-disable-next-line no-await-in-loop
  const ready = await tryOnce();
  if (ready) process.exit(0);
  // eslint-disable-next-line no-await-in-loop
  await sleep(intervalMs);
}

console.error(`Timed out waiting for ${target}`);
process.exit(1);
