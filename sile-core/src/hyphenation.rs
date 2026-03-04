use std::collections::HashMap;

use hyphenation::{Hyphenator, Language, Load, Standard};

pub struct HyphenationDictionary {
    dictionaries: HashMap<String, Standard>,
    pub min_word: usize,
    pub left_min: usize,
    pub right_min: usize,
}

impl Default for HyphenationDictionary {
    fn default() -> Self {
        Self::new()
    }
}

impl HyphenationDictionary {
    pub fn new() -> Self {
        Self {
            dictionaries: HashMap::new(),
            min_word: 5,
            left_min: 2,
            right_min: 2,
        }
    }

    pub fn load_language(&mut self, lang: &str) -> bool {
        if self.dictionaries.contains_key(lang) {
            return true;
        }
        if let Some(language) = language_from_code(lang)
            && let Ok(dict) = Standard::from_embedded(language) {
                self.dictionaries.insert(lang.to_string(), dict);
                return true;
            }
        false
    }

    pub fn hyphenate_word(&mut self, word: &str, lang: &str) -> Vec<String> {
        let char_count = word.chars().count();
        if char_count < self.min_word {
            return vec![word.to_string()];
        }

        if !self.load_language(lang) {
            return vec![word.to_string()];
        }

        let dict = &self.dictionaries[lang];
        let hyphenated = dict.hyphenate(word);
        let breaks = &hyphenated.breaks;

        if breaks.is_empty() {
            return vec![word.to_string()];
        }

        // Split word at break byte offsets
        let mut segments = Vec::with_capacity(breaks.len() + 1);
        let mut start = 0;
        for &brk in breaks {
            segments.push(word[start..brk].to_string());
            start = brk;
        }
        segments.push(word[start..].to_string());

        // Enforce left_min / right_min: merge segments that are too close to edges
        let mut result = Vec::new();
        let mut left_chars = 0usize;
        let mut buffer = String::new();
        let total_chars = char_count;

        for (i, seg) in segments.iter().enumerate() {
            let seg_chars = seg.chars().count();
            left_chars += seg_chars;

            buffer.push_str(seg);

            if i < segments.len() - 1 {
                let right_chars = total_chars - left_chars;
                if left_chars >= self.left_min && right_chars >= self.right_min {
                    result.push(std::mem::take(&mut buffer));
                }
            }
        }
        if !buffer.is_empty() {
            result.push(buffer);
        }

        if result.is_empty() {
            vec![word.to_string()]
        } else {
            result
        }
    }
}

fn language_from_code(lang: &str) -> Option<Language> {
    let normalized = lang.to_lowercase().replace('_', "-");
    match normalized.as_str() {
        "en" | "en-us" => Some(Language::EnglishUS),
        "en-gb" => Some(Language::EnglishGB),
        "de" | "de-de" => Some(Language::German1996),
        "de-1901" => Some(Language::German1901),
        "fr" | "fr-fr" => Some(Language::French),
        "es" | "es-es" => Some(Language::Spanish),
        "it" | "it-it" => Some(Language::Italian),
        "pt" | "pt-pt" => Some(Language::Portuguese),
        "pt-br" => Some(Language::Portuguese),
        "nl" | "nl-nl" => Some(Language::Dutch),
        "ru" | "ru-ru" => Some(Language::Russian),
        "uk" | "uk-ua" => Some(Language::Ukrainian),
        "pl" | "pl-pl" => Some(Language::Polish),
        "cs" | "cs-cz" => Some(Language::Czech),
        "sk" | "sk-sk" => Some(Language::Slovak),
        "sv" | "sv-se" => Some(Language::Swedish),
        "da" | "da-dk" => Some(Language::Danish),
        "nb" | "nn" | "no" => Some(Language::NorwegianBokmal),
        "fi" | "fi-fi" => Some(Language::Finnish),
        "hu" | "hu-hu" => Some(Language::Hungarian),
        "tr" | "tr-tr" => Some(Language::Turkish),
        "el" | "el-gr" => Some(Language::GreekMono),
        "bg" | "bg-bg" => Some(Language::Bulgarian),
        "hr" | "hr-hr" => Some(Language::Croatian),
        "sr" | "sr-rs" => Some(Language::SerbianCyrillic),
        "ca" | "ca-es" => Some(Language::Catalan),
        "ro" | "ro-ro" => Some(Language::Romanian),
        "la" => Some(Language::Latin),
        _ => None,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyphenate_long_word() {
        let mut dict = HyphenationDictionary::new();
        let segments = dict.hyphenate_word("hyphenation", "en");
        assert!(
            segments.len() >= 2,
            "expected multiple segments, got {segments:?}"
        );
        let rejoined: String = segments.concat();
        assert_eq!(rejoined, "hyphenation");
    }

    #[test]
    fn hyphenate_short_word_unchanged() {
        let mut dict = HyphenationDictionary::new();
        let segments = dict.hyphenate_word("the", "en");
        assert_eq!(segments, vec!["the"]);
    }

    #[test]
    fn hyphenate_unknown_language() {
        let mut dict = HyphenationDictionary::new();
        let segments = dict.hyphenate_word("something", "xx-unknown");
        assert_eq!(segments, vec!["something"]);
    }

    #[test]
    fn hyphenate_preserves_text() {
        let mut dict = HyphenationDictionary::new();
        for word in &["international", "extraordinary", "communication", "responsibility"] {
            let segments = dict.hyphenate_word(word, "en");
            let rejoined: String = segments.concat();
            assert_eq!(&rejoined, word, "rejoined segments must match original");
        }
    }

    #[test]
    fn hyphenate_german() {
        let mut dict = HyphenationDictionary::new();
        let segments = dict.hyphenate_word("Donaudampfschifffahrt", "de");
        assert!(segments.len() >= 2, "expected multiple segments for long German word");
        let rejoined: String = segments.concat();
        assert_eq!(rejoined, "Donaudampfschifffahrt");
    }

    #[test]
    fn load_language_caches() {
        let mut dict = HyphenationDictionary::new();
        assert!(dict.load_language("en"));
        assert!(dict.load_language("en")); // second call uses cache
        assert!(dict.dictionaries.contains_key("en"));
    }

    #[test]
    fn left_right_min_enforcement() {
        let mut dict = HyphenationDictionary::new();
        dict.left_min = 3;
        dict.right_min = 3;
        let segments = dict.hyphenate_word("hyphenation", "en");
        for (i, seg) in segments.iter().enumerate() {
            let chars = seg.chars().count();
            if i == 0 {
                assert!(chars >= 3, "first segment too short: {seg}");
            }
            if i == segments.len() - 1 {
                assert!(chars >= 3, "last segment too short: {seg}");
            }
        }
    }
}
