use newt_common::operation::{OperationRequest, StartOperationRequest};
use newt_common::vfs::VfsPath;
use shell_quote::Quote;

use crate::GlobalContext;
use crate::common::Error;
use crate::main_window::MainWindowContext;
use crate::main_window::OperationStatus;
use crate::main_window::PaneHandle;

#[cfg(unix)]
fn host_hostname() -> String {
    nix::unistd::gethostname()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_default()
}

#[cfg(windows)]
fn host_hostname() -> String {
    use windows_sys::Win32::System::SystemInformation::{
        ComputerNamePhysicalDnsHostname, GetComputerNameExW,
    };
    // Probe size first (call returns 0 + sets `size` to required len when
    // buffer is too small), then issue the real call.
    let mut size: u32 = 0;
    unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            std::ptr::null_mut(),
            &mut size,
        );
    }
    if size == 0 {
        return String::new();
    }
    let mut buf = vec![0u16; size as usize];
    let ok =
        unsafe { GetComputerNameExW(ComputerNamePhysicalDnsHostname, buf.as_mut_ptr(), &mut size) };
    if ok == 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..size as usize])
}

// --- Template types ---

/// Template object for file data in minijinja templates.
#[derive(Debug)]
struct FileTemplateObject {
    name: String,
    path: String,
    /// For virtual entries that alias a real file in another location
    /// (e.g. a search hit), this is the underlying path. `None` for
    /// ordinary filesystem entries.
    source: Option<String>,
    ext: String,
    stem: String,
    is_dir: bool,
    size: Option<u64>,
    modified: Option<i64>,
}

impl std::fmt::Display for FileTemplateObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl minijinja::value::Object for FileTemplateObject {
    fn get_value(self: &std::sync::Arc<Self>, key: &minijinja::Value) -> Option<minijinja::Value> {
        match key.as_str()? {
            "name" => Some(minijinja::Value::from(self.name.clone())),
            "path" => Some(minijinja::Value::from(self.path.clone())),
            "source" => match &self.source {
                Some(s) => Some(minijinja::Value::from(s.clone())),
                None => Some(minijinja::Value::from(())),
            },
            "ext" => Some(minijinja::Value::from(self.ext.clone())),
            "stem" => Some(minijinja::Value::from(self.stem.clone())),
            "is_dir" => Some(minijinja::Value::from(self.is_dir)),
            "size" => match self.size {
                Some(s) => Some(minijinja::Value::from(s)),
                None => Some(minijinja::Value::from(())),
            },
            "modified" => match self.modified {
                Some(ns) => Some(minijinja::Value::from(ns / 1_000_000_000)),
                None => Some(minijinja::Value::from(())),
            },
            _ => None,
        }
    }
}

/// Environment variables accessor for minijinja templates.
#[derive(Debug)]
struct EnvObject;

impl std::fmt::Display for EnvObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<env>")
    }
}

impl minijinja::value::Object for EnvObject {
    fn get_value(self: &std::sync::Arc<Self>, key: &minijinja::Value) -> Option<minijinja::Value> {
        let key_str = key.as_str()?;
        match std::env::var(key_str) {
            Ok(val) => Some(minijinja::Value::from(val)),
            Err(_) => Some(minijinja::Value::from(())),
        }
    }
}

/// Collected prompt/confirm calls from a template scanning pass.
#[derive(Default, Clone)]
struct CollectedInputs {
    prompts: Vec<crate::main_window::UserCommandPrompt>,
    confirms: Vec<String>,
}

// --- Template engine setup ---

/// Set up the minijinja environment with all custom filters and functions.
fn setup_template_env(
    env: &mut minijinja::Environment<'_>,
    prompt_responses: Option<Vec<String>>,
    confirm_responses: Option<Vec<bool>>,
    collected: Option<std::sync::Arc<parking_lot::Mutex<CollectedInputs>>>,
) {
    // Filters
    env.add_filter("shell_quote", |value: String| -> String {
        shell_quote::Bash::quote(&value)
    });
    env.add_filter("basename", |value: String| -> String {
        std::path::Path::new(&value)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or(value)
    });
    env.add_filter("dirname", |value: String| -> String {
        std::path::Path::new(&value)
            .parent()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    env.add_filter("stem", |value: String| -> String {
        std::path::Path::new(&value)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or(value)
    });
    env.add_filter("ext", |value: String| -> String {
        std::path::Path::new(&value)
            .extension()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    env.add_filter(
        "regex_replace",
        |value: String, pattern: String, replacement: String| -> Result<String, minijinja::Error> {
            let re = regex::Regex::new(&pattern).map_err(|e| {
                minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    format!("invalid regex: {}", e),
                )
            })?;
            Ok(re.replace_all(&value, replacement.as_str()).to_string())
        },
    );
    env.add_filter("join_path", |parts: Vec<String>| -> String {
        let mut path = std::path::PathBuf::new();
        for part in parts {
            path.push(part);
        }
        path.to_string_lossy().to_string()
    });

    // Functions: prompt and confirm
    if let Some(responses) = prompt_responses {
        let counter = std::sync::atomic::AtomicUsize::new(0);
        env.add_function(
            "prompt",
            move |_label: String, _default: Option<String>| -> String {
                let i = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                responses.get(i).cloned().unwrap_or_default()
            },
        );
    } else if let Some(ref collected) = collected {
        let collected = collected.clone();
        env.add_function(
            "prompt",
            move |label: String, default: Option<String>| -> String {
                collected
                    .lock()
                    .prompts
                    .push(crate::main_window::UserCommandPrompt {
                        label,
                        default: default.unwrap_or_default(),
                    });
                String::new()
            },
        );
    }

    if let Some(responses) = confirm_responses {
        let counter = std::sync::atomic::AtomicUsize::new(0);
        env.add_function("confirm", move |_message: String| -> bool {
            let i = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            responses.get(i).copied().unwrap_or(true)
        });
    } else if let Some(ref collected) = collected {
        let collected = collected.clone();
        env.add_function("confirm", move |message: String| -> bool {
            collected.lock().confirms.push(message);
            true
        });
    }
}

// --- Template context and rendering ---

/// Build the template context (file objects, dir, env, hostname) for a user command.
fn build_template_context(
    pane_path: &VfsPath,
    effective_files: &[&newt_common::filesystem::File],
) -> (
    minijinja::Value,
    Vec<minijinja::Value>,
    String,
    String,
    minijinja::Value,
) {
    let dir = pane_path.path.as_wire_str().to_string();

    let file_objects: Vec<minijinja::Value> = effective_files
        .iter()
        .map(|f| {
            // Use `key()` for the in-VFS path component: for ordinary
            // entries it equals `name`, for virtual entries (e.g. nested
            // search hits) it's the unique identifier under the pane root.
            let path = pane_path.join(f.key()).path.as_wire_str().to_string();
            let source = f.source.as_ref().map(|p| p.path.as_wire_str().to_string());
            minijinja::Value::from_object(FileTemplateObject {
                name: f.name.clone(),
                path,
                source,
                ext: std::path::Path::new(&f.name)
                    .extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_default(),
                stem: std::path::Path::new(&f.name)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                is_dir: f.is_dir,
                size: f.size,
                modified: f.modified,
            })
        })
        .collect();

    let file_value = file_objects
        .first()
        .cloned()
        .unwrap_or(minijinja::Value::UNDEFINED);

    let hostname = host_hostname();

    let env_obj = minijinja::Value::from_object(EnvObject);

    (file_value, file_objects, dir, hostname, env_obj)
}

/// Render a user command template with the given prompt/confirm responses.
fn render_template(
    template_str: &str,
    pane_path: &VfsPath,
    other_dir: &str,
    effective_files: &[&newt_common::filesystem::File],
    prompt_responses: Option<&[String]>,
    confirm_responses: Option<&[bool]>,
) -> Result<(String, CollectedInputs), Error> {
    let (file_value, file_objects, dir, hostname, env_obj) =
        build_template_context(pane_path, effective_files);

    let collected = std::sync::Arc::new(parking_lot::Mutex::new(CollectedInputs::default()));
    let is_scanning = prompt_responses.is_none();

    let mut env = minijinja::Environment::new();
    setup_template_env(
        &mut env,
        prompt_responses.map(|s| s.to_vec()),
        confirm_responses.map(|s| s.to_vec()),
        if is_scanning {
            Some(collected.clone())
        } else {
            None
        },
    );
    env.add_template("cmd", template_str)
        .map_err(|e| Error::Custom(format!("template error: {}", e)))?;

    let template_ctx = minijinja::context! {
        dir => dir,
        other_dir => other_dir.to_string(),
        file => file_value,
        files => file_objects,
        hostname => hostname,
        env => env_obj,
    };

    let tmpl = env
        .get_template("cmd")
        .map_err(|e| Error::Custom(format!("template error: {}", e)))?;
    let rendered = tmpl
        .render(&template_ctx)
        .map_err(|e| Error::Custom(format!("template render error: {}", e)))?;

    let inputs = std::sync::Arc::try_unwrap(collected)
        .unwrap_or_else(|arc| parking_lot::Mutex::new(arc.lock().clone()))
        .into_inner();

    Ok((rendered, inputs))
}

/// Extract the per-pane keys for a selection. Each selected `VfsPath`
/// is constructed as `pane_path.path.join(key)` by the pane layer, so
/// stripping that prefix recovers the keys. Returning keys (rather than
/// basenames) preserves identity for synthetic VFSes like search results
/// where multiple entries can share the same `name`.
fn selection_keys(pane_path: &VfsPath, selection: &[VfsPath]) -> std::collections::HashSet<String> {
    selection
        .iter()
        .filter_map(|p| p.strip_prefix(pane_path))
        .map(|tail| tail.to_string())
        .collect()
}

// --- Execution ---

/// Collect pane context needed for user command template rendering.
/// Returns owned data so nothing borrows across await points.
fn collect_pane_context(
    ctx: &MainWindowContext,
    pane_handle: PaneHandle,
) -> Result<(VfsPath, String, Vec<newt_common::filesystem::File>), Error> {
    let pane = ctx.panes().get(pane_handle).unwrap();
    let pane_path = pane.path();
    let other_pane = ctx.with_update(|gs| Ok(gs.other_pane(pane_handle)))?;
    let other_dir = other_pane.path().path.as_wire_str().to_string();

    let selection = pane.get_effective_selection();
    let selection_keys = selection_keys(&pane_path, &selection);

    let file_list = pane.file_list();
    let effective_files: Vec<newt_common::filesystem::File> = file_list
        .files()
        .iter()
        .filter(|f| selection_keys.contains(f.key()))
        .cloned()
        .collect();

    Ok((pane_path, other_dir, effective_files))
}

/// Execute a rendered command (terminal or operation mode).
async fn execute_rendered(
    ctx: &MainWindowContext,
    title: &str,
    rendered: String,
    pane_path: std::path::PathBuf,
    terminal_mode: bool,
) -> Result<(), Error> {
    if terminal_mode {
        let terminal_client = ctx.terminal_client()?;
        let options = newt_common::terminal::TerminalOptions {
            working_dir: Some(pane_path),
            command: Some("sh".to_string()),
            args: Some(vec!["-c".to_string(), rendered]),
            ..Default::default()
        };

        let handle = terminal_client.create(options).await?;
        let terminal =
            crate::main_window::terminal::Terminal::from_handle(ctx.clone(), ctx.window(), handle);

        ctx.with_update(|s| {
            let terminal = s.terminals.insert(handle, terminal);
            let mut opts = s.display_options.0.write();
            opts.active_terminal = Some(terminal.handle);
            opts.panes_focused = false;
            opts.terminal_panel_visible = true;
            Ok(())
        })?;
    } else {
        let id = ctx.next_operation_id()?;

        {
            let mut ops = ctx.operations().state.write();
            ops.insert(
                id,
                crate::main_window::OperationState {
                    id,
                    kind: "command".to_string(),
                    description: title.to_string(),
                    total_bytes: None,
                    total_items: None,
                    bytes_done: 0,
                    items_done: 0,
                    current_item: String::new(),
                    status: OperationStatus::Scanning,
                    error: None,
                    issue: None,
                    backgrounded: false,
                    scanning_items: None,
                    scanning_bytes: None,
                },
            );
        }
        ctx.publish()?;

        let req = StartOperationRequest {
            id,
            request: OperationRequest::RunCommand {
                command: rendered,
                working_dir: Some(pane_path),
            },
        };
        if let Err(e) = ctx.operations_client()?.start_operation(req).await {
            let mut ops = ctx.operations().state.write();
            if let Some(op) = ops.get_mut(&id) {
                op.status = OperationStatus::Failed;
                op.error = Some(e.to_string());
            }
        }
        ctx.publish()?;
    }

    Ok(())
}

// --- Tauri commands ---

#[tauri::command]
#[specta::specta]
pub async fn run_user_command(
    ctx: MainWindowContext,
    global_ctx: tauri::State<'_, GlobalContext>,
    pane_handle: PaneHandle,
    index: usize,
) -> Result<(), Error> {
    let prefs = global_ctx.preferences().resolved();
    let uc = prefs
        .user_commands
        .get(index)
        .ok_or_else(|| Error::Custom(format!("user command index {} out of range", index)))?
        .clone();

    let (pane_path, other_dir, effective_files) = collect_pane_context(&ctx, pane_handle)?;
    let file_refs: Vec<&newt_common::filesystem::File> = effective_files.iter().collect();

    // Scanning pass: detect prompt() and confirm() calls
    let (rendered, inputs) =
        render_template(&uc.run, &pane_path, &other_dir, &file_refs, None, None)?;

    if !inputs.prompts.is_empty() || !inputs.confirms.is_empty() {
        // Need user input — open the input dialog
        ctx.with_update(|gs| {
            let mut modal = gs.modal.0.write();
            *modal = Some(crate::main_window::ModalData {
                kind: crate::main_window::ModalDataKind::UserCommandInput {
                    command_index: index,
                    command_title: uc.title.clone(),
                    prompts: inputs.prompts,
                    confirms: inputs.confirms,
                },
                context: crate::main_window::ModalContext {
                    pane_handle: Some(pane_handle),
                },
            });
            Ok(())
        })?;
        return Ok(());
    }

    // No prompts/confirms — close modal and execute directly
    ctx.with_update(|gs| {
        gs.close_modal();
        Ok(())
    })?;
    let pane_dir = newt_common::vfs::local::to_native(&pane_path.path);
    execute_rendered(&ctx, &uc.title, rendered, pane_dir, uc.terminal).await
}

#[tauri::command]
#[specta::specta]
pub async fn execute_user_command(
    ctx: MainWindowContext,
    global_ctx: tauri::State<'_, GlobalContext>,
    pane_handle: PaneHandle,
    index: usize,
    prompt_responses: Vec<String>,
    confirm_responses: Vec<bool>,
) -> Result<(), Error> {
    // Check if any confirm was declined
    if confirm_responses.iter().any(|&c| !c) {
        ctx.with_update(|gs| {
            gs.close_modal();
            Ok(())
        })?;
        return Ok(());
    }

    let prefs = global_ctx.preferences().resolved();
    let uc = prefs
        .user_commands
        .get(index)
        .ok_or_else(|| Error::Custom(format!("user command index {} out of range", index)))?
        .clone();

    let (pane_path, other_dir, effective_files) = collect_pane_context(&ctx, pane_handle)?;
    let file_refs: Vec<&newt_common::filesystem::File> = effective_files.iter().collect();

    let (rendered, _) = render_template(
        &uc.run,
        &pane_path,
        &other_dir,
        &file_refs,
        Some(&prompt_responses),
        Some(&confirm_responses),
    )?;

    ctx.with_update(|gs| {
        gs.close_modal();
        Ok(())
    })?;

    let pane_dir = newt_common::vfs::local::to_native(&pane_path.path);
    execute_rendered(&ctx, &uc.title, rendered, pane_dir, uc.terminal).await
}

#[tauri::command]
#[specta::specta]
pub fn add_user_command_entry(
    global_ctx: tauri::State<'_, GlobalContext>,
    entry: crate::preferences::schema::UserCommandEntry,
) -> Result<(), Error> {
    global_ctx
        .preferences()
        .add_user_command(&entry)
        .map_err(Error::Custom)
}

#[tauri::command]
#[specta::specta]
pub fn remove_user_command_entry(
    global_ctx: tauri::State<'_, GlobalContext>,
    index: usize,
) -> Result<(), Error> {
    global_ctx
        .preferences()
        .remove_user_command(index)
        .map_err(Error::Custom)
}

#[tauri::command]
#[specta::specta]
pub fn update_user_command_entry(
    global_ctx: tauri::State<'_, GlobalContext>,
    index: usize,
    entry: crate::preferences::schema::UserCommandEntry,
) -> Result<(), Error> {
    global_ctx
        .preferences()
        .update_user_command(index, &entry)
        .map_err(Error::Custom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use newt_common::filesystem::File;

    fn file_with(name: &str, key: Option<&str>, source: Option<VfsPath>) -> File {
        File {
            name: name.to_string(),
            key: key.map(|s| s.to_string()),
            source,
            ..Default::default()
        }
    }

    #[test]
    fn template_path_uses_key_for_nested_synthetic_entries() {
        // Two search hits that share a basename but live in different
        // subdirectories: their keys disambiguate, names do not.
        let pane = VfsPath::from_wire_str(newt_common::vfs::VfsId::ROOT, "/");
        let a = file_with(
            "foo.txt",
            Some("a/foo.txt"),
            Some(VfsPath::from_wire_str(
                newt_common::vfs::VfsId::ROOT,
                "/src/a/foo.txt",
            )),
        );
        let b = file_with(
            "foo.txt",
            Some("b/foo.txt"),
            Some(VfsPath::from_wire_str(
                newt_common::vfs::VfsId::ROOT,
                "/src/b/foo.txt",
            )),
        );
        let files = [&a, &b];

        let (_, file_objects, _, _, _) = build_template_context(&pane, &files);
        let paths: Vec<String> = file_objects
            .iter()
            .map(|v| v.get_attr("path").unwrap().as_str().unwrap().to_string())
            .collect();
        assert_eq!(paths, vec!["/a/foo.txt", "/b/foo.txt"]);
    }

    #[test]
    fn template_source_exposed_when_set() {
        let pane = VfsPath::from_wire_str(newt_common::vfs::VfsId::ROOT, "/");
        let f = file_with(
            "foo.txt",
            Some("a/foo.txt"),
            Some(VfsPath::from_wire_str(
                newt_common::vfs::VfsId::ROOT,
                "/src/a/foo.txt",
            )),
        );
        let files = [&f];

        let (_, file_objects, _, _, _) = build_template_context(&pane, &files);
        let source = file_objects[0].get_attr("source").unwrap();
        assert_eq!(source.as_str(), Some("/src/a/foo.txt"));
    }

    #[test]
    fn template_source_undefined_for_ordinary_entries() {
        let pane = VfsPath::from_wire_str(newt_common::vfs::VfsId::ROOT, "/home/user");
        let f = file_with("foo.txt", None, None);
        let files = [&f];

        let (_, file_objects, _, _, _) = build_template_context(&pane, &files);
        let source = file_objects[0].get_attr("source").unwrap();
        assert!(source.is_none(), "expected None, got {:?}", source);
    }

    #[test]
    fn selection_keys_strips_pane_prefix_preserving_subdirs() {
        // Search-VFS pane at root: selection paths are `/key` and the
        // key may itself contain subdirectory separators.
        let pane = VfsPath::from_wire_str(newt_common::vfs::VfsId::ROOT, "/");
        let selection = vec![
            VfsPath::from_wire_str(newt_common::vfs::VfsId::ROOT, "/a/foo.txt"),
            VfsPath::from_wire_str(newt_common::vfs::VfsId::ROOT, "/b/foo.txt"),
        ];
        let keys = selection_keys(&pane, &selection);
        assert_eq!(keys.len(), 2);
        assert!(keys.contains("a/foo.txt"));
        assert!(keys.contains("b/foo.txt"));
    }

    #[test]
    fn selection_keys_real_filesystem_pane() {
        let pane = VfsPath::from_wire_str(newt_common::vfs::VfsId::ROOT, "/home/user");
        let selection = vec![
            VfsPath::from_wire_str(newt_common::vfs::VfsId::ROOT, "/home/user/foo.txt"),
            VfsPath::from_wire_str(newt_common::vfs::VfsId::ROOT, "/home/user/bar.txt"),
        ];
        let keys = selection_keys(&pane, &selection);
        assert_eq!(keys.len(), 2);
        assert!(keys.contains("foo.txt"));
        assert!(keys.contains("bar.txt"));
    }
}
