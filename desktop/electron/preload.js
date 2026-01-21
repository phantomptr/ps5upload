const { contextBridge, ipcRenderer } = require('electron');

// Expose protected methods to the renderer process
contextBridge.exposeInMainWorld('electronAPI', {
  // App
  appVersion: () => ipcRenderer.invoke('app_version'),

  // Window controls
  windowMinimize: () => ipcRenderer.invoke('window_minimize'),
  windowMaximize: () => ipcRenderer.invoke('window_maximize'),
  windowClose: () => ipcRenderer.invoke('window_close'),

  // Dialogs
  dialogOpen: (options) => ipcRenderer.invoke('dialog_open', options),
  dialogSave: (options) => ipcRenderer.invoke('dialog_save', options),

  // Config
  configLoad: () => ipcRenderer.invoke('config_load'),
  configSave: (config) => ipcRenderer.invoke('config_save', config),
  configUpdate: (config) => ipcRenderer.invoke('config_update', config),

  // Profiles
  profilesLoad: () => ipcRenderer.invoke('profiles_load'),
  profilesSave: (data) => ipcRenderer.invoke('profiles_save', data),
  profilesUpdate: (data) => ipcRenderer.invoke('profiles_update', data),

  // Queue
  queueLoad: () => ipcRenderer.invoke('queue_load'),
  queueSave: (data) => ipcRenderer.invoke('queue_save', data),
  queueUpdate: (data) => ipcRenderer.invoke('queue_update', data),

  // History
  historyLoad: () => ipcRenderer.invoke('history_load'),
  historyAdd: (record) => ipcRenderer.invoke('history_add', record),
  historyClear: () => ipcRenderer.invoke('history_clear'),

  // Logging
  setSaveLogs: (enabled) => ipcRenderer.invoke('set_save_logs', enabled),
  setUiLogEnabled: (enabled) => ipcRenderer.invoke('set_ui_log_enabled', enabled),

  // Storage
  storageList: (ip) => ipcRenderer.invoke('storage_list', ip),

  // Port check
  portCheck: (ip, port) => ipcRenderer.invoke('port_check', ip, port),

  // Connection
  connectionSetIp: (ip) => ipcRenderer.invoke('connection_set_ip', ip),
  connectionPollingSet: (enabled) => ipcRenderer.invoke('connection_polling_set', enabled),
  connectionAutoSet: (enabled) => ipcRenderer.invoke('connection_auto_set', enabled),
  connectionSnapshot: () => ipcRenderer.invoke('connection_snapshot'),
  connectionConnect: (ip) => ipcRenderer.invoke('connection_connect', ip),

  // Payload
  payloadSend: (ip, filepath) => ipcRenderer.invoke('payload_send', ip, filepath),
  payloadDownloadAndSend: (ip, fetch) => ipcRenderer.invoke('payload_download_and_send', ip, fetch),
  payloadCheck: (ip) => ipcRenderer.invoke('payload_check', ip),
  payloadProbe: (filepath) => ipcRenderer.invoke('payload_probe', filepath),
  payloadStatus: (ip) => ipcRenderer.invoke('payload_status', ip),
  payloadStatusSnapshot: () => ipcRenderer.invoke('payload_status_snapshot'),
  payloadStatusRefresh: (ip) => ipcRenderer.invoke('payload_status_refresh', ip),
  payloadPollingSet: (enabled) => ipcRenderer.invoke('payload_polling_set', enabled),
  payloadSetIp: (ip) => ipcRenderer.invoke('payload_set_ip', ip),
  payloadAutoReloadSet: (enabled, mode, localPath) => ipcRenderer.invoke('payload_auto_reload_set', enabled, mode, localPath),
  payloadQueueExtract: (ip, src, dst) => ipcRenderer.invoke('payload_queue_extract', ip, src, dst),
  payloadQueueCancel: (ip, id) => ipcRenderer.invoke('payload_queue_cancel', ip, id),
  payloadQueueClear: (ip) => ipcRenderer.invoke('payload_queue_clear', ip),

  // Manage
  manageList: (ip, path) => ipcRenderer.invoke('manage_list', ip, path),
  manageListSnapshot: () => ipcRenderer.invoke('manage_list_snapshot'),
  manageListRefresh: (ip, path) => ipcRenderer.invoke('manage_list_refresh', ip, path),
  managePollingSet: (enabled) => ipcRenderer.invoke('manage_polling_set', enabled),
  manageSetIp: (ip) => ipcRenderer.invoke('manage_set_ip', ip),
  manageSetPath: (path) => ipcRenderer.invoke('manage_set_path', path),
  manageCancel: () => ipcRenderer.invoke('manage_cancel'),
  manageDelete: (ip, path) => ipcRenderer.invoke('manage_delete', ip, path),
  manageRename: (ip, src, dst) => ipcRenderer.invoke('manage_rename', ip, src, dst),
  manageCreateDir: (ip, path) => ipcRenderer.invoke('manage_create_dir', ip, path),
  manageChmod: (ip, path) => ipcRenderer.invoke('manage_chmod', ip, path),
  manageMove: (ip, src, dst) => ipcRenderer.invoke('manage_move', ip, src, dst),
  manageCopy: (ip, src, dst) => ipcRenderer.invoke('manage_copy', ip, src, dst),
  manageExtract: (ip, src, dst) => ipcRenderer.invoke('manage_extract', ip, src, dst),
  manageDownloadFile: (ip, path, dest) => ipcRenderer.invoke('manage_download_file', ip, path, dest),
  manageDownloadDir: (ip, path, dest, compression) => ipcRenderer.invoke('manage_download_dir', ip, path, dest, compression),
  manageUpload: (ip, destRoot, paths) => ipcRenderer.invoke('manage_upload', ip, destRoot, paths),

  // Transfer
  transferCheckDest: (ip, destPath) => ipcRenderer.invoke('transfer_check_dest', ip, destPath),
  transferScan: (sourcePath) => ipcRenderer.invoke('transfer_scan', sourcePath),
  transferCancel: () => ipcRenderer.invoke('transfer_cancel'),
  transferStatus: () => ipcRenderer.invoke('transfer_status'),
  transferStart: (req) => ipcRenderer.invoke('transfer_start', req),

  // Updates
  updateCheck: (includePrerelease) => ipcRenderer.invoke('update_check', includePrerelease),
  updateCheckTag: (tag) => ipcRenderer.invoke('update_check_tag', tag),
  updateDownloadAsset: (url, destPath) => ipcRenderer.invoke('update_download_asset', url, destPath),
  updateCurrentAssetName: () => ipcRenderer.invoke('update_current_asset_name'),
  updatePrepareSelf: (assetUrl) => ipcRenderer.invoke('update_prepare_self', assetUrl),
  updateApplySelf: () => ipcRenderer.invoke('update_apply_self'),

  // Chat
  chatInfo: () => ipcRenderer.invoke('chat_info'),
  chatGenerateName: () => ipcRenderer.invoke('chat_generate_name'),
  chatStart: () => ipcRenderer.invoke('chat_start'),
  chatSend: (name, text) => ipcRenderer.invoke('chat_send', name, text),

  // Game meta
  gameMetaLoad: (sourcePath) => ipcRenderer.invoke('game_meta_load', sourcePath),
  manageRarMetadata: (ip, filepath) => ipcRenderer.invoke('manage_rar_metadata', ip, filepath),

  // Event listeners
  on: (channel, callback) => {
    const validChannels = [
      'transfer_progress',
      'transfer_scan',
      'transfer_complete',
      'transfer_error',
      'transfer_log',
      'payload_done',
      'payload_busy',
      'payload_version',
      'payload_status_update',
      'payload_log',
      'connection_status_update',
      'manage_progress',
      'manage_done',
      'manage_log',
      'manage_list_update',
      'chat_message',
      'chat_status',
      'chat_ack',
      'update_ready',
      'update_error',
    ];
    if (validChannels.includes(channel)) {
      const subscription = (_event, ...args) => callback(...args);
      ipcRenderer.on(channel, subscription);
      return () => ipcRenderer.removeListener(channel, subscription);
    }
    return () => {};
  },

  once: (channel, callback) => {
    const validChannels = [
      'transfer_progress',
      'transfer_scan',
      'transfer_complete',
      'transfer_error',
      'transfer_log',
      'payload_done',
      'payload_busy',
      'payload_version',
      'payload_status_update',
      'payload_log',
      'connection_status_update',
      'manage_progress',
      'manage_done',
      'manage_log',
      'manage_list_update',
      'chat_message',
      'chat_status',
      'chat_ack',
      'update_ready',
      'update_error',
    ];
    if (validChannels.includes(channel)) {
      ipcRenderer.once(channel, (_event, ...args) => callback(...args));
    }
  },

  removeAllListeners: (channel) => {
    ipcRenderer.removeAllListeners(channel);
  },
});

// Also expose a Tauri-compatible API for easier migration
contextBridge.exposeInMainWorld('__TAURI_INTERNALS__', {
  invoke: async (cmd, args = {}) => {
    // Map Tauri command names to Electron IPC calls
    const commandMap = {
      'app_version': () => ipcRenderer.invoke('app_version'),
      'config_load': () => ipcRenderer.invoke('config_load'),
      'config_save': () => ipcRenderer.invoke('config_save', args.config),
      'config_update': () => ipcRenderer.invoke('config_update', args.config),
      'profiles_load': () => ipcRenderer.invoke('profiles_load'),
      'profiles_save': () => ipcRenderer.invoke('profiles_save', args.data),
      'profiles_update': () => ipcRenderer.invoke('profiles_update', args.data),
      'queue_load': () => ipcRenderer.invoke('queue_load'),
      'queue_save': () => ipcRenderer.invoke('queue_save', args.data),
      'queue_update': () => ipcRenderer.invoke('queue_update', args.data),
      'history_load': () => ipcRenderer.invoke('history_load'),
      'history_add': () => ipcRenderer.invoke('history_add', args.record),
      'history_clear': () => ipcRenderer.invoke('history_clear'),
      'set_save_logs': () => ipcRenderer.invoke('set_save_logs', args.enabled),
      'set_ui_log_enabled': () => ipcRenderer.invoke('set_ui_log_enabled', args.enabled),
      'storage_list': () => ipcRenderer.invoke('storage_list', args.ip),
      'port_check': () => ipcRenderer.invoke('port_check', args.ip, args.port),
      'connection_set_ip': () => ipcRenderer.invoke('connection_set_ip', args.ip),
      'connection_polling_set': () => ipcRenderer.invoke('connection_polling_set', args.enabled),
      'connection_auto_set': () => ipcRenderer.invoke('connection_auto_set', args.enabled),
      'connection_snapshot': () => ipcRenderer.invoke('connection_snapshot'),
      'connection_connect': () => ipcRenderer.invoke('connection_connect', args.ip),
      'payload_send': () => ipcRenderer.invoke('payload_send', args.ip, args.path),
      'payload_download_and_send': () => ipcRenderer.invoke('payload_download_and_send', args.ip, args.fetch),
      'payload_check': () => ipcRenderer.invoke('payload_check', args.ip),
      'payload_probe': () => ipcRenderer.invoke('payload_probe', args.path),
      'payload_status': () => ipcRenderer.invoke('payload_status', args.ip),
      'payload_status_snapshot': () => ipcRenderer.invoke('payload_status_snapshot'),
      'payload_status_refresh': () => ipcRenderer.invoke('payload_status_refresh', args.ip),
      'payload_polling_set': () => ipcRenderer.invoke('payload_polling_set', args.enabled),
      'payload_set_ip': () => ipcRenderer.invoke('payload_set_ip', args.ip),
      'payload_auto_reload_set': () => ipcRenderer.invoke('payload_auto_reload_set', args.enabled, args.mode, args.localPath || args.local_path),
      'payload_queue_extract': () => ipcRenderer.invoke('payload_queue_extract', args.ip, args.src, args.dst),
      'payload_queue_cancel': () => ipcRenderer.invoke('payload_queue_cancel', args.ip, args.id),
      'payload_queue_clear': () => ipcRenderer.invoke('payload_queue_clear', args.ip),
      'dialog_open': () => {
        const properties = ['openFile'];
        if (args.directory) {
          properties.length = 0;
          properties.push('openDirectory');
        }
        if (args.multiple) {
          properties.push('multiSelections');
        }
        return ipcRenderer.invoke('dialog_open', { ...args, properties });
      },
      'dialog_save': () => ipcRenderer.invoke('dialog_save', args),
      'manage_list': () => ipcRenderer.invoke('manage_list', args.ip, args.path),
      'manage_list_snapshot': () => ipcRenderer.invoke('manage_list_snapshot'),
      'manage_list_refresh': () => ipcRenderer.invoke('manage_list_refresh', args.ip, args.path),
      'manage_polling_set': () => ipcRenderer.invoke('manage_polling_set', args.enabled),
      'manage_set_ip': () => ipcRenderer.invoke('manage_set_ip', args.ip),
      'manage_set_path': () => ipcRenderer.invoke('manage_set_path', args.path),
      'manage_cancel': () => ipcRenderer.invoke('manage_cancel'),
      'manage_delete': () => ipcRenderer.invoke('manage_delete', args.ip, args.path),
      'manage_rename': () => ipcRenderer.invoke('manage_rename', args.ip, args.src_path || args.srcPath, args.dst_path || args.dstPath),
      'manage_create_dir': () => ipcRenderer.invoke('manage_create_dir', args.ip, args.path),
      'manage_chmod': () => ipcRenderer.invoke('manage_chmod', args.ip, args.path),
      'manage_move': () => ipcRenderer.invoke('manage_move', args.ip, args.src_path || args.srcPath, args.dst_path || args.dstPath),
      'manage_copy': () => ipcRenderer.invoke('manage_copy', args.ip, args.src_path || args.srcPath, args.dst_path || args.dstPath),
      'manage_extract': () => ipcRenderer.invoke('manage_extract', args.ip, args.src_path || args.srcPath, args.dst_path || args.dstPath),
      'manage_download_file': () => ipcRenderer.invoke('manage_download_file', args.ip, args.path, args.dest_path || args.destPath),
      'manage_download_dir': () => ipcRenderer.invoke('manage_download_dir', args.ip, args.path, args.dest_path || args.destPath, args.compression),
      'manage_upload': () => ipcRenderer.invoke('manage_upload', args.ip, args.dest_root || args.destRoot, args.paths),
      'transfer_check_dest': () => ipcRenderer.invoke('transfer_check_dest', args.ip, args.dest_path || args.destPath),
      'transfer_scan': () => ipcRenderer.invoke('transfer_scan', args.source_path || args.sourcePath),
      'transfer_cancel': () => ipcRenderer.invoke('transfer_cancel'),
      'transfer_status': () => ipcRenderer.invoke('transfer_status'),
      'transfer_start': () => ipcRenderer.invoke('transfer_start', args.req || args),
      'update_check': () => ipcRenderer.invoke('update_check', args.include_prerelease || args.includePrerelease),
      'update_check_tag': () => ipcRenderer.invoke('update_check_tag', args.tag),
      'update_download_asset': () => ipcRenderer.invoke('update_download_asset', args.url, args.dest_path || args.destPath),
      'update_current_asset_name': () => ipcRenderer.invoke('update_current_asset_name'),
      'update_prepare_self': () => ipcRenderer.invoke('update_prepare_self', args.asset_url || args.assetUrl),
      'update_apply_self': () => ipcRenderer.invoke('update_apply_self'),
      'chat_info': () => ipcRenderer.invoke('chat_info'),
      'chat_generate_name': () => ipcRenderer.invoke('chat_generate_name'),
      'chat_start': () => ipcRenderer.invoke('chat_start'),
      'chat_send': () => ipcRenderer.invoke('chat_send', args.name, args.text),
      'game_meta_load': () => ipcRenderer.invoke('game_meta_load', args.source_path || args.sourcePath),
      'manage_rar_metadata': () => ipcRenderer.invoke('manage_rar_metadata', args.ip, args.path),
    };

    const handler = commandMap[cmd];
    if (handler) {
      return handler();
    }
    throw new Error(`Unknown command: ${cmd}`);
  },
});

// Tauri-compatible event system
const eventListeners = new Map();

contextBridge.exposeInMainWorld('__TAURI_EVENT__', {
  listen: (event, callback) => {
    const handler = (_event, payload) => {
      callback({ payload });
    };
    ipcRenderer.on(event, handler);

    if (!eventListeners.has(event)) {
      eventListeners.set(event, []);
    }
    eventListeners.get(event).push({ callback, handler });

    // Return unlisten function
    return Promise.resolve(() => {
      ipcRenderer.removeListener(event, handler);
      const listeners = eventListeners.get(event);
      if (listeners) {
        const idx = listeners.findIndex(l => l.callback === callback);
        if (idx !== -1) listeners.splice(idx, 1);
      }
    });
  },

  once: (event, callback) => {
    return new Promise((resolve) => {
      ipcRenderer.once(event, (_event, payload) => {
        callback({ payload });
        resolve();
      });
    });
  },

  emit: (event, payload) => {
    // Emit to main process
    ipcRenderer.send(event, payload);
  },
});
