'use strict';

const DEFAULT_PORT = 10331;
const DEFAULT_HOST = '0.0.0.0';

function parseIntStrict(value, fallback) {
  if (typeof value !== 'string') return fallback;
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed <= 0 || parsed > 65535) return fallback;
  return parsed;
}

function normalizeHost(value) {
  if (typeof value !== 'string') return DEFAULT_HOST;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : DEFAULT_HOST;
}

function parseCliArgs(argv) {
  const args = Array.isArray(argv) ? argv : [];
  const out = {};

  for (let i = 0; i < args.length; i += 1) {
    const current = args[i];
    if (current === '--host' || current === '-H') {
      out.host = args[i + 1];
      i += 1;
      continue;
    }
    if (current === '--port' || current === '-p') {
      out.port = args[i + 1];
      i += 1;
      continue;
    }
  }

  return out;
}

function getRuntimeConfig(options = {}) {
  const env = options.env || process.env;
  const cli = parseCliArgs(options.argv || process.argv.slice(2));

  const host = normalizeHost(cli.host || env.APP_HOST);
  const port = parseIntStrict(String(cli.port || env.APP_PORT || ''), DEFAULT_PORT);

  return {
    host,
    port,
  };
}

module.exports = {
  DEFAULT_HOST,
  DEFAULT_PORT,
  getRuntimeConfig,
};
