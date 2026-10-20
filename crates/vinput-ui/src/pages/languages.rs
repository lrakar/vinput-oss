//! The languages dictation can be set to, and the row that picks them.
//!
//! The settings hold codes such as `en-US, sl-SI`. Nobody should have to
//! know those, so the row shows each chosen language as a chip with its own
//! name on it, and under the chips a list to search, in which every
//! language stands under the name its speakers call it by, with its English
//! name beside it. The languages Windows is set to come first in that list.
//! A code the list does not have, written into the settings file by hand,
//! is kept and shown as it is.

use vinput_view::{Change, Command};

use crate::node::{
    Action, Expander, Icon, Node, Pickers, Search, Tone, chip, expander, flow, header, note,
    plain_setting,
};
use crate::theme::metric;

/// A language: its code, its name in English, and what its speakers call it.
type Language = (&'static str, &'static str, &'static str);

/// The languages on offer, by their English names.
const LANGUAGES: &[Language] = &[
    ("af-ZA", "Afrikaans", "Afrikaans"),
    ("sq-AL", "Albanian", "Shqip"),
    (
        "ar-SA",
        "Arabic",
        "\u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629}",
    ),
    (
        "hy-AM",
        "Armenian",
        "\u{540}\u{561}\u{575}\u{565}\u{580}\u{565}\u{576}",
    ),
    ("az-AZ", "Azerbaijani", "Az\u{259}rbaycanca"),
    ("eu-ES", "Basque", "Euskara"),
    (
        "be-BY",
        "Belarusian",
        "\u{411}\u{435}\u{43b}\u{430}\u{440}\u{443}\u{441}\u{43a}\u{430}\u{44f}",
    ),
    ("bn-BD", "Bengali", "\u{9ac}\u{9be}\u{982}\u{9b2}\u{9be}"),
    ("bs-BA", "Bosnian", "Bosanski"),
    (
        "bg-BG",
        "Bulgarian",
        "\u{411}\u{44a}\u{43b}\u{433}\u{430}\u{440}\u{441}\u{43a}\u{438}",
    ),
    ("ca-ES", "Catalan", "Catal\u{e0}"),
    (
        "zh-CN",
        "Chinese (Simplified)",
        "\u{4e2d}\u{6587}\u{ff08}\u{7b80}\u{4f53}\u{ff09}",
    ),
    (
        "zh-TW",
        "Chinese (Traditional)",
        "\u{4e2d}\u{6587}\u{ff08}\u{7e41}\u{9ad4}\u{ff09}",
    ),
    ("hr-HR", "Croatian", "Hrvatski"),
    ("cs-CZ", "Czech", "\u{10c}e\u{161}tina"),
    ("da-DK", "Danish", "Dansk"),
    ("nl-NL", "Dutch", "Nederlands"),
    (
        "en-US",
        "English (United States)",
        "English (United States)",
    ),
    (
        "en-GB",
        "English (United Kingdom)",
        "English (United Kingdom)",
    ),
    ("et-EE", "Estonian", "Eesti"),
    ("fil-PH", "Filipino", "Filipino"),
    ("fi-FI", "Finnish", "Suomi"),
    ("fr-FR", "French", "Fran\u{e7}ais"),
    ("fr-CA", "French (Canada)", "Fran\u{e7}ais (Canada)"),
    ("gl-ES", "Galician", "Galego"),
    (
        "ka-GE",
        "Georgian",
        "\u{10e5}\u{10d0}\u{10e0}\u{10d7}\u{10e3}\u{10da}\u{10d8}",
    ),
    ("de-DE", "German", "Deutsch"),
    (
        "el-GR",
        "Greek",
        "\u{395}\u{3bb}\u{3bb}\u{3b7}\u{3bd}\u{3b9}\u{3ba}\u{3ac}",
    ),
    (
        "gu-IN",
        "Gujarati",
        "\u{a97}\u{ac1}\u{a9c}\u{ab0}\u{abe}\u{aa4}\u{ac0}",
    ),
    ("he-IL", "Hebrew", "\u{5e2}\u{5d1}\u{5e8}\u{5d9}\u{5ea}"),
    (
        "hi-IN",
        "Hindi",
        "\u{939}\u{93f}\u{928}\u{94d}\u{926}\u{940}",
    ),
    ("hu-HU", "Hungarian", "Magyar"),
    ("is-IS", "Icelandic", "\u{cd}slenska"),
    ("id-ID", "Indonesian", "Bahasa Indonesia"),
    ("it-IT", "Italian", "Italiano"),
    ("ja-JP", "Japanese", "\u{65e5}\u{672c}\u{8a9e}"),
    ("kn-IN", "Kannada", "\u{c95}\u{ca8}\u{ccd}\u{ca8}\u{ca1}"),
    (
        "kk-KZ",
        "Kazakh",
        "\u{49a}\u{430}\u{437}\u{430}\u{49b}\u{448}\u{430}",
    ),
    ("ko-KR", "Korean", "\u{d55c}\u{ad6d}\u{c5b4}"),
    ("lv-LV", "Latvian", "Latvie\u{161}u"),
    ("lt-LT", "Lithuanian", "Lietuvi\u{173}"),
    (
        "mk-MK",
        "Macedonian",
        "\u{41c}\u{430}\u{43a}\u{435}\u{434}\u{43e}\u{43d}\u{441}\u{43a}\u{438}",
    ),
    ("ms-MY", "Malay", "Bahasa Melayu"),
    (
        "ml-IN",
        "Malayalam",
        "\u{d2e}\u{d32}\u{d2f}\u{d3e}\u{d33}\u{d02}",
    ),
    ("mr-IN", "Marathi", "\u{92e}\u{930}\u{93e}\u{920}\u{940}"),
    ("nb-NO", "Norwegian", "Norsk"),
    ("fa-IR", "Persian", "\u{641}\u{627}\u{631}\u{633}\u{6cc}"),
    ("pl-PL", "Polish", "Polski"),
    ("pt-PT", "Portuguese", "Portugu\u{ea}s"),
    ("pt-BR", "Portuguese (Brazil)", "Portugu\u{ea}s (Brasil)"),
    (
        "pa-IN",
        "Punjabi",
        "\u{a2a}\u{a70}\u{a1c}\u{a3e}\u{a2c}\u{a40}",
    ),
    ("ro-RO", "Romanian", "Rom\u{e2}n\u{103}"),
    (
        "ru-RU",
        "Russian",
        "\u{420}\u{443}\u{441}\u{441}\u{43a}\u{438}\u{439}",
    ),
    (
        "sr-RS",
        "Serbian",
        "\u{421}\u{440}\u{43f}\u{441}\u{43a}\u{438}",
    ),
    ("sk-SK", "Slovak", "Sloven\u{10d}ina"),
    ("sl-SI", "Slovenian", "Sloven\u{161}\u{10d}ina"),
    ("es-ES", "Spanish", "Espa\u{f1}ol"),
    ("es-MX", "Spanish (Mexico)", "Espa\u{f1}ol (M\u{e9}xico)"),
    ("sw-KE", "Swahili", "Kiswahili"),
    ("sv-SE", "Swedish", "Svenska"),
    ("ta-IN", "Tamil", "\u{ba4}\u{bae}\u{bbf}\u{bb4}\u{bcd}"),
    (
        "te-IN",
        "Telugu",
        "\u{c24}\u{c46}\u{c32}\u{c41}\u{c17}\u{c41}",
    ),
    ("th-TH", "Thai", "\u{e44}\u{e17}\u{e22}"),
    ("tr-TR", "Turkish", "T\u{fc}rk\u{e7}e"),
    (
        "uk-UA",
        "Ukrainian",
        "\u{423}\u{43a}\u{440}\u{430}\u{457}\u{43d}\u{441}\u{44c}\u{43a}\u{430}",
    ),
    ("ur-PK", "Urdu", "\u{627}\u{631}\u{62f}\u{648}"),
    ("vi-VN", "Vietnamese", "Ti\u{1ebf}ng Vi\u{1ec7}t"),
    ("cy-GB", "Welsh", "Cymraeg"),
];

/// What the row is called.
pub(super) const TITLE: &str = "Languages";
/// What `VinPUT` Pro can do for someone who has named one language.
pub(super) const MORE: &str = "Speak more than one language? Add each of them, and VinPUT Pro \
                               follows you when you switch.";

/// The part of a code that names the language: `sl` of `sl-SI`.
fn spoken(code: &str) -> String {
    let language = code.split(['-', '_']).next().unwrap_or_default();
    language.trim().to_ascii_lowercase()
}

/// The language `code` stands for: the one with that very code, or failing
/// that the first one of the same language, so that `en` and `en-AU` have a
/// name too.
fn known(code: &str) -> Option<&'static Language> {
    let code = code.trim();
    let exact = LANGUAGES
        .iter()
        .find(|(known, ..)| known.eq_ignore_ascii_case(code));
    exact.or_else(|| {
        let language = spoken(code);
        LANGUAGES
            .iter()
            .find(|(known, ..)| spoken(known) == language)
    })
}

/// What a chip says for `code`: the language as its speakers call it, or
/// the code itself for one that is not on the list.
fn name(code: &str) -> String {
    match known(code) {
        // A region the list does not have is said with its code, so that
        // two regions of one language can be told apart.
        Some((listed, _, native)) if !listed.eq_ignore_ascii_case(code.trim()) => {
            let short = native.split(" (").next().unwrap_or(native);
            match code.trim().split_once(['-', '_']) {
                Some((_, region)) => format!("{short} ({})", region.to_ascii_uppercase()),
                None => short.to_owned(),
            }
        }
        Some((_, _, native)) => (*native).to_owned(),
        None => code.trim().to_owned(),
    }
}

/// The codes in `list`, which is what the settings hold: `en-US, sl-SI`.
fn codes(list: &str) -> Vec<&str> {
    list.split(',')
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .collect()
}

/// How many languages `list` names. Two regions of one language, such as
/// en-US and en-GB, are one.
pub(super) fn count(list: &str) -> usize {
    let mut languages: Vec<String> = codes(list).into_iter().map(spoken).collect();
    languages.retain(|language| !language.is_empty());
    languages.sort_unstable();
    languages.dedup();
    languages.len()
}

/// What changing the languages to `codes` does.
fn set(codes: &[&str]) -> Action {
    Action::send(Command::Set(Change::Languages(codes.join(", "))))
}

/// The row of a language in the list: its own name, its English name, and
/// a tick while it is chosen. A click adds it, or takes it away again
/// unless it is the only one.
fn language_row(language: &Language, chosen: &[&str]) -> (String, Node) {
    let (code, english, native) = *language;
    let at = chosen
        .iter()
        .position(|chosen| chosen.eq_ignore_ascii_case(code));
    let action = match at {
        Some(_) if chosen.len() == 1 => Action::None,
        Some(at) => {
            let mut left = chosen.to_vec();
            left.remove(at);
            set(&left)
        }
        None => {
            let mut more = chosen.to_vec();
            more.push(code);
            set(&more)
        }
    };
    let mut row = plain_setting(native).compact().action(action);
    if native != english {
        row = row.detail(english);
    }
    if at.is_some() {
        row = row.control(Node::Icon(Icon::Check, Tone::Accent));
    }
    (format!("{native} {english} {code}"), row.into())
}

/// The row that picks the languages of dictation. `list` is what the
/// settings hold, `windows` the languages Windows is set to, most wanted
/// first, and `more` says whether to add the line about speaking several.
pub(super) fn picker(list: &str, windows: &[String], more: bool) -> Expander {
    let chosen = codes(list);
    let chips = chosen.iter().enumerate().map(|(at, code)| {
        // The last one stays: dictation needs a language.
        let remove = (chosen.len() > 1).then(|| {
            let mut left = chosen.clone();
            left.remove(at);
            set(&left)
        });
        chip(name(code), remove)
    });
    let mut suggested: Vec<&Language> = Vec::new();
    for code in windows {
        if let Some(language) = known(code)
            && !suggested.contains(&language)
        {
            suggested.push(language);
        }
    }
    let mut rows = Vec::with_capacity(LANGUAGES.len() + 2);
    if !suggested.is_empty() {
        // A heading is found by nothing, so a search shows only languages.
        rows.push((String::new(), header("From Windows")));
        rows.extend(
            suggested
                .iter()
                .map(|language| language_row(language, &chosen)),
        );
        rows.push((String::new(), header("All languages")));
    }
    let rest = LANGUAGES
        .iter()
        .filter(|language| !suggested.contains(language));
    rows.extend(rest.map(|language| language_row(language, &chosen)));
    let mut inside = vec![flow(chips)];
    if more {
        inside.push(note(MORE));
    }
    inside.push(
        Search {
            key: "language-search",
            placeholder: "Search languages".to_owned(),
            max: metric::LIST_MAX,
            rows,
            none: "No language by that name".to_owned(),
            pickers: Pickers,
        }
        .into(),
    );
    let names: Vec<String> = chosen.iter().map(|code| name(code)).collect();
    expander("languages", Icon::Globe, TITLE, inside).value(names.join(", "))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::super::common::probe::{Seen, all, set as change};
    use super::*;

    fn windows(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|code| (*code).to_owned()).collect()
    }

    fn languages(list: &str) -> Change {
        Change::Languages(list.to_owned())
    }

    #[test]
    fn every_language_has_one_code_and_both_names() {
        let codes: HashSet<_> = LANGUAGES.iter().map(|(code, ..)| *code).collect();
        assert_eq!(codes.len(), LANGUAGES.len());
        for (code, english, native) in LANGUAGES {
            assert!(code.contains('-') && !english.is_empty() && !native.is_empty());
        }
        let names: Vec<_> = LANGUAGES.iter().map(|(_, english, _)| *english).collect();
        assert!(names.is_sorted_by(|a, b| a.split(" (").next() <= b.split(" (").next()));
    }

    #[test]
    fn a_code_is_shown_by_the_name_its_speakers_use() {
        assert_eq!(name("sl-SI"), "Sloven\u{161}\u{10d}ina");
        assert_eq!(name(" SL-si "), "Sloven\u{161}\u{10d}ina");
        assert_eq!(name("en-US"), "English (United States)");
        // A region or a bare language the list does not have still has a name.
        assert_eq!(name("en-AU"), "English (AU)");
        assert_eq!(name("de"), "Deutsch");
        // And what is no language of the list is shown as it was written.
        assert_eq!(name("tlh"), "tlh");
    }

    #[test]
    fn languages_are_counted_by_what_is_spoken() {
        assert_eq!(count(""), 0);
        assert_eq!(count(" , "), 0);
        assert_eq!(count("en-US"), 1);
        assert_eq!(count("en-US, EN_gb,en"), 1);
        assert_eq!(count("en-US, sl-SI, de"), 3);
    }

    #[test]
    fn the_closed_row_says_what_is_chosen() {
        let row = picker("en-US, sl-SI", &[], false);
        assert_eq!(row.title, "Languages");
        assert_eq!(
            row.value,
            "English (United States), Sloven\u{161}\u{10d}ina"
        );
        assert!(!row.open);
    }

    #[test]
    fn a_chip_takes_its_language_away_and_the_last_one_stays() {
        let row = picker("en-US, sl-SI, tlh", &[], false);
        let seen = Seen::of(&row.children);
        let removes: Vec<_> = seen
            .presses
            .iter()
            .map(|press| (press.label.as_str(), change(&press.action)))
            .collect();
        assert_eq!(
            removes,
            [
                (
                    "Remove English (United States)",
                    Some(languages("sl-SI, tlh"))
                ),
                (
                    "Remove Sloven\u{161}\u{10d}ina",
                    Some(languages("en-US, tlh"))
                ),
                ("Remove tlh", Some(languages("en-US, sl-SI"))),
            ]
        );
        let alone = picker("sl-SI", &[], false);
        assert!(Seen::of(&alone.children).presses.is_empty());
    }

    #[test]
    fn a_row_of_the_list_adds_its_language_or_takes_it_away() {
        let row = picker("en-US, sl-SI", &[], false);
        let seen = Seen::of(&row.children);
        let click = |title: &str| {
            let row = seen.rows.iter().find(|row| row.label == title);
            row.and_then(|row| change(&row.action))
        };
        assert_eq!(click("Deutsch"), Some(languages("en-US, sl-SI, de-DE")));
        assert_eq!(click("Sloven\u{161}\u{10d}ina"), Some(languages("en-US")));
        // Every language is there, and is found by either name or its code.
        let Some(Node::Search(search)) = row.children.last() else {
            unreachable!("the list was expected last");
        };
        assert_eq!(search.rows.len(), LANGUAGES.len());
        let german = search
            .rows
            .iter()
            .find(|(words, _)| words.contains("de-DE"));
        assert!(
            german.is_some_and(|(words, _)| words.contains("German") && words.contains("Deutsch"))
        );
        // The only language cannot be clicked away.
        let alone = picker("sl-SI", &[], false);
        assert!(
            Seen::of(&alone.children)
                .rows
                .iter()
                .all(|row| row.label != "Sloven\u{161}\u{10d}ina")
        );
    }

    #[test]
    fn the_languages_of_windows_come_first() {
        let row = picker("en-US", &windows(&["sl-SI", "en-GB", "sl", "tlh"]), false);
        let nodes = all(&row.children);
        let titles: Vec<&str> = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Header(text) => Some(text.as_str()),
                Node::Setting(row) => Some(row.title.as_str()),
                _ => None,
            })
            .take(5)
            .collect();
        assert_eq!(
            titles,
            [
                "From Windows",
                "Sloven\u{161}\u{10d}ina",
                "English (United Kingdom)",
                "All languages",
                "Afrikaans",
            ]
        );
        // None is listed twice.
        let rows = nodes.iter().filter(|node| matches!(node, Node::Setting(_)));
        assert_eq!(rows.count(), LANGUAGES.len());
        // Without any, the list has no headings at all.
        let plain = picker("en-US", &[], false);
        assert!(
            all(&plain.children)
                .iter()
                .all(|node| !matches!(node, Node::Header(_)))
        );
    }

    #[test]
    fn the_line_about_several_languages_is_there_when_asked_for() {
        let says =
            |more: bool| Seen::of(&picker("en-US", &[], more).children).says("more than one");
        assert!(says(true) && !says(false));
    }
}
