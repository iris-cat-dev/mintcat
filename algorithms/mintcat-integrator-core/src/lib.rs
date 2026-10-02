use std::io::{Read, Seek};

pub mod common;
pub mod progress;
pub mod uasset_utils;
pub mod validation;
pub use common::audio_pak::{
    audio_pak_filename, is_mintcat_audio_pak, sanitize_mod_name,
    verify_audio_only_from_bytes, verify_audio_only_pak_file, zip_contains_file_name,
    AUDIO_PAK_INFIX, AUDIO_PAK_SUFFIX,
};
pub use common::mod_bundle_writer::ModBundleWriter;
pub use common::unpacked_mod::UnpackedMod;
pub use mintcat_integrator_api::{
    json_value, text, InstallEvent, InstallProgress, ModInfo,
};
pub use uasset_utils::paths::{
    pak_path_to_game_path, PakPath, PakPathBuf, PakPathComponent, PakPathComponentTrait,
};
pub use validation::{is_valid_mod_directory, is_valid_zip};

pub trait ReadSeek: Read + Seek + Send {}

impl<T: Seek + Read + Send> ReadSeek for T {}
