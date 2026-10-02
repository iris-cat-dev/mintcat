use std::{fs, io, path::{Path, PathBuf}};

/// This stable name also pins Tauri's Linux resource directory. Development and
/// production bundles must not rename it independently of the compiled desktop.
pub const DESKTOP_PRODUCT_NAME: &str = "MintCat";

/// Locate only layouts emitted by the release assembler: a standalone stage,
/// Windows siblings, macOS Contents/{MacOS,Resources}, or Linux usr/{bin,lib}.
/// An incomplete packaged layout is returned too, so callers fail on its missing
/// configuration instead of silently falling back to an unconfigured runtime.
pub fn packaged_resource_directory(executable: &Path) -> io::Result<Option<PathBuf>> {
    let sibling = executable.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime executable has no parent directory"))?;
    if declares_runtime(sibling)? { return Ok(Some(sibling.to_path_buf())); }
    #[cfg(target_os = "macos")]
    if sibling.file_name().is_some_and(|name| name == "MacOS") && sibling.parent().and_then(Path::file_name).is_some_and(|name| name == "Contents") {
        let resources = sibling.join("../Resources");
        if declares_runtime(&resources)? { return Ok(Some(resources)); }
    }
    #[cfg(target_os = "linux")]
    {
        // AppImage executables also resolve to APPDIR/usr/bin after current_exe;
        // no process-controlled APPDIR or unrelated global directory is needed.
        let resources = sibling.join("../lib").join(DESKTOP_PRODUCT_NAME);
        if declares_runtime(&resources)? { return Ok(Some(resources)); }
    }
    Ok(None)
}

fn declares_runtime(directory: &Path) -> io::Result<bool> {
    for name in ["release-manifest.json", "runtime.json"] {
        match fs::symlink_metadata(directory.join(name)) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {},
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_sibling_release_does_not_fall_back_to_another_baseline() {
        let root = tempfile::tempdir().unwrap();
        let sibling = root.path().join("Contents/MacOS");
        let resources = root.path().join("Contents/Resources");
        fs::create_dir_all(&sibling).unwrap();
        fs::create_dir_all(&resources).unwrap();
        fs::write(resources.join("runtime.json"), b"resource baseline").unwrap();
        fs::write(sibling.join("release-manifest.json"), b"declared sibling baseline").unwrap();
        let directory = packaged_resource_directory(&sibling.join("mintcat")).unwrap().unwrap();
        assert_eq!(directory, sibling);
        assert!(!directory.join("runtime.json").exists());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installed_cli_finds_app_resources_but_unbundled_executable_does_not() {
        let root = tempfile::tempdir().unwrap();
        let sibling = root.path().join("Contents/MacOS");
        let resources = root.path().join("Contents/Resources");
        fs::create_dir_all(&sibling).unwrap();
        fs::create_dir_all(&resources).unwrap();
        fs::write(resources.join("runtime.json"), b"authenticated configuration").unwrap();
        let directory = packaged_resource_directory(&sibling.join("mintcat")).unwrap().unwrap();
        assert_eq!(fs::canonicalize(directory).unwrap(), fs::canonicalize(resources).unwrap());
        assert!(packaged_resource_directory(&root.path().join("Contents/mintcat")).unwrap().is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cli_and_daemon_discover_the_same_installed_product_resources() {
        let root = tempfile::tempdir().unwrap();
        let sibling = root.path().join("usr/bin");
        let resources = root.path().join("usr/lib").join(DESKTOP_PRODUCT_NAME);
        fs::create_dir_all(&sibling).unwrap();
        fs::create_dir_all(&resources).unwrap();
        fs::write(resources.join("runtime.json"), b"authenticated configuration").unwrap();
        for name in ["mintcat", "mintcatd"] {
            let directory = packaged_resource_directory(&sibling.join(name)).unwrap().unwrap();
            assert_eq!(fs::canonicalize(directory).unwrap(), fs::canonicalize(&resources).unwrap());
        }
    }
}
