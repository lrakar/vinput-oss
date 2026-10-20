//! What the PUTLAB hub shows: the programs of the family, which of them are
//! on this PC, and what can be done with each.
//!
//! The account is not here: it is the same [`crate::Pro`] every program of
//! the family shows, because it is the same sign-in.

use serde::{Deserialize, Serialize};

/// Everything the Products page shows.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Hub {
    /// Where the list of programs stands.
    pub catalog: Catalog,
    /// The programs, in the order to list them.
    pub products: Vec<Product>,
}

/// Where the list of programs on offer stands. What is installed on this PC
/// is known without it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Catalog {
    /// It is being fetched.
    #[default]
    Loading,
    /// It is here.
    Ready,
    /// It could not be fetched.
    Failed {
        /// Why, in words for the user.
        reason: String,
    },
}

/// One program of the family.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Product {
    /// What [`HubAsk`] names it by.
    pub id: String,
    /// Its name.
    pub name: String,
    /// One line about what it does.
    pub summary: String,
    /// The version on this PC, if it is installed.
    pub installed: Option<String>,
    /// The newest version on offer, once the catalog and its release are
    /// known.
    pub latest: Option<String>,
    /// Bytes the download of the newest version takes.
    pub bytes: u64,
    /// Whether it is announced and cannot be installed yet.
    pub soon: bool,
    /// What is being done with it.
    pub state: ProductState,
}

impl Product {
    /// Whether a newer version than the installed one is on offer.
    #[must_use]
    pub fn has_update(&self) -> bool {
        match (&self.installed, &self.latest) {
            (Some(installed), Some(latest)) => newer(latest, installed),
            _ => false,
        }
    }
}

/// Whether version `candidate` is newer than `current`, by their three
/// numbers. Anything that is not three numbers is not newer.
fn newer(candidate: &str, current: &str) -> bool {
    fn numbers(version: &str) -> Option<[u32; 3]> {
        let mut parts = version.trim().trim_start_matches('v').split('.');
        let mut numbers = [0u32; 3];
        for number in &mut numbers {
            *number = parts.next()?.parse().ok()?;
        }
        parts.next().is_none().then_some(numbers)
    }
    match (numbers(candidate), numbers(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// What is being done with a program.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProductState {
    /// Nothing.
    #[default]
    Idle,
    /// Its setup program is being downloaded.
    Downloading {
        /// Bytes so far.
        done: u64,
        /// Bytes in total.
        total: u64,
    },
    /// Its setup program is running.
    Installing,
    /// It is being removed.
    Removing,
    /// The last thing asked of it failed.
    Failed {
        /// Why, in words for the user.
        reason: String,
    },
}

/// What the user asks of the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HubAsk {
    /// Look again at what is on offer and what is installed.
    Refresh,
    /// Download and install this program, or its newer version.
    Install(String),
    /// Stop the download of this program.
    Cancel(String),
    /// Start this program, or bring up its settings if it is running.
    Open(String),
    /// Remove this program from the PC.
    Uninstall(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_is_a_newer_version_than_the_installed_one() {
        let product = |installed: Option<&str>, latest: Option<&str>| Product {
            installed: installed.map(str::to_owned),
            latest: latest.map(str::to_owned),
            ..Product::default()
        };
        assert!(product(Some("0.4.8"), Some("0.4.10")).has_update());
        assert!(!product(Some("0.4.8"), Some("0.4.8")).has_update());
        assert!(!product(Some("0.5.0"), Some("0.4.8")).has_update());
        assert!(!product(None, Some("0.4.8")).has_update());
        assert!(!product(Some("0.4.8"), None).has_update());
        assert!(!product(Some("0.4.8"), Some("newest")).has_update());
    }
}
