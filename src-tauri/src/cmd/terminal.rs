use newt_common::terminal::TerminalHandle;
use shell_quote::Quote;

use crate::common::Error;
use crate::main_window::{MainWindowContext, PaneHandle};

#[tauri::command]
pub async fn cmd_send_to_terminal(
    ctx: MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let terminal = if let Some(terminal) = ctx.active_terminal() {
        ctx.with_update(|c| {
            let mut opts = c.display_options.0.write();
            opts.panes_focused = false;
            opts.terminal_panel_visible = true;
            Ok(())
        })?;
        terminal
    } else {
        let cwd = ctx.vfs_info()?.resolve_terminal_cwd(&pane.path());
        ctx.create_terminal(cwd.as_deref()).await?
    };

    let input: Vec<_> = pane
        .get_effective_selection()
        .iter()
        .filter_map(|p| {
            p.path
                .file_name()
                .map(shell_quote::Bash::quote)
                .map(|mut b: Vec<u8>| {
                    b.push(b' ');
                    b
                })
        })
        .flatten()
        .collect();

    terminal.input(input).await?;

    Ok(())
}

#[tauri::command]
pub async fn terminal_write(
    ctx: MainWindowContext,
    handle: TerminalHandle,
    data: Vec<u8>,
) -> Result<(), Error> {
    let term = ctx
        .terminals()
        .get(handle)
        .ok_or_else(|| Error::Custom("terminal does not exit".into()))?;
    term.input(data).await?;

    Ok(())
}

#[tauri::command]
pub async fn terminal_resize(
    ctx: MainWindowContext,
    handle: TerminalHandle,
    rows: u16,
    cols: u16,
) -> Result<(), Error> {
    let term = ctx
        .terminals()
        .get(handle)
        .ok_or_else(|| Error::Custom("terminal does not exit".into()))?;
    term.resize(rows, cols).await?;

    Ok(())
}

#[tauri::command]
pub fn terminal_focus(ctx: MainWindowContext, handle: TerminalHandle) -> Result<(), Error> {
    ctx.with_update(|gs| {
        let mut opts = gs.display_options.0.write();
        opts.active_terminal = Some(handle);
        opts.panes_focused = false;

        Ok(())
    })
}

#[tauri::command]
pub async fn cmd_create_terminal(
    ctx: MainWindowContext,
    _pane_handle: PaneHandle,
) -> Result<(), Error> {
    let cwd = match ctx.active_pane() {
        Some(p) => ctx.vfs_info()?.resolve_terminal_cwd(&p.path()),
        None => None,
    };
    ctx.create_terminal(cwd.as_deref()).await?;
    Ok(())
}

#[tauri::command]
pub fn close_terminal(ctx: MainWindowContext, handle: TerminalHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        c.terminals.remove(handle);
        let mut opts = c.display_options.0.write();
        if opts.active_terminal == Some(handle) {
            opts.active_terminal = c.terminals.first_handle();
        }
        if c.terminals.is_empty() {
            opts.terminal_panel_visible = false;
            opts.panes_focused = true;
        }
        Ok(())
    })
}

#[tauri::command]
pub async fn cmd_toggle_terminal_panel(
    ctx: MainWindowContext,
    _pane_handle: PaneHandle,
) -> Result<(), Error> {
    let visible = !ctx.terminals().is_empty()
        && ctx.with_update(|c| Ok(c.display_options.0.read().terminal_panel_visible))?;

    if visible {
        // Hide the panel, focus panes
        ctx.with_update(|c| {
            let mut opts = c.display_options.0.write();
            opts.terminal_panel_visible = false;
            opts.panes_focused = true;
            Ok(())
        })
    } else {
        // Show the panel — auto-create a terminal if none exist
        if ctx.terminals().is_empty() {
            let cwd = match ctx.active_pane() {
                Some(p) => ctx.vfs_info()?.resolve_terminal_cwd(&p.path()),
                None => None,
            };
            ctx.create_terminal(cwd.as_deref()).await?;
        } else {
            ctx.with_update(|c| {
                let mut opts = c.display_options.0.write();
                opts.terminal_panel_visible = true;
                opts.panes_focused = false;
                if opts.active_terminal.is_none() {
                    opts.active_terminal = c.terminals.first_handle();
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}

#[tauri::command]
pub fn activate_terminal(ctx: MainWindowContext, handle: TerminalHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        let mut opts = c.display_options.0.write();
        opts.active_terminal = Some(handle);
        opts.panes_focused = false;
        Ok(())
    })
}

#[tauri::command]
pub fn cmd_focus_panes(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        let mut opts = c.display_options.0.write();
        opts.panes_focused = true;
        Ok(())
    })
}

#[tauri::command]
pub async fn cmd_focus_terminal(
    ctx: MainWindowContext,
    _pane_handle: PaneHandle,
) -> Result<(), Error> {
    if ctx.terminals().is_empty() {
        let cwd = match ctx.active_pane() {
            Some(p) => ctx.vfs_info()?.resolve_terminal_cwd(&p.path()),
            None => None,
        };
        ctx.create_terminal(cwd.as_deref()).await?;
    } else {
        ctx.with_update(|c| {
            let mut opts = c.display_options.0.write();
            opts.terminal_panel_visible = true;
            opts.panes_focused = false;
            if opts.active_terminal.is_none() {
                opts.active_terminal = c.terminals.first_handle();
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[tauri::command]
pub fn cmd_next_terminal(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        let handles = c.terminals.handles_sorted();
        if handles.is_empty() {
            return Ok(());
        }
        let mut opts = c.display_options.0.write();
        let current = opts.active_terminal;
        let idx = current
            .and_then(|h| handles.iter().position(|&x| x == h))
            .map(|i| (i + 1) % handles.len())
            .unwrap_or(0);
        opts.active_terminal = Some(handles[idx]);
        Ok(())
    })
}

#[tauri::command]
pub fn cmd_prev_terminal(ctx: MainWindowContext, _pane_handle: PaneHandle) -> Result<(), Error> {
    ctx.with_update(|c| {
        let handles = c.terminals.handles_sorted();
        if handles.is_empty() {
            return Ok(());
        }
        let mut opts = c.display_options.0.write();
        let current = opts.active_terminal;
        let idx = current
            .and_then(|h| handles.iter().position(|&x| x == h))
            .map(|i| (i + handles.len() - 1) % handles.len())
            .unwrap_or(0);
        opts.active_terminal = Some(handles[idx]);
        Ok(())
    })
}
