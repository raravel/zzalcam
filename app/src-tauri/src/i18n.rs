//! UI strings for the Rust side (tray, overlays, toasts). The JSON files are shared with the web UI.

use std::collections::HashMap;

use windows::Win32::Globalization::GetUserDefaultUILanguage;

const KO: &str = include_str!("../../src/locales/ko.json");
const EN: &str = include_str!("../../src/locales/en.json");
const LANG_KOREAN: u16 = 0x12;

/// Resolve a language setting (`auto`, `ko`, `en`) to a supported language code.
pub fn resolve(setting: &str) -> &'static str {
    match setting {
        "ko" => "ko",
        "en" => "en",
        _ if unsafe { GetUserDefaultUILanguage() } & 0x3ff == LANG_KOREAN => "ko",
        _ => "en",
    }
}

pub struct I18n(HashMap<String, String>);

impl I18n {
    pub fn new(setting: &str) -> Self {
        let src = if resolve(setting) == "ko" { KO } else { EN };
        Self(serde_json::from_str(src).expect("locale files are valid JSON"))
    }

    pub fn t(&self, key: &str) -> String {
        self.0.get(key).cloned().unwrap_or_else(|| key.to_string())
    }

    /// Translate and fill `{name}` placeholders.
    pub fn tf(&self, key: &str, args: &[(&str, &str)]) -> String {
        args.iter()
            .fold(self.t(key), |s, (k, v)| s.replace(&format!("{{{k}}}"), v))
    }

    pub fn overlay(&self) -> zzalcam_overlay::Strings {
        zzalcam_overlay::Strings {
            region: self.t("overlay.region"),
            window: self.t("overlay.window"),
            control: self.t("overlay.control"),
            monitor: self.t("overlay.monitor"),
            help: self.t("overlay.help"),
            help_control: self.t("overlay.help_control"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_have_identical_keys() {
        let ko: HashMap<String, String> = serde_json::from_str(KO).unwrap();
        let en: HashMap<String, String> = serde_json::from_str(EN).unwrap();
        let mut a: Vec<_> = ko.keys().collect();
        let mut b: Vec<_> = en.keys().collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn placeholders_are_filled() {
        let i = I18n::new("en");
        assert_eq!(i.tf("bar.frames", &[("n", "12")]), "12 frames");
    }
}
