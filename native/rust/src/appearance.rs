//! The system colour scheme, as the host reports it to FigCraft.
//!
//! HarmonyOS tells the ArkUI window light/dark; the host forwards a token
//! (`light`, `dark`, or `auto`) and this maps it to an egui theme when the UI
//! asks for the "system" appearance.
use egui::Theme;

#[derive(Clone, Default)]
pub struct Appearance {
    token: String,
}

impl Appearance {
    pub fn from_token(token: &str) -> Self {
        Self {
            token: token.to_ascii_lowercase(),
        }
    }

    /// Whether the system requests a dark scheme. `auto` is unresolved here (the
    /// host resolves it before handing it over), so it reports `None`.
    pub fn is_dark(&self) -> Option<bool> {
        match self.token.as_str() {
            "dark" => Some(true),
            "light" => Some(false),
            _ => None,
        }
    }

    /// The theme the system currently requests, if the token names one.
    pub fn current(&self) -> Option<Theme> {
        match self.token.as_str() {
            "dark" => Some(Theme::Dark),
            "light" => Some(Theme::Light),
            _ => None,
        }
    }
}
