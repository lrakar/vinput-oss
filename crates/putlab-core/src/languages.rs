//! The languages Windows is set to for the signed-in user.
//!
//! Windows keeps the list from "Language & region" as codes such as
//! `sl-SI`, the most wanted first. A program that asks which languages its
//! user speaks offers those before all the others, so that most people find
//! theirs without looking.

use std::sync::OnceLock;

use crate::registry;

/// Where Windows keeps the list.
const KEY: &str = r"Control Panel\International\User Profile";
/// The value that holds it.
const VALUE: &str = "Languages";
/// More than this many are not handed on; nobody is set to that many.
const MOST: usize = 8;

/// The languages Windows is set to, the most wanted first, as they were
/// when this was first asked. Empty where Windows does not say.
#[must_use]
pub fn preferred() -> &'static [String] {
    static FOUND: OnceLock<Vec<String>> = OnceLock::new();
    FOUND.get_or_init(|| tidy(registry::read_list(KEY, VALUE)))
}

/// The codes among `listed` that read as codes, each once, in their order.
fn tidy(listed: Vec<String>) -> Vec<String> {
    let mut codes: Vec<String> = Vec::new();
    for code in listed {
        let code = code.trim();
        let plain = !code.is_empty()
            && code.len() <= 35
            && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if plain && !codes.iter().any(|known| known.eq_ignore_ascii_case(code)) {
            codes.push(code.to_owned());
        }
        if codes.len() == MOST {
            break;
        }
    }
    codes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|code| (*code).to_owned()).collect()
    }

    #[test]
    fn the_list_is_handed_on_in_its_order_without_what_is_no_code() {
        assert_eq!(
            tidy(listed(&[
                "sl-SI",
                " en-US ",
                "",
                "SL-si",
                "en US",
                "sr-Latn-RS"
            ])),
            ["sl-SI", "en-US", "sr-Latn-RS"]
        );
        assert!(tidy(Vec::new()).is_empty());
        let many: Vec<String> = (0..20).map(|number| format!("x{number}")).collect();
        assert_eq!(tidy(many).len(), MOST);
    }

    #[test]
    fn asking_windows_never_fails() {
        // Whatever this PC is set to, the answer is a short list of codes.
        let codes = preferred();
        assert!(codes.len() <= MOST);
        assert!(codes.iter().all(|code| !code.is_empty()));
    }
}
