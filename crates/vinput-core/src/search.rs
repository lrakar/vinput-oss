//! Dictate and search: turns what was said into the address of a web search,
//! which the host opens in the default browser.

/// The search the settings start with. `%s` stands for what was said.
pub const DEFAULT_TEMPLATE: &str = "https://www.google.com/search?q=%s";

/// Where what was said goes in a search address.
const PLACEHOLDER: &str = "%s";

/// The longest address handed to the browser. Windows itself stops a little
/// above 2000 characters, and nobody dictates a longer search on purpose.
const MAX_URL: usize = 2000;

/// Whether `template` can be used as a search address: a plain web address.
/// Anything else is refused, because the address is handed to Windows to open
/// and must never name a program or a file.
#[must_use]
pub fn is_valid(template: &str) -> bool {
    let rest = template
        .strip_prefix("https://")
        .or_else(|| template.strip_prefix("http://"));
    rest.is_some_and(|rest| {
        !rest.is_empty()
            && !rest.starts_with('/')
            && template
                .chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, '"' | '<' | '>' | '\\' | '^' | '`'))
    })
}

/// The address that searches for `said` with `template`, or `None` if nothing
/// was said. The words replace `%s` in the template, or follow it if it has
/// none; a template that is not a web address gives way to the default.
#[must_use]
pub fn url(template: &str, said: &str) -> Option<String> {
    let query = tidy(said);
    if query.is_empty() {
        return None;
    }
    let template = if is_valid(template) {
        template
    } else {
        DEFAULT_TEMPLATE
    };
    let (before, after) = template.split_once(PLACEHOLDER).unwrap_or((template, ""));
    let after = after.replace(PLACEHOLDER, "");
    let room = MAX_URL.saturating_sub(before.len() + after.len());
    Some(format!("{before}{}{after}", encode(&query, room)))
}

/// What was said as a search: on one line, single spaces, and without the full
/// stop a speech model likes to end a sentence with.
fn tidy(said: &str) -> String {
    let line = said.split_whitespace().collect::<Vec<_>>().join(" ");
    line.trim_end_matches(['.', '\u{3002}'])
        .trim_end()
        .to_owned()
}

/// Percent-encodes `text` for a query, stopping at a whole character before
/// the result would be longer than `room`.
fn encode(text: &str, room: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::new();
    let mut bytes = [0u8; 4];
    for c in text.chars() {
        let start = encoded.len();
        for &byte in c.encode_utf8(&mut bytes).as_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(byte));
            } else {
                encoded.push('%');
                encoded.push(char::from(HEX[usize::from(byte >> 4)]));
                encoded.push(char::from(HEX[usize::from(byte & 15)]));
            }
        }
        if encoded.len() > room {
            encoded.truncate(start);
            break;
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_said_becomes_a_search_address() {
        assert_eq!(
            url(DEFAULT_TEMPLATE, " Weather in  Ljubljana.\n").as_deref(),
            Some("https://www.google.com/search?q=Weather%20in%20Ljubljana")
        );
        assert_eq!(
            url("https://duckduckgo.com/?q=%s&ia=web", "c++ & rust?").as_deref(),
            Some("https://duckduckgo.com/?q=c%2B%2B%20%26%20rust%3F&ia=web")
        );
        assert_eq!(
            url("https://www.bing.com/search?q=", "čaj").as_deref(),
            Some("https://www.bing.com/search?q=%C4%8Daj")
        );
    }

    #[test]
    fn silence_searches_for_nothing() {
        assert_eq!(url(DEFAULT_TEMPLATE, ""), None);
        assert_eq!(url(DEFAULT_TEMPLATE, " . \n"), None);
    }

    #[test]
    fn only_web_addresses_are_opened() {
        for bad in [
            "",
            "calc.exe",
            r"C:\Windows\System32\cmd.exe /c %s",
            "file:///C:/x?%s",
            "javascript:alert(%s)",
            "https://",
            "https:///x?q=%s",
            "https://example.com/?q=%s \"& calc",
        ] {
            assert!(!is_valid(bad), "{bad:?}");
            assert_eq!(
                url(bad, "tea").as_deref(),
                Some("https://www.google.com/search?q=tea"),
                "{bad:?}"
            );
        }
        assert!(is_valid(DEFAULT_TEMPLATE));
        assert!(is_valid("http://intranet/search?text=%s"));
    }

    #[test]
    fn a_long_search_is_cut_at_a_whole_character() {
        let address = url(DEFAULT_TEMPLATE, &"š".repeat(5000)).unwrap_or_default();
        assert!(address.len() <= MAX_URL && address.len() > MAX_URL - 6);
        assert!(address.ends_with("%C5%A1"));
    }
}
