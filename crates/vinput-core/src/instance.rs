//! Running a second, separate Vinput beside the installed one.
//!
//! Everything by which a running Vinput is known to the rest of the PC has a
//! fixed name: the mutex that says it is running, its hidden window, the
//! pipe of the local API, and the folders of its settings and models. A copy
//! started with the environment variable [`ENV`] set to a short name puts
//! `-<name>` behind every one of them, so it neither finds nor disturbs the
//! installed copy, and keeps settings and models of its own. Such a copy
//! also never registers itself with Windows; see [`is_separate`]. And it
//! keeps to itself on the desktop: no notifications and no sounds; see
//! [`is_quiet`].
//!
//! This is a development aid, for trying a build out next to the Vinput one
//! works with. Without the variable every name is what it has always been.
//!
//! Tests are separate copies too, without being told: a test must never
//! write to the log or the settings of the Vinput its author works with,
//! nor show or play anything. `vinput-core` built for its own tests, or with
//! the feature `test-instance`, which every other crate of the workspace
//! asks for among its dev-dependencies, goes by the name [`TESTS`] when the
//! variable names nothing, or names only what cargo gives everything it
//! starts ([`CARGO`]): tests keep folders of their own, apart from those of
//! a build someone runs. Nothing that ships is built that way.

use std::sync::OnceLock;

/// The environment variable that names a separate copy.
pub const ENV: &str = "VINPUT_INSTANCE";

/// The longest name a separate copy can have.
const MAX_NAME: usize = 32;

/// The name tests run under when [`ENV`] gives them none.
pub const TESTS: &str = "tests";

/// The name `.cargo/config.toml` gives whatever cargo starts from the
/// repository when [`ENV`] is not set. Keep the two in step.
pub const CARGO: &str = "dev";

/// The name of a copy that was given none: none at all for a Vinput that
/// is run, and [`TESTS`] for one built to be tested.
const UNNAMED: Option<&str> = if cfg!(any(test, feature = "test-instance")) {
    Some(TESTS)
} else {
    None
};

/// The name of a copy started with [`ENV`] set to `given`, if it was set,
/// and called `unnamed` otherwise. A value that is not a usable name counts
/// as not set. So does [`CARGO`] for a copy that has a name of its own when
/// unnamed, which is one built to be tested: that name was not chosen by
/// anyone, cargo gave it, and tests do not share the folders of the copy a
/// developer runs.
fn chosen(given: Option<&str>, unnamed: Option<&str>) -> Option<String> {
    given
        .and_then(|name| valid(name.trim()))
        .filter(|name| unnamed.is_none() || *name != CARGO)
        .or(unnamed)
        .map(str::to_owned)
}

/// `name` if it can be the name of a separate copy: ASCII letters, digits
/// and dashes, at least one and at most [`MAX_NAME`] of them.
fn valid(name: &str) -> Option<&str> {
    let fits = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    fits.then_some(name)
}

/// `base`, with `-<name>` behind it for a separate copy.
fn with(base: &str, name: Option<&str>) -> String {
    match name {
        Some(name) => format!("{base}-{name}"),
        None => base.to_owned(),
    }
}

/// The name of this copy if it is a separate one. Read from [`ENV`] once; a
/// value that is not a usable name counts as none.
#[must_use]
pub fn name() -> Option<&'static str> {
    static NAME: OnceLock<Option<String>> = OnceLock::new();
    NAME.get_or_init(|| chosen(std::env::var(ENV).ok().as_deref(), UNNAMED))
        .as_deref()
}

/// Whether this is a separate copy. One never writes what belongs to the
/// installed Vinput: the entry that starts it with Windows, its Start menu
/// entry, its place under "Installed apps", or the program itself.
#[must_use]
pub fn is_separate() -> bool {
    name().is_some()
}

/// Whether this copy keeps to itself on the desktop: it shows no
/// notification and no error box and plays no sound, and writes what it
/// would have said to its log instead. A separate copy does, always: it is
/// there to be tried out or tested, often by a program, beside a Vinput
/// whose user must not be told of failures that are not theirs.
#[must_use]
pub fn is_quiet() -> bool {
    quiet_as(name())
}

/// The rule behind [`is_quiet`], for a copy of this name.
fn quiet_as(name: Option<&str>) -> bool {
    name.is_some()
}

/// `base` as this copy names it: unchanged, or with `-<name>` behind it for
/// a separate copy.
#[must_use]
pub fn named(base: &str) -> String {
    with(base, name())
}

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
    fn a_name_is_letters_digits_and_dashes_and_not_too_long() {
        for good in ["test", "T2", "a-b-3", &"x".repeat(MAX_NAME)] {
            assert_eq!(valid(good), Some(good));
        }
        for bad in [
            "",
            " ",
            "a b",
            "a_b",
            r"..\x",
            "a/b",
            "ime-š",
            "a\nb",
            &"x".repeat(MAX_NAME + 1),
        ] {
            assert_eq!(valid(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn every_name_gets_the_same_ending_and_none_without_a_name() {
        assert_eq!(with(crate::api::PIPE_NAME, None), r"\\.\pipe\vinput");
        assert_eq!(
            with(crate::api::PIPE_NAME, Some("test")),
            r"\\.\pipe\vinput-test"
        );
        assert_eq!(with("Vinput", Some("test")), "Vinput-test");
        assert_eq!(with("Vinput", None), "Vinput");
        assert_eq!(
            with("Local\\VinputSingleInstance", Some("b2")),
            "Local\\VinputSingleInstance-b2"
        );
    }

    #[test]
    fn a_copy_goes_by_the_name_it_was_given_or_by_none() {
        assert_eq!(chosen(None, None), None);
        assert_eq!(chosen(Some(" b2 "), None).as_deref(), Some("b2"));
        // A name that cannot be one is no name.
        assert_eq!(chosen(Some("a b"), None), None);
        assert_eq!(chosen(Some(""), None), None);
        // A copy built to be tested has a name without being given one,
        // and takes the one it is given.
        assert_eq!(chosen(None, Some(TESTS)).as_deref(), Some(TESTS));
        assert_eq!(chosen(Some("a b"), Some(TESTS)).as_deref(), Some(TESTS));
        assert_eq!(chosen(Some("s31"), Some(TESTS)).as_deref(), Some("s31"));
        // The name cargo gives everything is not one it was given: tests
        // keep to their own folders, a build that is run goes by it.
        assert_eq!(chosen(Some(CARGO), Some(TESTS)).as_deref(), Some(TESTS));
        assert_eq!(chosen(Some(CARGO), None).as_deref(), Some(CARGO));
    }

    #[test]
    fn the_name_cargo_gives_is_the_one_in_its_configuration() {
        let config = include_str!("../../../.cargo/config.toml");
        let entry = format!("VINPUT_INSTANCE = {{ value = \"{CARGO}\", force = false }}");
        assert!(config.contains(&entry), "{entry}");
    }

    #[test]
    fn only_the_one_and_only_copy_is_heard_and_seen() {
        assert!(!quiet_as(None));
        assert!(quiet_as(Some("test")));
        assert!(quiet_as(Some(TESTS)));
    }

    #[test]
    fn the_tests_themselves_never_run_as_the_installed_copy() {
        // Whatever the variable says, a test is a separate copy: its log,
        // settings and pipe are not those of the Vinput on this PC, and it
        // shows and plays nothing.
        assert_eq!(UNNAMED, Some(TESTS));
        assert!(is_separate() && is_quiet());
        assert_ne!(pipe_name(), crate::api::PIPE_NAME);
        assert_ne!(named("Vinput"), "Vinput");
        let folder = crate::settings::data_dir();
        assert!(folder.is_none_or(|folder| !folder.ends_with("Vinput")));
    }
}
