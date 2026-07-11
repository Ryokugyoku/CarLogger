use gettextrs::{LocaleCategory, setlocale};
use std::sync::RwLock;

pub const LANGUAGE_SETTING_KEY: &str = "language";

static CURRENT_LANGUAGE: RwLock<Language> = RwLock::new(Language::English);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English,
    Japanese,
    Spanish,
}

impl Language {
    pub fn from_combo_id(id: &str) -> Self {
        match id {
            "ja" => Self::Japanese,
            "es" => Self::Spanish,
            _ => Self::English,
        }
    }

    pub fn from_locale(locale: &str) -> Self {
        let locale = locale.to_ascii_lowercase();

        if locale.contains("ja") {
            Self::Japanese
        } else if locale.contains("es") {
            Self::Spanish
        } else {
            Self::English
        }
    }

    pub fn from_saved_or_environment(saved_locale: Option<&str>) -> Self {
        saved_locale.map(Self::from_locale).unwrap_or_else(|| {
            std::env::var("LANG")
                .map(|locale| Self::from_locale(&locale))
                .unwrap_or(Self::English)
        })
    }

    pub fn combo_id(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Japanese => "ja",
            Self::Spanish => "es",
        }
    }

    pub fn locale(self) -> &'static str {
        match self {
            Self::English => "en_US.UTF-8",
            Self::Japanese => "ja_JP.UTF-8",
            Self::Spanish => "es_ES.UTF-8",
        }
    }
}

pub fn apply_language(language: Language) {
    setlocale(LocaleCategory::LcAll, language.locale());

    if let Ok(mut current_language) = CURRENT_LANGUAGE.write() {
        *current_language = language;
    }
}

pub fn translate(msgid: &str) -> String {
    let language = CURRENT_LANGUAGE
        .read()
        .map(|current_language| *current_language)
        .unwrap_or(Language::English);

    translate_for_language(language, msgid).to_string()
}

fn translate_for_language(language: Language, msgid: &str) -> &str {
    match language {
        Language::English => msgid,
        Language::Japanese => match msgid {
            "Dashboard" => "ダッシュボード",
            "Log Analysis" => "ログ分析",
            "Maintenance" => "整備記録",
            "Settings" => "設定",
            "Language" => "言語",
            "Some changes might require restart." => {
                "一部の変更を適用するには再起動が必要な場合があります。"
            }
            _ => msgid,
        },
        Language::Spanish => match msgid {
            "Dashboard" => "Tablero",
            "Log Analysis" => "Análisis de registros",
            "Maintenance" => "Mantenimiento",
            "Settings" => "Configuración",
            "Language" => "Idioma",
            "Some changes might require restart." => "Algunos cambios pueden requerir un reinicio.",
            _ => msgid,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_locales() {
        assert_eq!(Language::from_locale("ja_JP.UTF-8"), Language::Japanese);
        assert_eq!(Language::from_locale("es_ES.UTF-8"), Language::Spanish);
        assert_eq!(Language::from_locale("en_US.UTF-8"), Language::English);
    }

    #[test]
    fn uses_saved_locale_before_environment() {
        assert_eq!(
            Language::from_saved_or_environment(Some("ja_JP.UTF-8")),
            Language::Japanese
        );
    }

    #[test]
    fn translates_known_messages() {
        assert_eq!(
            translate_for_language(Language::Japanese, "Settings"),
            "設定"
        );
        assert_eq!(
            translate_for_language(Language::Spanish, "Language"),
            "Idioma"
        );
        assert_eq!(
            translate_for_language(Language::Japanese, "Unknown"),
            "Unknown"
        );
    }
}
