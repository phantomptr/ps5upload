//! A game folder from a game image or a debug-key package (#469): the reverse of Convert's
//! image and package builds.
//!
//! The build code is `ps5upload_convert_core::folder_build`, shared with the converter that runs
//! on the PS5, and is re-exported here. What stays in the engine: writing the folder to a saved
//! server (`server_out`), which the core reaches through [`RemoteFolder`].

pub(crate) use ps5upload_convert_core::folder_build::*;

impl RemoteFolder for crate::server_out::ServerFolder<'_> {
    fn write_folder(
        &self,
        tree: &mut dyn ps5upload_fpkg::source::SourceTree,
        cancel: &std::sync::atomic::AtomicBool,
        stage: &mut dyn FnMut(&str, u64, u64),
        check: &mut Check,
    ) -> Result<FolderBuilt, String> {
        crate::server_out::write_folder(tree, self, cancel, stage, check)
    }

    fn describe(&self) -> String {
        self.dest.url(&self.dest.path_of(&self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let root =
            crate::test_tmp::temp_path(format!("ps5upload-fold-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn only_a_marked_partial_folder_is_ours_and_swept() {
        let root = scratch("own");
        let ours = root.join("A.partial");
        let theirs = root.join("B.partial");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(ours.join(MARKER), b"").unwrap();
        assert!(is_our_partial_dir(&ours));
        assert!(!is_our_partial_dir(&theirs));
        // The output folder's sweep removes ours, unless a running build is writing it.
        let active = root.join("C.partial");
        std::fs::create_dir_all(&active).unwrap();
        std::fs::write(active.join(MARKER), b"").unwrap();
        let writing = crate::fpkg_api::Writing::claim(active.clone()).unwrap();
        std::fs::create_dir_all(&active).unwrap();
        std::fs::write(active.join(MARKER), b"").unwrap();
        crate::fpkg_api::sweep_output_leftovers(&root);
        assert!(!ours.exists());
        assert!(theirs.is_dir() && active.is_dir());
        // A second build of the same name is refused while the first runs.
        assert!(crate::fpkg_api::Writing::claim(active.clone()).is_none());
        drop(writing);
        crate::fpkg_api::sweep_output_leftovers(&root);
        assert!(!active.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
