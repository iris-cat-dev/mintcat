//! 内联 uasset_utils（原 trumank/uasset_utils），含 UE 5.6 AssetRegistry 兼容

pub mod asset_registry;
pub mod paths;
pub use asset_registry::get_root_export;

pub mod splice;
