//! Per-window options for the table (CSV/TSV) mode. Each option is `None`
//! ("auto") until picked in the Table menu; the frontend detects the
//! delimiter and header row from the file's first chunk and reports them
//! here, for the Auto entries to name what they resolved to.

use serde::Serialize;
use tauri::Wry;
use tauri::menu::{CheckMenuItem, IsMenuItem, PredefinedMenuItem, Submenu};

use crate::common::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum TableDelimiter {
    Comma,
    Semicolon,
    Tab,
    Pipe,
}

impl TableDelimiter {
    const ALL: [TableDelimiter; 4] = [
        TableDelimiter::Comma,
        TableDelimiter::Semicolon,
        TableDelimiter::Tab,
        TableDelimiter::Pipe,
    ];

    fn id(self) -> &'static str {
        match self {
            TableDelimiter::Comma => "comma",
            TableDelimiter::Semicolon => "semicolon",
            TableDelimiter::Tab => "tab",
            TableDelimiter::Pipe => "pipe",
        }
    }

    fn label(self) -> &'static str {
        match self {
            TableDelimiter::Comma => "Comma",
            TableDelimiter::Semicolon => "Semicolon",
            TableDelimiter::Tab => "Tab",
            TableDelimiter::Pipe => "Pipe",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.id() == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, specta::Type)]
pub struct TableOptions {
    pub delimiter: Option<TableDelimiter>,
    pub detected_delimiter: Option<TableDelimiter>,
    /// Honour `"` quoting (RFC 4180): quoted fields may hold delimiters,
    /// newlines and `""` for a quote.
    pub quoted: bool,
    /// Whether the first row is a header.
    pub header: Option<bool>,
    pub detected_header: Option<bool>,
}

impl Default for TableOptions {
    fn default() -> Self {
        Self {
            delimiter: None,
            detected_delimiter: None,
            quoted: true,
            header: None,
            detected_header: None,
        }
    }
}

impl TableOptions {
    /// Apply a Table menu item (its id without the window prefix and the
    /// `tbl_` tag). Returns false for an id that isn't one.
    pub fn apply_menu(&mut self, id: &str) -> bool {
        if let Some(delim) = id.strip_prefix("delim_") {
            self.delimiter = if delim == "auto" {
                None
            } else {
                match TableDelimiter::from_id(delim) {
                    Some(d) => Some(d),
                    None => return false,
                }
            };
            return true;
        }
        match id {
            "quoted" => self.quoted = !self.quoted,
            "header_auto" => self.header = None,
            "header_on" => self.header = Some(true),
            "header_off" => self.header = Some(false),
            _ => return false,
        }
        true
    }
}

pub fn table_submenu(
    app_handle: &tauri::AppHandle,
    prefix: &str,
    options: &TableOptions,
) -> Result<Submenu<Wry>, Error> {
    let radio = |id: &str, label: &str, active: bool| {
        super::radio_item(app_handle, format!("{prefix}tbl_{id}"), label, active)
    };

    let auto_delimiter = match options.detected_delimiter {
        Some(d) => format!("Auto-detect ({})", d.label()),
        None => "Auto-detect".to_string(),
    };
    let mut delimiter_items = vec![radio(
        "delim_auto",
        &auto_delimiter,
        options.delimiter.is_none(),
    )?];
    for d in TableDelimiter::ALL {
        delimiter_items.push(radio(
            &format!("delim_{}", d.id()),
            d.label(),
            options.delimiter == Some(d),
        )?);
    }
    let delimiter_refs: Vec<&dyn IsMenuItem<Wry>> =
        delimiter_items.iter().map(|i| i.as_ref()).collect();
    let delimiter_submenu = Submenu::with_items(app_handle, "Delimiter", true, &delimiter_refs)?;

    let auto_header = match options.detected_header {
        Some(true) => "Auto-detect (Yes)",
        Some(false) => "Auto-detect (No)",
        None => "Auto-detect",
    };
    let header_items = [
        radio("header_auto", auto_header, options.header.is_none())?,
        radio("header_on", "Yes", options.header == Some(true))?,
        radio("header_off", "No", options.header == Some(false))?,
    ];
    let header_refs: Vec<&dyn IsMenuItem<Wry>> = header_items.iter().map(|i| i.as_ref()).collect();
    let header_submenu =
        Submenu::with_items(app_handle, "First Row Is Header", true, &header_refs)?;

    let quoted_item = CheckMenuItem::with_id(
        app_handle,
        format!("{prefix}tbl_quoted"),
        "Quoted Fields",
        true,
        options.quoted,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app_handle)?;

    Ok(Submenu::with_items(
        app_handle,
        "Table",
        true,
        &[
            &delimiter_submenu,
            &header_submenu,
            &separator,
            &quoted_item,
        ],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_apply() {
        let mut o = TableOptions::default();
        assert!(o.apply_menu("delim_tab"));
        assert_eq!(o.delimiter, Some(TableDelimiter::Tab));
        assert!(o.apply_menu("delim_auto"));
        assert_eq!(o.delimiter, None);
        assert!(o.apply_menu("quoted"));
        assert!(!o.quoted);
        assert!(o.apply_menu("header_off"));
        assert_eq!(o.header, Some(false));
        assert!(o.apply_menu("header_auto"));
        assert_eq!(o.header, None);
        assert!(!o.apply_menu("delim_colon"));
        assert!(!o.apply_menu("nope"));
    }
}
