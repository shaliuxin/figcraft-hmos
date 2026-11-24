//! Host platform services for FigCraft on HarmonyOS.
//!
//! FigCraft's `Services` trait has a no-op default for every method, so this host
//! starts by overriding only what a first launch needs: the system language, the
//! system colour scheme, opening a file through the ArkTS document picker, and
//! saving through it. Clipboard, URL opening and folder picking are wired in a
//! later patch.
use figcraft_ui_egui::{ContentsFuture, DialogFuture, Services};
use std::path::PathBuf;

use crate::bridge;

/// The host's services: file picking and saving flow to ArkTS, the rest stays default.
pub struct HostServices {
    pub language: String,
    pub system_dark: Option<bool>,
    /// Where the engine may read a freshly picked file (inside the sandbox).
    pub sandbox_dir: Option<String>,
}

impl HostServices {
    pub fn pick_open_bytes(&mut self, name: &str, exts: &[&str]) -> Option<(String, Vec<u8>)> {
        let _ = (name, exts);
        let result = bridge::request("open", "", &[]);
        match result {
            Ok((name, bytes)) => Some((name, bytes)),
            Err(_) => None,
        }
    }
}

impl Services for HostServices {
    /// The browser-style "pick and hand over the bytes" path. The host returns the
    /// picked file's name and contents; the engine opens it without a real path.
    fn pick_open_contents(&mut self, _title: &str, _exts: &[&str]) -> Option<ContentsFuture> {
        let result = bridge::request("open", "", &[]);
        let value = match result {
            Ok((name, bytes)) => Some(Ok((name, bytes))),
            Err(e) if e == "cancelled" => None,
            Err(e) => Some(Err(e)),
        };
        Some(Box::pin(std::future::ready(value)))
    }

    /// A file picker that returns a path the engine can read.
    fn pick_open(&mut self, _title: &str, _exts: &[&str]) -> DialogFuture {
        let result = bridge::request("open", "", &[]);
        let value = match result {
            Ok((name, bytes)) => {
                if let Some(dir) = &self.sandbox_dir {
                    let path = PathBuf::from(dir).join(&name);
                    if std::fs::create_dir_all(dir).is_ok() && std::fs::write(&path, &bytes).is_ok() {
                        Some(PathBuf::from(name))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        Box::pin(std::future::ready(value))
    }

    fn pick_save(&mut self, _title: &str, name: &str, _exts: &[&str]) -> DialogFuture {
        let value = bridge::request("save_select", name, &[]).ok().map(|(n, _)| PathBuf::from(n));
        Box::pin(std::future::ready(value))
    }

    fn pick_folder(&mut self, _title: &str) -> DialogFuture {
        let value = bridge::request("folder", "output", &[]).ok().map(|(path, _)| PathBuf::from(path));
        Box::pin(std::future::ready(value))
    }

    fn open_url(&mut self, url: &str) {
        bridge::notify(0, "url", url, &[]);
    }

    fn system_language(&self) -> Option<String> {
        Some(self.language.clone())
    }

    fn system_dark(&self) -> Option<bool> {
        self.system_dark
    }
}
