use std::fs::File;
use zip::read::ZipArchive;

use crate::UnpackedMod;

pub fn is_valid_mod_directory(path: &str) -> bool {
    let path = std::path::Path::new(path);
    if !path.is_dir() {
        return false;
    }
    if UnpackedMod::is_valid_unpacked_mod(path) {
        return true;
    }
    path.join("pak").is_dir()
        || path.join("js").join("main.js").exists()
        || path.join("dll").is_dir()
}

pub fn is_valid_zip(path: &str) -> bool {
    match File::open(path) {
        Ok(file) => ZipArchive::new(file).is_ok(),
        Err(_) => false,
    }
}
