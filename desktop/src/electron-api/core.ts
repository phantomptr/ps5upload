// Electron adapter for core API

export async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    const api = window.electronAPI;
    if (!api) {
      throw new Error('Electron API not available');
    }
    
    switch (cmd) {
      case 'app_version':
        return api.appVersion();
      case 'app_platform':
        return api.appPlatform();
      case 'config_load':
        return api.configLoad();
      case 'config_save':
        return api.configSave(args.config);
      case 'config_update':
        return api.configUpdate(args.config);
      case 'profiles_load':
        return api.profilesLoad();
      case 'profiles_save':
        return api.profilesSave(args.data);
      case 'profiles_update':
        return api.profilesUpdate(args.data);
      case 'queue_load':
        return api.queueLoad();
      case 'queue_save':
        return api.queueSave(args.data);
      case 'queue_update':
        return api.queueUpdate(args.data);
      case 'history_load':
        return api.historyLoad();
      case 'history_save':
        return api.historySave(args.data);
      case 'history_add':
        return api.historyAdd(args.record);
      case 'sleep_set':
        return api.sleepSet(args.enabled);
      case 'sleep_status':
        return api.sleepStatus();
      case 'history_clear':
        return api.historyClear();
      case 'dialog_open':
        return api.dialogOpen(args as any);
      case 'dialog_save':
        return api.dialogSave(args as any);
      case 'open_external':
        return api.openExternal(args.url);
      case 'set_save_logs':
        return api.setSaveLogs(args.enabled);
      case 'set_ui_log_enabled':
        return api.setUiLogEnabled(args.enabled);
      case 'faq_load':
        return api.faqLoad();
      case 'storage_list':
        return api.storageList(args.ip);
      case 'port_check':
        return api.portCheck(args.ip, args.port);
      case 'connection_set_ip':
        return api.connectionSetIp(args.ip);
      case 'connection_polling_set':
        return api.connectionPollingSet(args.enabled);
      case 'connection_auto_set':
        return api.connectionAutoSet(args.enabled);
      case 'connection_snapshot':
        return api.connectionSnapshot();
      case 'connection_connect':
        return api.connectionConnect(args.ip);
      case 'payload_send':
        return api.payloadSend(args.ip, args.path);
      case 'payload_download_and_send':
        return api.payloadDownloadAndSend(args.ip, args.fetch);
      case 'payload_check':
        return api.payloadCheck(args.ip);
      case 'payload_probe':
        return api.payloadProbe(args.path);
      case 'payload_status':
        return api.payloadStatus(args.ip);
      case 'payload_status_snapshot':
        return api.payloadStatusSnapshot();
      case 'payload_status_refresh':
        return api.payloadStatusRefresh(args.ip);
      case 'payload_polling_set':
        return api.payloadPollingSet(args.enabled);
      case 'payload_set_ip':
        return api.payloadSetIp(args.ip);
      case 'payload_auto_reload_set':
        return api.payloadAutoReloadSet(args.enabled, args.mode, args.local_path);
      case 'payload_queue_extract':
        return api.payloadQueueExtract(args.ip, args.src, args.dst);
      case 'payload_queue_cancel':
        return api.payloadQueueCancel(args.ip, args.id);
      case 'payload_queue_clear':
        return api.payloadQueueClear(args.ip);
      case 'payload_queue_clear_all':
        return api.payloadQueueClearAll(args.ip);
      case 'payload_queue_clear_failed':
        return api.payloadQueueClearFailed(args.ip);
      case 'payload_reset':
        return api.payloadReset(args.ip);
      case 'payload_clear_tmp':
        return api.payloadClearTmp(args.ip);
      case 'payload_queue_reorder':
        return api.payloadQueueReorder(args.ip, args.ids);
      case 'payload_queue_process':
        return api.payloadQueueProcess(args.ip);
      case 'payload_queue_pause':
        return api.payloadQueuePause(args.ip, args.id);
      case 'payload_queue_retry':
        return api.payloadQueueRetry(args.ip, args.id);
      case 'payload_sync_info':
        return api.payloadSyncInfo(args.ip);
      case 'payload_upload_queue_get':
        return api.payloadUploadQueueGet(args.ip);
      case 'payload_upload_queue_sync':
        return api.payloadUploadQueueSync(args.ip, args.payload);
      case 'payload_history_get':
        return api.payloadHistoryGet(args.ip);
      case 'payload_history_sync':
        return api.payloadHistorySync(args.ip, args.payload);
      case 'manage_list':
        return api.manageList(args.ip, args.path);
      case 'manage_list_snapshot':
        return api.manageListSnapshot();
      case 'manage_list_refresh':
        return api.manageListRefresh(args.ip, args.path);
      case 'manage_polling_set':
        return api.managePollingSet(args.enabled);
      case 'manage_set_ip':
        return api.manageSetIp(args.ip);
      case 'manage_set_path':
        return api.manageSetPath(args.path);
      case 'manage_cancel':
        return api.manageCancel();
      case 'manage_delete':
        return api.manageDelete(args.ip, args.path);
      case 'manage_rename':
        return api.manageRename(args.ip, args.src_path, args.dst_path);
      case 'manage_create_dir':
        return api.manageCreateDir(args.ip, args.path);
      case 'manage_chmod':
        return api.manageChmod(args.ip, args.path);
      case 'manage_move':
        return api.manageMove(args.ip, args.src_path, args.dst_path);
      case 'manage_copy':
        return api.manageCopy(args.ip, args.src_path, args.dst_path);
      case 'manage_extract':
        return api.manageExtract(args.ip, args.src_path, args.dst_path);
      case 'manage_download_file':
        return api.manageDownloadFile(args.ip, args.path, args.dest_path);
      case 'manage_download_dir':
        return api.manageDownloadDir(args.ip, args.path, args.dest_path, args.compression);
      case 'manage_upload':
        return api.manageUpload(args.ip, args.dest_root, args.paths);
      case 'manage_upload_rar':
        return api.manageUploadRar(args.ip, args.rar_path, args.dest_path, args.mode, args.temp_root);
      case 'transfer_check_dest':
        return api.transferCheckDest(args.ip, args.destPath);
      case 'transfer_scan':
        return api.transferScan(args);
      case 'transfer_scan_cancel':
        return api.transferScanCancel();
      case 'transfer_cancel':
        return api.transferCancel();
      case 'transfer_status':
        return api.transferStatus();
      case 'transfer_active':
        return api.transferActive();
      case 'transfer_reset':
        return api.transferReset();
      case 'transfer_start':
        return api.transferStart(args.req);
      case 'update_check':
        return api.updateCheck(args.includePrerelease);
      case 'update_check_tag':
        return api.updateCheckTag(args.tag);
      case 'update_download_asset':
        return api.updateDownloadAsset(args.url, args.dest_path);
      case 'update_current_asset_name':
        return api.updateCurrentAssetName();
      case 'update_prepare_self':
        return api.updatePrepareSelf(args.asset_url);
      case 'update_apply_self':
        return api.updateApplySelf();
      case 'chat_info':
        return api.chatInfo();
      case 'chat_generate_name':
        return api.chatGenerateName();
      case 'chat_start':
        return api.chatStart();
      case 'chat_stop':
        return api.chatStop();
      case 'chat_send':
        return api.chatSend(args.name, args.text);
      case 'game_meta_load':
        return api.gameMetaLoad(args.path);
      case 'manage_rar_metadata':
        return api.manageRarMetadata(args.ip, args.path);
      default:
        return Promise.reject(new Error(`Unknown command: ${cmd}`));
    }
}
