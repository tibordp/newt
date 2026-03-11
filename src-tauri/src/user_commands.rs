use newt_common::operation::{OperationRequest, StartOperationRequest};
use newt_common::vfs::VfsPath;
use shell_quote::Quote;

use crate::GlobalContext;
use crate::common::Error;
use crate::main_window::MainWindowContext;
use crate::main_window::OperationStatus;
use crate::main_window::PaneHandle;

// --- Template types ---

/// Template object for file data in minijinja templates.
#[derive(Debug)]
struct FileTemplateObject {
    name: String,
    path: String,
    ext: String,
    stem: String,
    is_dir: bool,
    size: Option<u64>,
    modified: Option<i128>,
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
    collected: Option<std::sync::Arc<std::sync::Mutex<CollectedInputs>>>,
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
                    .unwrap()
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
            collected.lock().unwrap().confirms.push(message);
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
    let dir = pane_path.path.to_string_lossy().to_string();

    let file_objects: Vec<minijinja::Value> = effective_files
        .iter()
        .map(|f| {
            let path = pane_path.path.join(&f.name);
            minijinja::Value::from_object(FileTemplateObject {
                name: f.name.clone(),
                path: path.to_string_lossy().to_string(),
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

    let hostname = nix::unistd::gethostname()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_default();

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

    let collected = std::sync::Arc::new(std::sync::Mutex::new(CollectedInputs::default()));
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
        .unwrap_or_else(|arc| std::sync::Mutex::new(arc.lock().unwrap().clone()))
        .into_inner()
        .unwrap();

    Ok((rendered, inputs))
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
    let other_dir = other_pane.path().path.to_string_lossy().to_string();

    let selection = pane.get_effective_selection();
    let selection_names: Vec<String> = selection
        .iter()
        .filter_map(|p| p.path.file_name().map(|n| n.to_string_lossy().to_string()))
        .collect();

    let file_list = pane.file_list();
    let effective_files: Vec<newt_common::filesystem::File> = file_list
        .files()
        .iter()
        .filter(|f| selection_names.contains(&f.name))
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
            let mut ops = ctx.operations().0.write();
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
            let mut ops = ctx.operations().0.write();
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
    let pane_dir = pane_path.path.clone();
    execute_rendered(&ctx, &uc.title, rendered, pane_dir, uc.terminal).await
}

#[tauri::command]
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

    let pane_dir = pane_path.path.clone();
    execute_rendered(&ctx, &uc.title, rendered, pane_dir, uc.terminal).await
}

#[tauri::command]
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
