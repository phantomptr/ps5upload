//! Live attach/detach of a real disk image.
//!
//! Touches the host OS (hdiutil / udisksctl) and mounts a volume, so it
//! is `#[ignore]`d. Point it at a real image and run:
//!
//!   PS5UPLOAD_TEST_IMAGE=/path/to.exfat \
//!     cargo test -p ps5upload-core --test live_local_image -- --ignored --nocapture
//!
//! The parsing is unit-tested; what this proves is the part unit tests
//! cannot: that the commands we build actually attach a real image, that
//! the device we parse is the one detach accepts, and that we leave
//! nothing behind.

use ps5upload_core::local_image::{attach, detach, status, unsupported_reason};

fn image() -> Option<String> {
    std::env::var("PS5UPLOAD_TEST_IMAGE")
        .ok()
        .filter(|s| !s.is_empty())
}

#[test]
#[ignore = "attaches a real disk image (set PS5UPLOAD_TEST_IMAGE)"]
fn live_attach_then_detach_leaves_nothing_behind() {
    if let Some(why) = unsupported_reason() {
        println!("skipping: {why}");
        return;
    }
    let Some(img) = image() else {
        panic!("set PS5UPLOAD_TEST_IMAGE to a real image path");
    };

    let a = attach(&img).expect("attach failed");
    println!(
        "attached {} -> {} at {:?}",
        a.image, a.device, a.mount_point
    );
    assert!(a.device.starts_with("/dev/"), "device: {}", a.device);
    assert_eq!(status().len(), 1, "attach must be tracked");

    // Attaching the same image again hands back the same device rather
    // than creating a second one.
    let again = attach(&img).expect("second attach failed");
    assert_eq!(again.device, a.device, "must not attach twice");
    assert_eq!(status().len(), 1);

    detach(&a.device).expect("detach failed");
    assert!(status().is_empty(), "detach must forget the image");

    // Detaching again must not error — the end state is what matters.
    let _ = detach(&a.device);
}

#[test]
#[ignore = "attaches a real disk image (set PS5UPLOAD_TEST_IMAGE)"]
fn live_refuses_a_path_that_is_not_an_image_file() {
    if unsupported_reason().is_some() {
        return;
    }
    assert!(attach("/definitely/not/here.exfat").is_err());
}
