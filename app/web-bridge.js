(function installPs5UploadWebBridge() {
  'use strict';

  if (window.ps5uploadWebAPI) return;

  var listeners = new Map();
  var transferMonitorTimer = null;
  var transferMonitorRunId = null;
  var connectionPollTimer = null;
  var payloadPollTimer = null;
  var managePollTimer = null;
  var bridgeState = {
    connectionIp: '',
    payloadIp: '',
    manageIp: '',
    managePath: '/data',
    connectionPollingEnabled: false,
    payloadPollingEnabled: false,
    managePollingEnabled: false,
  };
  window.__PS5UPLOAD_WEB_MODE__ = true;

  function emit(event, payload) {
    var handlers = listeners.get(event);
    if (!handlers) return;
    for (var i = 0; i < handlers.length; i += 1) {
      try {
        handlers[i](payload);
      } catch (err) {
        console.error('[web-bridge] listener failed', err);
      }
    }
  }

  function on(event, callback) {
    var handlers = listeners.get(event) || [];
    handlers.push(callback);
    listeners.set(event, handlers);
    return function unlisten() {
      var current = listeners.get(event) || [];
      listeners.set(event, current.filter(function (cb) { return cb !== callback; }));
    };
  }

  function installHostPathBanner() {
    if (window.localStorage && window.localStorage.getItem('ps5upload.webBannerDismissed') === '1') {
      return;
    }
    if (document.getElementById('ps5upload-web-host-banner')) return;
    var banner = document.createElement('div');
    banner.id = 'ps5upload-web-host-banner';
    banner.style.display = 'flex';
    banner.style.alignItems = 'center';
    banner.style.justifyContent = 'space-between';
    banner.style.gap = '10px';
    banner.style.position = 'fixed';
    banner.style.left = '12px';
    banner.style.right = '12px';
    banner.style.bottom = '12px';
    banner.style.zIndex = '99999';
    banner.style.padding = '10px 12px';
    banner.style.borderRadius = '10px';
    banner.style.font = '600 12px/1.4 "Noto Sans", "Segoe UI", sans-serif';
    banner.style.background = 'rgba(22, 101, 52, 0.92)';
    banner.style.color = '#fff';
    banner.style.boxShadow = '0 8px 24px rgba(0, 0, 0, 0.25)';

    var text = document.createElement('span');
    text.textContent = 'Web mode: file and folder paths are on the server host running PS5Upload, not on this browser device.';
    text.style.flex = '1';
    banner.appendChild(text);

    var close = document.createElement('button');
    close.type = 'button';
    close.textContent = 'x';
    close.setAttribute('aria-label', 'Close web mode notice');
    close.style.border = '1px solid rgba(255,255,255,0.35)';
    close.style.background = 'rgba(255,255,255,0.12)';
    close.style.color = '#fff';
    close.style.borderRadius = '8px';
    close.style.padding = '4px 8px';
    close.style.cursor = 'pointer';
    close.style.font = '700 12px/1 "Noto Sans", "Segoe UI", sans-serif';
    close.addEventListener('click', function () {
      if (window.localStorage) {
        window.localStorage.setItem('ps5upload.webBannerDismissed', '1');
      }
      banner.remove();
    });
    banner.appendChild(close);

    document.body.appendChild(banner);
  }

  function once(event, callback) {
    var unlisten = on(event, function (payload) {
      unlisten();
      callback(payload);
    });
  }

  async function invokeRemote(cmd, args) {
    var response = await fetch('/api/invoke', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ cmd: cmd, args: args || {} }),
    });
    var payload = await response.json();
    if (!response.ok || !payload.ok) {
      throw new Error(payload && payload.error ? payload.error : ('Command failed: ' + cmd));
    }
    return payload.result;
  }

  function clearTransferMonitor() {
    if (transferMonitorTimer) {
      clearInterval(transferMonitorTimer);
      transferMonitorTimer = null;
    }
    transferMonitorRunId = null;
  }

  function isTransferTerminal(statusText) {
    var text = String(statusText || '');
    return text.indexOf('Complete') === 0 || text.indexOf('Cancelled') === 0 || text.indexOf('Error') === 0;
  }

  function startTransferMonitor(runId) {
    clearTransferMonitor();
    transferMonitorRunId = Number(runId) || null;
    transferMonitorTimer = setInterval(async function () {
      try {
        var status = await invokeRemote('transfer_status', {});
        if (!status || typeof status !== 'object') return;
        var statusRunId = Number(status.run_id) || transferMonitorRunId;
        var statusText = String(status.status || '');
        if (!isTransferTerminal(statusText)) return;
        clearTransferMonitor();
        if (statusText.indexOf('Complete') === 0) {
          emit('transfer_complete', {
            run_id: statusRunId || transferMonitorRunId || 0,
            files: Number(status.files) || 0,
            bytes: Number(status.sent) || Number(status.total) || 0,
          });
        } else {
          emit('transfer_error', {
            run_id: statusRunId || transferMonitorRunId || 0,
            message: statusText || 'Transfer failed',
          });
        }
      } catch {
        // Keep polling; transient network/server errors can happen mid-transfer.
      }
    }, 500);
  }

  function stopConnectionPoll() {
    if (connectionPollTimer) {
      clearInterval(connectionPollTimer);
      connectionPollTimer = null;
    }
  }

  function stopPayloadPoll() {
    if (payloadPollTimer) {
      clearInterval(payloadPollTimer);
      payloadPollTimer = null;
    }
  }

  function stopManagePoll() {
    if (managePollTimer) {
      clearInterval(managePollTimer);
      managePollTimer = null;
    }
  }

  function startConnectionPoll() {
    stopConnectionPoll();
    if (!bridgeState.connectionPollingEnabled || !bridgeState.connectionIp) return;
    connectionPollTimer = setInterval(async function () {
      try {
        var snapshot = await invokeRemote('connection_connect', { ip: bridgeState.connectionIp });
        emit('connection_status_update', snapshot);
      } catch {
        // ignore poll failures
      }
    }, 2000);
  }

  function startPayloadPoll() {
    stopPayloadPoll();
    if (!bridgeState.payloadPollingEnabled || !bridgeState.payloadIp) return;
    payloadPollTimer = setInterval(async function () {
      try {
        var snapshot = await invokeRemote('payload_status_refresh', { ip: bridgeState.payloadIp });
        emit('payload_status_update', snapshot);
      } catch {
        // ignore poll failures
      }
    }, 1200);
  }

  function startManagePoll() {
    stopManagePoll();
    if (!bridgeState.managePollingEnabled || !bridgeState.manageIp || !bridgeState.managePath) return;
    managePollTimer = setInterval(async function () {
      try {
        var snapshot = await invokeRemote('manage_list_refresh', {
          ip: bridgeState.manageIp,
          path: bridgeState.managePath,
        });
        emit('manage_list_update', snapshot);
      } catch {
        // ignore poll failures
      }
    }, 1800);
  }

  function mapManageOp(cmd) {
    if (cmd === 'manage_upload' || cmd === 'manage_upload_rar') return 'Upload';
    if (cmd === 'manage_download_file' || cmd === 'manage_download_dir') return 'Download';
    if (cmd === 'manage_move' || cmd === 'manage_rename') return 'Move';
    if (cmd === 'manage_copy') return 'Copy';
    if (cmd === 'manage_extract') return 'Extract';
    if (cmd === 'manage_delete') return 'Delete';
    if (cmd === 'manage_create_dir') return 'Create Folder';
    if (cmd === 'manage_chmod') return 'Permissions';
    return cmd;
  }

  function openHostBrowserWindow(options, saveMode) {
    return new Promise(function (resolve) {
      var opts = options || {};
      var token = Math.random().toString(36).slice(2);
      var query = new URLSearchParams({
        mode: saveMode ? 'save' : 'open',
        directory: opts.directory ? '1' : '0',
        multiple: '0',
        defaultPath: opts.defaultPath || '',
        token: token,
      });
      var popup = window.open('/hostfs-browser?' + query.toString(), 'ps5uploadHostBrowser', 'width=1040,height=760');
      if (!popup) {
        resolve({ status: 'blocked', value: null });
        return;
      }

      var settled = false;
      var timer = setInterval(function () {
        if (popup.closed && !settled) {
          settled = true;
          clearInterval(timer);
          window.removeEventListener('message', onMessage);
          resolve({ status: 'closed', value: null });
        }
      }, 300);

      function onMessage(event) {
        var data = event && event.data;
        if (!data || data.type !== 'ps5upload-hostfs-select' || data.token !== token) return;
        if (settled) return;
        settled = true;
        clearInterval(timer);
        window.removeEventListener('message', onMessage);
        resolve({ status: 'selected', value: data.value });
      }
      window.addEventListener('message', onMessage);
    });
  }

  async function fetchHostFs(pathname) {
    var response = await fetch(pathname);
    var payload = await response.json();
    if (!response.ok) {
      throw new Error(payload && payload.error ? payload.error : 'Host file browser request failed');
    }
    return payload;
  }

  function joinPath(base, name) {
    if (!base) return name;
    if (base === '/') return '/' + name;
    if (base.endsWith('\\') || base.endsWith('/')) return base + name;
    if (base.indexOf('\\') !== -1 && base.indexOf('/') === -1) return base + '\\' + name;
    return base + '/' + name;
  }

  function createModalShell(title) {
    var shellBg = '#ffffff';
    var textColor = '#111827';
    var borderColor = '#d1d5db';
    var surfaceAlt = '#f3f4f6';
    var accent = '#2563eb';
    var inputBg = '#ffffff';
    var rowBg = '#ffffff';

    var overlay = document.createElement('div');
    overlay.style.position = 'fixed';
    overlay.style.inset = '0';
    overlay.style.zIndex = '100000';
    overlay.style.background = 'rgba(0,0,0,0.45)';
    overlay.style.display = 'grid';
    overlay.style.placeItems = 'center';

    var modal = document.createElement('div');
    modal.style.width = 'min(980px, 96vw)';
    modal.style.height = 'min(720px, 92vh)';
    modal.style.background = shellBg;
    modal.style.color = textColor;
    modal.style.borderRadius = '12px';
    modal.style.display = 'grid';
    modal.style.gridTemplateRows = 'auto auto 1fr auto';
    modal.style.border = '1px solid ' + borderColor;
    modal.style.overflow = 'hidden';
    modal.style.fontFamily = '"Noto Sans","Segoe UI",sans-serif';

    var header = document.createElement('div');
    header.style.padding = '12px 14px';
    header.style.background = surfaceAlt;
    header.style.borderBottom = '1px solid ' + borderColor;
    header.style.fontWeight = '700';
    header.textContent = title;

    var toolbar = document.createElement('div');
    toolbar.style.display = 'flex';
    toolbar.style.gap = '8px';
    toolbar.style.padding = '10px 12px';
    toolbar.style.borderBottom = '1px solid ' + borderColor;
    toolbar.style.alignItems = 'center';

    var list = document.createElement('div');
    list.style.overflow = 'auto';
    list.style.padding = '8px';
    list.style.display = 'grid';
    list.style.gap = '4px';
    list.style.minHeight = '260px';

    var footer = document.createElement('div');
    footer.style.display = 'flex';
    footer.style.justifyContent = 'flex-end';
    footer.style.gap = '8px';
    footer.style.padding = '10px 12px';
    footer.style.borderTop = '1px solid ' + borderColor;

    modal.appendChild(header);
    modal.appendChild(toolbar);
    modal.appendChild(list);
    modal.appendChild(footer);
    overlay.appendChild(modal);
    document.body.appendChild(overlay);

    return { overlay: overlay, modal: modal, toolbar: toolbar, list: list, footer: footer };
  }

  function openHostPathModal(options, saveMode) {
    return new Promise(async function (resolve) {
      var opts = options || {};
      var shell = createModalShell(saveMode ? 'Select Save Location (Host)' : 'Select Host Path');
      var currentPath = (opts.defaultPath || '').trim();
      var selected = null;
      var parentPath = null;
      var fileName = '';

      function close(value) {
        shell.overlay.remove();
        resolve(value);
      }

      function button(label, onClick) {
        var btn = document.createElement('button');
        btn.type = 'button';
        btn.textContent = label;
        btn.style.padding = '8px 12px';
        btn.style.border = '1px solid ' + borderColor;
        btn.style.borderRadius = '8px';
        btn.style.background = inputBg;
        btn.style.color = textColor;
        btn.style.cursor = 'pointer';
        btn.addEventListener('click', onClick);
        return btn;
      }

      var pathInput = document.createElement('input');
      pathInput.type = 'text';
      pathInput.style.flex = '1';
      pathInput.style.padding = '8px 10px';
      pathInput.style.border = '1px solid ' + borderColor;
      pathInput.style.borderRadius = '8px';
      pathInput.style.background = inputBg;
      pathInput.style.color = textColor;
      pathInput.placeholder = 'Host path';

      var upBtn = button('Up', function () {
        if (parentPath) loadPath(parentPath);
      });
      var goBtn = button('Go', function () {
        loadPath(pathInput.value);
      });
      var rootsBtn = button('Roots', async function () {
        var roots = await fetchHostFs('/api/hostfs/roots');
        shell.list.innerHTML = '';
        (roots.roots || []).forEach(function (root) {
          var row = button(root.label || root.path, function () { loadPath(root.path); });
          row.style.textAlign = 'left';
          row.style.justifyContent = 'flex-start';
          row.style.width = '100%';
          shell.list.appendChild(row);
        });
      });

      shell.toolbar.appendChild(upBtn);
      shell.toolbar.appendChild(rootsBtn);
      shell.toolbar.appendChild(pathInput);
      shell.toolbar.appendChild(goBtn);

      if (saveMode) {
        var nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.placeholder = 'File name';
        nameInput.style.marginLeft = '8px';
        nameInput.style.padding = '8px 10px';
        nameInput.style.border = '1px solid ' + borderColor;
        nameInput.style.borderRadius = '8px';
        nameInput.style.background = inputBg;
        nameInput.style.color = textColor;
        nameInput.addEventListener('input', function () {
          fileName = nameInput.value.trim();
        });
        shell.toolbar.appendChild(nameInput);
        if (opts.defaultPath) {
          var parts = opts.defaultPath.split(/[\\/]/);
          fileName = parts[parts.length - 1] || '';
          nameInput.value = fileName;
          if (parts.length > 1) currentPath = opts.defaultPath.slice(0, opts.defaultPath.length - fileName.length).replace(/[\\/]$/, '');
        }
      }

      async function loadPath(targetPath) {
        try {
          shell.list.innerHTML = '';
          var loading = document.createElement('div');
          loading.textContent = 'Loading...';
          loading.style.padding = '8px';
          loading.style.opacity = '0.8';
          shell.list.appendChild(loading);

          var payload = await fetchHostFs('/api/hostfs/list?path=' + encodeURIComponent(targetPath || currentPath || ''));
          currentPath = payload.path;
          parentPath = payload.parent;
          pathInput.value = currentPath;
          selected = saveMode ? currentPath : null;
          shell.list.innerHTML = '';
          (payload.entries || []).forEach(function (entry) {
            var row = document.createElement('button');
            row.type = 'button';
            row.style.padding = '8px 10px';
            row.style.border = '1px solid ' + borderColor;
            row.style.borderRadius = '8px';
            row.style.background = rowBg;
            row.style.color = textColor;
            row.style.textAlign = 'left';
            row.style.cursor = 'pointer';
            row.textContent = (entry.type === 'dir' ? '[DIR] ' : '') + entry.name;
            row.addEventListener('click', function () {
              if (entry.type === 'dir') {
                selected = entry.path;
              } else if (!opts.directory) {
                selected = entry.path;
              }
            });
            row.addEventListener('mouseenter', function () {
              row.style.borderColor = accent;
            });
            row.addEventListener('mouseleave', function () {
              row.style.borderColor = borderColor;
            });
            row.addEventListener('dblclick', function () {
              if (entry.type === 'dir') loadPath(entry.path);
              else if (!opts.directory) close(opts.multiple ? [entry.path] : entry.path);
            });
            shell.list.appendChild(row);
          });
          if (!payload.entries || payload.entries.length === 0) {
            var empty = document.createElement('div');
            empty.textContent = 'Folder is empty.';
            empty.style.padding = '8px';
            empty.style.opacity = '0.75';
            shell.list.appendChild(empty);
          }
        } catch (err) {
          console.error('[web-bridge] hostfs list failed', err);
          shell.list.innerHTML = '';
          var msg = document.createElement('div');
          msg.textContent = 'Failed to open path: ' + (err && err.message ? err.message : String(err));
          msg.style.color = accent;
          msg.style.padding = '8px';
          shell.list.appendChild(msg);
          try {
            var roots = await fetchHostFs('/api/hostfs/roots');
            if (roots && Array.isArray(roots.roots) && roots.roots.length > 0) {
              var hint = document.createElement('div');
              hint.textContent = 'Try a root path:';
              hint.style.padding = '4px 8px';
              shell.list.appendChild(hint);
              roots.roots.forEach(function (root) {
                var row = button(root.label || root.path, function () { loadPath(root.path); });
                row.style.textAlign = 'left';
                row.style.width = '100%';
                shell.list.appendChild(row);
              });
            }
          } catch (_ignored) {
            // ignore fallback errors
          }
        }
      }

      shell.footer.appendChild(button('Cancel', function () { close(saveMode ? null : []); }));
      if (!saveMode && opts.directory) {
        shell.footer.appendChild(button('Select Folder', function () { close(opts.multiple ? [selected || currentPath] : (selected || currentPath)); }));
      }
      shell.footer.appendChild(button(saveMode ? 'Save' : 'Select', function () {
        if (saveMode) {
          var target = fileName ? joinPath(currentPath, fileName) : currentPath;
          close(target || null);
          return;
        }
        if (!selected) {
          close(opts.multiple ? [] : null);
          return;
        }
        close(opts.multiple ? [selected] : selected);
      }));

      if (!currentPath) {
        var roots = await fetchHostFs('/api/hostfs/roots');
        currentPath = roots && roots.roots && roots.roots[0] ? roots.roots[0].path : '/';
      }
      loadPath(currentPath);

      function onEsc(event) {
        if (event.key === 'Escape') {
          document.removeEventListener('keydown', onEsc);
          close(saveMode ? null : []);
        }
      }
      document.addEventListener('keydown', onEsc);
    });
  }

  async function invoke(cmd, args) {
    if (cmd === 'connection_set_ip') {
      bridgeState.connectionIp = args && args.ip ? String(args.ip).trim() : '';
    } else if (cmd === 'payload_set_ip') {
      bridgeState.payloadIp = args && args.ip ? String(args.ip).trim() : '';
    } else if (cmd === 'manage_set_ip') {
      bridgeState.manageIp = args && args.ip ? String(args.ip).trim() : '';
    } else if (cmd === 'manage_set_path') {
      bridgeState.managePath = args && args.path ? String(args.path).trim() : '/data';
    }

    if (cmd === 'dialog_open') {
      var selectedResult = await openHostBrowserWindow(args || {}, false);
      var selected = selectedResult ? selectedResult.value : null;
      if (selected == null) return [];
      return Array.isArray(selected) ? selected : [selected];
    }

    if (cmd === 'dialog_save') {
      var saveResult = await openHostBrowserWindow(args || {}, true);
      return saveResult ? saveResult.value : null;
    }

    if (cmd === 'open_external') {
      var url = args && args.url ? String(args.url) : '';
      if (url) window.open(url, '_blank', 'noopener,noreferrer');
      return true;
    }

    var sendsPayload = cmd === 'payload_send' || cmd === 'payload_download_and_send';
    var scanProgressTimer = null;
    var manageProgressTimer = null;
    var isManageOp = cmd === 'manage_download_file' || cmd === 'manage_download_dir' || cmd === 'manage_upload' || cmd === 'manage_upload_rar' || cmd === 'manage_copy' || cmd === 'manage_extract' || cmd === 'manage_delete' || cmd === 'manage_move' || cmd === 'manage_rename' || cmd === 'manage_create_dir' || cmd === 'manage_chmod';
    var manageOp = mapManageOp(cmd);
    if (sendsPayload) {
      emit('payload_busy', { busy: true });
    }
    if (cmd === 'transfer_scan') {
      emit('scan_progress', { files: 0, total: 0 });
      scanProgressTimer = setInterval(function () {
        invokeRemote('transfer_scan_status', {}).then(function (state) {
          if (!state || typeof state !== 'object') return;
          emit('scan_progress', {
            files: Number(state.files) || 0,
            total: Number(state.total) || 0,
          });
        }).catch(function () {
          // ignore scan poll failures
        });
      }, 700);
    }
    if (isManageOp) {
      manageProgressTimer = setInterval(function () {
        if (cmd === 'manage_upload' || cmd === 'manage_upload_rar') {
          invokeRemote('transfer_status', {}).then(function (status) {
            if (!status || typeof status !== 'object') return;
            emit('manage_progress', {
              op: manageOp,
              processed: Number(status.sent) || 0,
              total: Number(status.total) || 0,
              current_file: status.current_file || '',
              speed_bps: Number(status.total_speed_bps) || Number(status.payload_speed_bps) || Number(status.ftp_speed_bps) || 0,
            });
          }).catch(function () {
            // ignore
          });
          return;
        }
        invokeRemote('manage_progress_status', {}).then(function (progress) {
          if (!progress || typeof progress !== 'object') return;
          emit('manage_progress', {
            op: manageOp,
            processed: Number(progress.processed) || 0,
            total: Number(progress.total) || 0,
            current_file: progress.current_file || '',
            speed_bps: Number(progress.speed_bps) || 0,
          });
        }).catch(function () {
          // ignore
        });
      }, 500);
    }

    var result;
    try {
      result = await invokeRemote(cmd, args);
    } catch (err) {
      if (scanProgressTimer) {
        clearInterval(scanProgressTimer);
      }
      if (manageProgressTimer) {
        clearInterval(manageProgressTimer);
      }
      if (cmd === 'transfer_scan') {
        emit('scan_error', { message: err && err.message ? err.message : String(err) });
      }
      if (isManageOp) {
        emit('manage_done', { op: manageOp, bytes: null, error: err && err.message ? err.message : String(err) });
      }
      if (sendsPayload) {
        emit('payload_done', { bytes: null, error: err && err.message ? err.message : String(err) });
        emit('payload_busy', { busy: false });
      }
      throw err;
    }

    if (cmd === 'payload_check') {
      emit('payload_version', { version: result, error: null });
    }
    if (cmd === 'payload_status_refresh' || cmd === 'payload_status_snapshot') {
      emit('payload_status_update', result);
    }
    if (cmd === 'connection_connect') {
      bridgeState.connectionIp = args && args.ip ? String(args.ip).trim() : bridgeState.connectionIp;
      emit('connection_status_update', result);
    }
    if (cmd === 'connection_polling_set') {
      bridgeState.connectionPollingEnabled = Boolean(args && args.enabled);
      startConnectionPoll();
    }
    if (cmd === 'payload_set_ip') {
      startPayloadPoll();
    }
    if (cmd === 'manage_list_refresh') {
      emit('manage_list_update', result);
    }
    if (cmd === 'manage_set_ip' || cmd === 'manage_set_path') {
      startManagePoll();
    }
    if (cmd === 'manage_polling_set') {
      bridgeState.managePollingEnabled = Boolean(args && args.enabled);
      startManagePoll();
    }
    if (cmd === 'payload_polling_set') {
      bridgeState.payloadPollingEnabled = Boolean(args && args.enabled);
      startPayloadPoll();
    }
    if (isManageOp) {
      if (manageProgressTimer) {
        clearInterval(manageProgressTimer);
      }
      emit('manage_done', {
        op: manageOp,
        bytes: typeof result === 'number' ? result : (result && typeof result.bytes === 'number' ? result.bytes : null),
        files: result && typeof result.files === 'number' ? result.files : null,
        error: null,
      });
    }
    if (cmd === 'transfer_scan') {
      if (scanProgressTimer) {
        clearInterval(scanProgressTimer);
      }
      emit('scan_complete', result);
    }
    if (cmd === 'transfer_start') {
      startTransferMonitor(result);
    }
    if (cmd === 'transfer_reset') {
      clearTransferMonitor();
    }
    if (sendsPayload) {
      emit('payload_done', { bytes: result, error: null });
      emit('payload_busy', { busy: false });
    }

    return result;
  }

  window.ps5uploadWebAPI = {
    invoke: invoke,
    on: on,
    once: once,
    emit: emit,
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', installHostPathBanner, { once: true });
  } else {
    installHostPathBanner();
  }

  // Notify UI that backend bridge is ready.
  setTimeout(function () {
    emit('payload_log', { message: 'Web bridge ready' });
  }, 0);
})();
