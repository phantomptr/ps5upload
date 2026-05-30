//! Local filesystem browsing + Android all-files-access permission.
//!
//! Desktop picks files/folders with native dialogs (plugin-dialog), which
//! return real filesystem paths. Android's scoped storage doesn't: the
//! dialog returns `content://` URIs (or nothing for a directory), which
//! the engine — it walks real `std::fs` paths — can't use, and copying a
//! multi-GB game into app cache is impractical.
//!
//! So on Android we instead (1) request all-files access
//! (`MANAGE_EXTERNAL_STORAGE`) so the app can read arbitrary paths under
//! `/storage/emulated/0`, then (2) browse the real filesystem in-app
//! (`local_list_dir`) so the user picks a real path the engine reads
//! directly — folders and big `.zip`s included, no copying. These
//! commands back that flow; on desktop they are harmless helpers.

use serde::Serialize;

#[derive(Serialize)]
pub struct LocalEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: u64,
}

/// List a real local directory: directories first, then files, each
/// alphabetical (case-insensitive). Unreadable entries are skipped rather
/// than failing the whole listing.
#[tauri::command]
pub async fn local_list_dir(path: String) -> Result<Vec<LocalEntry>, String> {
    let rd = std::fs::read_dir(&path).map_err(|e| format!("read_dir {path}: {e}"))?;
    let mut out: Vec<LocalEntry> = Vec::new();
    for ent in rd.flatten() {
        let md = match ent.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push(LocalEntry {
            name: ent.file_name().to_string_lossy().into_owned(),
            path: ent.path().to_string_lossy().into_owned(),
            is_dir: md.is_dir(),
            size: if md.is_file() { md.len() } else { 0 },
        });
    }
    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(out)
}

/// Roots to seed the in-app browser. Android: primary shared storage plus
/// any mounted removable volumes under `/storage`. Desktop: the home dir.
#[tauri::command]
pub async fn local_storage_roots() -> Result<Vec<String>, String> {
    #[cfg(target_os = "android")]
    {
        let mut roots: Vec<String> = Vec::new();
        fn push_unique(roots: &mut Vec<String>, p: String) {
            if !p.is_empty() && !roots.contains(&p) {
                roots.push(p);
            }
        }

        // Primary shared storage first — the common case.
        let primary = "/storage/emulated/0";
        if std::path::Path::new(primary).exists() {
            push_unique(&mut roots, primary.to_string());
        }

        // Authoritative removable-volume enumeration via
        // StorageManager.getStorageVolumes() → getDirectory(). This is
        // what surfaces SD cards and auto-mounted USB-OTG drives with
        // their real, app-readable paths — a blind /storage scan misses
        // them because stat() is denied on a removable mount until the
        // framework has surfaced the volume. All-files access
        // (MANAGE_EXTERNAL_STORAGE) covers reading them; there is no
        // separate SD/OTG permission to request.
        for dir in android::storage_volume_dirs() {
            push_unique(&mut roots, dir);
        }

        // Fallback scan of /storage for anything the framework list
        // didn't return. Lenient on purpose: include every non-pseudo
        // entry rather than gating on is_dir(), since a removable mount
        // can exist as a /storage entry while its stat() still EACCES's
        // until first real access. A genuinely unreadable root simply
        // errors when the user opens it, which beats hiding it.
        if let Ok(rd) = std::fs::read_dir("/storage") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == "emulated" || name == "self" {
                    continue;
                }
                push_unique(&mut roots, e.path().to_string_lossy().into_owned());
            }
        }

        if roots.is_empty() {
            roots.push("/sdcard".to_string());
        }
        Ok(roots)
    }
    #[cfg(not(target_os = "android"))]
    {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/".to_string());
        Ok(vec![home])
    }
}

/// Whether the app currently has all-files access. Android: reflects
/// `Environment.isExternalStorageManager()`. Elsewhere: always true (no
/// scoped-storage restriction), so the UI skips the grant step.
#[tauri::command]
pub async fn storage_access_granted() -> Result<bool, String> {
    #[cfg(target_os = "android")]
    {
        android::is_external_storage_manager()
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(true)
    }
}

/// Open this app's "All files access" settings page so the user can grant
/// `MANAGE_EXTERNAL_STORAGE`. No-op off Android.
#[tauri::command]
pub async fn request_storage_access() -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        android::request_all_files_access()
    }
    #[cfg(not(target_os = "android"))]
    {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_list_dir_sorts_dirs_first_then_files_case_insensitive() {
        let tmp = std::env::temp_dir().join(format!("ps5_lf_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("Zsub")).unwrap();
        std::fs::create_dir_all(tmp.join("alpha")).unwrap();
        std::fs::write(tmp.join("b.txt"), b"hello").unwrap();
        std::fs::write(tmp.join("A.bin"), b"xy").unwrap();

        let out = local_list_dir(tmp.to_string_lossy().into_owned())
            .await
            .unwrap();
        let names: Vec<&str> = out.iter().map(|e| e.name.as_str()).collect();
        // Directories first (alpha, Zsub — case-insensitive), then files
        // (A.bin, b.txt — case-insensitive).
        assert_eq!(names, vec!["alpha", "Zsub", "A.bin", "b.txt"]);
        assert!(out[0].is_dir);
        assert_eq!(out[0].size, 0);
        let btxt = out.iter().find(|e| e.name == "b.txt").unwrap();
        assert!(!btxt.is_dir);
        assert_eq!(btxt.size, 5);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn local_list_dir_errors_on_missing_dir() {
        let r = local_list_dir("/no/such/ps5upload-test-dir-xyz".into()).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn desktop_storage_helpers() {
        // Off Android these are plain helpers: roots = home dir,
        // access always granted, request is a no-op.
        let roots = local_storage_roots().await.unwrap();
        assert!(!roots.is_empty(), "expected at least the home dir");
        assert!(storage_access_granted().await.unwrap());
        assert!(request_storage_access().await.is_ok());
    }
}

#[cfg(target_os = "android")]
mod android {
    use jni::objects::{JObject, JString, JValue};
    use jni::{JNIEnv, JavaVM};
    use std::ffi::c_void;
    use std::sync::OnceLock;

    /// JavaVM captured at native-library load.
    ///
    /// Tauri 2's Android backend (a Kotlin WryActivity that `loadLibrary`s
    /// this `.so`) does NOT initialize the `ndk_context` crate's global —
    /// tao carries its own internal glue and never calls
    /// `ndk_context::initialize_android_context`. So the old
    /// `ndk_context::android_context()` here always hit an uninitialized
    /// global and aborted the whole app on first use ("android context was
    /// not initialized"). We capture the VM ourselves in `JNI_OnLoad`
    /// instead — the runtime calls it once when the library loads, before
    /// any Tauri command can run, and neither wry nor tao defines it.
    static JVM: OnceLock<JavaVM> = OnceLock::new();

    /// Android runtime entry point, invoked on `System.loadLibrary`.
    #[no_mangle]
    pub extern "system" fn JNI_OnLoad(
        vm: *mut jni::sys::JavaVM,
        _reserved: *mut c_void,
    ) -> jni::sys::jint {
        if let Ok(vm) = unsafe { JavaVM::from_raw(vm) } {
            let _ = JVM.set(vm);
        }
        jni::sys::JNI_VERSION_1_6
    }

    fn vm() -> Result<&'static JavaVM, String> {
        JVM.get()
            .ok_or_else(|| "Android JavaVM unavailable (JNI_OnLoad not run)".to_string())
    }

    /// The app's Application context — a `Context` we can use for
    /// `getSystemService` and `startActivity(FLAG_ACTIVITY_NEW_TASK)`.
    /// Fetched via `ActivityThread.currentApplication()` so we don't need a
    /// captured Activity (or the unavailable `ndk_context`).
    fn app_context<'local>(env: &mut JNIEnv<'local>) -> Result<JObject<'local>, String> {
        let app = env
            .call_static_method(
                "android/app/ActivityThread",
                "currentApplication",
                "()Landroid/app/Application;",
                &[],
            )
            .map_err(|e| format!("currentApplication: {e}"))?
            .l()
            .map_err(|e| format!("application obj: {e}"))?;
        if app.is_null() {
            return Err("currentApplication() returned null".into());
        }
        Ok(app)
    }

    fn sdk_int(env: &mut jni::JNIEnv) -> Result<i32, String> {
        env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
            .map_err(|e| format!("SDK_INT: {e}"))?
            .i()
            .map_err(|e| format!("SDK_INT int: {e}"))
    }

    pub fn is_external_storage_manager() -> Result<bool, String> {
        let vm = vm()?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("attach: {e}"))?;
        // isExternalStorageManager() is API 30+. On older devices the
        // legacy storage permissions apply instead, so report "granted"
        // and let the runtime perms (declared in the manifest) cover it.
        if sdk_int(&mut env).unwrap_or(30) < 30 {
            return Ok(true);
        }
        env.call_static_method(
            "android/os/Environment",
            "isExternalStorageManager",
            "()Z",
            &[],
        )
        .map_err(|e| format!("isExternalStorageManager: {e}"))?
        .z()
        .map_err(|e| format!("bool: {e}"))
    }

    pub fn request_all_files_access() -> Result<(), String> {
        let vm = vm()?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("attach: {e}"))?;
        let context = app_context(&mut env)?;

        // pkg = context.getPackageName()
        let pkg_obj = env
            .call_method(&context, "getPackageName", "()Ljava/lang/String;", &[])
            .map_err(|e| format!("getPackageName: {e}"))?
            .l()
            .map_err(|e| format!("pkg obj: {e}"))?;
        let pkg_jstr = JString::from(pkg_obj);
        let pkg_str: String = env
            .get_string(&pkg_jstr)
            .map_err(|e| format!("pkg str: {e}"))?
            .into();

        // uri = Uri.parse("package:<pkg>")
        let uri_arg = env
            .new_string(format!("package:{pkg_str}"))
            .map_err(|e| format!("uri arg: {e}"))?;
        let uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&uri_arg)],
            )
            .map_err(|e| format!("Uri.parse: {e}"))?
            .l()
            .map_err(|e| format!("uri obj: {e}"))?;

        // intent = new Intent(ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, uri)
        let action = env
            .new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")
            .map_err(|e| format!("action: {e}"))?;
        let intent = env
            .new_object(
                "android/content/Intent",
                "(Ljava/lang/String;Landroid/net/Uri;)V",
                &[JValue::Object(&action), JValue::Object(&uri)],
            )
            .map_err(|e| format!("new Intent: {e}"))?;

        // The Context here may not be an Activity, so the launch needs
        // FLAG_ACTIVITY_NEW_TASK.
        const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;
        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(FLAG_ACTIVITY_NEW_TASK)],
        )
        .map_err(|e| format!("addFlags: {e}"))?;
        env.call_method(
            &context,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&intent)],
        )
        .map_err(|e| format!("startActivity: {e}"))?;
        Ok(())
    }

    /// Real, app-readable directory path for every mounted storage volume
    /// (primary + SD card + auto-mounted USB-OTG), via
    /// `StorageManager.getStorageVolumes()` → `StorageVolume.getDirectory()`.
    ///
    /// Best-effort: returns whatever it can and an empty vec on any failure
    /// (older API where `getDirectory()` doesn't exist, no volumes, JNI
    /// hiccup) — the caller falls back to scanning /storage. CRUCIALLY it
    /// clears any pending Java exception before returning, so a failed call
    /// never leaves this thread faulted for later JNI use.
    ///
    /// `getStorageVolumes()` is API 24+; `getDirectory()` is API 30+ (the
    /// same floor as our all-files-access flow). On API 24–29 the first
    /// `getDirectory()` throws, we early-return [], and the /storage scan
    /// covers those devices.
    pub fn storage_volume_dirs() -> Vec<String> {
        let Ok(vm) = vm() else {
            return Vec::new();
        };
        let Ok(mut env) = vm.attach_current_thread() else {
            return Vec::new();
        };
        let Ok(context) = app_context(&mut env) else {
            return Vec::new();
        };
        let result = collect_volume_dirs(&mut env, &context);
        // Never hand the thread back with a pending exception.
        let _ = env.exception_clear();
        result.unwrap_or_default()
    }

    fn collect_volume_dirs(
        env: &mut jni::JNIEnv,
        context: &JObject,
    ) -> Result<Vec<String>, jni::errors::Error> {
        // sm = context.getSystemService(Context.STORAGE_SERVICE /* "storage" */)
        let svc = env.new_string("storage")?;
        let sm = env
            .call_method(
                context,
                "getSystemService",
                "(Ljava/lang/String;)Ljava/lang/Object;",
                &[JValue::Object(&svc)],
            )?
            .l()?;
        // List<StorageVolume> vols = sm.getStorageVolumes()
        let list = env
            .call_method(&sm, "getStorageVolumes", "()Ljava/util/List;", &[])?
            .l()?;
        let n = env.call_method(&list, "size", "()I", &[])?.i()?;
        let mut out: Vec<String> = Vec::new();
        for i in 0..n {
            let vol = env
                .call_method(&list, "get", "(I)Ljava/lang/Object;", &[JValue::Int(i)])?
                .l()?;
            // File dir = vol.getDirectory()  — null when the volume isn't
            // in a state with a usable path (unmounted, ejecting, …).
            let dir = env
                .call_method(&vol, "getDirectory", "()Ljava/io/File;", &[])?
                .l()?;
            if dir.is_null() {
                continue;
            }
            let path_obj = env
                .call_method(&dir, "getAbsolutePath", "()Ljava/lang/String;", &[])?
                .l()?;
            let s: String = env.get_string(&JString::from(path_obj))?.into();
            if !s.is_empty() {
                out.push(s);
            }
        }
        Ok(out)
    }
}
