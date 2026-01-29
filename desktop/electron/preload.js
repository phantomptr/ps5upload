const { contextBridge, ipcRenderer } = require('electron');

// Expose protected methods to the renderer process
contextBridge.exposeInMainWorld('electronAPI', {
  // App
  appVersion: () => ipcRenderer.invoke('app_version'),
  appPlatform: () => ipcRenderer.invoke('app_platform'),

  // Window controls
  windowMinimize: () => ipcRenderer.invoke('window_minimize'),
  windowMaximize: () => ipcRenderer.invoke('window_maximize'),
  windowClose: () => ipcRenderer.invoke('window_close'),

  // Dialogs
  dialogOpen: (options) => ipcRenderer.invoke('dialog_open', options),
  dialogSave: (options) => ipcRenderer.invoke('dialog_save', options),
  openExternal: (url) => ipcRenderer.invoke('open_external', url),

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
  historySave: (data) => ipcRenderer.invoke('history_save', data),
  historyAdd: (record) => ipcRenderer.invoke('history_add', record),
  sleepSet: (enabled) => ipcRenderer.invoke('sleep_set', enabled),
  sleepStatus: () => ipcRenderer.invoke('sleep_status'),
  historyClear: () => ipcRenderer.invoke('history_clear'),

  // Logging
  setSaveLogs: (enabled) => ipcRenderer.invoke('set_save_logs', enabled),
  setUiLogEnabled: (enabled) => ipcRenderer.invoke('set_ui_log_enabled', enabled),
  faqLoad: () => ipcRenderer.invoke('faq_load'),

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
  payloadQueueClearAll: (ip) => ipcRenderer.invoke('payload_queue_clear_all', ip),
  payloadQueueClearFailed: (ip) => ipcRenderer.invoke('payload_queue_clear_failed', ip),
  payloadReset: (ip) => ipcRenderer.invoke('payload_reset', ip),
  payloadClearTmp: (ip) => ipcRenderer.invoke('payload_clear_tmp', ip),
  payloadQueueReorder: (ip, ids) => ipcRenderer.invoke('payload_queue_reorder', ip, ids),
  payloadQueueProcess: (ip) => ipcRenderer.invoke('payload_queue_process', ip),
  payloadQueuePause: (ip, id) => ipcRenderer.invoke('payload_queue_pause', ip, id),
  payloadQueueRetry: (ip, id) => ipcRenderer.invoke('payload_queue_retry', ip, id),
  payloadQueueRemove: (ip, id) => ipcRenderer.invoke('payload_queue_remove', ip, id),
  payloadSyncInfo: (ip) => ipcRenderer.invoke('payload_sync_info', ip),
  payloadUploadQueueGet: (ip) => ipcRenderer.invoke('payload_upload_queue_get', ip),
  payloadUploadQueueSync: (ip, payload) => ipcRenderer.invoke('payload_upload_queue_sync', ip, payload),
  payloadHistoryGet: (ip) => ipcRenderer.invoke('payload_history_get', ip),
  payloadHistorySync: (ip, payload) => ipcRenderer.invoke('payload_history_sync', ip, payload),

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
  manageUploadRar: (ip, rarPath, destPath, mode, tempRoot) => ipcRenderer.invoke('manage_upload_rar', ip, rarPath, destPath, mode, tempRoot),

  // Transfer
  transferCheckDest: (ip, destPath) => ipcRenderer.invoke('transfer_check_dest', ip, destPath),
  transferScan: (args) => ipcRenderer.invoke('transfer_scan', args),
  transferScanCancel: () => ipcRenderer.invoke('transfer_scan_cancel'),
  transferCancel: () => ipcRenderer.invoke('transfer_cancel'),
  transferStatus: () => ipcRenderer.invoke('transfer_status'),
  transferActive: () => ipcRenderer.invoke('transfer_active'),
  transferReset: () => ipcRenderer.invoke('transfer_reset'),
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
  chatStop: () => ipcRenderer.invoke('chat_stop'),
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
      'scan_progress',
      'scan_complete',
      'scan_error',
      'payload_done',
      'payload_busy',
      'payload_version',
      'payload_status_update',
      'payload_log',
      'queue_hint',
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
      'scan_progress',
      'scan_complete',
      'scan_error',
      'payload_done',
      'payload_busy',
      'payload_version',
      'payload_status_update',
      'payload_log',
      'queue_hint',
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
