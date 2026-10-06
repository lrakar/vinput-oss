//! The languages the engine knows, by the names it answers with.
//!
//! A request names a language by its code, but the answer says which language
//! was spoken with its English name in lower case, so the two are matched
//! here. A name that is not in the list is no configured language, which
//! only costs the second look described in [`super::dictation`].

/// The code of the language the engine calls `name`. A code is its own.
pub(super) fn code_of(name: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|(code, known)| *known == name || *code == name)
        .map(|(code, _)| *code)
}

/// The name of the language with this code as the models of the second
/// engine write it, `English`, if it is one they were taught. They write
/// down any other language as the nearest one they know.
pub(super) fn taught(code: &str) -> Option<String> {
    let mut taught = crate::models::QWEN_LANGUAGES.split(' ');
    taught.any(|taught| taught == code).then(|| name(code))
}

/// What to call the language `language`, a code such as `sl` or `sl-SI`:
/// `Slovenian`. One without a name here is called as it was written.
pub(crate) fn name(language: &str) -> String {
    let code = language.split(['-', '_']).next().unwrap_or_default();
    let known = LANGUAGES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(code));
    let Some((_, name)) = known else {
        return language.to_owned();
    };
    let (first, rest) = name.split_at(1);
    first.to_ascii_uppercase() + rest
}

/// Whether text in this language has spaces between its words. Where it has
/// none, a sentence follows the one before it without one too.
pub(super) fn spaced(code: &str) -> bool {
    !matches!(code, "zh" | "ja" | "th" | "lo" | "my" | "km" | "bo" | "yue")
}

/// Code and name of every language, as whisper.cpp lists them.
const LANGUAGES: &[(&str, &str)] = &[
    ("en", "english"),
    ("zh", "chinese"),
    ("de", "german"),
    ("es", "spanish"),
    ("ru", "russian"),
    ("ko", "korean"),
    ("fr", "french"),
    ("ja", "japanese"),
    ("pt", "portuguese"),
    ("tr", "turkish"),
    ("pl", "polish"),
    ("ca", "catalan"),
    ("nl", "dutch"),
    ("ar", "arabic"),
    ("sv", "swedish"),
    ("it", "italian"),
    ("id", "indonesian"),
    ("hi", "hindi"),
    ("fi", "finnish"),
    ("vi", "vietnamese"),
    ("he", "hebrew"),
    ("uk", "ukrainian"),
    ("el", "greek"),
    ("ms", "malay"),
    ("cs", "czech"),
    ("ro", "romanian"),
    ("da", "danish"),
    ("hu", "hungarian"),
    ("ta", "tamil"),
    ("no", "norwegian"),
    ("th", "thai"),
    ("ur", "urdu"),
    ("hr", "croatian"),
    ("bg", "bulgarian"),
    ("lt", "lithuanian"),
    ("la", "latin"),
    ("mi", "maori"),
    ("ml", "malayalam"),
    ("cy", "welsh"),
    ("sk", "slovak"),
    ("te", "telugu"),
    ("fa", "persian"),
    ("lv", "latvian"),
    ("bn", "bengali"),
    ("sr", "serbian"),
    ("az", "azerbaijani"),
    ("sl", "slovenian"),
    ("kn", "kannada"),
    ("et", "estonian"),
    ("mk", "macedonian"),
    ("br", "breton"),
    ("eu", "basque"),
    ("is", "icelandic"),
    ("hy", "armenian"),
    ("ne", "nepali"),
    ("mn", "mongolian"),
    ("bs", "bosnian"),
    ("kk", "kazakh"),
    ("sq", "albanian"),
    ("sw", "swahili"),
    ("gl", "galician"),
    ("mr", "marathi"),
    ("pa", "punjabi"),
    ("si", "sinhala"),
    ("km", "khmer"),
    ("sn", "shona"),
    ("yo", "yoruba"),
    ("so", "somali"),
    ("af", "afrikaans"),
    ("oc", "occitan"),
    ("ka", "georgian"),
    ("be", "belarusian"),
    ("tg", "tajik"),
    ("sd", "sindhi"),
    ("gu", "gujarati"),
    ("am", "amharic"),
    ("yi", "yiddish"),
    ("lo", "lao"),
    ("uz", "uzbek"),
    ("fo", "faroese"),
    ("ht", "haitian creole"),
    ("ps", "pashto"),
    ("tk", "turkmen"),
    ("nn", "nynorsk"),
    ("mt", "maltese"),
    ("sa", "sanskrit"),
    ("lb", "luxembourgish"),
    ("my", "myanmar"),
    ("bo", "tibetan"),
    ("tl", "tagalog"),
    ("mg", "malagasy"),
    ("as", "assamese"),
    ("tt", "tatar"),
    ("haw", "hawaiian"),
    ("ln", "lingala"),
    ("ha", "hausa"),
    ("ba", "bashkir"),
    ("jw", "javanese"),
    ("su", "sundanese"),
    ("yue", "cantonese"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_language_is_known_by_name_and_by_code() {
        assert_eq!(code_of("english"), Some("en"));
        assert_eq!(code_of("slovenian"), Some("sl"));
        assert_eq!(code_of("sl"), Some("sl"));
        assert_eq!(code_of("klingon"), None);
        assert_eq!(taught("en").as_deref(), Some("English"));
        assert_eq!(taught("sl"), None);
        assert_eq!(taught("auto"), None);
        assert_eq!(name("sl-SI"), "Slovenian");
        assert_eq!(name("EN_us"), "English");
        assert_eq!(name("tlh"), "tlh");
        assert!(spaced("sl"));
        assert!(!spaced("ja"));
    }
}
