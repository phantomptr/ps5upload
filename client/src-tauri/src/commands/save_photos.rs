//! Remote Play screenshots and clips into the phone's photo library (Photos on iPhone and iPad,
//! the gallery on Android), so they sit with the phone's own pictures and can be shared from
//! there. The renderer falls back to a file in Downloads when this says no
//! (`rpCapture.ts::saveCapture`).
//!
//! * Android 10+: MediaStore, into Pictures/PS5Upload or Movies/PS5Upload. No permission is
//!   needed to add the app's own files. Older Android has no RELATIVE_PATH: refused.
//! * iOS 14+: PHPhotoLibrary with add-only access (the system asks once; the Info.plist's
//!   NSPhotoLibraryAddUsageDescription is the question's text). Refused access is reported as
//!   `photos_denied` so the renderer can say so.
//! * Elsewhere: refused (`unsupported`).
//!
//! The bytes ride as the raw IPC body, the name and type in percent-encoded headers, as for
//! `save_binary_file`.

use super::save_text_file::{body_bytes, percent_decode};

#[tauri::command]
pub async fn save_to_photos(request: tauri::ipc::Request<'_>) -> Result<String, String> {
    let header = |k: &str| -> String {
        request
            .headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(percent_decode)
            .unwrap_or_default()
    };
    let name = header("x-name");
    let mime = header("x-mime");
    let bytes = body_bytes(request.body())?;
    if name.trim().is_empty() || !(mime.starts_with("image/") || mime.starts_with("video/")) {
        return Err("save_to_photos: a picture or video with a name".into());
    }
    tokio::task::spawn_blocking(move || save(&name, &mime, &bytes))
        .await
        .map_err(|e| format!("save_to_photos join: {e}"))?
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn save(_name: &str, _mime: &str, _bytes: &[u8]) -> Result<String, String> {
    Err("unsupported".into())
}

#[cfg(target_os = "android")]
fn save(name: &str, mime: &str, bytes: &[u8]) -> Result<String, String> {
    android::save(name, mime, bytes)
}

#[cfg(target_os = "ios")]
fn save(name: &str, mime: &str, bytes: &[u8]) -> Result<String, String> {
    ios::save(name, mime, bytes)
}

#[cfg(target_os = "android")]
mod android {
    use crate::android_jni::{app_context, vm};
    use jni::objects::{JObject, JValue};
    use jni::JNIEnv;

    /// A Java call's error, with its pending exception cleared (a later call would abort).
    fn fail(env: &mut JNIEnv, what: &str, e: jni::errors::Error) -> String {
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        format!("{what}: {e}")
    }

    fn put_str(env: &mut JNIEnv, values: &JObject, k: &str, v: &str) -> Result<(), String> {
        let k = env.new_string(k).map_err(|e| fail(env, "key", e))?;
        let v = env.new_string(v).map_err(|e| fail(env, "value", e))?;
        env.call_method(
            values,
            "put",
            "(Ljava/lang/String;Ljava/lang/String;)V",
            &[JValue::Object(&k), JValue::Object(&v)],
        )
        .map_err(|e| fail(env, "ContentValues.put", e))?;
        Ok(())
    }

    fn put_int(env: &mut JNIEnv, values: &JObject, k: &str, v: i32) -> Result<(), String> {
        let k = env.new_string(k).map_err(|e| fail(env, "key", e))?;
        let v = env
            .call_static_method(
                "java/lang/Integer",
                "valueOf",
                "(I)Ljava/lang/Integer;",
                &[JValue::Int(v)],
            )
            .and_then(|v| v.l())
            .map_err(|e| fail(env, "Integer.valueOf", e))?;
        env.call_method(
            values,
            "put",
            "(Ljava/lang/String;Ljava/lang/Integer;)V",
            &[JValue::Object(&k), JValue::Object(&v)],
        )
        .map_err(|e| fail(env, "ContentValues.put", e))?;
        Ok(())
    }

    pub fn save(name: &str, mime: &str, bytes: &[u8]) -> Result<String, String> {
        let mut env = vm()?
            .attach_current_thread()
            .map_err(|e| format!("attach: {e}"))?;
        let sdk = env
            .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
            .and_then(|v| v.i())
            .map_err(|e| fail(&mut env, "SDK_INT", e))?;
        if sdk < 29 {
            return Err("unsupported".into());
        }
        let video = mime.starts_with("video/");
        let context = app_context(&mut env)?;
        let resolver = env
            .call_method(
                &context,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )
            .and_then(|v| v.l())
            .map_err(|e| fail(&mut env, "getContentResolver", e))?;
        let collection = env
            .get_static_field(
                if video {
                    "android/provider/MediaStore$Video$Media"
                } else {
                    "android/provider/MediaStore$Images$Media"
                },
                "EXTERNAL_CONTENT_URI",
                "Landroid/net/Uri;",
            )
            .and_then(|v| v.l())
            .map_err(|e| fail(&mut env, "EXTERNAL_CONTENT_URI", e))?;
        let values = env
            .new_object("android/content/ContentValues", "()V", &[])
            .map_err(|e| fail(&mut env, "ContentValues", e))?;
        put_str(&mut env, &values, "_display_name", name)?;
        put_str(&mut env, &values, "mime_type", mime)?;
        put_str(
            &mut env,
            &values,
            "relative_path",
            if video {
                "Movies/PS5Upload"
            } else {
                "Pictures/PS5Upload"
            },
        )?;
        // Hidden from the gallery until it is whole.
        put_int(&mut env, &values, "is_pending", 1)?;
        let uri = env
            .call_method(
                &resolver,
                "insert",
                "(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;",
                &[JValue::Object(&collection), JValue::Object(&values)],
            )
            .and_then(|v| v.l())
            .map_err(|e| fail(&mut env, "insert", e))?;
        if uri.is_null() {
            return Err("insert: no entry".into());
        }
        let written = write(&mut env, &resolver, &uri, bytes).and_then(|()| {
            let done = env
                .new_object("android/content/ContentValues", "()V", &[])
                .map_err(|e| fail(&mut env, "ContentValues", e))?;
            put_int(&mut env, &done, "is_pending", 0)?;
            env.call_method(
                &resolver,
                "update",
                "(Landroid/net/Uri;Landroid/content/ContentValues;Ljava/lang/String;[Ljava/lang/String;)I",
                &[
                    JValue::Object(&uri),
                    JValue::Object(&done),
                    JValue::Object(&JObject::null()),
                    JValue::Object(&JObject::null()),
                ],
            )
            .map_err(|e| fail(&mut env, "update", e))?;
            Ok(())
        });
        if let Err(e) = written {
            // No half file left in the gallery.
            let _ = env
                .call_method(
                    &resolver,
                    "delete",
                    "(Landroid/net/Uri;Ljava/lang/String;[Ljava/lang/String;)I",
                    &[
                        JValue::Object(&uri),
                        JValue::Object(&JObject::null()),
                        JValue::Object(&JObject::null()),
                    ],
                )
                .map_err(|e| fail(&mut env, "delete", e));
            return Err(e);
        }
        Ok(if video {
            "Movies/PS5Upload".into()
        } else {
            "Pictures/PS5Upload".into()
        })
    }

    /// The bytes into the entry, a MiB at a time (a clip can be hundreds of MB).
    fn write(
        env: &mut JNIEnv,
        resolver: &JObject,
        uri: &JObject,
        bytes: &[u8],
    ) -> Result<(), String> {
        let out = env
            .call_method(
                resolver,
                "openOutputStream",
                "(Landroid/net/Uri;)Ljava/io/OutputStream;",
                &[JValue::Object(uri)],
            )
            .and_then(|v| v.l())
            .map_err(|e| fail(env, "openOutputStream", e))?;
        if out.is_null() {
            return Err("openOutputStream: none".into());
        }
        let mut result = Ok(());
        for chunk in bytes.chunks(1 << 20) {
            let arr = match env.byte_array_from_slice(chunk) {
                Ok(a) => a,
                Err(e) => {
                    result = Err(fail(env, "byte array", e));
                    break;
                }
            };
            let r = env.call_method(&out, "write", "([B)V", &[JValue::Object(&arr)]);
            let _ = env.delete_local_ref(arr);
            if let Err(e) = r {
                result = Err(fail(env, "write", e));
                break;
            }
        }
        if let Err(e) = env.call_method(&out, "close", "()V", &[]) {
            let e = fail(env, "close", e);
            result = result.and(Err(e));
        }
        result
    }
}

#[cfg(target_os = "ios")]
mod ios {
    use block2::RcBlock;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::sync::mpsc;
    use std::sync::Mutex;
    use std::time::Duration;

    extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    }

    /// PHAuthorizationStatusAuthorized; add-only access has no "limited".
    const AUTHORIZED: isize = 3;
    /// PHAccessLevelAddOnly.
    const ADD_ONLY: isize = 1;

    /// The Photos framework's classes (the app does not link it; loading it here keeps the
    /// build untouched for everything else).
    fn class(name: &std::ffi::CStr) -> Result<&'static AnyClass, String> {
        static LOADED: std::sync::Once = std::sync::Once::new();
        LOADED.call_once(|| unsafe {
            dlopen(
                c"/System/Library/Frameworks/Photos.framework/Photos".as_ptr(),
                1,
            );
        });
        AnyClass::get(name).ok_or_else(|| format!("{} unavailable", name.to_string_lossy()))
    }

    fn ns_url(path: &str) -> Result<*mut AnyObject, String> {
        let c = CString::new(path).map_err(|_| "path has a NUL".to_string())?;
        unsafe {
            let s: *mut AnyObject =
                msg_send![class(c"NSString")?, stringWithUTF8String: c.as_ptr()];
            let url: *mut AnyObject = msg_send![class(c"NSURL")?, fileURLWithPath: s];
            Ok(url)
        }
    }

    /// Asks for add-only access once (the system remembers the answer).
    fn authorized() -> Result<bool, String> {
        let lib = class(c"PHPhotoLibrary")?;
        let (tx, rx) = mpsc::channel::<isize>();
        let tx = Mutex::new(Some(tx));
        let handler = RcBlock::new(move |status: isize| {
            if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
                let _ = tx.send(status);
            }
        });
        unsafe {
            let _: () =
                msg_send![lib, requestAuthorizationForAccessLevel: ADD_ONLY, handler: &*handler];
        }
        // The question waits on the player; give them time to read it.
        let status = rx
            .recv_timeout(Duration::from_secs(300))
            .map_err(|_| "no answer to the Photos question".to_string())?;
        Ok(status == AUTHORIZED)
    }

    pub fn save(name: &str, mime: &str, bytes: &[u8]) -> Result<String, String> {
        if !authorized()? {
            return Err("photos_denied".into());
        }
        // Photos copies from a file: write one where the system cleans up after us.
        let dir = std::env::temp_dir().join("ps5upload-photos");
        std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;
        let path = dir.join(name);
        std::fs::write(&path, bytes).map_err(|e| format!("temp file: {e}"))?;
        let result = add(&path.display().to_string(), mime.starts_with("video/"));
        let _ = std::fs::remove_file(&path);
        result.map(|()| "Photos".into())
    }

    fn add(path: &str, video: bool) -> Result<(), String> {
        let lib = class(c"PHPhotoLibrary")?;
        // Checked here: the change block cannot report a failure of its own.
        class(c"PHAssetCreationRequest")?;
        ns_url(path)?;
        let path = path.to_string();
        let change = RcBlock::new(move || {
            let (Ok(req), Ok(url)) = (class(c"PHAssetCreationRequest"), ns_url(&path)) else {
                return;
            };
            unsafe {
                let _: *mut AnyObject = if video {
                    msg_send![req, creationRequestForAssetFromVideoAtFileURL: url]
                } else {
                    msg_send![req, creationRequestForAssetFromImageAtFileURL: url]
                };
            }
        });
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        let tx = Mutex::new(Some(tx));
        let done = RcBlock::new(move |ok: Bool, err: *mut AnyObject| {
            let r = if ok.as_bool() {
                Ok(())
            } else {
                let why = if err.is_null() {
                    String::new()
                } else {
                    unsafe {
                        let d: *mut AnyObject = msg_send![err, localizedDescription];
                        let p: *const c_char = msg_send![d, UTF8String];
                        if p.is_null() {
                            String::new()
                        } else {
                            std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
                        }
                    }
                };
                Err(format!("Photos refused it: {why}"))
            };
            if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
                let _ = tx.send(r);
            }
        });
        unsafe {
            let shared: *mut AnyObject = msg_send![lib, sharedPhotoLibrary];
            let _: () = msg_send![shared, performChanges: &*change, completionHandler: &*done];
        }
        // A long clip takes a while to copy in.
        rx.recv_timeout(Duration::from_secs(600))
            .map_err(|_| "Photos did not answer".to_string())?
    }
}
