//! The parts of the platform's own look the frontend's token layer follows:
//! which design language, the system accent colour and UI font, and on macOS
//! the height of the title bar the content extends under.
//!
//! Every window is built with the current values as an initialization
//! script, so its first paint already has them; the frontend asks again
//! when a window regains focus, which is when a change made in the system
//! settings meanwhile shows.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum PlatformStyle {
    Macos,
    Windows,
    Gnome,
    Kde,
}

#[derive(Debug, Clone, PartialEq, Serialize, specta::Type)]
pub struct SystemAppearance {
    pub platform: PlatformStyle,
    /// `#rrggbb`.
    pub accent: Option<String>,
    pub font_family: Option<String>,
    /// In CSS pixels.
    pub font_size: Option<f64>,
    /// The title bar the window's content is drawn under, in CSS pixels.
    pub titlebar_height: Option<f64>,
}

pub fn system_appearance() -> SystemAppearance {
    platform::read()
}

pub(crate) fn initialization_script() -> String {
    let json = serde_json::to_string(&system_appearance()).expect("appearance serializes");
    format!("window.__NEWT_APPEARANCE__ = {json};")
}

/// Applies the platform's window chrome and the appearance script to a
/// window being built.
pub(crate) fn window_chrome<'a, M: tauri::Manager<tauri::Wry>>(
    builder: tauri::WebviewWindowBuilder<'a, tauri::Wry, M>,
) -> tauri::WebviewWindowBuilder<'a, tauri::Wry, M> {
    let builder = builder.initialization_script(initialization_script());
    #[cfg(target_os = "macos")]
    let builder = builder.title_bar_style(tauri::TitleBarStyle::Overlay);
    builder
}

/// Records what can only be read on the main thread; called from `setup`.
pub(crate) fn init() {
    #[cfg(target_os = "macos")]
    platform::init();
}

#[tauri::command]
#[specta::specta]
pub fn get_system_appearance() -> SystemAppearance {
    system_appearance()
}

fn hex(r: f64, g: f64, b: f64) -> String {
    let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", c(r), c(g), c(b))
}

/// Points to CSS pixels.
#[cfg(any(target_os = "linux", test))]
fn pt_to_px(pt: f64) -> f64 {
    pt * 96.0 / 72.0
}

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::OnceLock;

    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSColor, NSColorSpace, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    use super::{PlatformStyle, SystemAppearance, hex};

    static TITLEBAR_HEIGHT: OnceLock<f64> = OnceLock::new();

    pub(super) fn init() {
        let mtm = MainThreadMarker::new().expect("appearance::init runs on the main thread");
        let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0));
        let frame =
            NSWindow::frameRectForContentRect_styleMask(content, NSWindowStyleMask::Titled, mtm);
        let _ = TITLEBAR_HEIGHT.set(frame.size.height - content.size.height);
    }

    pub(super) fn read() -> SystemAppearance {
        let accent = NSColor::controlAccentColor()
            .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
            .map(|c| hex(c.redComponent(), c.greenComponent(), c.blueComponent()));
        SystemAppearance {
            platform: PlatformStyle::Macos,
            accent,
            font_family: None,
            font_size: None,
            titlebar_height: TITLEBAR_HEIGHT.get().copied(),
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows::UI::ViewManagement::{UIColorType, UISettings};

    use super::{PlatformStyle, SystemAppearance, hex};

    pub(super) fn read() -> SystemAppearance {
        let accent = UISettings::new()
            .and_then(|s| s.GetColorValue(UIColorType::Accent))
            .ok()
            .map(|c| hex(c.R as f64 / 255.0, c.G as f64 / 255.0, c.B as f64 / 255.0));
        SystemAppearance {
            platform: PlatformStyle::Windows,
            accent,
            font_family: None,
            font_size: None,
            titlebar_height: None,
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::PathBuf;

    use gio::prelude::SettingsExt;

    use super::{PlatformStyle, SystemAppearance, kde};

    pub(super) fn read() -> SystemAppearance {
        let is_kde = std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|v| v.split(':').any(|d| d.eq_ignore_ascii_case("kde")));
        if is_kde {
            let globals = kdeglobals().and_then(|p| std::fs::read_to_string(p).ok());
            let globals = globals.as_deref().map(kde::parse).unwrap_or_default();
            SystemAppearance {
                platform: PlatformStyle::Kde,
                accent: globals.accent,
                font_family: globals.font.as_ref().map(|f| f.0.clone()),
                font_size: globals.font.map(|f| f.1),
                titlebar_height: None,
            }
        } else {
            let (accent, font) = gnome();
            SystemAppearance {
                platform: PlatformStyle::Gnome,
                accent,
                font_family: font.as_ref().map(|f| f.0.clone()),
                font_size: font.map(|f| f.1),
                titlebar_height: None,
            }
        }
    }

    fn kdeglobals() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(config.join("kdeglobals"))
    }

    /// `org.gnome.desktop.interface`: `accent-color` (GNOME 47+) names one
    /// of libadwaita's accents; `font-name` is a Pango description.
    fn gnome() -> (Option<String>, Option<(String, f64)>) {
        const SCHEMA: &str = "org.gnome.desktop.interface";
        let Some(schema) =
            gio::SettingsSchemaSource::default().and_then(|s| s.lookup(SCHEMA, true))
        else {
            return (None, None);
        };
        let settings = gio::Settings::new(SCHEMA);
        let accent = schema
            .has_key("accent-color")
            .then(|| settings.string("accent-color"))
            .and_then(|name| super::libadwaita_accent(&name))
            .map(str::to_owned);
        let font = schema
            .has_key("font-name")
            .then(|| gtk::pango::FontDescription::from_string(&settings.string("font-name")))
            .and_then(|desc| {
                let family = desc.family()?.to_string();
                let size = desc.size() as f64 / gtk::pango::SCALE as f64;
                let px = if desc.is_size_absolute() {
                    size
                } else {
                    super::pt_to_px(size)
                };
                (size > 0.0).then_some((family, px))
            });
        (accent, font)
    }
}

/// libadwaita's accent colours, by their `accent-color` names.
#[cfg(any(target_os = "linux", test))]
fn libadwaita_accent(name: &str) -> Option<&'static str> {
    Some(match name {
        "blue" => "#3584e4",
        "teal" => "#2190a4",
        "green" => "#3a944a",
        "yellow" => "#c88800",
        "orange" => "#ed5b00",
        "red" => "#e62d42",
        "pink" => "#d56199",
        "purple" => "#9141ac",
        "slate" => "#6f8396",
        _ => return None,
    })
}

#[cfg(any(target_os = "linux", test))]
mod kde {
    #[derive(Debug, Default, PartialEq)]
    pub(super) struct Globals {
        pub accent: Option<String>,
        /// Family and size in CSS pixels.
        pub font: Option<(String, f64)>,
    }

    /// The accent is `[General] AccentColor`, or the colour scheme's
    /// selection colour when the accent follows the scheme; the font is
    /// `[General] font`, a Qt font string (`family,pointSize,…`).
    pub(super) fn parse(text: &str) -> Globals {
        let mut section = "";
        let mut accent = None;
        let mut selection = None;
        let mut font = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name;
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match (section, key.trim()) {
                ("General", "AccentColor") => accent = rgb(value),
                ("Colors:Selection", "BackgroundNormal") => selection = rgb(value),
                ("General", "font") => font = qt_font(value),
                _ => {}
            }
        }
        Globals {
            accent: accent.or(selection),
            font,
        }
    }

    fn rgb(value: &str) -> Option<String> {
        let parts: Vec<f64> = value
            .split(',')
            .map(|p| p.trim().parse::<f64>().map(|v| v / 255.0))
            .collect::<Result<_, _>>()
            .ok()?;
        match parts[..] {
            [r, g, b] | [r, g, b, _] => Some(super::hex(r, g, b)),
            _ => None,
        }
    }

    fn qt_font(value: &str) -> Option<(String, f64)> {
        let mut fields = value.split(',');
        let family = fields.next()?.trim();
        let size: f64 = fields.next()?.trim().parse().ok()?;
        (!family.is_empty() && size > 0.0).then(|| (family.to_owned(), super::pt_to_px(size)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kdeglobals_accent_and_font() {
        let globals = kde::parse(
            "[Colors:Selection]\nBackgroundNormal=61,174,233\n\n\
             [General]\nAccentColor=233,100,58\nfont=Noto Sans,10,-1,5,400,0,0,0,0,0,0,0,0,0,0,1\n",
        );
        assert_eq!(globals.accent.as_deref(), Some("#e9643a"));
        let (family, px) = globals.font.unwrap();
        assert_eq!(family, "Noto Sans");
        assert!((px - 13.333).abs() < 0.01);
    }

    #[test]
    fn kdeglobals_accent_follows_the_scheme_without_its_own() {
        let globals = kde::parse("[Colors:Selection]\nBackgroundNormal=61,174,233\n");
        assert_eq!(globals.accent.as_deref(), Some("#3daee9"));
        assert_eq!(globals.font, None);
    }

    #[test]
    fn libadwaita_accent_names() {
        assert_eq!(libadwaita_accent("teal"), Some("#2190a4"));
        assert_eq!(libadwaita_accent("chartreuse"), None);
    }
}
