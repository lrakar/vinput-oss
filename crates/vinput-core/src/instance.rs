//! Running a second, separate Vinput beside the installed one: the names of
//! a copy are [`putlab_core::instance`], which every program of the family
//! shares, and the one name only Vinput has, the pipe of its local API.

pub use putlab_core::instance::{CARGO, ENV, TESTS, is_quiet, is_separate, name, named};

/// The pipe of the local API of this copy: [`crate::api::PIPE_NAME`] unless
/// this is a separate copy.
#[must_use]
pub fn pipe_name() -> String {
    named(crate::api::PIPE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tests_themselves_never_talk_to_the_installed_copy() {
        assert!(is_separate() && is_quiet());
        assert_ne!(pipe_name(), crate::api::PIPE_NAME);
        let folder = crate::settings::data_dir();
        assert!(folder.is_none_or(|folder| !folder.ends_with("Vinput")));
    }
}
