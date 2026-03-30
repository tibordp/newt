use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use log::debug;
use tokio::io::AsyncRead;
use tokio::sync::mpsc;

use crate::file_reader::{FileChunk, FileDetails};
use crate::filesystem::File;
use crate::{Error, ErrorKind};

use super::{
    Breadcrumb, DisplayPathMatch, RegisteredDescriptor, Vfs, VfsChangeNotifier, VfsDescriptor,
};

// ---------------------------------------------------------------------------
// K8sVfsDescriptor
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct K8sVfsDescriptor;

impl VfsDescriptor for K8sVfsDescriptor {
    fn type_name(&self) -> &'static str {
        "k8s"
    }
    fn display_name(&self) -> &'static str {
        "Kubernetes"
    }
    fn auto_mount_request(&self) -> Option<super::MountRequest> {
        None
    }
    fn can_watch(&self) -> bool {
        true
    }
    fn can_read_sync(&self) -> bool {
        false
    }
    fn can_read_async(&self) -> bool {
        true
    }
    fn can_overwrite_sync(&self) -> bool {
        false
    }
    fn can_overwrite_async(&self) -> bool {
        false
    }
    fn can_create_directory(&self) -> bool {
        false
    }
    fn can_create_symlink(&self) -> bool {
        false
    }
    fn can_touch(&self) -> bool {
        false
    }
    fn can_truncate(&self) -> bool {
        false
    }
    fn can_set_metadata(&self) -> bool {
        false
    }
    fn can_remove(&self) -> bool {
        false
    }
    fn can_remove_tree(&self) -> bool {
        false
    }
    fn has_symlinks(&self) -> bool {
        true
    }
    fn can_stat_directories(&self) -> bool {
        false
    }
    fn can_fs_stats(&self) -> bool {
        false
    }
    fn can_rename(&self) -> bool {
        false
    }
    fn can_copy_within(&self) -> bool {
        false
    }
    fn can_hard_link(&self) -> bool {
        false
    }
    fn auto_refresh(&self) -> bool {
        false
    }

    fn format_path(&self, path: &Path, mount_meta: &[u8]) -> String {
        let context = String::from_utf8_lossy(mount_meta);
        let s = path.to_string_lossy();
        format!("k8s://{}{}", context, s)
    }

    fn breadcrumbs(&self, path: &Path, mount_meta: &[u8]) -> Vec<Breadcrumb> {
        let context = String::from_utf8_lossy(mount_meta);
        let mut crumbs = Vec::new();
        let s = path.to_string_lossy();
        let segments: Vec<&str> = s.split('/').filter(|s| !s.is_empty()).collect();

        crumbs.push(Breadcrumb {
            label: format!("k8s://{}/", context),
            nav_path: "/".to_string(),
        });

        let mut accumulated = String::new();
        for (i, seg) in segments.iter().enumerate() {
            accumulated.push('/');
            accumulated.push_str(seg);
            let is_last = i == segments.len() - 1;
            crumbs.push(Breadcrumb {
                label: if is_last {
                    seg.to_string()
                } else {
                    format!("{}/", seg)
                },
                nav_path: accumulated.clone(),
            });
        }

        crumbs
    }

    fn try_parse_display_path(&self, input: &str, mount_meta: &[u8]) -> Option<DisplayPathMatch> {
        let rest = input.strip_prefix("k8s://")?;
        let context = String::from_utf8_lossy(mount_meta);
        let after_ctx = rest.strip_prefix(context.as_ref())?;
        let path = if after_ctx.is_empty() || after_ctx == "/" {
            PathBuf::from("/")
        } else if after_ctx.starts_with('/') {
            PathBuf::from(after_ctx)
        } else {
            return None;
        };
        Some(DisplayPathMatch::exact(path))
    }

    fn mount_label(&self, mount_meta: &[u8]) -> Option<String> {
        let context = String::from_utf8_lossy(mount_meta);
        if context.is_empty() {
            None
        } else {
            Some(context.into_owned())
        }
    }
}

pub static K8S_VFS_DESCRIPTOR: K8sVfsDescriptor = K8sVfsDescriptor;
inventory::submit!(RegisteredDescriptor(&K8S_VFS_DESCRIPTOR));

// ---------------------------------------------------------------------------
// API resource discovery
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ApiResource {
    /// Plural name, e.g. "deployments"
    name: String,
    /// Short names, e.g. ["deploy"]
    #[allow(dead_code)]
    short_names: Vec<String>,
    /// API group, e.g. "apps" (empty for core/v1)
    group: String,
    /// API version, e.g. "v1"
    version: String,
    /// Whether this resource is namespaced
    namespaced: bool,
}

impl ApiResource {
    /// Fully qualified directory path component: `<group>/<version>/<plural>`
    /// For core resources (empty group): `v1/<plural>`
    fn qualified_dir(&self) -> PathBuf {
        if self.group.is_empty() {
            PathBuf::from(&self.version).join(&self.name)
        } else {
            PathBuf::from(&self.group)
                .join(&self.version)
                .join(&self.name)
        }
    }
}

// ---------------------------------------------------------------------------
// K8sVfs
// ---------------------------------------------------------------------------

pub struct K8sVfs {
    context: String,
    resources: Vec<ApiResource>,
    /// Map from short/plural name → qualified dir, for building symlinks.
    /// Only populated where the name is unambiguous.
    symlinks: HashMap<String, PathBuf>,
    notifier: VfsChangeNotifier,
}

impl K8sVfs {
    /// Connect to a Kubernetes cluster. If `context` is empty, the current
    /// kubeconfig default context is used.
    pub async fn connect(context: &str) -> Result<Self, Error> {
        let context = if context.is_empty() {
            get_current_context().await?
        } else {
            context.to_string()
        };

        let resources = discover_api_resources(&context).await?;
        let symlinks = build_symlinks(&resources);

        Ok(Self {
            context,
            resources,
            symlinks,
            notifier: VfsChangeNotifier::new(),
        })
    }
}

/// Get the current kubectl context name.
async fn get_current_context() -> Result<String, Error> {
    let output = tokio::process::Command::new("kubectl")
        .args(["config", "current-context"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| Error {
            kind: ErrorKind::Connection,
            message: format!("Failed to run kubectl: {}", e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error {
            kind: ErrorKind::Connection,
            message: format!("Failed to get current kubectl context: {}", stderr.trim()),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run kubectl and parse api-resources output.
async fn discover_api_resources(context: &str) -> Result<Vec<ApiResource>, Error> {
    let output = run_kubectl(
        context,
        &["api-resources", "--verbs=list,get", "-o", "wide"],
    )
    .await?;

    let mut resources = Vec::new();
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return Ok(resources);
    }

    // Parse header to find column positions
    let header = lines[0];
    let name_col = 0;
    let short_col = header.find("SHORTNAMES").unwrap_or(usize::MAX);
    let apiversion_col = header.find("APIVERSION").unwrap_or(usize::MAX);
    let namespaced_col = header.find("NAMESPACED").unwrap_or(usize::MAX);
    let kind_col = header.find("KIND").unwrap_or(usize::MAX);

    if apiversion_col == usize::MAX {
        return Err(Error::custom(
            "unexpected kubectl api-resources output format",
        ));
    }

    for line in &lines[1..] {
        if line.trim().is_empty() {
            continue;
        }

        let get_field = |start: usize, end: usize| -> &str {
            if start >= line.len() {
                return "";
            }
            let end = end.min(line.len());
            line[start..end].trim()
        };

        let name = get_field(name_col, short_col.min(apiversion_col));
        let short_names_str = if short_col < apiversion_col {
            get_field(short_col, apiversion_col)
        } else {
            ""
        };
        let apiversion = get_field(apiversion_col, namespaced_col.min(kind_col));
        let namespaced_str = if namespaced_col < kind_col {
            get_field(namespaced_col, kind_col)
        } else {
            ""
        };

        if name.is_empty() || apiversion.is_empty() {
            continue;
        }

        let (group, version) = if let Some(idx) = apiversion.find('/') {
            (
                apiversion[..idx].to_string(),
                apiversion[idx + 1..].to_string(),
            )
        } else {
            (String::new(), apiversion.to_string())
        };

        let short_names: Vec<String> = short_names_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let namespaced = namespaced_str.eq_ignore_ascii_case("true");

        resources.push(ApiResource {
            name: name.to_string(),
            short_names,
            group,
            version,
            namespaced,
        });
    }

    debug!(
        "k8s: discovered {} api-resources for context '{}'",
        resources.len(),
        context
    );

    Ok(resources)
}

/// Build symlink map: plural name → qualified path.
///
/// When a resource name appears in multiple API groups (e.g. `pods` in both
/// `v1` and `metrics.k8s.io/v1beta1`), we pick a preferred version:
/// core API group (empty group) wins, otherwise the most stable version wins.
fn build_symlinks(resources: &[ApiResource]) -> HashMap<String, PathBuf> {
    let mut by_name: HashMap<&str, Vec<&ApiResource>> = HashMap::new();
    for r in resources {
        by_name.entry(&r.name).or_default().push(r);
    }

    let mut symlinks = HashMap::new();
    for (name, candidates) in &by_name {
        let preferred = candidates.iter().min_by_key(|r| {
            // Sort key: (group priority, version instability)
            // Core group (empty) gets 0, everything else 1.
            // v1 gets 0, v1beta* gets 1, v1alpha* gets 2, anything else 3.
            let group_pri: u8 = if r.group.is_empty() { 0 } else { 1 };
            let version_pri: u8 = if r.version == "v1" {
                0
            } else if r.version.contains("beta") {
                1
            } else if r.version.contains("alpha") {
                2
            } else {
                0 // e.g. "v2" is stable
            };
            (group_pri, version_pri)
        });
        if let Some(r) = preferred {
            symlinks.insert(name.to_string(), r.qualified_dir());
        }
    }
    symlinks
}

/// Run a kubectl command and return stdout.
async fn run_kubectl(context: &str, args: &[&str]) -> Result<String, Error> {
    let mut cmd = tokio::process::Command::new("kubectl");
    cmd.arg("--context").arg(context);
    cmd.args(args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let output = cmd.output().await.map_err(|e| Error {
        kind: ErrorKind::Connection,
        message: format!("Failed to run kubectl: {}", e),
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error {
            kind: ErrorKind::Connection,
            message: format!("kubectl failed: {}", stderr.trim()),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Path parsing — maps VFS paths to k8s concepts
// ---------------------------------------------------------------------------

/// What a VFS path resolves to in the k8s model.
enum PathResolution<'a> {
    /// `/` — root, shows "cluster" and "namespaces" dirs
    Root,
    /// `/cluster` — cluster-scoped resource types
    ClusterRoot,
    /// `/namespaces` — list of namespaces
    NamespaceList,
    /// `/namespaces/<ns>` — resource types within a namespace
    Namespace(&'a str),
    /// A group or group/version intermediate directory
    IntermediateDir {
        namespaced: bool,
        prefix: Vec<&'a str>,
    },
    /// `/cluster/<group>/<version>/<resource>` or
    /// `/namespaces/<ns>/<group>/<version>/<resource>` — list of resources
    ResourceList {
        namespace: Option<&'a str>,
        resource: &'a ApiResource,
    },
    /// `.../resource/<name>.yaml` — a single resource
    Resource {
        namespace: Option<&'a str>,
        resource: &'a ApiResource,
        name: &'a str,
    },
    /// Not found
    NotFound,
}

impl K8sVfs {
    /// Resolve a VFS path, following symlinks internally.
    fn resolve_path<'a>(&'a self, path: &'a Path) -> PathResolution<'a> {
        let components: Vec<&str> = path
            .components()
            .filter_map(|c| match c {
                std::path::Component::Normal(s) => s.to_str(),
                _ => None,
            })
            .collect();

        match components.as_slice() {
            [] => PathResolution::Root,
            ["cluster"] => PathResolution::ClusterRoot,
            ["namespaces"] => PathResolution::NamespaceList,
            ["namespaces", ns] => PathResolution::Namespace(ns),

            // Cluster-scoped: /cluster/...
            ["cluster", rest @ ..] => self.resolve_resource_path(None, rest),
            // Namespaced: /namespaces/<ns>/...
            ["namespaces", ns, rest @ ..] => self.resolve_resource_path(Some(ns), rest),

            _ => PathResolution::NotFound,
        }
    }

    fn resolve_resource_path<'a>(
        &'a self,
        namespace: Option<&'a str>,
        components: &[&'a str],
    ) -> PathResolution<'a> {
        let namespaced = namespace.is_some();

        // If the first component is a symlink name, expand it and re-resolve.
        if let Some(first) = components.first()
            && let Some(target) = self.symlinks.get(*first)
        {
            let target_components: Vec<&str> = target
                .components()
                .filter_map(|c| match c {
                    std::path::Component::Normal(s) => s.to_str(),
                    _ => None,
                })
                .collect();
            // Check that this symlink applies to the right scope
            let target_matches = self
                .resources
                .iter()
                .any(|r| r.name == *first && r.namespaced == namespaced);
            if target_matches {
                let mut expanded = target_components;
                expanded.extend_from_slice(&components[1..]);
                return self.resolve_expanded(namespace, &expanded);
            }
        }

        self.resolve_expanded(namespace, components)
    }

    fn resolve_expanded<'a>(
        &'a self,
        namespace: Option<&'a str>,
        components: &[&'a str],
    ) -> PathResolution<'a> {
        let namespaced = namespace.is_some();

        match components {
            // 1 component: group or version directory
            [single] => {
                let has_children = self.resources.iter().any(|r| {
                    r.namespaced == namespaced
                        && (r.group == *single || (r.group.is_empty() && r.version == *single))
                });
                if has_children {
                    PathResolution::IntermediateDir {
                        namespaced,
                        prefix: vec![single],
                    }
                } else {
                    PathResolution::NotFound
                }
            }

            // 2 components: core resource list (v1/<resource>) or group/version dir
            [a, b] => {
                // Try as core resource list: v1/<resource>
                if let Some(r) = self.find_resource("", a, b, namespace) {
                    return PathResolution::ResourceList {
                        namespace,
                        resource: r,
                    };
                }
                // Try as group/version intermediate dir
                let has_children = self
                    .resources
                    .iter()
                    .any(|r| r.namespaced == namespaced && r.group == *a && r.version == *b);
                if has_children {
                    PathResolution::IntermediateDir {
                        namespaced,
                        prefix: vec![a, b],
                    }
                } else {
                    PathResolution::NotFound
                }
            }

            // 3 components: ambiguous — core resource item or grouped resource list
            [a, b, c] => {
                if c.ends_with(".yaml") {
                    // Core resource item: v1/<resource>/<name.yaml>
                    let name = c.strip_suffix(".yaml").unwrap_or(c);
                    if let Some(r) = self.find_resource("", a, b, namespace) {
                        PathResolution::Resource {
                            namespace,
                            resource: r,
                            name,
                        }
                    } else {
                        PathResolution::NotFound
                    }
                } else {
                    // Grouped resource list: <group>/<version>/<resource>
                    if let Some(r) = self.find_resource(a, b, c, namespace) {
                        PathResolution::ResourceList {
                            namespace,
                            resource: r,
                        }
                    } else {
                        PathResolution::NotFound
                    }
                }
            }

            // 4 components: grouped resource item
            [group, version, resource, name] => {
                let name = name.strip_suffix(".yaml").unwrap_or(name);
                if let Some(r) = self.find_resource(group, version, resource, namespace) {
                    PathResolution::Resource {
                        namespace,
                        resource: r,
                        name,
                    }
                } else {
                    PathResolution::NotFound
                }
            }

            _ => PathResolution::NotFound,
        }
    }

    fn find_resource<'a>(
        &'a self,
        group: &str,
        version: &str,
        name: &str,
        namespace: Option<&str>,
    ) -> Option<&'a ApiResource> {
        self.resources.iter().find(|r| {
            r.group == group
                && r.version == version
                && r.name == name
                && if namespace.is_some() {
                    r.namespaced
                } else {
                    !r.namespaced
                }
        })
    }

    /// List the group/version directory tree entries for a given scope.
    fn list_resource_type_dirs(&self, namespaced: bool) -> Vec<File> {
        let mut files = Vec::new();
        let mut seen_groups = std::collections::HashSet::new();

        for r in &self.resources {
            if r.namespaced != namespaced {
                continue;
            }

            let top_dir = if r.group.is_empty() {
                &r.version
            } else {
                &r.group
            };

            if seen_groups.insert(top_dir.to_string()) {
                files.push(dir_entry(top_dir));
            }
        }

        // Add symlinks for unambiguous plural names
        for (plural, target) in &self.symlinks {
            let target_namespaced = self
                .resources
                .iter()
                .any(|r| r.name == *plural && r.namespaced == namespaced);
            if target_namespaced {
                files.push(File {
                    name: plural.clone(),
                    size: None,
                    is_dir: true,
                    is_hidden: false,
                    is_symlink: true,
                    symlink_target: Some(target.clone()),
                    user: None,
                    group: None,
                    mode: None,
                    modified: None,
                    accessed: None,
                    created: None,
                });
            }
        }

        files
    }

    /// List sub-entries under a group or group/version directory.
    fn list_group_entries(&self, namespaced: bool, prefix: &[&str]) -> Vec<File> {
        let mut files = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for r in &self.resources {
            if r.namespaced != namespaced {
                continue;
            }

            match prefix {
                // Listing contents of a group dir → show version dirs
                [group] if r.group == *group => {
                    if seen.insert(r.version.clone()) {
                        files.push(dir_entry(&r.version));
                    }
                }
                // Core resource: version is the top dir → show resource plural dirs
                [version] if r.group.is_empty() && r.version == *version => {
                    files.push(dir_entry(&r.name));
                }
                // Listing contents of a group/version dir → show resource plural dirs
                [group, version] if r.group == *group && r.version == *version => {
                    files.push(dir_entry(&r.name));
                }
                _ => {}
            }
        }

        files
    }
}

fn dotdot_entry() -> File {
    File {
        name: "..".to_string(),
        size: None,
        is_dir: true,
        is_hidden: false,
        is_symlink: false,
        symlink_target: None,
        user: None,
        group: None,
        mode: None,
        modified: None,
        accessed: None,
        created: None,
    }
}

fn dir_entry(name: &str) -> File {
    File {
        name: name.to_string(),
        size: None,
        is_dir: true,
        is_hidden: false,
        is_symlink: false,
        symlink_target: None,
        user: None,
        group: None,
        mode: None,
        modified: None,
        accessed: None,
        created: None,
    }
}

fn yaml_file_entry(name: &str) -> File {
    File {
        name: format!("{}.yaml", name),
        size: None,
        is_dir: false,
        is_hidden: false,
        is_symlink: false,
        symlink_target: None,
        user: None,
        group: None,
        mode: None,
        modified: None,
        accessed: None,
        created: None,
    }
}

// ---------------------------------------------------------------------------
// Vfs implementation
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl Vfs for K8sVfs {
    fn descriptor(&self) -> &'static dyn VfsDescriptor {
        &K8S_VFS_DESCRIPTOR
    }

    fn mount_meta(&self) -> Vec<u8> {
        self.context.as_bytes().to_vec()
    }

    async fn list_files(
        &self,
        path: &Path,
        _batch_tx: Option<mpsc::Sender<Vec<File>>>,
    ) -> Result<Vec<File>, Error> {
        debug!("k8s: list_files {}", path.display());

        match self.resolve_path(path) {
            PathResolution::Root => Ok(vec![dir_entry("cluster"), dir_entry("namespaces")]),
            PathResolution::ClusterRoot => {
                let mut files = vec![dotdot_entry()];
                files.extend(self.list_resource_type_dirs(false));
                Ok(files)
            }
            PathResolution::NamespaceList => {
                let output = run_kubectl(
                    &self.context,
                    &[
                        "get",
                        "namespaces",
                        "-o",
                        "jsonpath={.items[*].metadata.name}",
                    ],
                )
                .await?;
                let mut files = vec![dotdot_entry()];
                let mut ns_files: Vec<File> = output.split_whitespace().map(dir_entry).collect();
                ns_files.sort_by(|a, b| a.name.cmp(&b.name));
                files.extend(ns_files);
                Ok(files)
            }
            PathResolution::Namespace(_ns) => {
                let mut files = vec![dotdot_entry()];
                files.extend(self.list_resource_type_dirs(true));
                Ok(files)
            }
            PathResolution::IntermediateDir { namespaced, prefix } => {
                let mut files = vec![dotdot_entry()];
                files.extend(self.list_group_entries(namespaced, &prefix));
                Ok(files)
            }
            PathResolution::ResourceList {
                namespace,
                resource,
            } => {
                let mut args = vec![
                    "get",
                    &resource.name,
                    "-o",
                    "jsonpath={.items[*].metadata.name}",
                ];
                let ns_flag;
                if let Some(ns) = namespace {
                    ns_flag = format!("--namespace={}", ns);
                    args.push(&ns_flag);
                }
                let output = run_kubectl(&self.context, &args).await?;
                let mut files = vec![dotdot_entry()];
                let mut res_files: Vec<File> =
                    output.split_whitespace().map(yaml_file_entry).collect();
                res_files.sort_by(|a, b| a.name.cmp(&b.name));
                files.extend(res_files);
                Ok(files)
            }
            PathResolution::Resource { .. } | PathResolution::NotFound => {
                Err(Error::custom("path not found"))
            }
        }
    }

    async fn poll_changes(&self, path: &Path) -> Result<(), Error> {
        self.notifier.watch(path).await;
        Ok(())
    }

    async fn fs_stats(&self, _path: &Path) -> Result<Option<crate::filesystem::FsStats>, Error> {
        Ok(None)
    }

    async fn file_details(&self, path: &Path) -> Result<FileDetails, Error> {
        match self.resolve_path(path) {
            PathResolution::Resource {
                namespace,
                resource,
                name,
            } => {
                let yaml = self.get_resource_yaml(namespace, resource, name).await?;
                Ok(FileDetails {
                    size: yaml.len() as u64,
                    mime_type: Some("text/yaml".to_string()),
                    is_dir: false,
                    is_symlink: false,
                    symlink_target: None,
                    user: None,
                    group: None,
                    mode: None,
                    modified: None,
                    accessed: None,
                    created: None,
                })
            }
            _ => Err(Error::not_supported()),
        }
    }

    async fn file_info(&self, path: &Path) -> Result<File, Error> {
        match self.resolve_path(path) {
            PathResolution::Resource { name, .. } => Ok(yaml_file_entry(name)),
            _ => Err(Error::custom("not a file")),
        }
    }

    async fn open_read_async(
        &self,
        path: &Path,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin>, Error> {
        match self.resolve_path(path) {
            PathResolution::Resource {
                namespace,
                resource,
                name,
            } => {
                let yaml = self.get_resource_yaml(namespace, resource, name).await?;
                Ok(Box::new(std::io::Cursor::new(yaml.into_bytes())))
            }
            _ => Err(Error::not_supported()),
        }
    }

    async fn read_range(&self, path: &Path, offset: u64, length: u64) -> Result<FileChunk, Error> {
        match self.resolve_path(path) {
            PathResolution::Resource {
                namespace,
                resource,
                name,
            } => {
                let yaml = self.get_resource_yaml(namespace, resource, name).await?;
                let total_size = yaml.len() as u64;
                let start = (offset as usize).min(yaml.len());
                let end = ((offset + length) as usize).min(yaml.len());
                Ok(FileChunk {
                    data: yaml.as_bytes()[start..end].to_vec(),
                    offset,
                    total_size,
                })
            }
            _ => Err(Error::not_supported()),
        }
    }
}

impl K8sVfs {
    async fn get_resource_yaml(
        &self,
        namespace: Option<&str>,
        resource: &ApiResource,
        name: &str,
    ) -> Result<String, Error> {
        let qualified = if resource.group.is_empty() {
            resource.name.clone()
        } else {
            format!("{}.{}", resource.name, resource.group)
        };
        let mut args = vec!["get", &qualified, name, "-o", "yaml"];
        let ns_flag;
        if let Some(ns) = namespace {
            ns_flag = format!("--namespace={}", ns);
            args.push(&ns_flag);
        }
        run_kubectl(&self.context, &args).await
    }
}
