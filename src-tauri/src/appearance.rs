//! The parts of the platform's own look the frontend's token layer follows:
//! which design language, the system accent colour and UI font, KDE's colour
//! scheme, and the window chrome — macOS's title bar the content extends
//! under, the header bar Newt draws itself on GNOME, Mica on Windows 11.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, specta::Type)]
#[serde(rename_all = "lowercase")]
pub enum WindowButton {
    Minimize,
    Maximize,
    Close,
}

/// The window buttons on either side of a header bar, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, specta::Type)]
pub struct WindowButtons {
    pub start: Vec<WindowButton>,
    pub end: Vec<WindowButton>,
}

/// KDE's colour scheme, each colour `#rrggbb` where the scheme sets it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, specta::Type)]
pub struct ColorScheme {
    /// Whether the scheme is a dark one, judged by its window colour.
    pub dark: bool,
    pub window_bg: Option<String>,
    pub window_fg: Option<String>,
    pub window_fg_inactive: Option<String>,
    pub view_bg: Option<String>,
    pub view_fg: Option<String>,
    pub header_bg: Option<String>,
    /// While the window doesn't have the keyboard.
    pub header_bg_inactive: Option<String>,
    pub header_fg: Option<String>,
    pub button_bg: Option<String>,
    pub button_fg: Option<String>,
    pub selection_fg: Option<String>,
    pub focus: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, specta::Type)]
pub struct SystemAppearance {
    pub platform: PlatformStyle,
    /// `#rrggbb`.
    pub accent: Option<String>,
    pub font_family: Option<String>,
    /// The UI font's size, in points.
    pub font_points: Option<f64>,
    /// The window buttons of the header bar Newt draws itself (GNOME);
    /// `None` where the desktop gives the window its title bar.
    pub window_buttons: Option<WindowButtons>,
    pub color_scheme: Option<ColorScheme>,
    /// Mica shows through the window's chrome.
    pub mica: bool,
    /// The window's background, which a plain title bar is drawn in
    /// (macOS); the pane headers take it so the title bar runs on into them.
    pub window_bg: Option<LightDark>,
}

/// A colour as the light and the dark appearance resolve it, `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Serialize, specta::Type)]
pub struct LightDark {
    pub light: String,
    pub dark: String,
}

pub fn system_appearance() -> SystemAppearance {
    platform::read()
}

pub(crate) fn initialization_script() -> String {
    let json = serde_json::to_string(&system_appearance()).expect("appearance serializes");
    format!("window.__NEWT_APPEARANCE__ = {json};")
}

/// Applies the platform's window chrome and the appearance script to a
/// window being built. A window built through this must be handed to
/// [`window_built`] afterwards, which shows it.
pub(crate) fn window_chrome<'a, M: tauri::Manager<tauri::Wry>>(
    builder: tauri::WebviewWindowBuilder<'a, tauri::Wry, M>,
) -> tauri::WebviewWindowBuilder<'a, tauri::Wry, M> {
    let builder = builder.initialization_script(initialization_script());
    #[cfg(windows)]
    let builder = if platform::mica() {
        builder
            .transparent(true)
            .effects(tauri::utils::config::WindowEffectsConfig {
                effects: vec![tauri::window::Effect::Mica],
                ..Default::default()
            })
    } else {
        builder
    };
    // GTK takes a window's title bar only before it is first shown. The
    // window is see-through so the page can round its own corners: GTK
    // rounds its frame but doesn't clip what's inside it.
    #[cfg(target_os = "linux")]
    let builder = if platform::is_gnome_desktop() {
        builder.visible(false).transparent(true)
    } else {
        builder
    };
    builder
}

/// Finishes a window built with [`window_chrome`]: tells the page whenever
/// the window becomes active or stops being so, and on GNOME itself has GTK
/// keep the window's frame — shadow, rounded corners, resize edges — but draw no
/// title bar, Newt's header bar standing in for it. Shows the window if
/// `visible` where [`window_chrome`] hid it.
pub(crate) fn window_built(window: &tauri::WebviewWindow, visible: bool) {
    let target = window.clone();
    let result = window.run_on_main_thread(move || {
        platform::watch_activation(&target);
        #[cfg(target_os = "linux")]
        if platform::is_gnome_desktop() {
            match target.gtk_window() {
                Ok(gtk_window) => platform::frame_only(&gtk_window, &target),
                Err(e) => log::warn!("no GTK window for the header bar: {e}"),
            }
            if visible {
                let _ = target.show();
                let _ = target.set_focus();
            }
        }
    });
    if let Err(e) = result {
        log::warn!("could not finish the window: {e}");
    }
    #[cfg(not(target_os = "linux"))]
    let _ = visible;
}

/// The page marks itself inactive (`data-window-inactive`) by the window
/// manager's account of which window is active, not by its own focus: a
/// window move's pointer grab, a native menu or WebView2's child window can
/// take the page's focus while the window stays active.
fn set_active(window: &tauri::WebviewWindow, active: bool) {
    let _ = window.eval(format!("window.__NEWT_SET_ACTIVE__?.({active})"));
}

/// Whether the window is the active one, for a page that has only just
/// loaded and so missed being told.
#[tauri::command]
#[specta::specta]
pub fn is_window_active(webview_window: tauri::WebviewWindow) -> bool {
    platform::is_active(&webview_window)
}

/// Whether GTK has squared the frame's corners (maximized, tiled, full
/// screen) under the header bar Newt draws, for a page that has only just
/// loaded; changes after that are pushed as they happen.
#[tauri::command]
#[specta::specta]
pub fn has_square_corners(webview_window: tauri::WebviewWindow) -> bool {
    #[cfg(target_os = "linux")]
    let square = platform::has_square_corners(&webview_window);
    #[cfg(not(target_os = "linux"))]
    let square = {
        let _ = webview_window;
        false
    };
    square
}

/// Whether Newt draws the window's title bar as a header bar (GNOME itself),
/// whose primary menu stands in for a viewer's or editor's menu bar.
pub(crate) fn draws_header_bar() -> bool {
    #[cfg(target_os = "linux")]
    let gnome = platform::is_gnome_desktop();
    #[cfg(not(target_os = "linux"))]
    let gnome = false;
    gnome
}

#[tauri::command]
#[specta::specta]
pub fn get_system_appearance() -> SystemAppearance {
    system_appearance()
}

/// The window manager's window menu, for a right click on the header bar
/// Newt draws (GNOME).
#[tauri::command]
#[specta::specta]
pub async fn show_window_menu(webview_window: tauri::WebviewWindow) {
    #[cfg(target_os = "linux")]
    {
        let target = webview_window.clone();
        let _ = webview_window.run_on_main_thread(move || {
            if let Ok(gtk_window) = target.gtk_window() {
                platform::show_window_menu(&gtk_window);
            }
        });
    }
    #[cfg(not(target_os = "linux"))]
    let _ = webview_window;
}

fn hex(r: f64, g: f64, b: f64) -> String {
    let c = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", c(r), c(g), c(b))
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::Cell;

    use block2::StackBlock;
    use objc2_app_kit::{
        NSAppearance, NSAppearanceName, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSColor,
        NSColorSpace,
    };

    use super::{LightDark, PlatformStyle, SystemAppearance, hex};

    /// The key window is the active one.
    pub(super) fn watch_activation(window: &tauri::WebviewWindow) {
        use tauri::Manager;

        let app_handle = window.app_handle().clone();
        let label = window.label().to_owned();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::Focused(active) = event
                && let Some(window) = app_handle.get_webview_window(&label)
            {
                super::set_active(&window, *active);
            }
        });
    }

    pub(super) fn is_active(window: &tauri::WebviewWindow) -> bool {
        window.is_focused().unwrap_or(true)
    }

    pub(super) fn read() -> SystemAppearance {
        SystemAppearance {
            platform: PlatformStyle::Macos,
            accent: srgb_hex(&NSColor::controlAccentColor()),
            font_family: None,
            font_points: None,
            window_buttons: None,
            color_scheme: None,
            mica: false,
            window_bg: light_dark(&NSColor::windowBackgroundColor()),
        }
    }

    fn srgb_hex(color: &NSColor) -> Option<String> {
        color
            .colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())
            .map(|c| hex(c.redComponent(), c.greenComponent(), c.blueComponent()))
    }

    /// A dynamic colour as each appearance resolves it.
    fn light_dark(color: &NSColor) -> Option<LightDark> {
        // SAFETY: AppKit's appearance names are immutable statics.
        let (light, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
        let resolve = |name: &NSAppearanceName| {
            let appearance = NSAppearance::appearanceNamed(name)?;
            let out = Cell::new(None);
            let block = StackBlock::new(|| out.set(srgb_hex(color)));
            appearance.performAsCurrentDrawingAppearance(&block);
            out.take()
        };
        Some(LightDark {
            light: resolve(light)?,
            dark: resolve(dark)?,
        })
    }
}

#[cfg(windows)]
mod platform {
    use windows::UI::ViewManagement::{UIColorType, UISettings};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, WA_INACTIVE, WM_ACTIVATE, WM_NCDESTROY,
    };

    use super::{PlatformStyle, SystemAppearance, hex};

    const ACTIVATION_SUBCLASS: usize = 0x4e41_4354;

    /// Activation rather than tao's `Focused`, which follows the keyboard
    /// focus of the top-level window and so goes false whenever WebView2's
    /// child window takes it.
    pub(super) fn watch_activation(window: &tauri::WebviewWindow) {
        let Ok(hwnd) = window.hwnd() else {
            return;
        };
        let hwnd = HWND(hwnd.0);
        let target = Box::into_raw(Box::new(window.clone()));
        // SAFETY: on the window's own thread; the subclass frees `target`
        // as the window goes.
        unsafe {
            let _ = SetWindowSubclass(
                hwnd,
                Some(activation_proc),
                ACTIVATION_SUBCLASS,
                target as usize,
            );
        }
    }

    unsafe extern "system" fn activation_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        refdata: usize,
    ) -> LRESULT {
        match msg {
            WM_ACTIVATE => {
                // SAFETY: refdata is the boxed window, alive until WM_NCDESTROY.
                let window = unsafe { &*(refdata as *const tauri::WebviewWindow) };
                super::set_active(window, (wparam.0 & 0xffff) as u32 != WA_INACTIVE);
            }
            WM_NCDESTROY => unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(activation_proc), ACTIVATION_SUBCLASS);
                drop(Box::from_raw(refdata as *mut tauri::WebviewWindow));
            },
            _ => {}
        }
        unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
    }

    pub(super) fn is_active(window: &tauri::WebviewWindow) -> bool {
        window
            .hwnd()
            .is_ok_and(|hwnd| unsafe { GetForegroundWindow() }.0 == hwnd.0)
    }

    /// Mica arrived with Windows 11; earlier, a transparent window would
    /// show the desktop through its chrome.
    pub(super) fn mica() -> bool {
        windows_version::OsVersion::current().build >= 22000
    }

    pub(super) fn read() -> SystemAppearance {
        let accent = UISettings::new()
            .and_then(|s| s.GetColorValue(UIColorType::Accent))
            .ok()
            .map(|c| hex(c.R as f64 / 255.0, c.G as f64 / 255.0, c.B as f64 / 255.0));
        SystemAppearance {
            platform: PlatformStyle::Windows,
            accent,
            font_family: None,
            font_points: None,
            window_buttons: None,
            color_scheme: None,
            mica: mica(),
            window_bg: None,
        }
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::PathBuf;

    use gio::prelude::SettingsExt;

    use super::{PlatformStyle, SystemAppearance, kde};

    /// GTK's frame for a window whose page draws the rest. GTK3 rounds a
    /// frame's corners but doesn't clip the window's contents to them, so
    /// the window is see-through and the page rounds its own corners to
    /// match (`_platform.scss`), squaring them, as GTK squares its frame,
    /// while the window is maximized, tiled or full screen.
    pub(super) fn frame_only(gtk_window: &gtk::ApplicationWindow, webview: &tauri::WebviewWindow) {
        use gtk::glib::Propagation;
        use gtk::prelude::{
            CssProviderExt, GtkSettingsExt, GtkWindowExt, StyleContextExt, WidgetExt,
        };

        thread_local!(static STYLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
        if !STYLED.replace(true)
            && let Some(screen) = gtk::prelude::WidgetExt::screen(gtk_window)
        {
            let provider = gtk::CssProvider::new();
            let load = {
                let provider = provider.clone();
                move |dark: bool| {
                    provider
                        .load_from_data(frame_css(dark).as_bytes())
                        .expect("frame stylesheet parses");
                }
            };
            // GTK's dark preference follows the theme in effect, as the
            // webview's colour scheme does.
            let settings = gtk::Settings::default();
            load(
                settings
                    .as_ref()
                    .is_some_and(|s| s.is_gtk_application_prefer_dark_theme()),
            );
            if let Some(settings) = settings {
                settings.connect_gtk_application_prefer_dark_theme_notify(move |s| {
                    load(s.is_gtk_application_prefer_dark_theme())
                });
            }
            gtk::StyleContext::add_provider_for_screen(
                &screen,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        gtk_window.style_context().add_class("newt-frame");
        // A transparent window is app-paintable, which keeps GTK from
        // painting its background; it paints the page's here, inside the
        // rounded frame, where the webview hasn't drawn yet — before the
        // page loads, and ahead of it in a resize.
        gtk_window.set_app_paintable(false);

        // GTK marks the widget it is given as the title bar `.titlebar`,
        // which the theme draws a border under; the stylesheet takes it
        // away again.
        let titlebar = gtk::EventBox::new();
        gtk_window.set_titlebar(Some(&titlebar));

        let _ = webview.with_webview(|pw| {
            pw.inner().connect_button_press_event(|_, event| {
                if event.button() == 3 {
                    RIGHT_PRESS.set(Some(event.clone()));
                }
                Propagation::Proceed
            });
        });

        let webview = webview.clone();
        gtk_window.connect_window_state_event(move |_, event| {
            if event.changed_mask().intersects(SQUARE) {
                let square = event.new_window_state().intersects(SQUARE);
                let _ = webview.eval(format!("window.__NEWT_SET_SQUARE_CORNERS__?.({square})"));
            }
            Propagation::Proceed
        });
    }

    /// The states GTK squares the frame's corners in.
    const SQUARE: gtk::gdk::WindowState = gtk::gdk::WindowState::MAXIMIZED
        .union(gtk::gdk::WindowState::FULLSCREEN)
        .union(gtk::gdk::WindowState::TILED)
        .union(gtk::gdk::WindowState::TOP_TILED)
        .union(gtk::gdk::WindowState::RIGHT_TILED)
        .union(gtk::gdk::WindowState::BOTTOM_TILED)
        .union(gtk::gdk::WindowState::LEFT_TILED);

    pub(super) fn has_square_corners(window: &tauri::WebviewWindow) -> bool {
        use gtk::prelude::WidgetExt;

        window
            .gtk_window()
            .ok()
            .and_then(|w| w.window())
            .is_some_and(|w| w.state().intersects(SQUARE))
    }

    thread_local! {
        /// The window menu is shown for the press that asked for it, which
        /// WebKit has taken by the time the page asks.
        static RIGHT_PRESS: std::cell::RefCell<Option<gtk::gdk::EventButton>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn show_window_menu(gtk_window: &gtk::ApplicationWindow) {
        use gtk::glib::translate::ToGlibPtrMut;
        use gtk::prelude::WidgetExt;

        let Some(mut press) = RIGHT_PRESS.take() else {
            return;
        };
        let (Some(toplevel), Some(mut from)) = (gtk_window.window(), press.window()) else {
            return;
        };
        // The press is in the webview's coordinates; the menu goes by the
        // window's. A press in another window never reaches this one.
        let (mut x, mut y) = press.position();
        while from != toplevel {
            (x, y) = from.coords_to_parent(x, y);
            let Some(parent) = from.parent() else {
                return;
            };
            from = parent;
        }
        // SAFETY: `press` owns the event, a button press.
        unsafe {
            let raw: *mut gtk::gdk::ffi::GdkEventButton = press.to_glib_none_mut().0;
            (*raw).x = x;
            (*raw).y = y;
        }
        toplevel.show_window_menu(&mut press);
    }

    /// GDK's `FOCUSED`: the window manager's active window, which GTK's
    /// own backdrop style follows.
    pub(super) fn watch_activation(window: &tauri::WebviewWindow) {
        use gtk::gdk::WindowState;
        use gtk::glib::Propagation;
        use gtk::prelude::WidgetExt;

        let Ok(gtk_window) = window.gtk_window() else {
            return;
        };
        let target = window.clone();
        gtk_window.connect_window_state_event(move |_, event| {
            if event.changed_mask().contains(WindowState::FOCUSED) {
                super::set_active(
                    &target,
                    event.new_window_state().contains(WindowState::FOCUSED),
                );
            }
            Propagation::Proceed
        });
    }

    pub(super) fn is_active(window: &tauri::WebviewWindow) -> bool {
        use gtk::prelude::WidgetExt;

        window
            .gtk_window()
            .ok()
            .and_then(|w| w.window())
            .is_none_or(|w| w.state().contains(gtk::gdk::WindowState::FOCUSED))
    }

    /// The page's background (`--color-bg` on GNOME), light and dark.
    fn frame_css(dark: bool) -> String {
        let background = if dark { "#1d1d20" } else { "#ffffff" };
        FRAME_CSS.replace("BACKGROUND", background)
    }

    /// The window's corner radius matches libadwaita's (`--window-radius`).
    const FRAME_CSS: &str = "
        window.newt-frame { background: BACKGROUND; }
        window.newt-frame, window.newt-frame decoration { border-radius: 12px; }
        window.newt-frame.maximized, window.newt-frame.maximized decoration,
        window.newt-frame.fullscreen, window.newt-frame.fullscreen decoration,
        window.newt-frame.tiled, window.newt-frame.tiled decoration,
        window.newt-frame.tiled-top, window.newt-frame.tiled-top decoration,
        window.newt-frame.tiled-right, window.newt-frame.tiled-right decoration,
        window.newt-frame.tiled-bottom, window.newt-frame.tiled-bottom decoration,
        window.newt-frame.tiled-left, window.newt-frame.tiled-left decoration {
            border-radius: 0;
        }
        window.newt-frame .titlebar {
            min-height: 0;
            padding: 0;
            margin: 0;
            border: none;
            box-shadow: none;
            background: none;
        }
    ";

    /// KDE by `XDG_CURRENT_DESKTOP`; every other desktop gets the GNOME look.
    pub(super) fn is_gnome() -> bool {
        !current_desktop_is("kde")
    }

    /// GNOME itself, where Newt draws a header bar. Elsewhere the window
    /// keeps the title bar the desktop gives it: the window manager's, or
    /// on a Wayland compositor that draws none, GTK's own.
    pub(super) fn is_gnome_desktop() -> bool {
        current_desktop_is("gnome")
    }

    fn current_desktop_is(name: &str) -> bool {
        std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|v| v.split(':').any(|d| d.eq_ignore_ascii_case(name)))
    }

    pub(super) fn read() -> SystemAppearance {
        if is_gnome() {
            let interface = settings("org.gnome.desktop.interface");
            let accent = interface
                .as_ref()
                .and_then(|s| string(s, "accent-color"))
                .and_then(|name| super::libadwaita_accent(&name))
                .map(str::to_owned);
            let font = interface
                .as_ref()
                .and_then(|s| string(s, "font-name"))
                .and_then(|name| pango_font(&name));
            let buttons = is_gnome_desktop().then(|| {
                settings("org.gnome.desktop.wm.preferences")
                    .and_then(|s| string(&s, "button-layout"))
                    .map(|layout| super::button_layout(&layout))
                    .unwrap_or_else(super::default_buttons)
            });
            SystemAppearance {
                platform: PlatformStyle::Gnome,
                accent,
                font_family: font.as_ref().map(|f| f.0.clone()),
                font_points: font.map(|f| f.1),
                window_buttons: buttons,
                color_scheme: None,
                mica: false,
                window_bg: None,
            }
        } else {
            let globals = kdeglobals().and_then(|p| std::fs::read_to_string(p).ok());
            let globals = globals.as_deref().map(kde::parse).unwrap_or_default();
            SystemAppearance {
                platform: PlatformStyle::Kde,
                accent: globals.accent,
                font_family: globals.font.as_ref().map(|f| f.0.clone()),
                font_points: globals.font.map(|f| f.1),
                window_buttons: None,
                color_scheme: globals.scheme,
                mica: false,
                window_bg: None,
            }
        }
    }

    /// The settings, if the schema is installed.
    fn settings(schema: &str) -> Option<(gio::Settings, gio::SettingsSchema)> {
        let source = gio::SettingsSchemaSource::default()?.lookup(schema, true)?;
        Some((gio::Settings::new(schema), source))
    }

    fn string(
        (settings, schema): &(gio::Settings, gio::SettingsSchema),
        key: &str,
    ) -> Option<String> {
        schema
            .has_key(key)
            .then(|| settings.string(key).to_string())
    }

    /// A Pango font description's family and size in points.
    fn pango_font(description: &str) -> Option<(String, f64)> {
        let desc = gtk::pango::FontDescription::from_string(description);
        let family = desc.family()?.to_string();
        let size = desc.size() as f64 / gtk::pango::SCALE as f64;
        let points = if desc.is_size_absolute() {
            size * 72.0 / 96.0
        } else {
            size
        };
        (points > 0.0).then_some((family, points))
    }

    fn kdeglobals() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(config.join("kdeglobals"))
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

/// GNOME's default `button-layout`.
#[cfg(any(target_os = "linux", test))]
fn default_buttons() -> WindowButtons {
    button_layout("appmenu:close")
}

/// `org.gnome.desktop.wm.preferences button-layout`: the buttons leading
/// and trailing, separated by a colon. Names other than the three buttons
/// (`appmenu`, `icon`, `spacer`) have nothing to stand for here.
#[cfg(any(target_os = "linux", test))]
fn button_layout(layout: &str) -> WindowButtons {
    let side = |names: &str| {
        names
            .split(',')
            .filter_map(|name| match name.trim() {
                "minimize" => Some(WindowButton::Minimize),
                "maximize" => Some(WindowButton::Maximize),
                "close" => Some(WindowButton::Close),
                _ => None,
            })
            .collect()
    };
    let (start, end) = layout.split_once(':').unwrap_or(("", layout));
    WindowButtons {
        start: side(start),
        end: side(end),
    }
}

#[cfg(any(target_os = "linux", test))]
mod kde {
    use super::ColorScheme;

    #[derive(Debug, Default, PartialEq)]
    pub(super) struct Globals {
        pub accent: Option<String>,
        /// Family and size in points.
        pub font: Option<(String, f64)>,
        pub scheme: Option<ColorScheme>,
    }

    /// The accent is `[General] AccentColor`, or the colour scheme's
    /// selection colour when the accent follows the scheme; the font is
    /// `[General] font`, a Qt font string (`family,pointSize,…`); the
    /// scheme is the `[Colors:*]` groups.
    pub(super) fn parse(text: &str) -> Globals {
        let mut section = "";
        let mut accent = None;
        let mut selection = None;
        let mut font = None;
        let mut scheme = ColorScheme::default();
        let mut window_rgb = None;
        for line in text.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name;
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let color = || rgb(value).map(|c| super::hex(c[0], c[1], c[2]));
            match (section, key.trim()) {
                ("General", "AccentColor") => accent = color(),
                ("General", "font") => font = qt_font(value),
                ("Colors:Selection", "BackgroundNormal") => selection = color(),
                ("Colors:Selection", "ForegroundNormal") => scheme.selection_fg = color(),
                ("Colors:Window", "BackgroundNormal") => {
                    window_rgb = rgb(value);
                    scheme.window_bg = color();
                }
                ("Colors:Window", "ForegroundNormal") => scheme.window_fg = color(),
                ("Colors:Window", "ForegroundInactive") => scheme.window_fg_inactive = color(),
                ("Colors:View", "BackgroundNormal") => scheme.view_bg = color(),
                ("Colors:View", "ForegroundNormal") => scheme.view_fg = color(),
                ("Colors:View", "DecorationFocus") => scheme.focus = color(),
                ("Colors:Header", "BackgroundNormal") => scheme.header_bg = color(),
                ("Colors:Header][Inactive", "BackgroundNormal") => {
                    scheme.header_bg_inactive = color()
                }
                ("Colors:Header", "ForegroundNormal") => scheme.header_fg = color(),
                ("Colors:Button", "BackgroundNormal") => scheme.button_bg = color(),
                ("Colors:Button", "ForegroundNormal") => scheme.button_fg = color(),
                _ => {}
            }
        }
        let scheme = window_rgb.map(|[r, g, b]| ColorScheme {
            dark: 0.2126 * r + 0.7152 * g + 0.0722 * b < 0.5,
            ..scheme
        });
        Globals {
            accent: accent.or(selection),
            font,
            scheme,
        }
    }

    fn rgb(value: &str) -> Option<[f64; 3]> {
        let parts: Vec<f64> = value
            .split(',')
            .map(|p| p.trim().parse::<f64>().map(|v| v / 255.0))
            .collect::<Result<_, _>>()
            .ok()?;
        match parts[..] {
            [r, g, b] | [r, g, b, _] => Some([r, g, b]),
            _ => None,
        }
    }

    fn qt_font(value: &str) -> Option<(String, f64)> {
        let mut fields = value.split(',');
        let family = fields.next()?.trim();
        let points: f64 = fields.next()?.trim().parse().ok()?;
        (!family.is_empty() && points > 0.0).then(|| (family.to_owned(), points))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_window_background_in_both_appearances() {
        let bg = system_appearance()
            .window_bg
            .expect("AppKit resolves the window background");
        let is_hex = |c: &str| c.len() == 7 && c.starts_with('#');
        assert!(is_hex(&bg.light) && is_hex(&bg.dark), "{bg:?}");
        assert_ne!(bg.light, bg.dark);
    }

    #[test]
    fn kdeglobals_accent_font_and_scheme() {
        let globals = kde::parse(
            "[Colors:Header]\nBackgroundNormal=222,224,226\n\n\
             [Colors:Header][Inactive]\nBackgroundNormal=239,240,241\n\n\
             [Colors:Selection]\nBackgroundNormal=61,174,233\n\n\
             [Colors:Window]\nBackgroundNormal=239,240,241\nForegroundNormal=35,38,41\n\n\
             [General]\nAccentColor=233,100,58\nfont=Noto Sans,10,-1,5,400,0,0,0,0,0,0,0,0,0,0,1\n",
        );
        assert_eq!(globals.accent.as_deref(), Some("#e9643a"));
        assert_eq!(globals.font, Some(("Noto Sans".into(), 10.0)));
        let scheme = globals.scheme.unwrap();
        assert!(!scheme.dark);
        assert_eq!(scheme.header_bg.as_deref(), Some("#dee0e2"));
        assert_eq!(scheme.header_bg_inactive.as_deref(), Some("#eff0f1"));
        assert_eq!(scheme.window_fg.as_deref(), Some("#232629"));
        assert_eq!(scheme.view_bg, None);
    }

    #[test]
    fn kdeglobals_accent_follows_the_scheme_without_its_own() {
        let globals = kde::parse(
            "[Colors:Selection]\nBackgroundNormal=61,174,233\n\
             [Colors:Window]\nBackgroundNormal=32,35,38\n",
        );
        assert_eq!(globals.accent.as_deref(), Some("#3daee9"));
        assert!(globals.scheme.unwrap().dark);
        assert_eq!(globals.font, None);
    }

    #[test]
    fn libadwaita_accent_names() {
        assert_eq!(libadwaita_accent("teal"), Some("#2190a4"));
        assert_eq!(libadwaita_accent("chartreuse"), None);
    }

    #[test]
    fn gnome_button_layouts() {
        use WindowButton::*;
        assert_eq!(
            default_buttons(),
            WindowButtons {
                start: vec![],
                end: vec![Close],
            }
        );
        assert_eq!(
            button_layout("close,minimize,maximize:appmenu"),
            WindowButtons {
                start: vec![Close, Minimize, Maximize],
                end: vec![],
            }
        );
        assert_eq!(
            button_layout(":minimize,maximize,close"),
            WindowButtons {
                start: vec![],
                end: vec![Minimize, Maximize, Close],
            }
        );
        assert_eq!(
            button_layout("close"),
            WindowButtons {
                start: vec![],
                end: vec![Close],
            }
        );
    }
}
