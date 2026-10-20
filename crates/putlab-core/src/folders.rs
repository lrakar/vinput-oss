//! The two folders a program keeps its files in.

use std::path::PathBuf;

use crate::{instance, product};

/// Folder for the settings and the log of this program: `%APPDATA%\Vinput`
/// for Vinput.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("APPDATA")?)
            .join(instance::named(product::current().folder)),
    )
}

/// Folder for what this program downloads, such as models:
/// `%LOCALAPPDATA%\Vinput` for Vinput. It can be large and does not roam.
#[must_use]
pub fn local_dir() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
            .join(instance::named(product::current().folder)),
    )
}
