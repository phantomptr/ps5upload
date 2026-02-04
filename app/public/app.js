'use strict';

async function fetchJson(url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`HTTP ${response.status}`);
  }
  return response.json();
}

function setText(id, value) {
  const el = document.getElementById(id);
  if (el) {
    el.textContent = value;
  }
}

function setHealth(ok, version) {
  const pill = document.getElementById('health-pill');
  if (!pill) return;
  if (ok) {
    pill.classList.add('ok');
    pill.textContent = `Service online - v${version}`;
  } else {
    pill.classList.remove('ok');
    pill.textContent = 'Service check failed';
  }
}

function formatStarted(isoTime) {
  if (!isoTime) return '-';
  const dt = new Date(isoTime);
  if (Number.isNaN(dt.getTime())) return isoTime;
  return dt.toLocaleString();
}

async function loadOverview() {
  const [health, config, ifaces] = await Promise.all([
    fetchJson('/api/health'),
    fetchJson('/api/config'),
    fetchJson('/api/network/interfaces'),
  ]);

  setHealth(Boolean(health.ok), health.version || 'dev');
  setText('metric-version', health.version || 'dev');
  setText('metric-bind', `${config.host}:${config.port}`);
  setText('metric-started', formatStarted(health.started_at));

  const list = document.getElementById('interfaces');
  if (list) {
    list.innerHTML = '';
    const items = ifaces.interfaces || [];
    if (!items.length) {
      const li = document.createElement('li');
      li.textContent = 'No non-loopback interfaces found.';
      list.appendChild(li);
    } else {
      for (const item of items) {
        const li = document.createElement('li');
        li.textContent = `${item.name}: ${item.address} (${item.family})`;
        list.appendChild(li);
      }
    }
  }

  setText(
    'server-info',
    JSON.stringify(
      {
        service: health.service,
        version: health.version,
        host: config.host,
        port: config.port,
        started_at: health.started_at,
      },
      null,
      2
    )
  );
}

function setupPortCheck() {
  const form = document.getElementById('port-check-form');
  if (!form) return;

  form.addEventListener('submit', async (event) => {
    event.preventDefault();
    const ipInput = document.getElementById('ip');
    const portInput = document.getElementById('port');
    const ip = ipInput ? ipInput.value.trim() : '';
    const port = portInput ? portInput.value.trim() : '';

    setText('port-check-result', 'Checking port reachability...');

    try {
      const result = await fetchJson(`/api/port-check?ip=${encodeURIComponent(ip)}&port=${encodeURIComponent(port)}`);
      setText('port-check-result', JSON.stringify(result, null, 2));
    } catch (error) {
      setText('port-check-result', `Port check failed: ${error.message}`);
    }
  });
}

async function init() {
  setupPortCheck();
  try {
    await loadOverview();
  } catch (error) {
    setHealth(false);
    setText('server-info', `Failed to load service data: ${error.message}`);
  }
}

init();
