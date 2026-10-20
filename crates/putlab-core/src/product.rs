//! Which program of the family this process is.
//!
//! The code in this crate is shared by Vinput and the programs beside it.
//! What differs between them is a handful of names: the folder of their
//! settings, the file of their log, where their updates are published. A
//! program says once, first thing, which one it is ([`set`]); a program that
//! says nothing is Vinput, whose names are what they have always been.

use std::sync::OnceLock;

/// The names of one program.
#[derive(Debug, PartialEq, Eq)]
pub struct Product {
    /// The name of its folders under `%APPDATA%` and `%LOCALAPPDATA%`.
    pub folder: &'static str,
    /// The file name of its log, in the first of those folders.
    pub log: &'static str,
    /// What its network requests say they are from.
    pub agent: &'static str,
    /// Its version, such as `1.2.3`.
    pub version: &'static str,
    /// Where the manifest of its newest release is published.
    pub manifest: &'static str,
    /// What the name of its setup program begins with; `-<version>.exe`
    /// follows.
    pub setup: &'static str,
}

/// Vinput.
pub static VINPUT: Product = Product {
    folder: "Vinput",
    log: "vinput.log",
    agent: concat!("vinput/", env!("CARGO_PKG_VERSION")),
    version: env!("CARGO_PKG_VERSION"),
    manifest: "https://vinput-dasvcr.web.app/releases/latest.json",
    setup: "VinputSetup",
};

static CURRENT: OnceLock<&'static Product> = OnceLock::new();

/// Says which program this process is. Called once, before anything else of
/// this crate is used; a later call changes nothing.
pub fn set(product: &'static Product) {
    let _ = CURRENT.set(product);
}

/// The program this process is: what [`set`] said, and Vinput if nothing did.
#[must_use]
pub fn current() -> &'static Product {
    CURRENT.get().copied().unwrap_or(&VINPUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_that_says_nothing_is_vinput_by_the_names_it_always_had() {
        let product = current();
        assert_eq!(product.folder, "Vinput");
        assert_eq!(product.log, "vinput.log");
        assert_eq!(product.setup, "VinputSetup");
        assert!(product.agent.starts_with("vinput/"));
        assert_eq!(
            product.manifest,
            "https://vinput-dasvcr.web.app/releases/latest.json"
        );
    }
}
