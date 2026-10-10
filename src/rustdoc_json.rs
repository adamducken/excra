use crate::cli::FeatureSelection;
use crate::resolver::{DependencyContext, is_library_target, package_spec};
use cargo_metadata::{DependencyKind, Metadata, Package, Target};
use rustdoc_types::{Crate, FORMAT_VERSION};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use syn::parse::Parser;
use syn::spanned::Spanned;

const PINNED_TOOLCHAIN: &str = "nightly-2025-09-10";
const GENERATION_MARKER: &str = "excra managed generation\n";
pub(crate) const CFG_UNAVAILABLE_ATTRIBUTE: &str = "#[excra_cfg_unavailable]";

pub(crate) fn selected_toolchain() -> String {
    env::var("EXCRA_TOOLCHAIN").unwrap_or_else(|_| PINNED_TOOLCHAIN.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CargoTargetSelection {
    pub(crate) toolchain: String,
    pub(crate) effective_triple: String,
    pub(crate) cargo_platform: Option<String>,
    pub(crate) command_line_override: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CargoUnitIdentity {
    pub(crate) features: Vec<String>,
    pub(crate) mode: String,
    pub(crate) platform: Option<String>,
    pub(crate) profile: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CargoUnitSelection {
    pub(crate) identity: CargoUnitIdentity,
    pub(crate) graph_index: usize,
    pub(crate) root_prerequisites: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct CargoParentUnit {
    pub(crate) package_id: String,
    pub(crate) graph_index: usize,
}

pub(crate) struct RustdocRequest<'a> {
    pub(crate) manifest_path: PathBuf,
    pub(crate) metadata: &'a Metadata,
    pub(crate) root_package: &'a Package,
    pub(crate) package: &'a Package,
    pub(crate) target: &'a Target,
    pub(crate) contexts: &'a [DependencyContext],
    pub(crate) target_selection: &'a CargoTargetSelection,
    pub(crate) feature_selection: &'a FeatureSelection,
    pub(crate) unit: &'a CargoUnitIdentity,
    pub(crate) root_prerequisites: &'a [PathBuf],
}

pub(crate) struct CargoUnitRequest<'a> {
    pub(crate) manifest_path: &'a Path,
    pub(crate) root_package: &'a Package,
    pub(crate) package: &'a Package,
    pub(crate) target: &'a Target,
    pub(crate) contexts: &'a [DependencyContext],
    pub(crate) parent: Option<&'a CargoParentUnit>,
    pub(crate) target_selection: &'a CargoTargetSelection,
    pub(crate) feature_selection: &'a FeatureSelection,
}

pub(crate) struct GenerationSession {
    generation_root: PathBuf,
    target_dir: PathBuf,
    next_unit: usize,
    unit_graphs: HashMap<(PathBuf, Vec<OsString>), UnitGraph>,
    _lock: JsonGenerationLock,
}

impl GenerationSession {
    pub(crate) fn start(metadata: &Metadata) -> Result<Self, String> {
        let generation_root = prepare_generation_root(metadata.target_directory.as_std_path())?;
        let lock = JsonGenerationLock::acquire(generation_root.join("generation.lock"))?;
        let target_dir = reset_generation_target_dir(&generation_root)?;
        Ok(Self {
            generation_root,
            target_dir,
            next_unit: 0,
            unit_graphs: HashMap::new(),
            _lock: lock,
        })
    }

    fn next_target_dir(&mut self, metadata: &Metadata) -> Result<PathBuf, String> {
        let requested_root = prepare_generation_root(metadata.target_directory.as_std_path())?;
        if requested_root != self.generation_root {
            return Err(format!(
                "cannot retain rustdoc JSON from target directories {} and {} in one generation session",
                self.generation_root.display(),
                requested_root.display()
            ));
        }
        let target_dir = self.target_dir.join(format!("unit-{}", self.next_unit));
        self.next_unit += 1;
        Ok(target_dir)
    }
}

pub(crate) fn load_or_generate(
    generation: &mut GenerationSession,
    mut request: RustdocRequest<'_>,
) -> Result<(Crate, PathBuf), String> {
    request.manifest_path = request.manifest_path.canonicalize().map_err(|err| {
        format!(
            "failed to resolve manifest path {}: {err}",
            request.manifest_path.display()
        )
    })?;
    let target_dir = generation.next_target_dir(request.metadata)?;
    let mut doc_dir = target_dir.clone();
    if let Some(platform) = &request.unit.platform {
        doc_dir.push(platform);
    }
    let json_path = doc_dir
        .join("doc")
        .join(format!("{}.json", request.target.name.replace('-', "_")));
    generate_json(
        &request,
        &target_dir,
        json_path.parent().expect("JSON path has doc directory"),
    )?;
    let mut krate = load_valid_json(&json_path, request.package)?;
    let compiler_directory = load_import_probe(&json_path)?.directory;
    let cfg = load_rustc_cfg(&json_path.with_extension("cfg"))?;
    normalize_span_paths(&mut krate, &compiler_directory);
    apply_non_doc_cfg(&mut krate, &cfg)?;
    strip_private_fields(&mut krate);
    Ok((krate, json_path))
}

fn prepare_generation_root(target_directory: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(target_directory).map_err(|err| {
        format!(
            "failed to create Cargo target directory {}: {err}",
            target_directory.display()
        )
    })?;
    let target_directory = target_directory.canonicalize().map_err(|err| {
        format!(
            "failed to resolve Cargo target directory {}: {err}",
            target_directory.display()
        )
    })?;
    let generation_root = target_directory.join("excra");
    match fs::symlink_metadata(&generation_root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(format!(
                "refusing to use non-directory or symlink excra managed root {}",
                generation_root.display()
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Err(err) = fs::create_dir(&generation_root) {
                if err.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(format!(
                        "failed to create excra managed root {}: {err}",
                        generation_root.display()
                    ));
                }
                let metadata = fs::symlink_metadata(&generation_root).map_err(|err| {
                    format!(
                        "failed to inspect concurrently created excra managed root {}: {err}",
                        generation_root.display()
                    )
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "refusing to use non-directory or symlink excra managed root {}",
                        generation_root.display()
                    ));
                }
            }
        }
        Err(err) => {
            return Err(format!(
                "failed to inspect excra managed root {}: {err}",
                generation_root.display()
            ));
        }
    }
    let resolved_root = generation_root.canonicalize().map_err(|err| {
        format!(
            "failed to resolve excra managed root {}: {err}",
            generation_root.display()
        )
    })?;
    if resolved_root.parent() != Some(target_directory.as_path()) {
        return Err(format!(
            "refusing to use excra managed root {} outside Cargo target directory {}",
            resolved_root.display(),
            target_directory.display()
        ));
    }
    Ok(resolved_root)
}

fn reset_generation_target_dir(generation_root: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(generation_root).map_err(|err| {
        format!(
            "failed to create excra generation root {}: {err}",
            generation_root.display()
        )
    })?;
    let target_dir = generation_root.join("generation");
    let target_exists = match fs::symlink_metadata(&target_dir) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(format!(
                    "refusing to replace unowned excra generation path {}",
                    target_dir.display()
                ));
            }
            let marker = target_dir.join(".excra-generation");
            let marker_metadata = fs::symlink_metadata(&marker).map_err(|err| {
                format!(
                    "refusing to replace unowned excra generation directory {}: failed to inspect ownership marker {}: {err}",
                    target_dir.display(),
                    marker.display()
                )
            })?;
            if marker_metadata.file_type().is_symlink() || !marker_metadata.is_file() {
                return Err(format!(
                    "refusing to replace unowned excra generation directory {}: ownership marker {} is not a regular non-symlink file",
                    target_dir.display(),
                    marker.display()
                ));
            }
            let contents = fs::read_to_string(&marker).map_err(|err| {
                format!(
                    "refusing to replace unowned excra generation directory {}: failed to read ownership marker {}: {err}",
                    target_dir.display(),
                    marker.display()
                )
            })?;
            if contents != GENERATION_MARKER {
                return Err(format!(
                    "refusing to replace unowned excra generation directory {}: invalid ownership marker",
                    target_dir.display()
                ));
            }
            true
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            return Err(format!(
                "failed to inspect managed generation directory {}: {err}",
                target_dir.display()
            ));
        }
    };

    cleanup_owned_generation_staging(generation_root)?;
    let staging = create_marked_generation_staging(generation_root)?;
    if target_exists {
        fs::remove_dir_all(&target_dir).map_err(|err| {
            format!(
                "failed to reset managed excra generation directory {}: {err}",
                target_dir.display()
            )
        })?;
    }
    fs::rename(&staging, &target_dir).map_err(|err| {
        format!(
            "failed to atomically install managed excra generation directory {} from {}: {err}",
            target_dir.display(),
            staging.display()
        )
    })?;
    Ok(target_dir)
}

fn cleanup_owned_generation_staging(generation_root: &Path) -> Result<(), String> {
    let entries = fs::read_dir(generation_root).map_err(|err| {
        format!(
            "failed to inspect excra generation root {}: {err}",
            generation_root.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|err| {
            format!(
                "failed to inspect an entry in excra generation root {}: {err}",
                generation_root.display()
            )
        })?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("generation.staging-") {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let marker = path.join(".excra-generation");
        let Ok(marker_metadata) = fs::symlink_metadata(&marker) else {
            continue;
        };
        if marker_metadata.file_type().is_symlink() || !marker_metadata.is_file() {
            continue;
        }
        if !fs::read_to_string(&marker).is_ok_and(|contents| contents == GENERATION_MARKER) {
            continue;
        }
        fs::remove_dir_all(&path).map_err(|err| {
            format!(
                "failed to remove interrupted managed generation staging directory {}: {err}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn create_marked_generation_staging(generation_root: &Path) -> Result<PathBuf, String> {
    for index in 0_u64.. {
        let staging = generation_root.join(format!("generation.staging-{index}"));
        match fs::create_dir(&staging) {
            Ok(()) => {
                if let Err(err) = fs::write(staging.join(".excra-generation"), GENERATION_MARKER) {
                    let _ = fs::remove_file(staging.join(".excra-generation"));
                    let _ = fs::remove_dir(&staging);
                    return Err(format!(
                        "failed to write ownership marker in generation staging directory {}: {err}",
                        staging.display()
                    ));
                }
                return Ok(staging);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => {
                return Err(format!(
                    "failed to create managed generation staging directory {}: {err}",
                    staging.display()
                ));
            }
        }
    }
    unreachable!("the generation staging index space is not exhausted")
}

#[derive(Debug)]
struct JsonGenerationLock {
    _file: File,
}

impl JsonGenerationLock {
    fn acquire(path: PathBuf) -> Result<Self, String> {
        Self::acquire_until(path, None)
    }

    #[cfg(test)]
    fn acquire_with_timeout(path: PathBuf, wait_timeout: Duration) -> Result<Self, String> {
        Self::acquire_until(path, Some(Instant::now() + wait_timeout))
    }

    fn acquire_until(path: PathBuf, deadline: Option<Instant>) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                format!(
                    "failed to create rustdoc JSON lock directory {}: {err}",
                    parent.display()
                )
            })?;
        }
        loop {
            let file = open_lock_file(&path)?;
            match file.try_lock() {
                Ok(()) => {
                    return Ok(Self { _file: file });
                }
                Err(TryLockError::WouldBlock) => {
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err(lock_timeout_message(&path));
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                Err(TryLockError::Error(err)) => {
                    return Err(format!(
                        "failed to acquire rustdoc JSON lock {}: {err}",
                        path.display()
                    ));
                }
            }
        }
    }
}

fn open_lock_file(path: &Path) -> Result<File, String> {
    let file = match File::create_new(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_lock_path(path)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|error| {
                    format!(
                        "failed to open rustdoc JSON lock {}: {error}",
                        path.display()
                    )
                })?
        }
        Err(error) => {
            return Err(format!(
                "failed to create rustdoc JSON lock {}: {error}",
                path.display()
            ));
        }
    };
    let path_metadata = validate_lock_path(path)?;
    let file_metadata = file.metadata().map_err(|error| {
        format!(
            "failed to inspect open rustdoc JSON lock {}: {error}",
            path.display()
        )
    })?;
    if !file_metadata.is_file() {
        return Err(invalid_lock_path_message(path));
    }
    validate_lock_identity(path, &path_metadata, &file_metadata)?;
    Ok(file)
}

fn validate_lock_path(path: &Path) -> Result<fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "failed to inspect rustdoc JSON lock {}: {error}",
            path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(invalid_lock_path_message(path));
    }
    Ok(metadata)
}

fn invalid_lock_path_message(path: &Path) -> String {
    format!(
        "refusing to use non-regular or symlink rustdoc JSON lock path {}",
        path.display()
    )
}

#[cfg(unix)]
fn validate_lock_identity(
    path: &Path,
    path_metadata: &fs::Metadata,
    file_metadata: &fs::Metadata,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;

    if path_metadata.dev() != file_metadata.dev() || path_metadata.ino() != file_metadata.ino() {
        return Err(format!(
            "rustdoc JSON lock path {} changed while it was being opened",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_lock_identity(
    _path: &Path,
    _path_metadata: &fs::Metadata,
    _file_metadata: &fs::Metadata,
) -> Result<(), String> {
    Ok(())
}

fn lock_timeout_message(path: &Path) -> String {
    format!(
        "timed out waiting for the active rustdoc JSON lock {}",
        path.display()
    )
}

fn normalize_span_paths(krate: &mut Crate, invocation_dir: &Path) {
    for item in krate.index.values_mut() {
        let Some(span) = item.span.as_mut() else {
            continue;
        };
        if span.filename.is_relative() {
            span.filename = invocation_dir.join(&span.filename);
        }
    }
}

fn strip_private_fields(krate: &mut Crate) {
    use rustdoc_types::{ItemEnum, StructKind, Visibility};

    let private_fields = krate
        .index
        .iter()
        .filter_map(|(id, item)| {
            (matches!(item.inner, ItemEnum::StructField(_))
                && !matches!(item.visibility, Visibility::Public | Visibility::Default))
            .then_some(*id)
        })
        .collect::<HashSet<_>>();
    for item in krate.index.values_mut() {
        let (fields, stripped) = match &mut item.inner {
            ItemEnum::Struct(struct_) => match &mut struct_.kind {
                StructKind::Tuple(fields) => {
                    for field in fields {
                        if field.is_some_and(|id| private_fields.contains(&id)) {
                            *field = None;
                        }
                    }
                    continue;
                }
                StructKind::Plain {
                    fields,
                    has_stripped_fields,
                } => (fields, has_stripped_fields),
                StructKind::Unit => continue,
            },
            ItemEnum::Union(union_) => (&mut union_.fields, &mut union_.has_stripped_fields),
            _ => continue,
        };
        fields.retain(|id| {
            if private_fields.contains(id) {
                *stripped = true;
                false
            } else {
                true
            }
        });
    }
}

fn load_valid_json(path: &PathBuf, package: &Package) -> Result<Crate, String> {
    let file = File::open(path)
        .map_err(|err| format!("rustdoc JSON missing at {}: {err}", path.display()))?;
    let krate: Crate = serde_json::from_reader(file)
        .map_err(|err| format!("failed to parse rustdoc JSON {}: {err}", path.display()))?;
    if krate.format_version != FORMAT_VERSION {
        return Err(format!(
            "rustdoc JSON format {} unsupported; supported: {}",
            krate.format_version, FORMAT_VERSION
        ));
    }
    if let Some(version) = &krate.crate_version
        && version != &package.version.to_string()
    {
        return Err(format!(
            "rustdoc JSON stale for {}: found version {}, expected {}",
            package.name, version, package.version
        ));
    }
    Ok(krate)
}

#[derive(Clone, Debug, Default)]
struct RustcCfg {
    flags: HashSet<String>,
    values: HashSet<(String, String)>,
    version_matches: HashMap<String, bool>,
}

impl RustcCfg {
    fn parse(output: &[u8]) -> Self {
        let mut cfg = Self::default();
        for line in String::from_utf8_lossy(output).lines().map(str::trim) {
            if let Some((name, value)) = line.split_once('=') {
                if let Ok(value) = syn::parse_str::<syn::LitStr>(value) {
                    cfg.values.insert((name.to_string(), value.value()));
                }
            } else if !line.is_empty() {
                cfg.flags.insert(line.to_string());
            }
        }
        cfg
    }

    fn version_matches(&self, tokens: &proc_macro2::TokenStream) -> Result<bool, String> {
        let versions = syn::punctuated::Punctuated::<syn::LitStr, syn::Token![,]>::parse_terminated
            .parse2(tokens.clone())
            .map_err(|error| format!("cannot evaluate cfg(version({tokens})): {error}"))?;
        if versions.len() != 1 {
            return Err("cfg(version(...)) requires exactly one version string".into());
        }
        let key = versions[0].value();
        self.version_matches.get(&key).copied().ok_or_else(|| {
            format!("cfg(version({tokens})) was not captured from the selected wrapped compiler")
        })
    }

    fn contains_flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    fn contains_value(&self, name: &str, value: &str) -> bool {
        self.values.contains(&(name.to_string(), value.to_string()))
    }
}

fn load_rustc_cfg(path: &Path) -> Result<RustcCfg, String> {
    let output = fs::read(path).map_err(|err| {
        format!(
            "non-doc rustc cfg output missing at {}: {err}",
            path.display()
        )
    })?;
    let mut cfg = RustcCfg::parse(&output);
    cfg.version_matches = serde_json::from_slice(
        &fs::read(path.with_extension("versions")).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(cfg)
}

pub(crate) fn reject_non_doc_only_source(
    krate: &Crate,
    target: &Target,
    json_path: &Path,
    import: &crate::imports::ImportPath,
) -> Result<Vec<String>, String> {
    let cfg = load_rustc_cfg(&json_path.with_extension("cfg"))?;
    let mut sources = krate
        .index
        .values()
        .filter(|item| item.crate_id == 0)
        .filter_map(|item| item.span.as_ref().map(|span| span.filename.clone()))
        .collect::<HashSet<_>>();
    sources.insert(target.src_path.as_std_path().to_path_buf());
    let mut sources = sources.into_iter().collect::<Vec<_>>();
    sources.sort();
    let mut doc_cfg = cfg.clone();
    doc_cfg.flags.insert("doc".to_string());

    fn attribute_is_non_doc_only(
        meta: &syn::Meta,
        normal: &RustcCfg,
        documentation: &RustcCfg,
    ) -> Result<bool, String> {
        Ok(
            cfg_attr_output_matches(meta, normal)?
                && !cfg_attr_output_matches(meta, documentation)?,
        )
    }

    fn non_doc_only_attribute(
        stream: proc_macro2::TokenStream,
        normal: &RustcCfg,
        documentation: &RustcCfg,
    ) -> Result<Option<usize>, String> {
        use proc_macro2::{Delimiter, TokenTree};
        let mut tokens = stream.into_iter().peekable();
        while let Some(token) = tokens.next() {
            match token {
                TokenTree::Punct(pound) if pound.as_char() == '#' => {
                    if matches!(tokens.peek(), Some(TokenTree::Punct(bang)) if bang.as_char() == '!')
                    {
                        tokens.next();
                    }
                    if matches!(tokens.peek(), Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Bracket)
                        && let Some(TokenTree::Group(group)) = tokens.next()
                        && let Ok(meta) = syn::parse2::<syn::Meta>(group.stream())
                        && attribute_is_non_doc_only(&meta, normal, documentation)?
                    {
                        return Ok(Some(pound.span().start().line));
                    }
                }
                TokenTree::Group(group) => {
                    if let Some(line) =
                        non_doc_only_attribute(group.stream(), normal, documentation)?
                    {
                        return Ok(Some(line));
                    }
                }
                _ => {}
            }
        }
        Ok(None)
    }

    struct Affected<'a> {
        normal: &'a RustcCfg,
        documentation: &'a RustcCfg,
        wanted: &'a [Vec<String>],
        module_path: Vec<String>,
        owner: Option<Vec<String>>,
        owner_is_container: bool,
        line: Option<usize>,
        error: Option<String>,
    }
    fn item_path(module: &[String], name: &str) -> Vec<String> {
        let mut path = module.to_vec();
        path.push(crate::imports::identifier_key(name).to_string());
        path
    }
    fn impl_type_path(module: &[String], ty: &syn::Type) -> Option<Vec<String>> {
        let syn::Type::Path(ty) = ty else {
            return None;
        };
        let mut segments =
            ty.path.segments.iter().map(|segment| {
                crate::imports::identifier_key(&segment.ident.to_string()).to_string()
            });
        let first = segments.next()?;
        let mut path = match first.as_str() {
            "crate" => Vec::new(),
            "self" => module.to_vec(),
            "super" => {
                let mut path = module.to_vec();
                path.pop();
                path
            }
            _ => {
                let mut path = module.to_vec();
                path.push(first);
                path
            }
        };
        for segment in segments {
            if segment == "super" {
                path.pop();
            } else {
                path.push(segment);
            }
        }
        Some(path)
    }
    fn use_tree_may_bind(tree: &syn::UseTree, name: &str) -> bool {
        match tree {
            syn::UseTree::Path(path) => use_tree_may_bind(&path.tree, name),
            syn::UseTree::Name(binding) => {
                crate::imports::identifier_key(&binding.ident.to_string())
                    == crate::imports::identifier_key(name)
            }
            syn::UseTree::Rename(binding) => {
                crate::imports::identifier_key(&binding.rename.to_string())
                    == crate::imports::identifier_key(name)
            }
            syn::UseTree::Glob(_) => true,
            syn::UseTree::Group(group) => {
                group.items.iter().any(|item| use_tree_may_bind(item, name))
            }
        }
    }
    impl<'ast> syn::visit::Visit<'ast> for Affected<'_> {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            let previous = self.owner.clone();
            let previous_is_container = self.owner_is_container;
            let module_len = self.module_path.len();
            self.owner_is_container = matches!(
                item,
                syn::Item::Mod(_)
                    | syn::Item::Enum(_)
                    | syn::Item::Trait(_)
                    | syn::Item::Struct(_)
                    | syn::Item::Union(_)
                    | syn::Item::Impl(_)
                    | syn::Item::ForeignMod(_)
            );
            self.owner = match item {
                syn::Item::Struct(item) => {
                    Some(item_path(&self.module_path, &item.ident.to_string()))
                }
                syn::Item::Enum(item) => {
                    Some(item_path(&self.module_path, &item.ident.to_string()))
                }
                syn::Item::Union(item) => {
                    Some(item_path(&self.module_path, &item.ident.to_string()))
                }
                syn::Item::Trait(item) => {
                    Some(item_path(&self.module_path, &item.ident.to_string()))
                }
                syn::Item::Type(item) => {
                    Some(item_path(&self.module_path, &item.ident.to_string()))
                }
                syn::Item::Fn(item) => {
                    Some(item_path(&self.module_path, &item.sig.ident.to_string()))
                }
                syn::Item::Const(item) => {
                    Some(item_path(&self.module_path, &item.ident.to_string()))
                }
                syn::Item::Static(item) => {
                    Some(item_path(&self.module_path, &item.ident.to_string()))
                }
                syn::Item::Mod(item) => Some(item_path(&self.module_path, &item.ident.to_string())),
                syn::Item::Use(item) => self
                    .wanted
                    .iter()
                    .find(|wanted| {
                        wanted.starts_with(&self.module_path)
                            && wanted.len() == self.module_path.len() + 1
                            && use_tree_may_bind(&item.tree, wanted.last().unwrap())
                    })
                    .cloned(),
                syn::Item::ExternCrate(item) => Some(item_path(
                    &self.module_path,
                    &item
                        .rename
                        .as_ref()
                        .map(|(_, ident)| ident)
                        .unwrap_or(&item.ident)
                        .to_string(),
                )),
                syn::Item::Macro(item) => item
                    .ident
                    .as_ref()
                    .map(|ident| item_path(&self.module_path, &ident.to_string()))
                    .or_else(|| {
                        self.wanted
                            .iter()
                            .find(|wanted| {
                                wanted.starts_with(&self.module_path)
                                    && macro_contains_name(
                                        &item.mac.tokens,
                                        &wanted[wanted.len() - 1..],
                                    )
                            })
                            .cloned()
                    }),
                syn::Item::Impl(item) => impl_type_path(&self.module_path, &item.self_ty),
                syn::Item::ForeignMod(block) => self
                    .wanted
                    .iter()
                    .find(|wanted| {
                        wanted.starts_with(&self.module_path)
                            && wanted.len() == self.module_path.len() + 1
                            && block.items.iter().any(|foreign| {
                                let normalized = match foreign {
                                    syn::ForeignItem::Verbatim(tokens) => {
                                        foreign_verbatim_declaration(tokens)
                                    }
                                    _ => None,
                                };
                                let foreign = normalized.as_ref().map_or(foreign, |(item, _)| item);
                                let name = match foreign {
                                    syn::ForeignItem::Fn(item) => &item.sig.ident,
                                    syn::ForeignItem::Static(item) => &item.ident,
                                    syn::ForeignItem::Type(item) => &item.ident,
                                    _ => return false,
                                };
                                crate::imports::identifier_key(&name.to_string())
                                    == wanted.last().unwrap()
                            })
                    })
                    .cloned(),
                _ => previous.clone(),
            };
            if let syn::Item::Macro(item_macro) = item
                && self.wanted.iter().any(|wanted| {
                    wanted.starts_with(&self.module_path)
                        && macro_contains_name(&item_macro.mac.tokens, &wanted[wanted.len() - 1..])
                })
            {
                match non_doc_only_attribute(
                    item_macro.mac.tokens.clone(),
                    self.normal,
                    self.documentation,
                ) {
                    Ok(Some(line)) => self.line = Some(line),
                    Err(error) => self.error = Some(error),
                    Ok(None) => {}
                }
            }
            if let syn::Item::Mod(item) = item {
                self.module_path
                    .push(crate::imports::identifier_key(&item.ident.to_string()).to_string());
            }
            syn::visit::visit_item(self, item);
            self.module_path.truncate(module_len);
            self.owner = previous;
            self.owner_is_container = previous_is_container;
        }

        fn visit_foreign_item(&mut self, foreign: &'ast syn::ForeignItem) {
            let normalized = match foreign {
                syn::ForeignItem::Verbatim(tokens) => foreign_verbatim_declaration(tokens),
                _ => None,
            };
            let foreign = normalized.as_ref().map_or(foreign, |(item, _)| item);
            let name = match foreign {
                syn::ForeignItem::Fn(item) => Some(&item.sig.ident),
                syn::ForeignItem::Static(item) => Some(&item.ident),
                syn::ForeignItem::Type(item) => Some(&item.ident),
                _ => None,
            };
            let previous = self.owner.clone();
            let previous_is_container = self.owner_is_container;
            self.owner = name.map(|name| item_path(&self.module_path, &name.to_string()));
            self.owner_is_container = false;
            syn::visit::visit_foreign_item(self, foreign);
            self.owner = previous;
            self.owner_is_container = previous_is_container;
        }

        fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
            let matches_query = self.owner.as_deref().is_some_and(|owner| {
                self.wanted.iter().any(|wanted| {
                    owner == wanted || (self.owner_is_container && wanted.starts_with(owner))
                })
            });
            if matches_query {
                match attribute_is_non_doc_only(&attribute.meta, self.normal, self.documentation) {
                    Ok(true) => self.line = Some(attribute.pound_token.span.start().line),
                    Err(error) => self.error = Some(error),
                    Ok(false) => {}
                }
            }
        }
    }

    fn macro_contains_name(tokens: &proc_macro2::TokenStream, parts: &[String]) -> bool {
        tokens.clone().into_iter().any(|token| match token {
            proc_macro2::TokenTree::Ident(ident) => parts
                .iter()
                .any(|part| crate::imports::identifier_key(&ident.to_string()) == part),
            proc_macro2::TokenTree::Group(group) => macro_contains_name(&group.stream(), parts),
            _ => false,
        })
    }

    fn source_module_path(krate: &Crate, root: &Path, source: &Path) -> Vec<String> {
        if source == root {
            return Vec::new();
        }
        let guessed = root
            .parent()
            .and_then(|parent| source.strip_prefix(parent).ok())
            .map(|relative| {
                let mut path = relative
                    .parent()
                    .into_iter()
                    .flat_map(Path::components)
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                if let Some(stem) = relative.file_stem().and_then(|stem| stem.to_str())
                    && stem != "mod"
                {
                    path.push(stem.to_string());
                }
                path
            });
        let canonical = krate
            .index
            .values()
            .filter(|item| item.crate_id == 0)
            .filter(|item| {
                item.span
                    .as_ref()
                    .is_some_and(|span| span.filename == source)
            })
            .filter_map(|item| {
                let summary = krate.paths.get(&item.id)?;
                if summary.crate_id != 0 || summary.path.last()? != item.name.as_ref()? {
                    return None;
                }
                let end = if matches!(&item.inner, rustdoc_types::ItemEnum::Module(_)) {
                    summary.path.len()
                } else {
                    summary.path.len() - 1
                };
                Some(
                    summary.path[1..end]
                        .iter()
                        .map(|part| crate::imports::identifier_key(part).to_string())
                        .collect::<Vec<_>>(),
                )
            })
            .min_by_key(Vec::len);
        match (guessed, canonical) {
            (Some(guessed), Some(canonical)) if canonical.starts_with(&guessed) => guessed,
            (_, Some(canonical)) => canonical,
            (Some(guessed), None) => guessed,
            (None, None) => Vec::new(),
        }
    }

    let mut parts = import
        .segments
        .iter()
        .map(|part| crate::imports::identifier_key(part).to_string())
        .collect::<Vec<_>>();
    parts.push(crate::imports::identifier_key(&import.item).to_string());
    let mut wanted = vec![parts];
    if let Ok(report) = crate::symbols::find_symbol_report(krate, import)
        && let Some(summary) = krate.paths.get(&report.resolved_id)
        && summary.crate_id == 0
    {
        let canonical = summary
            .path
            .iter()
            .skip(1)
            .map(|part| crate::imports::identifier_key(part).to_string())
            .collect::<Vec<_>>();
        if !wanted.contains(&canonical) {
            wanted.push(canonical);
        }
    }
    for path in sources {
        let source = fs::read_to_string(&path).map_err(|error| {
            format!(
                "cannot verify non-doc API source {}: {error}",
                path.display()
            )
        })?;
        if !source.contains("cfg") || !source.contains("doc") {
            continue;
        }
        let file = syn::parse_file(&source).map_err(|error| {
            format!(
                "cannot verify non-doc API source {}: {error}",
                path.display()
            )
        })?;
        let module_path = source_module_path(krate, target.src_path.as_std_path(), &path);
        let mut affected = Affected {
            normal: &cfg,
            documentation: &doc_cfg,
            wanted: &wanted,
            // File attributes belong to the crate or module; unmatched items have no owner.
            owner: Some(module_path.clone()),
            module_path,
            owner_is_container: true,
            line: None,
            error: None,
        };
        syn::visit::Visit::visit_file(&mut affected, &file);
        if let Some(error) = affected.error {
            return Err(format!(
                "cannot verify non-doc API source {}: {error}",
                path.display()
            ));
        }
        if let Some(line) = affected.line {
            return Err(format!(
                "non-doc API extraction is incomplete: {}:{line} enables source in the selected compilation but excludes it from Rustdoc JSON under cfg(doc)",
                path.display()
            ));
        }
    }
    reject_non_doc_expansion(
        krate,
        json_path,
        import,
        target.edition == cargo_metadata::Edition::E2015,
    )
}

fn reject_non_doc_expansion(
    krate: &Crate,
    json_path: &Path,
    import: &crate::imports::ImportPath,
    legacy_use_paths: bool,
) -> Result<Vec<String>, String> {
    let normal_path = json_path.with_extension("normal.rs");
    let doc_path = json_path.with_extension("doc.rs");
    let normal = fs::read_to_string(&normal_path).map_err(|error| {
        format!(
            "cannot read normal compiler expansion {}: {error}",
            normal_path.display()
        )
    })?;
    let doc = fs::read_to_string(&doc_path).map_err(|error| {
        format!(
            "cannot read doc compiler expansion {}: {error}",
            doc_path.display()
        )
    })?;
    let mut paths = vec![import.clone()];
    let resolved_id = crate::symbols::find_symbol_report(krate, import)
        .ok()
        .map(|report| report.resolved_id);
    if let Some(summary) = resolved_id.and_then(|id| krate.paths.get(&id))
        && summary.crate_id == 0
        && summary.path.len() > 1
    {
        let path = &summary.path[1..];
        let canonical = crate::imports::ImportPath {
            crate_name: import.crate_name.clone(),
            segments: path[..path.len() - 1].to_vec(),
            item: path[path.len() - 1].clone(),
            namespace: import.namespace,
        };
        if canonical.full_path() != import.full_path() {
            paths.push(canonical);
        }
    }
    let reported_members = resolved_id
        .and_then(|id| krate.index.get(&id))
        .and_then(|item| reported_api_members(krate, item));
    let foreign_declaration = resolved_id
        .and_then(|id| krate.index.get(&id))
        .is_some_and(|item| match &item.inner {
            rustdoc_types::ItemEnum::Function(function) => !function.has_body,
            rustdoc_types::ItemEnum::Static(static_) => static_.expr.is_empty(),
            rustdoc_types::ItemEnum::ExternType => true,
            _ => false,
        });
    let mut verified_foreign = false;
    let mut missing_impls = Vec::new();
    for path in paths {
        let normal = expanded_api_shape(&normal, &path, legacy_use_paths)?;
        let doc = expanded_api_shape(&doc, &path, legacy_use_paths)?;
        if normal.resolved_path != doc.resolved_path {
            return Err(format!(
                "non-doc API extraction is incomplete: requested path '{}' resolves to different definitions under cfg(doc) (normal: {:?}; doc: {:?})",
                path.full_path(),
                normal.resolved_path,
                doc.resolved_path,
            ));
        }
        if normal.resolved_path.is_none() && resolved_id.is_some() {
            return Err(format!(
                "non-doc API extraction is incomplete: cannot resolve requested path '{}' in the compiler expansions",
                path.full_path(),
            ));
        }
        verified_foreign |= normal.has_definition && doc.has_definition;
        for missing in normal.shapes.difference(&doc.shapes) {
            if let Some(impl_header) = missing.strip_prefix("derived ") {
                missing_impls.push(impl_header.to_string());
            } else {
                return Err(format!(
                    "non-doc API extraction is incomplete: compiler expansion for '{}' contains {missing}, which is absent under cfg(doc)",
                    path.full_path()
                ));
            }
        }
        if let Some(reported) = &reported_members
            && (normal.has_definition || doc.has_definition)
        {
            // Normal-only derives are restored separately. Every other header
            // must survive filtering, including doc-generated derives.
            let surviving_members = normal
                .members
                .iter()
                .filter(|(_, shape)| !shape.starts_with("derived ") || doc.shapes.contains(shape));
            let mut reported_members = reported.members.clone();
            // Count repeated names across inherent impls: one surviving method
            // must not conceal a missing method on another specialization.
            for (member, shape) in surviving_members.clone() {
                let count = reported_members.entry(member.clone()).or_default();
                if *count == 0 {
                    return Err(format!(
                        "non-doc API extraction is incomplete: compiler expansion for '{}' contains {shape}, which is absent from the filtered Rustdoc JSON members",
                        path.full_path()
                    ));
                }
                *count -= 1;
            }
            if let Some((member, _)) = reported_members.iter().find(|(_, count)| **count != 0) {
                return Err(format!(
                    "non-doc API extraction is incomplete: filtered Rustdoc JSON for '{}' reports {member}, which is absent from the normal compiler expansion",
                    path.full_path()
                ));
            }
            let mut normal_members: HashMap<&str, Vec<&str>> = HashMap::new();
            let mut doc_members: HashMap<&str, Vec<&str>> = HashMap::new();
            for (member, shape) in surviving_members {
                normal_members.entry(member).or_default().push(shape);
            }
            for (member, shape) in &doc.members {
                doc_members.entry(member).or_default().push(shape);
            }
            for (member, mut normal_shapes) in normal_members {
                let mut doc_shapes = doc_members.remove(member).unwrap_or_default();
                normal_shapes.sort();
                doc_shapes.sort();
                let shape = normal_shapes[0];
                // If candidates were filtered, every possible survivor must
                // have the same signature and owner. Counts cannot identify
                // which specialization or trait arguments remain otherwise.
                if normal_shapes != doc_shapes
                    && !(normal_shapes.iter().all(|candidate| *candidate == shape)
                        && doc_shapes.len() >= normal_shapes.len()
                        && doc_shapes.iter().all(|candidate| *candidate == shape))
                {
                    return Err(format!(
                        "non-doc API extraction is incomplete: cannot verify the signature and owner of {shape} in the filtered Rustdoc JSON for '{}'",
                        path.full_path()
                    ));
                }
            }
            for (member, requirements) in &normal.attributes {
                let mut normal_attributes = requirements
                    .iter()
                    .map(|(_, attributes)| attributes.clone())
                    .collect::<Vec<_>>();
                let mut reported_attributes =
                    reported.attributes.get(member).cloned().unwrap_or_default();
                normal_attributes.sort();
                reported_attributes.sort();
                let mut doc_requirements = doc.attributes.get(member).cloned().unwrap_or_default();
                let mut normal_requirements = requirements.clone();
                normal_requirements.sort();
                doc_requirements.sort();
                if normal_attributes != reported_attributes
                    || (normal_attributes.windows(2).any(|pair| pair[0] != pair[1])
                        && normal_requirements != doc_requirements)
                {
                    return Err(format!(
                        "non-doc API extraction is incomplete: compiler expansion for '{}' has semantic attributes for {member} that cannot be verified in the filtered Rustdoc JSON (normal: {normal_attributes:?}; reported: {reported_attributes:?})",
                        path.full_path(),
                    ));
                }
            }
        }
        // Declaration dependencies must be verified even for item kinds without
        // a reported member collector, such as constants, statics, and aliases.
        for (shape, dependencies) in &normal.dependencies {
            if shape.starts_with("derived ") && !doc.shapes.contains(shape) {
                continue;
            }
            let candidates = |dependencies: &[HashSet<String>]| {
                let mut candidates = dependencies
                    .iter()
                    .map(|dependencies| {
                        let mut paths = dependencies.iter().cloned().collect::<Vec<_>>();
                        paths.sort();
                        paths
                    })
                    .collect::<Vec<_>>();
                candidates.sort();
                candidates
            };
            if !doc.dependencies.get(shape).is_some_and(|doc| {
                candidates(dependencies) == candidates(doc)
                    || doc.iter().all(|candidate| {
                        dependencies
                            .iter()
                            .all(|required| required.is_subset(candidate))
                    })
            }) {
                return Err(format!(
                    "non-doc API extraction is incomplete: cannot verify the signature and owner of {shape} in the filtered Rustdoc JSON for '{}': type aliases or resolved paths differ under cfg(doc)",
                    path.full_path()
                ));
            }
        }
    }
    if foreign_declaration && !verified_foreign {
        return Err(format!(
            "non-doc API extraction is incomplete: cannot verify foreign declaration '{}' in the normal and doc compiler expansions",
            import.full_path()
        ));
    }
    missing_impls.sort();
    missing_impls.dedup();
    Ok(missing_impls)
}

#[derive(Default)]
struct ExpandedApi {
    shapes: HashSet<String>,
    members: Vec<(String, String)>,
    attributes: HashMap<String, Vec<(String, Vec<String>)>>,
    dependencies: HashMap<String, Vec<HashSet<String>>>,
    has_definition: bool,
    resolved_path: Option<Vec<String>>,
}

#[derive(Default)]
struct ReportedApi {
    members: HashMap<String, usize>,
    attributes: HashMap<String, Vec<Vec<String>>>,
}

fn semantic_attributes(
    attrs: &[rustdoc_types::Attribute],
    deprecation: Option<&rustdoc_types::Deprecation>,
) -> Vec<String> {
    use rustdoc_types::Attribute;

    let mut attributes = attrs
        .iter()
        .filter(|attr| match attr {
            Attribute::Repr(repr) => *repr != cfg_attr_repr(&[]),
            Attribute::NonExhaustive | Attribute::MustUse { .. } => true,
            _ => false,
        })
        .map(|attr| serde_json::to_string(attr).expect("attributes serialize"))
        .collect::<Vec<_>>();
    let features = normalize_target_features(
        attrs
            .iter()
            .filter_map(|attr| match attr {
                Attribute::TargetFeature { enable } => Some(enable),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect(),
    );
    if !features.is_empty() {
        attributes.push(format!("target_feature {features:?}"));
    }
    if let Some(deprecation) = deprecation {
        attributes.push(format!(
            "deprecated {}",
            serde_json::to_string(deprecation).expect("deprecation serializes")
        ));
    }
    attributes.sort();
    attributes.dedup();
    attributes
}

fn expanded_deprecation(
    attrs: &[syn::Attribute],
    inherited: Option<&rustdoc_types::Deprecation>,
) -> Option<rustdoc_types::Deprecation> {
    cfg_attr_deprecation(attrs.iter().map(|attr| &attr.meta)).or_else(|| inherited.cloned())
}

fn expanded_member_attributes(
    shape: &str,
    attrs: &[syn::Attribute],
    inherited_deprecation: Option<&rustdoc_types::Deprecation>,
) -> (String, Vec<String>) {
    let outputs = attrs.iter().map(|attr| &attr.meta);
    let mut attributes = semantic_cfg_attr_outputs(outputs.clone());
    attributes.push(rustdoc_types::Attribute::Repr(cfg_attr_repr(
        outputs.clone(),
    )));
    attributes.push(rustdoc_types::Attribute::TargetFeature {
        enable: cfg_attr_target_features(outputs),
    });
    let deprecation = expanded_deprecation(attrs, inherited_deprecation);
    (
        shape.to_string(),
        semantic_attributes(&attributes, deprecation.as_ref()),
    )
}

fn normalize_target_features(mut features: Vec<String>) -> Vec<String> {
    features.sort();
    features.dedup();
    features
}

fn member_key(prefix: &str, name: &str) -> String {
    format!("{prefix} {}", crate::imports::identifier_key(name))
}

fn trait_member_key(kind: &str, name: &str, has_default: bool) -> String {
    let requirement = if has_default { "provided" } else { "required" };
    format!(
        "{requirement} trait {kind} {}",
        crate::imports::identifier_key(name)
    )
}

fn reported_api_members(krate: &Crate, item: &rustdoc_types::Item) -> Option<ReportedApi> {
    use rustdoc_types::{ItemEnum, StructKind, VariantKind, Visibility};

    fn collect(krate: &Crate, item: &rustdoc_types::Item, prefix: &str, api: &mut ReportedApi) {
        let name = item.name.as_deref().unwrap_or_default();
        let mut prefix = prefix.to_string();
        let key = match &item.inner {
            ItemEnum::StructField(_)
                if matches!(item.visibility, Visibility::Public | Visibility::Default) =>
            {
                Some(member_key(&format!("{prefix} field"), name))
            }
            ItemEnum::Variant(_) => {
                prefix = member_key("variant", name);
                Some(prefix.clone())
            }
            ItemEnum::Function(method) if prefix == "trait" => {
                Some(trait_member_key("method", name, method.has_body))
            }
            ItemEnum::Function(_) if prefix == "function" => Some(member_key(&prefix, name)),
            ItemEnum::AssocConst { value, .. } if prefix == "trait" => {
                Some(trait_member_key("const", name, value.is_some()))
            }
            ItemEnum::AssocType { type_, .. } if prefix == "trait" => {
                Some(trait_member_key("type", name, type_.is_some()))
            }
            ItemEnum::Function(_) | ItemEnum::AssocConst { .. }
                if prefix == "inherent"
                    && matches!(item.visibility, Visibility::Public | Visibility::Default) =>
            {
                let kind = if matches!(item.inner, ItemEnum::Function(_)) {
                    "method"
                } else {
                    "const"
                };
                Some(member_key(&format!("inherent {kind}"), name))
            }
            _ => None,
        };
        if let Some(key) = key {
            *api.members.entry(key.clone()).or_default() += 1;
            api.attributes
                .entry(key)
                .or_default()
                .push(semantic_attributes(&item.attrs, item.deprecation.as_ref()));
        }
        let tuple_fields = match &item.inner {
            ItemEnum::Struct(item) => match &item.kind {
                StructKind::Tuple(fields) => Some(fields),
                _ => None,
            },
            ItemEnum::Variant(item) => match &item.kind {
                VariantKind::Tuple(fields) => Some(fields),
                _ => None,
            },
            _ => None,
        };
        if let Some(fields) = tuple_fields {
            // Pruning can change tuple positions; validate the positions that
            // the filtered graph actually reports, preserving private slots.
            for (index, id) in fields.iter().enumerate() {
                if let Some(child) = id.and_then(|id| krate.index.get(&id))
                    && matches!(child.visibility, Visibility::Public | Visibility::Default)
                {
                    let key = member_key(&format!("{prefix} field"), &index.to_string());
                    *api.members.entry(key.clone()).or_default() += 1;
                    api.attributes
                        .entry(key)
                        .or_default()
                        .push(semantic_attributes(
                            &child.attrs,
                            child.deprecation.as_ref(),
                        ));
                } else {
                    *api.members
                        .entry(member_key(
                            &format!("{prefix} private field"),
                            &index.to_string(),
                        ))
                        .or_default() += 1;
                }
            }
        } else {
            let stripped = match &item.inner {
                ItemEnum::Struct(item) => matches!(
                    item.kind,
                    StructKind::Plain {
                        has_stripped_fields: true,
                        ..
                    }
                ),
                ItemEnum::Union(item) => item.has_stripped_fields,
                ItemEnum::Variant(item) => matches!(
                    item.kind,
                    VariantKind::Struct {
                        has_stripped_fields: true,
                        ..
                    }
                ),
                _ => false,
            };
            if stripped {
                api.members.insert(format!("{prefix} private fields"), 1);
            }
            for id in lexical_children(&item.inner) {
                if let Some(child) = krate.index.get(&id) {
                    collect(krate, child, &prefix, api);
                }
            }
        }
    }

    let (prefix, impls) = match &item.inner {
        ItemEnum::Struct(item) => ("struct", item.impls.as_slice()),
        ItemEnum::Enum(item) => ("enum", item.impls.as_slice()),
        ItemEnum::Union(item) => ("union", item.impls.as_slice()),
        ItemEnum::Trait(_) => ("trait", &[][..]),
        ItemEnum::Variant(_) => ("variant", &[][..]),
        ItemEnum::Function(_) => ("function", &[][..]),
        ItemEnum::Static(_) | ItemEnum::ExternType => ("item", &[][..]),
        _ => return None,
    };
    let mut api = ReportedApi::default();
    api.attributes.insert(
        "item".into(),
        vec![semantic_attributes(&item.attrs, item.deprecation.as_ref())],
    );
    collect(krate, item, prefix, &mut api);
    for id in impls {
        if let Some(item) = krate.index.get(id)
            && let ItemEnum::Impl(imp) = &item.inner
            && !imp.is_synthetic
            && imp.blanket_impl.is_none()
        {
            if let Some(trait_) = &imp.trait_ {
                let derived = item
                    .attrs
                    .contains(&rustdoc_types::Attribute::AutomaticallyDerived);
                *api.members
                    .entry(member_key(
                        if derived {
                            "derived trait impl"
                        } else {
                            "trait impl"
                        },
                        trait_.path.rsplit("::").next().unwrap_or_default(),
                    ))
                    .or_default() += 1;
            } else if !imp.is_negative {
                collect(krate, item, "inherent", &mut api);
            }
        }
    }
    Some(api)
}

// Syn represents explicit safety on some foreign declarations as raw tokens.
// Remove only that marker for traversal; callers retain its declaration semantics.
fn foreign_verbatim_declaration(
    tokens: &proc_macro2::TokenStream,
) -> Option<(syn::ForeignItem, bool)> {
    let mut tokens = tokens.clone().into_iter().collect::<Vec<_>>();
    let index = tokens.windows(2).position(|pair| {
        matches!(&pair[0], proc_macro2::TokenTree::Ident(safety)
            if (safety == "safe" && matches!(&pair[1], proc_macro2::TokenTree::Ident(kind) if kind == "fn" || kind == "static"))
                || (safety == "unsafe" && matches!(&pair[1], proc_macro2::TokenTree::Ident(kind) if kind == "static")))
    })?;
    let safe = matches!(&tokens[index], proc_macro2::TokenTree::Ident(safety) if safety == "safe");
    tokens.remove(index);
    let item = syn::parse2(tokens.into_iter().collect()).ok()?;
    matches!(item, syn::ForeignItem::Fn(_) | syn::ForeignItem::Static(_)).then_some((item, safe))
}

fn expanded_api_shape(
    source: &str,
    import: &crate::imports::ImportPath,
    legacy_use_paths: bool,
) -> Result<ExpandedApi, String> {
    use syn::{Fields, ImplItem, Item, Visibility};
    let file = syn::parse_file(source)
        .map_err(|error| format!("cannot parse selected compiler expansion: {error}"))?;
    let mut wanted = import
        .segments
        .iter()
        .map(|part| crate::imports::identifier_key(part).to_string())
        .collect::<Vec<_>>();
    wanted.push(crate::imports::identifier_key(&import.item).to_string());
    let mut api = ExpandedApi::default();

    fn text(source: &str, span: proc_macro2::Span) -> Result<&str, String> {
        source
            .get(span.byte_range())
            .ok_or_else(|| "compiler expansion has an invalid UTF-8 span".to_string())
    }
    fn visibility(source: &str, vis: &Visibility) -> Result<String, String> {
        match vis {
            Visibility::Inherited => Ok(String::new()),
            _ => Ok(text(source, vis.span())?.to_string()),
        }
    }
    fn fields(
        source: &str,
        fields: &Fields,
        prefix: &str,
        api: &mut ExpandedApi,
        deprecation: Option<&rustdoc_types::Deprecation>,
    ) -> Result<(), String> {
        let kind = match fields {
            Fields::Named(_) => "named",
            Fields::Unnamed(_) => "tuple",
            Fields::Unit => "unit",
        };
        api.shapes.insert(format!("{prefix} {kind} fields"));
        for (index, field) in fields.iter().enumerate() {
            let name = field
                .ident
                .as_ref()
                .map_or_else(|| index.to_string(), ToString::to_string);
            let shape = format!(
                "{prefix} field {name}: {} {}",
                visibility(source, &field.vis)?,
                text(source, field.ty.span())?
            );
            api.shapes.insert(shape.clone());
            if prefix.starts_with("variant ") || matches!(field.vis, Visibility::Public(_)) {
                let key = member_key(&format!("{prefix} field"), &name);
                api.attributes
                    .entry(key.clone())
                    .or_default()
                    .push(expanded_member_attributes(
                        &shape,
                        &field.attrs,
                        deprecation,
                    ));
                api.members.push((key, shape));
            } else if matches!(fields, Fields::Unnamed(_)) {
                let key = member_key(&format!("{prefix} private field"), &name);
                api.members.push((key.clone(), key));
            } else {
                let key = format!("{prefix} private fields");
                if !api.members.iter().any(|(member, _)| member == &key) {
                    api.members.push((key.clone(), key));
                }
            }
        }
        Ok(())
    }
    fn variant_shape(source: &str, variant: &syn::Variant) -> Result<String, String> {
        let prefix = member_key("variant", &variant.ident.to_string());
        let discriminant = variant
            .discriminant
            .as_ref()
            .map(|(_, value)| text(source, value.span()))
            .transpose()?;
        Ok(format!("{prefix} discriminant {discriminant:?}"))
    }
    fn use_may_bind(tree: &syn::UseTree, wanted: &str) -> bool {
        match tree {
            syn::UseTree::Path(path) => use_may_bind(&path.tree, wanted),
            syn::UseTree::Name(name) => {
                crate::imports::identifier_key(&name.ident.to_string()) == wanted
            }
            syn::UseTree::Rename(name) => {
                crate::imports::identifier_key(&name.rename.to_string()) == wanted
            }
            syn::UseTree::Glob(_) => true,
            syn::UseTree::Group(group) => group.items.iter().any(|tree| use_may_bind(tree, wanted)),
        }
    }
    fn use_targets(
        tree: &syn::UseTree,
        prefix: &[String],
        name: &str,
        globs: bool,
    ) -> Vec<Vec<String>> {
        let key =
            |ident: &syn::Ident| crate::imports::identifier_key(&ident.to_string()).to_string();
        match tree {
            syn::UseTree::Path(path) => {
                let mut prefix = prefix.to_vec();
                prefix.push(key(&path.ident));
                use_targets(&path.tree, &prefix, name, globs)
            }
            syn::UseTree::Group(group) => group
                .items
                .iter()
                .flat_map(|tree| use_targets(tree, prefix, name, globs))
                .collect(),
            syn::UseTree::Name(binding)
                if !globs
                    && (key(&binding.ident) == name
                        || (binding.ident == "self"
                            && prefix.last().is_some_and(|part| part == name))) =>
            {
                let mut path = prefix.to_vec();
                if binding.ident != "self" {
                    path.push(name.to_string());
                }
                vec![path]
            }
            syn::UseTree::Rename(binding) if !globs && key(&binding.rename) == name => {
                let mut path = prefix.to_vec();
                if binding.ident != "self" {
                    path.push(key(&binding.ident));
                }
                vec![path]
            }
            syn::UseTree::Glob(_) if globs => {
                let mut path = prefix.to_vec();
                path.push(name.to_string());
                vec![path]
            }
            _ => Vec::new(),
        }
    }
    fn type_path(ty: &syn::Type, legacy_use_paths: bool) -> Option<Vec<String>> {
        let syn::Type::Path(ty) = ty else {
            return None;
        };
        if ty.qself.is_some() {
            return None;
        }
        Some(path_parts(&ty.path, legacy_use_paths))
    }
    fn path_parts(path: &syn::Path, legacy_use_paths: bool) -> Vec<String> {
        let mut parts = Vec::new();
        if path.leading_colon.is_some() {
            // Modern absolute paths start in the extern prelude, not the crate root.
            parts.push(if legacy_use_paths { "crate" } else { "::" }.to_string());
        }
        parts.extend(
            path.segments.iter().map(|segment| {
                crate::imports::identifier_key(&segment.ident.to_string()).to_string()
            }),
        );
        parts
    }
    fn self_crate_alias(items: &[Item], name: &str) -> bool {
        items.iter().any(|item| {
            matches!(item, Item::ExternCrate(item)
                if item.ident == "self"
                    && crate::imports::identifier_key(
                        &item.rename.as_ref().map_or(&item.ident, |(_, name)| name).to_string()
                    ) == name)
        })
    }
    #[derive(Default)]
    struct PathTrace<'a> {
        visiting: HashSet<Vec<String>>,
        aliases: Vec<Alias<'a>>,
        query: bool,
        type_only: bool,
        values: Vec<ValueDependency<'a>>,
        value_path: bool,
        evaluation: bool,
        pattern: bool,
    }
    #[derive(Clone)]
    struct LocalScope<'a> {
        module: Vec<String>,
        items: Vec<&'a Item>,
        module_scope: bool,
    }

    #[derive(Clone)]
    struct Alias<'a> {
        name: String,
        item: &'a syn::ItemType,
        module: Vec<String>,
        scopes: Vec<LocalScope<'a>>,
    }

    #[derive(Clone)]
    struct ValueDependency<'a> {
        name: String,
        item: &'a Item,
        module: Vec<String>,
        scopes: Vec<LocalScope<'a>>,
    }

    fn resolve_block_path<'a>(
        root: &'a [Item],
        module: &[String],
        scopes: &[LocalScope<'a>],
        path: &[String],
        legacy_use_paths: bool,
        allow_external: bool,
        trace: &mut PathTrace<'a>,
    ) -> Result<Option<Vec<String>>, String> {
        let mut lookup_module = module.to_vec();
        let mut lookup_path = path;
        let qualified = matches!(path.first().map(String::as_str), Some("self" | "super"));
        if lookup_path.first().is_some_and(|part| part == "self") {
            lookup_path = &lookup_path[1..];
        }
        while lookup_path.first().is_some_and(|part| part == "super") {
            if lookup_module.pop().is_none() {
                return Ok(None);
            }
            lookup_path = &lookup_path[1..];
        }
        let path = lookup_path;
        let Some(name) = path.first() else {
            return Ok(None);
        };
        if !matches!(name.as_str(), "crate" | "self" | "super" | "::") {
            for (index, scope) in scopes.iter().enumerate().rev() {
                if scope.module != lookup_module || (qualified && !scope.module_scope) {
                    continue;
                }
                let module = &scope.module;
                let mut binding = vec!["block".into(), index.to_string()];
                binding.extend(module.iter().cloned());
                binding.push(name.clone());
                if !trace.visiting.insert(binding.clone()) {
                    return Ok(None);
                }
                let result = (|| {
                    // Value bindings take precedence over same-named types.
                    let value = (trace.value_path && path.len() == 1)
                        .then(|| {
                            scope.items.iter().find(|item| {
                                let ident = match item {
                                    Item::Const(item) => &item.ident,
                                    Item::Static(item) => &item.ident,
                                    Item::Fn(item) => &item.sig.ident,
                                    _ => return false,
                                };
                                crate::imports::identifier_key(&ident.to_string()) == name
                            })
                        })
                        .flatten();
                    for item in value.into_iter().chain(scope.items.iter()) {
                        let ident = match item {
                            Item::Struct(item) => &item.ident,
                            Item::Enum(item) => &item.ident,
                            Item::Union(item) => &item.ident,
                            Item::Trait(item) => &item.ident,
                            Item::Type(item) => &item.ident,
                            Item::Mod(item) => &item.ident,
                            Item::Const(item) if trace.value_path && path.len() == 1 => &item.ident,
                            Item::Static(item) if trace.value_path && path.len() == 1 => {
                                &item.ident
                            }
                            Item::Fn(item) if trace.value_path && path.len() == 1 => {
                                &item.sig.ident
                            }
                            Item::ExternCrate(item) => {
                                item.rename.as_ref().map_or(&item.ident, |(_, name)| name)
                            }
                            _ => continue,
                        };
                        if crate::imports::identifier_key(&ident.to_string()) != name {
                            continue;
                        }
                        match item {
                            Item::Struct(_) | Item::Enum(_) | Item::Union(_)
                                if trace.evaluation && path.len() == 1 =>
                            {
                                let name = format!("block {index} {}::{name}", module.join("::"));
                                trace.values.push(ValueDependency {
                                    name: name.clone(),
                                    item,
                                    module: module.to_vec(),
                                    scopes: scopes[..=index].to_vec(),
                                });
                                return Ok(Some(Some(vec![name])));
                            }
                            Item::Const(_) | Item::Static(_) | Item::Fn(_) => {
                                if path.len() != 1 {
                                    return Ok(Some(None));
                                }
                                let name = format!("block {index} {}::{name}", module.join("::"));
                                trace.values.push(ValueDependency {
                                    name: name.clone(),
                                    item,
                                    module: module.to_vec(),
                                    scopes: scopes[..=index].to_vec(),
                                });
                                return Ok(Some(Some(vec![name])));
                            }
                            Item::Type(item) => {
                                trace.aliases.push(Alias {
                                    name: format!("block {index} {}::{name}", module.join("::")),
                                    item,
                                    module: module.to_vec(),
                                    scopes: scopes[..=index].to_vec(),
                                });
                                let Some(mut target) = type_path(&item.ty, legacy_use_paths) else {
                                    return Ok(Some(None));
                                };
                                target.extend_from_slice(&path[1..]);
                                return resolve_block_path(
                                    root,
                                    module,
                                    &scopes[..=index],
                                    &target,
                                    legacy_use_paths,
                                    allow_external,
                                    trace,
                                )
                                .map(Some);
                            }
                            Item::ExternCrate(item) => {
                                let mut target = if item.ident == "self" {
                                    vec!["crate".into()]
                                } else {
                                    if !allow_external {
                                        return Ok(Some(None));
                                    }
                                    vec!["::".into(), item.ident.to_string()]
                                };
                                target.extend_from_slice(&path[1..]);
                                return resolve_block_path(
                                    root,
                                    module,
                                    scopes,
                                    &target,
                                    legacy_use_paths,
                                    allow_external,
                                    trace,
                                )
                                .map(Some);
                            }
                            Item::Mod(item) => {
                                let Some((_, children)) = &item.content else {
                                    return Ok(Some(None));
                                };
                                let mut child_module = module.clone();
                                child_module.push(name.clone());
                                let mut child_scopes = scopes[..=index].to_vec();
                                child_scopes.push(LocalScope {
                                    module: child_module.clone(),
                                    items: children.iter().collect(),
                                    module_scope: true,
                                });
                                return resolve_block_path(
                                    root,
                                    &child_module,
                                    &child_scopes,
                                    &path[1..],
                                    legacy_use_paths,
                                    allow_external,
                                    trace,
                                )
                                .map(Some);
                            }
                            // Block-local types cannot be the publicly queried type.
                            _ => return Ok(Some(None)),
                        }
                    }
                    for globs in [false, true] {
                        let mut explicit = false;
                        for item in &scope.items {
                            let Item::Use(item) = item else { continue };
                            for mut target in use_targets(&item.tree, &[], name, globs) {
                                explicit |= !globs;
                                target.extend_from_slice(&path[1..]);
                                if item.leading_colon.is_some() && !legacy_use_paths {
                                    target.insert(0, "::".into());
                                }
                                let (use_module, use_scopes) = if legacy_use_paths
                                    && !matches!(
                                        target.first().map(String::as_str),
                                        Some("self" | "super")
                                    ) {
                                    (&[][..], &[][..])
                                } else {
                                    (module.as_slice(), &scopes[..=index])
                                };
                                if let Some(resolved) = resolve_block_path(
                                    root,
                                    use_module,
                                    use_scopes,
                                    &target,
                                    legacy_use_paths,
                                    allow_external,
                                    trace,
                                )? {
                                    return Ok(Some(Some(resolved)));
                                }
                            }
                        }
                        if explicit {
                            return Ok(Some(None));
                        }
                    }
                    Ok(None)
                })();
                trace.visiting.remove(&binding);
                if let Some(resolved) = result? {
                    return Ok(resolved);
                }
            }
        }
        let absolute;
        let path = if qualified {
            absolute = std::iter::once("crate".to_string())
                .chain(lookup_module.iter().cloned())
                .chain(path.iter().cloned())
                .collect::<Vec<_>>();
            &absolute
        } else {
            path
        };
        Ok(resolve_path(
            root,
            module,
            path,
            legacy_use_paths,
            allow_external,
            trace,
        ))
    }
    fn resolve_path<'a>(
        root: &'a [Item],
        module: &[String],
        path: &[String],
        legacy_use_paths: bool,
        allow_external: bool,
        trace: &mut PathTrace<'a>,
    ) -> Option<Vec<String>> {
        let mut absolute = module.to_vec();
        let mut parts = path.iter().peekable();
        let mut prelude_index = None;
        match parts.peek().map(|part| part.as_str()) {
            Some("::") => {
                parts.next();
                if !self_crate_alias(root, parts.next()?) {
                    return allow_external.then(|| path.to_vec());
                }
                absolute.clear();
            }
            Some("crate") => {
                absolute.clear();
                parts.next();
            }
            Some("self") => {
                parts.next();
            }
            Some("super") => {}
            _ => {
                if !legacy_use_paths {
                    prelude_index = Some(module.len());
                }
            }
        }
        while parts.peek().is_some_and(|part| part.as_str() == "super") {
            absolute.pop()?;
            parts.next();
        }
        absolute.extend(parts.cloned());
        resolve_absolute(
            root,
            &absolute,
            legacy_use_paths,
            allow_external,
            trace,
            prelude_index,
        )
    }
    fn resolve_absolute<'a>(
        root: &'a [Item],
        path: &[String],
        legacy_use_paths: bool,
        allow_external: bool,
        trace: &mut PathTrace<'a>,
        prelude_index: Option<usize>,
    ) -> Option<Vec<String>> {
        let mut items = root;
        for (index, name) in path.iter().enumerate() {
            let module = &path[..index];
            if items.iter().any(|item| {
                matches!(item, Item::ForeignMod(block) if block.items.iter().any(|foreign| {
                    matches!(foreign, syn::ForeignItem::Type(ty)
                        if crate::imports::identifier_key(&ty.ident.to_string()) == name)
                }))
            }) {
                return (index + 1 == path.len()).then(|| path.to_vec());
            }
            let value = (trace.value_path && index + 1 == path.len())
                .then(|| {
                    items.iter().find(|item| {
                        let ident = match item {
                            Item::Const(item) => &item.ident,
                            Item::Static(item) => &item.ident,
                            Item::Fn(item) => &item.sig.ident,
                            _ => return false,
                        };
                        crate::imports::identifier_key(&ident.to_string()) == name
                    })
                })
                .flatten();
            let named = value.or_else(|| {
                items.iter().find(|item| {
                    let ident = match item {
                        Item::Mod(item) => &item.ident,
                        Item::Struct(item) => &item.ident,
                        Item::Enum(item) => &item.ident,
                        Item::Union(item) => &item.ident,
                        Item::Type(item) => &item.ident,
                        Item::Trait(item) => &item.ident,
                        Item::TraitAlias(item) if trace.query => &item.ident,
                        Item::Fn(item)
                            if index + 1 == path.len()
                                && (trace.value_path || (trace.query && !trace.type_only)) =>
                        {
                            &item.sig.ident
                        }
                        Item::Const(item)
                            if index + 1 == path.len()
                                && (trace.value_path || (trace.query && !trace.type_only)) =>
                        {
                            &item.ident
                        }
                        Item::Static(item)
                            if index + 1 == path.len()
                                && (trace.value_path || (trace.query && !trace.type_only)) =>
                        {
                            &item.ident
                        }
                        Item::Macro(item)
                            if trace.query && !trace.type_only && index + 1 == path.len() =>
                        {
                            let Some(ident) = &item.ident else {
                                return false;
                            };
                            ident
                        }
                        Item::ExternCrate(item) => {
                            item.rename.as_ref().map_or(&item.ident, |(_, name)| name)
                        }
                        _ => return false,
                    };
                    crate::imports::identifier_key(&ident.to_string()) == name
                })
            });
            match named {
                Some(item @ (Item::Struct(_) | Item::Enum(_) | Item::Union(_)))
                    if trace.evaluation && index + 1 == path.len() =>
                {
                    trace.values.push(ValueDependency {
                        name: path.join("::"),
                        item,
                        module: module.to_vec(),
                        scopes: Vec::new(),
                    });
                    return Some(path.to_vec());
                }
                Some(item @ (Item::Const(_) | Item::Static(_) | Item::Fn(_))) => {
                    if index + 1 != path.len() {
                        return None;
                    }
                    trace.values.push(ValueDependency {
                        name: path.join("::"),
                        item,
                        module: module.to_vec(),
                        scopes: Vec::new(),
                    });
                    return Some(path.to_vec());
                }
                Some(Item::Mod(item)) => {
                    items = &item.content.as_ref()?.1;
                    continue;
                }
                Some(Item::Type(item)) => {
                    // A queried alias is its own declaration. Dependency and
                    // implementation paths still follow its underlying type.
                    if trace.query && index + 1 == path.len() {
                        return Some(path.to_vec());
                    }
                    trace.aliases.push(Alias {
                        name: path[..=index].join("::"),
                        item,
                        module: module.to_vec(),
                        scopes: Vec::new(),
                    });
                    let target = type_path(&item.ty, legacy_use_paths)?;
                    let mut resolved = resolve_path(
                        root,
                        module,
                        &target,
                        legacy_use_paths,
                        allow_external,
                        trace,
                    )?;
                    resolved.extend_from_slice(&path[index + 1..]);
                    return resolve_absolute(
                        root,
                        &resolved,
                        legacy_use_paths,
                        allow_external,
                        trace,
                        None,
                    );
                }
                Some(Item::ExternCrate(item)) => {
                    if item.ident != "self" {
                        return allow_external.then(|| {
                            let mut external = vec!["::".to_string(), item.ident.to_string()];
                            external.extend_from_slice(&path[index + 1..]);
                            external
                        });
                    }
                    return resolve_absolute(
                        root,
                        &path[index + 1..],
                        legacy_use_paths,
                        allow_external,
                        trace,
                        None,
                    );
                }
                Some(item @ Item::Enum(_))
                    if (trace.query || trace.pattern) && index + 2 == path.len() =>
                {
                    let Item::Enum(enum_) = item else {
                        unreachable!()
                    };
                    if trace.pattern {
                        trace.values.push(ValueDependency {
                            name: path[..=index].join("::"),
                            item,
                            module: module.to_vec(),
                            scopes: Vec::new(),
                        });
                    }
                    return enum_
                        .variants
                        .iter()
                        .any(|variant| {
                            crate::imports::identifier_key(&variant.ident.to_string())
                                == path[index + 1]
                        })
                        .then(|| path.to_vec());
                }
                Some(_) => return (index + 1 == path.len()).then(|| path.to_vec()),
                None => {}
            }
            if trace.query && index + 1 == path.len() {
                for item in items {
                    let Item::ForeignMod(item) = item else {
                        continue;
                    };
                    if item.items.iter().any(|item| {
                        let normalized = match item {
                            syn::ForeignItem::Verbatim(tokens) => {
                                foreign_verbatim_declaration(tokens).map(|(item, _)| item)
                            }
                            _ => None,
                        };
                        let item = normalized.as_ref().unwrap_or(item);
                        let ident = match item {
                            syn::ForeignItem::Fn(item) if !trace.type_only => &item.sig.ident,
                            syn::ForeignItem::Static(item) if !trace.type_only => &item.ident,
                            syn::ForeignItem::Type(item) => &item.ident,
                            _ => return false,
                        };
                        crate::imports::identifier_key(&ident.to_string()) == name
                    }) {
                        return Some(path.to_vec());
                    }
                }
            }
            let binding = path[..=index].to_vec();
            if !trace.visiting.insert(binding.clone()) {
                return None;
            }
            // Explicit imports shadow glob bindings regardless of source order.
            let mut explicit_binding = false;
            for globs in [false, true] {
                for item in items {
                    let Item::Use(item) = item else { continue };
                    for mut target in use_targets(&item.tree, &[], name, globs) {
                        // Query lookup filters candidates by namespace. A value
                        // import cannot shadow a type supplied by a glob.
                        explicit_binding |= !globs && !trace.query;
                        target.extend_from_slice(&path[index + 1..]);
                        if item.leading_colon.is_some() && !legacy_use_paths {
                            target.insert(0, "::".to_string());
                        }
                        let use_module = if legacy_use_paths
                            && !matches!(target.first().map(String::as_str), Some("self" | "super"))
                        {
                            &[][..]
                        } else {
                            module
                        };
                        if let Some(resolved) = resolve_path(
                            root,
                            use_module,
                            &target,
                            legacy_use_paths,
                            allow_external,
                            trace,
                        ) {
                            trace.visiting.remove(&binding);
                            return Some(resolved);
                        }
                    }
                }
                if explicit_binding {
                    trace.visiting.remove(&binding);
                    return None;
                }
            }
            let resolved = if prelude_index == Some(index) && self_crate_alias(root, name) {
                resolve_absolute(
                    root,
                    &path[index + 1..],
                    legacy_use_paths,
                    allow_external,
                    trace,
                    None,
                )
            } else if allow_external
                && (!trace.pattern || index + 1 < path.len())
                && !(trace.query
                    && trace.type_only
                    && index + 1 == path.len()
                    && items.iter().any(|item| {
                        let ident = match item {
                            Item::Fn(item) => &item.sig.ident,
                            Item::Const(item) => &item.ident,
                            Item::Static(item) => &item.ident,
                            Item::Macro(item) => {
                                let Some(ident) = &item.ident else {
                                    return false;
                                };
                                ident
                            }
                            _ => return false,
                        };
                        crate::imports::identifier_key(&ident.to_string()) == name
                    }))
                && (prelude_index == Some(index) || (legacy_use_paths && index == 0))
            {
                let mut external = vec!["::".to_string()];
                external.extend_from_slice(&path[index..]);
                Some(external)
            } else {
                None
            };
            trace.visiting.remove(&binding);
            return resolved;
        }
        Some(path.to_vec())
    }
    fn impl_mentions_type(
        root: &[Item],
        module: &[String],
        imp: &syn::ItemImpl,
        wanted: &[String],
        legacy_use_paths: bool,
        scopes: &[LocalScope<'_>],
    ) -> Result<bool, String> {
        let Some(wanted) = resolve_path(
            root,
            &[],
            wanted,
            legacy_use_paths,
            false,
            &mut PathTrace::default(),
        ) else {
            return Ok(false);
        };
        struct Mentions<'a> {
            root: &'a [Item],
            module: &'a [String],
            wanted: &'a [String],
            generics: &'a syn::Generics,
            scopes: &'a [LocalScope<'a>],
            legacy_use_paths: bool,
            found: bool,
            error: Option<String>,
        }
        impl<'ast> syn::visit::Visit<'ast> for Mentions<'_> {
            fn visit_type(&mut self, ty: &'ast syn::Type) {
                let inner = match ty {
                    syn::Type::Reference(ty) => Some(&ty.elem),
                    syn::Type::Paren(ty) => Some(&ty.elem),
                    syn::Type::Group(ty) => Some(&ty.elem),
                    _ => None,
                };
                if let Some(inner) = inner {
                    self.visit_type(inner);
                    return;
                }
                if let Some(path) = type_path(ty, self.legacy_use_paths)
                    && !self.generics.params.iter().any(|param| {
                        matches!(param, syn::GenericParam::Type(param)
                        if path.first().is_some_and(|first| {
                            first == crate::imports::identifier_key(&param.ident.to_string())
                        }))
                    })
                {
                    match resolve_block_path(
                        self.root,
                        self.module,
                        self.scopes,
                        &path,
                        self.legacy_use_paths,
                        false,
                        &mut PathTrace::default(),
                    ) {
                        Ok(Some(path)) if path == self.wanted => self.found = true,
                        Err(error) => self.error = Some(error),
                        _ => {}
                    }
                }
            }
        }
        let mut mentions = Mentions {
            root,
            module,
            wanted: &wanted,
            generics: &imp.generics,
            scopes,
            legacy_use_paths,
            found: false,
            error: None,
        };
        if let Some((_, trait_, _)) = &imp.trait_ {
            // Rustdoc attaches impls to the outer types of the self type
            // and trait arguments, unwrapping references but not generics.
            syn::visit::Visit::visit_type(&mut mentions, &imp.self_ty);
            syn::visit::Visit::visit_path(&mut mentions, trait_);
        } else if let Some(path) = type_path(&imp.self_ty, legacy_use_paths) {
            mentions.found = resolve_block_path(
                root,
                module,
                scopes,
                &path,
                legacy_use_paths,
                false,
                &mut PathTrace::default(),
            )?
            .is_some_and(|path| path == wanted);
        }
        if let Some(error) = mentions.error {
            return Err(error);
        }
        Ok(mentions.found)
    }
    fn api_dependencies(
        source: &str,
        root: &[Item],
        module: &[String],
        scopes: &[LocalScope<'_>],
        item: &Item,
        legacy_use_paths: bool,
        selection: (Option<&syn::Variant>, Option<&syn::ForeignItem>),
    ) -> Result<HashSet<String>, String> {
        let (variant, foreign) = selection;
        struct Dependencies<'a> {
            source: &'a str,
            root: &'a [Item],
            module: Vec<String>,
            scopes: Vec<LocalScope<'a>>,
            legacy_use_paths: bool,
            shadowed: Vec<String>,
            bindings: Vec<String>,
            trace: PathTrace<'a>,
            shapes: HashSet<String>,
            error: Option<String>,
        }
        impl<'ast> syn::visit::Visit<'ast> for Dependencies<'ast> {
            fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}
            fn visit_expr(&mut self, expression: &'ast syn::Expr) {
                let evaluation = self.trace.evaluation;
                self.trace.evaluation = true;
                syn::visit::visit_expr(self, expression);
                self.trace.evaluation = evaluation;
            }
            fn visit_generic_argument(&mut self, argument: &'ast syn::GenericArgument) {
                // Syn cannot distinguish `Container<LEN>` from a type argument.
                if let syn::GenericArgument::Type(syn::Type::Path(path)) = argument
                    && path.qself.is_none()
                    && path.path.segments.len() == 1
                {
                    let parts = path_parts(&path.path, self.legacy_use_paths);
                    if !parts
                        .first()
                        .is_some_and(|name| self.shadowed.contains(name))
                    {
                        let mut trace = PathTrace {
                            value_path: true,
                            ..PathTrace::default()
                        };
                        let resolved = resolve_block_path(
                            self.root,
                            &self.module,
                            &self.scopes,
                            &parts,
                            self.legacy_use_paths,
                            true,
                            &mut trace,
                        );
                        if trace
                            .values
                            .iter()
                            .any(|value| matches!(value.item, Item::Const(_)))
                        {
                            self.trace.aliases.extend(trace.aliases);
                            self.trace.values.extend(trace.values);
                            match resolved {
                                Ok(resolved) => match text(self.source, path.path.span()) {
                                    Ok(path) => {
                                        self.shapes.insert(format!(
                                            "path {path} resolves to {resolved:?}"
                                        ));
                                    }
                                    Err(error) => self.error = Some(error),
                                },
                                Err(error) => self.error = Some(error),
                            }
                            for segment in &path.path.segments {
                                self.visit_path_arguments(&segment.arguments);
                            }
                            return;
                        }
                    }
                }
                syn::visit::visit_generic_argument(self, argument);
            }
            fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
                self.visit_signature(&item.sig);
            }
            fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
                self.visit_signature(&item.sig);
            }
            fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
                self.visit_signature(&item.sig);
            }
            fn visit_block(&mut self, block: &'ast syn::Block) {
                let shadowed = self.shadowed.clone();
                let bindings = self.bindings.clone();
                self.scopes.push(LocalScope {
                    module: self.module.clone(),
                    module_scope: false,
                    items: block
                        .stmts
                        .iter()
                        .filter_map(|stmt| match stmt {
                            syn::Stmt::Item(item) => Some(item),
                            _ => None,
                        })
                        .collect(),
                });
                // Inner item declarations shadow bindings from outer blocks.
                for item in &self.scopes.last().unwrap().items {
                    let ident = match item {
                        Item::Const(item) => &item.ident,
                        Item::Static(item) => &item.ident,
                        Item::Fn(item) => &item.sig.ident,
                        _ => continue,
                    };
                    let name = crate::imports::identifier_key(&ident.to_string()).to_string();
                    self.shadowed.retain(|binding| binding != &name);
                    self.bindings.retain(|binding| binding != &name);
                }
                for stmt in &block.stmts {
                    // Item definitions affect evaluation only when referenced.
                    if !matches!(stmt, syn::Stmt::Item(_)) {
                        self.visit_stmt(stmt);
                    }
                }
                self.scopes.pop();
                self.shadowed = shadowed;
                self.bindings = bindings;
            }
            fn visit_local(&mut self, local: &'ast syn::Local) {
                // The initializer and let-else branch precede the new bindings.
                if let Some(initializer) = &local.init {
                    self.visit_expr(&initializer.expr);
                    if let Some((_, diverge)) = &initializer.diverge {
                        self.visit_expr(diverge);
                    }
                }
                self.visit_pat(&local.pat);
            }
            fn visit_pat_ident(&mut self, pattern: &'ast syn::PatIdent) {
                let name = crate::imports::identifier_key(&pattern.ident.to_string()).to_string();
                let mut trace = PathTrace {
                    value_path: true,
                    evaluation: true,
                    pattern: true,
                    ..PathTrace::default()
                };
                let resolved = resolve_block_path(
                    self.root,
                    &self.module,
                    &self.scopes,
                    std::slice::from_ref(&name),
                    self.legacy_use_paths,
                    true,
                    &mut trace,
                );
                match resolved {
                    Ok(Some(path))
                        if trace.values.iter().any(|value| match value.item {
                            Item::Const(_) => true,
                            Item::Struct(item) => matches!(item.fields, syn::Fields::Unit),
                            Item::Enum(_) => value.name != path.join("::"),
                            _ => false,
                        }) || path.first().is_some_and(|part| part == "::") =>
                    {
                        self.shapes
                            .insert(format!("pattern {name} resolves to {path:?}"));
                        self.trace.aliases.extend(trace.aliases);
                        self.trace.values.extend(trace.values);
                    }
                    Err(error) => self.error = Some(error),
                    _ => self.bindings.push(name),
                }
                syn::visit::visit_pat_ident(self, pattern);
            }
            fn visit_expr_let(&mut self, expression: &'ast syn::ExprLet) {
                self.visit_expr(&expression.expr);
                self.visit_pat(&expression.pat);
            }
            fn visit_expr_if(&mut self, expression: &'ast syn::ExprIf) {
                let previous = self.bindings.len();
                self.visit_expr(&expression.cond);
                self.visit_block(&expression.then_branch);
                self.bindings.truncate(previous);
                if let Some((_, branch)) = &expression.else_branch {
                    self.visit_expr(branch);
                }
            }
            fn visit_expr_while(&mut self, expression: &'ast syn::ExprWhile) {
                let previous = self.bindings.len();
                self.visit_expr(&expression.cond);
                self.visit_block(&expression.body);
                self.bindings.truncate(previous);
            }
            fn visit_expr_for_loop(&mut self, expression: &'ast syn::ExprForLoop) {
                self.visit_expr(&expression.expr);
                let previous = self.bindings.len();
                self.visit_pat(&expression.pat);
                self.visit_block(&expression.body);
                self.bindings.truncate(previous);
            }
            fn visit_arm(&mut self, arm: &'ast syn::Arm) {
                let previous = self.bindings.len();
                self.visit_pat(&arm.pat);
                if let Some((_, guard)) = &arm.guard {
                    self.visit_expr(guard);
                }
                self.visit_expr(&arm.body);
                self.bindings.truncate(previous);
            }
            fn visit_generics(&mut self, generics: &'ast syn::Generics) {
                self.shadowed
                    .extend(generics.params.iter().filter_map(|param| {
                        let ident = match param {
                            syn::GenericParam::Type(param) => &param.ident,
                            syn::GenericParam::Const(param) => &param.ident,
                            syn::GenericParam::Lifetime(_) => return None,
                        };
                        Some(crate::imports::identifier_key(&ident.to_string()).to_string())
                    }));
                syn::visit::visit_generics(self, generics);
            }
            fn visit_item(&mut self, item: &'ast Item) {
                let previous = self.shadowed.len();
                syn::visit::visit_item(self, item);
                self.shadowed.truncate(previous);
            }
            fn visit_signature(&mut self, signature: &'ast syn::Signature) {
                let previous = self.shadowed.len();
                let bindings = self.bindings.len();
                syn::visit::visit_signature(self, signature);
                self.shadowed.truncate(previous);
                self.bindings.truncate(bindings);
            }
            fn visit_trait_item(&mut self, item: &'ast syn::TraitItem) {
                let previous = self.shadowed.len();
                syn::visit::visit_trait_item(self, item);
                self.shadowed.truncate(previous);
            }
            fn visit_impl_item(&mut self, item: &'ast ImplItem) {
                let previous = self.shadowed.len();
                syn::visit::visit_impl_item(self, item);
                self.shadowed.truncate(previous);
            }
            fn visit_expr_path(&mut self, expression: &'ast syn::ExprPath) {
                let parts = path_parts(&expression.path, self.legacy_use_paths);
                if parts.first().is_some_and(|name| {
                    self.shadowed.contains(name) || self.bindings.contains(name)
                }) {
                    for segment in &expression.path.segments {
                        self.visit_path_arguments(&segment.arguments);
                    }
                    return;
                }
                if expression.qself.is_some() {
                    self.error = Some("non-doc API extraction is incomplete: cannot verify an associated constant evaluation dependency".into());
                    return;
                }
                self.trace.value_path = true;
                let resolved = resolve_block_path(
                    self.root,
                    &self.module,
                    &self.scopes,
                    &parts,
                    self.legacy_use_paths,
                    true,
                    &mut self.trace,
                );
                self.trace.value_path = false;
                match resolved {
                    Ok(Some(resolved)) => match text(self.source, expression.path.span()) {
                        Ok(path) => {
                            self.shapes
                                .insert(format!("path {path} resolves to {:?}", Some(resolved)));
                        }
                        Err(error) => self.error = Some(error),
                    },
                    Ok(None) => {
                        self.error = Some(format!(
                            "non-doc API extraction is incomplete: cannot verify constant evaluation dependency '{}'",
                            parts.join("::")
                        ))
                    }
                    Err(error) => self.error = Some(error),
                }
                // Generic arguments contain their own type/value dependencies.
                for segment in &expression.path.segments {
                    self.visit_path_arguments(&segment.arguments);
                }
            }
            fn visit_expr_method_call(&mut self, _: &'ast syn::ExprMethodCall) {
                self.error = Some("non-doc API extraction is incomplete: cannot verify a method used in constant evaluation".into());
            }
            fn visit_path(&mut self, path: &'ast syn::Path) {
                let parts = path_parts(path, self.legacy_use_paths);
                if !parts
                    .first()
                    .is_some_and(|name| self.shadowed.contains(name))
                {
                    match resolve_block_path(
                        self.root,
                        &self.module,
                        &self.scopes,
                        &parts,
                        self.legacy_use_paths,
                        true,
                        &mut self.trace,
                    ) {
                        Ok(resolved) => match text(self.source, path.span()) {
                            Ok(path) => {
                                self.shapes
                                    .insert(format!("path {path} resolves to {resolved:?}"));
                            }
                            Err(error) => self.error = Some(error),
                        },
                        Err(error) => self.error = Some(error),
                    }
                }
                syn::visit::visit_path(self, path);
            }
        }
        fn pattern_bindings(pattern: &syn::Pat, bindings: &mut Vec<String>) {
            struct Bindings<'a>(&'a mut Vec<String>);
            impl<'ast> syn::visit::Visit<'ast> for Bindings<'_> {
                fn visit_pat_ident(&mut self, pattern: &'ast syn::PatIdent) {
                    self.0.push(
                        crate::imports::identifier_key(&pattern.ident.to_string()).to_string(),
                    );
                    syn::visit::visit_pat_ident(self, pattern);
                }
            }
            syn::visit::Visit::visit_pat(&mut Bindings(bindings), pattern);
        }
        let mut dependencies = Dependencies {
            source,
            root,
            module: module.to_vec(),
            scopes: scopes.to_vec(),
            legacy_use_paths,
            shadowed: Vec::new(),
            bindings: Vec::new(),
            trace: PathTrace::default(),
            shapes: HashSet::new(),
            error: None,
        };
        if let Some(foreign) = foreign {
            syn::visit::Visit::visit_foreign_item(&mut dependencies, foreign);
        } else if let Some(variant) = variant {
            if let Item::Enum(item) = item {
                syn::visit::Visit::visit_generics(&mut dependencies, &item.generics);
            }
            syn::visit::Visit::visit_variant(&mut dependencies, variant);
        } else {
            syn::visit::Visit::visit_item(&mut dependencies, item);
        }
        let mut seen = HashSet::new();
        let mut alias_index = 0;
        let mut value_index = 0;
        // Aliases and evaluated values can refer to each other. Compare their
        // complete closure rather than trusting unchanged path spellings.
        loop {
            if let Some(alias) = dependencies.trace.aliases.get(alias_index).cloned() {
                alias_index += 1;
                let start = alias.item.ident.span().start();
                if !seen.insert((start.line, start.column)) {
                    continue;
                }
                dependencies.shapes.insert(format!(
                    "type alias {}{} = {}",
                    alias.name,
                    text(source, alias.item.generics.span())?,
                    text(source, alias.item.ty.span())?
                ));
                dependencies.shadowed.clear();
                dependencies.bindings.clear();
                dependencies.module = alias.module;
                dependencies.scopes = alias.scopes;
                syn::visit::Visit::visit_generics(&mut dependencies, &alias.item.generics);
                syn::visit::Visit::visit_type(&mut dependencies, &alias.item.ty);
            } else if let Some(value) = dependencies.trace.values.get(value_index).cloned() {
                value_index += 1;
                let start = value.item.span().start();
                if !seen.insert((start.line, start.column)) {
                    continue;
                }
                dependencies.shadowed.clear();
                dependencies.bindings.clear();
                dependencies.module = value.module;
                dependencies.scopes = value.scopes;
                let definition = match value.item {
                    Item::Const(item) => format!(
                        "const {}{}: {} = {}",
                        value.name,
                        text(source, item.generics.span())?,
                        text(source, item.ty.span())?,
                        text(source, item.expr.span())?
                    ),
                    Item::Static(item) => format!(
                        "static {}: {} = {}",
                        value.name,
                        text(source, item.ty.span())?,
                        text(source, item.expr.span())?
                    ),
                    Item::Fn(item) => {
                        let body = if item.sig.constness.is_some() {
                            text(source, item.block.span())?
                        } else {
                            ""
                        };
                        format!(
                            "function {}: {} {body}",
                            value.name,
                            text(source, item.sig.span())?
                        )
                    }
                    Item::Struct(_) | Item::Enum(_) | Item::Union(_) => format!(
                        "evaluation type {}: {}",
                        value.name,
                        text(source, value.item.span())?
                    ),
                    _ => unreachable!(),
                };
                dependencies.shapes.insert(definition);
                syn::visit::Visit::visit_item(&mut dependencies, value.item);
                if let Item::Fn(item) = value.item
                    && item.sig.constness.is_some()
                {
                    syn::visit::Visit::visit_generics(&mut dependencies, &item.sig.generics);
                    for argument in &item.sig.inputs {
                        if let syn::FnArg::Typed(argument) = argument {
                            pattern_bindings(&argument.pat, &mut dependencies.bindings);
                        }
                    }
                    syn::visit::Visit::visit_block(&mut dependencies, &item.block);
                }
            } else {
                break;
            }
        }
        if let Some(error) = dependencies.error {
            return Err(error);
        }
        Ok(dependencies.shapes)
    }
    // Items in any expression block can add implementations to module-level
    // types, even inside private functions that are never called.
    struct BlockItems<'ast, 'walk> {
        source: &'ast str,
        root: &'ast [Item],
        scopes: Vec<LocalScope<'ast>>,
        modules: &'walk mut Vec<String>,
        wanted: (&'walk [String], bool),
        api: &'walk mut ExpandedApi,
        deprecation: Option<&'walk rustdoc_types::Deprecation>,
        legacy_use_paths: bool,
        error: Option<String>,
    }
    impl<'ast> syn::visit::Visit<'ast> for BlockItems<'ast, '_> {
        fn visit_item(&mut self, item: &'ast Item) {
            if self.error.is_none() {
                self.error = walk(
                    self.source,
                    self.root,
                    (std::slice::from_ref(item), self.deprecation, &self.scopes),
                    self.modules,
                    self.wanted,
                    self.api,
                    self.legacy_use_paths,
                )
                .err();
            }
        }
        // Modules are traversed by walk, with their own module path.
        fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {}
        fn visit_attribute(&mut self, _: &'ast syn::Attribute) {}
        fn visit_block(&mut self, block: &'ast syn::Block) {
            if self.error.is_some() {
                return;
            }
            self.scopes.push(LocalScope {
                module: self.modules.clone(),
                module_scope: false,
                items: block
                    .stmts
                    .iter()
                    .filter_map(|stmt| match stmt {
                        syn::Stmt::Item(item) => Some(item),
                        _ => None,
                    })
                    .collect(),
            });
            syn::visit::visit_block(self, block);
            self.scopes.pop();
        }
    }
    fn walk(
        source: &str,
        root: &[Item],
        items: (
            &[Item],
            Option<&rustdoc_types::Deprecation>,
            &[LocalScope<'_>],
        ),
        modules: &mut Vec<String>,
        wanted: (&[String], bool),
        api: &mut ExpandedApi,
        legacy_use_paths: bool,
    ) -> Result<(), String> {
        let (items, inherited_deprecation, scopes) = items;
        let (wanted, type_only) = wanted;
        for item in items {
            let attrs = match item {
                Item::Fn(item) => &item.attrs,
                Item::Const(item) => &item.attrs,
                Item::Static(item) => &item.attrs,
                Item::Impl(item) => &item.attrs,
                Item::Trait(item) => &item.attrs,
                _ => &[][..],
            };
            let deprecation = expanded_deprecation(attrs, inherited_deprecation);
            let mut blocks = BlockItems {
                source,
                root,
                scopes: scopes.to_vec(),
                modules,
                wanted: (wanted, type_only),
                api,
                deprecation: deprecation.as_ref(),
                legacy_use_paths,
                error: None,
            };
            syn::visit::visit_item(&mut blocks, item);
            if let Some(error) = blocks.error {
                return Err(error);
            }
            if type_only
                && matches!(
                    item,
                    Item::Fn(_) | Item::Const(_) | Item::Static(_) | Item::Macro(_)
                )
            {
                continue;
            }
            if let Item::Mod(module) = item {
                let deprecation = expanded_deprecation(&module.attrs, inherited_deprecation);
                modules.push(crate::imports::identifier_key(&module.ident.to_string()).to_string());
                if modules == wanted {
                    api.shapes
                        .insert(format!("module {}", visibility(source, &module.vis)?));
                }
                if let Some((_, children)) = &module.content {
                    let mut module_scopes = scopes.to_vec();
                    if !scopes.is_empty() {
                        module_scopes.push(LocalScope {
                            module: modules.clone(),
                            items: children.iter().collect(),
                            module_scope: true,
                        });
                    }
                    walk(
                        source,
                        root,
                        (children, deprecation.as_ref(), &module_scopes),
                        modules,
                        (wanted, type_only),
                        api,
                        legacy_use_paths,
                    )?;
                }
                modules.pop();
                continue;
            }
            if let Item::ForeignMod(block) = item {
                let deprecation = expanded_deprecation(&block.attrs, inherited_deprecation);
                for foreign in &block.items {
                    let normalized = match foreign {
                        syn::ForeignItem::Verbatim(tokens) => foreign_verbatim_declaration(tokens),
                        _ => None,
                    };
                    let safe = normalized.as_ref().is_some_and(|(_, safe)| *safe);
                    let foreign = normalized.as_ref().map_or(foreign, |(item, _)| item);
                    let (name, attrs, vis, declaration) = match foreign {
                        syn::ForeignItem::Fn(function) if !type_only => (
                            &function.sig.ident,
                            function.attrs.as_slice(),
                            &function.vis,
                            format!("{};", text(source, function.sig.span())?),
                        ),
                        syn::ForeignItem::Static(static_) if !type_only => (
                            &static_.ident,
                            static_.attrs.as_slice(),
                            &static_.vis,
                            format!(
                                "static {}{}: {};",
                                if matches!(static_.mutability, syn::StaticMutability::Mut(_)) {
                                    "mut "
                                } else {
                                    ""
                                },
                                static_.ident,
                                text(source, static_.ty.span())?
                            ),
                        ),
                        syn::ForeignItem::Type(ty) => (
                            &ty.ident,
                            ty.attrs.as_slice(),
                            &ty.vis,
                            format!("type {}{};", ty.ident, text(source, ty.generics.span())?),
                        ),
                        _ => continue,
                    };
                    if !scopes.is_empty()
                        || modules.len() + 1 != wanted.len()
                        || modules != &wanted[..modules.len()]
                        || crate::imports::identifier_key(&name.to_string())
                            != wanted[modules.len()]
                    {
                        continue;
                    }
                    let link_attributes = block
                        .attrs
                        .iter()
                        .chain(attrs)
                        .filter(|attr| {
                            ["link", "link_name", "link_ordinal"]
                                .iter()
                                .any(|name| attr.path().is_ident(name))
                        })
                        .map(|attr| text(source, attr.span()).map(str::to_string))
                        .collect::<Result<Vec<_>, _>>()?;
                    let shape = format!(
                        "foreign {}{}: {} {} {} {link_attributes:?}",
                        if block.unsafety.is_some() {
                            "unsafe "
                        } else {
                            ""
                        },
                        text(source, block.abi.span())?,
                        visibility(source, vis)?,
                        if safe { "safe" } else { "unsafe" },
                        declaration,
                    );
                    api.has_definition = true;
                    api.shapes.insert(shape.clone());
                    api.dependencies
                        .entry("definition".into())
                        .or_default()
                        .push(api_dependencies(
                            source,
                            root,
                            modules,
                            scopes,
                            item,
                            legacy_use_paths,
                            (None, Some(foreign)),
                        )?);
                    api.attributes.entry("item".into()).or_default().push(
                        expanded_member_attributes("item", attrs, deprecation.as_ref()),
                    );
                    if let syn::ForeignItem::Fn(function) = foreign {
                        let key = member_key("function", &function.sig.ident.to_string());
                        api.members.push((key.clone(), shape.clone()));
                        api.attributes
                            .entry(key)
                            .or_default()
                            .push(expanded_member_attributes(
                                &shape,
                                attrs,
                                deprecation.as_ref(),
                            ));
                    }
                }
                continue;
            }
            let (name, attrs) = match item {
                Item::Struct(item) => (Some(&item.ident), item.attrs.as_slice()),
                Item::Enum(item) => (Some(&item.ident), item.attrs.as_slice()),
                Item::Union(item) => (Some(&item.ident), item.attrs.as_slice()),
                Item::Fn(item) => (Some(&item.sig.ident), item.attrs.as_slice()),
                Item::Trait(item) => (Some(&item.ident), item.attrs.as_slice()),
                Item::TraitAlias(item) => (Some(&item.ident), item.attrs.as_slice()),
                Item::Type(item) => (Some(&item.ident), item.attrs.as_slice()),
                Item::Const(item) => (Some(&item.ident), item.attrs.as_slice()),
                Item::Static(item) => (Some(&item.ident), item.attrs.as_slice()),
                Item::ExternCrate(item) => (
                    Some(item.rename.as_ref().map_or(&item.ident, |(_, name)| name)),
                    item.attrs.as_slice(),
                ),
                Item::Macro(item) => (item.ident.as_ref(), item.attrs.as_slice()),
                _ => (None, &[][..]),
            };
            let matches = scopes.is_empty()
                && name.is_some_and(|name| {
                    modules.len() + 1 == wanted.len()
                        && modules == &wanted[..modules.len()]
                        && crate::imports::identifier_key(&name.to_string())
                            == wanted[modules.len()]
                });
            if matches {
                api.has_definition = true;
                api.dependencies
                    .entry("definition".into())
                    .or_default()
                    .push(api_dependencies(
                        source,
                        root,
                        modules,
                        scopes,
                        item,
                        legacy_use_paths,
                        (None, None),
                    )?);
                let deprecation = expanded_deprecation(attrs, inherited_deprecation);
                api.attributes
                    .entry("item".into())
                    .or_default()
                    .push(expanded_member_attributes(
                        "item",
                        attrs,
                        inherited_deprecation,
                    ));
                match item {
                    Item::Struct(item) => {
                        api.shapes.insert(format!(
                            "struct {} {}",
                            visibility(source, &item.vis)?,
                            text(source, item.generics.span())?
                        ));
                        fields(source, &item.fields, "struct", api, deprecation.as_ref())?;
                    }
                    Item::Enum(item) => {
                        api.shapes.insert(format!(
                            "enum {} {}",
                            visibility(source, &item.vis)?,
                            text(source, item.generics.span())?
                        ));
                        for variant in &item.variants {
                            let prefix = member_key("variant", &variant.ident.to_string());
                            let shape = variant_shape(source, variant)?;
                            api.shapes.insert(shape.clone());
                            api.members.push((prefix.clone(), shape.clone()));
                            api.attributes.entry(prefix.clone()).or_default().push(
                                expanded_member_attributes(
                                    &shape,
                                    &variant.attrs,
                                    deprecation.as_ref(),
                                ),
                            );
                            let deprecation =
                                expanded_deprecation(&variant.attrs, deprecation.as_ref());
                            fields(source, &variant.fields, &prefix, api, deprecation.as_ref())?;
                        }
                    }
                    Item::Union(item) => {
                        api.shapes.insert(format!(
                            "union {} {}",
                            visibility(source, &item.vis)?,
                            text(source, item.generics.span())?
                        ));
                        for field in &item.fields.named {
                            let shape = format!(
                                "union field {}: {} {}",
                                field.ident.as_ref().expect("union fields are named"),
                                visibility(source, &field.vis)?,
                                text(source, field.ty.span())?
                            );
                            api.shapes.insert(shape.clone());
                            if matches!(field.vis, Visibility::Public(_)) {
                                let key = member_key(
                                    "union field",
                                    &field.ident.as_ref().unwrap().to_string(),
                                );
                                api.attributes.entry(key.clone()).or_default().push(
                                    expanded_member_attributes(
                                        &shape,
                                        &field.attrs,
                                        deprecation.as_ref(),
                                    ),
                                );
                                api.members.push((key, shape));
                            } else {
                                let key = "union private fields".to_string();
                                if !api.members.iter().any(|(member, _)| member == &key) {
                                    api.members.push((key.clone(), key));
                                }
                            }
                        }
                    }
                    Item::Fn(item) => {
                        let shape = format!(
                            "fn {} {}",
                            visibility(source, &item.vis)?,
                            text(source, item.sig.span())?
                        );
                        api.shapes.insert(shape.clone());
                        let key = member_key("function", &item.sig.ident.to_string());
                        api.members.push((key.clone(), shape.clone()));
                        api.attributes
                            .entry(key)
                            .or_default()
                            .push(expanded_member_attributes(
                                &shape,
                                &item.attrs,
                                inherited_deprecation,
                            ));
                    }
                    Item::Trait(item) => {
                        api.shapes.insert(format!(
                            "{}{}trait {} {} {}",
                            if item.unsafety.is_some() {
                                "unsafe "
                            } else {
                                ""
                            },
                            if item.auto_token.is_some() {
                                "auto "
                            } else {
                                ""
                            },
                            visibility(source, &item.vis)?,
                            text(source, item.generics.span())?,
                            text(source, item.supertraits.span())?
                        ));
                        for member in &item.items {
                            let (key, shape) = match member {
                                syn::TraitItem::Fn(method) => {
                                    let requirement = if method.default.is_some() {
                                        "provided"
                                    } else {
                                        "required"
                                    };
                                    (
                                        trait_member_key(
                                            "method",
                                            &method.sig.ident.to_string(),
                                            method.default.is_some(),
                                        ),
                                        format!(
                                            "{requirement} trait method {}",
                                            text(source, method.sig.span())?
                                        ),
                                    )
                                }
                                syn::TraitItem::Const(constant) => {
                                    let key = trait_member_key(
                                        "const",
                                        &constant.ident.to_string(),
                                        constant.default.is_some(),
                                    );
                                    let default = constant
                                        .default
                                        .as_ref()
                                        .map(|(_, value)| {
                                            text(source, value.span())
                                                .map(|value| format!(" = {value}"))
                                        })
                                        .transpose()?
                                        .unwrap_or_default();
                                    let shape = format!(
                                        "{key}{}: {}{default}",
                                        text(source, constant.generics.span())?,
                                        text(source, constant.ty.span())?
                                    );
                                    (key, shape)
                                }
                                syn::TraitItem::Type(ty) => {
                                    let key = trait_member_key(
                                        "type",
                                        &ty.ident.to_string(),
                                        ty.default.is_some(),
                                    );
                                    let default = ty
                                        .default
                                        .as_ref()
                                        .map(|(_, value)| {
                                            text(source, value.span())
                                                .map(|value| format!(" = {value}"))
                                        })
                                        .transpose()?
                                        .unwrap_or_default();
                                    let shape = format!(
                                        "{key}{}: {}{default}",
                                        text(source, ty.generics.span())?,
                                        text(source, ty.bounds.span())?
                                    );
                                    (key, shape)
                                }
                                _ => continue,
                            };
                            api.shapes.insert(shape.clone());
                            api.members.push((key.clone(), shape.clone()));
                            let attrs = match member {
                                syn::TraitItem::Fn(member) => &member.attrs,
                                syn::TraitItem::Const(member) => &member.attrs,
                                syn::TraitItem::Type(member) => &member.attrs,
                                _ => unreachable!(),
                            };
                            api.attributes.entry(key).or_default().push(
                                expanded_member_attributes(&shape, attrs, deprecation.as_ref()),
                            );
                        }
                    }
                    _ => {
                        api.shapes
                            .insert(format!("item {}", text(source, item.span())?));
                    }
                }
            }
            if let Item::Enum(enum_item) = item
                && scopes.is_empty()
                && modules.len() + 2 == wanted.len()
                && modules == &wanted[..modules.len()]
                && crate::imports::identifier_key(&enum_item.ident.to_string())
                    == wanted[modules.len()]
                && let Some(variant) = enum_item.variants.iter().find(|variant| {
                    crate::imports::identifier_key(&variant.ident.to_string())
                        == wanted[modules.len() + 1]
                })
            {
                api.has_definition = true;
                api.dependencies
                    .entry("definition".into())
                    .or_default()
                    .push(api_dependencies(
                        source,
                        root,
                        modules,
                        scopes,
                        item,
                        legacy_use_paths,
                        (Some(variant), None),
                    )?);
                let prefix = member_key("variant", &variant.ident.to_string());
                let shape = variant_shape(source, variant)?;
                let deprecation = expanded_deprecation(&enum_item.attrs, inherited_deprecation);
                let attributes =
                    expanded_member_attributes(&shape, &variant.attrs, deprecation.as_ref());
                api.attributes
                    .entry("item".into())
                    .or_default()
                    .push(attributes.clone());
                api.attributes
                    .entry(prefix.clone())
                    .or_default()
                    .push(attributes);
                api.shapes.insert(shape.clone());
                api.members.push((prefix.clone(), shape));
                let deprecation = expanded_deprecation(&variant.attrs, deprecation.as_ref());
                fields(source, &variant.fields, &prefix, api, deprecation.as_ref())?;
            }
            if let Item::Use(item) = item
                && scopes.is_empty()
                && modules == &wanted[..wanted.len() - 1]
                && use_may_bind(&item.tree, &wanted[wanted.len() - 1])
            {
                api.shapes.insert(format!(
                    "use {} {}",
                    visibility(source, &item.vis)?,
                    text(source, item.tree.span())?
                ));
            }
            if let Item::Impl(imp) = item
                && impl_mentions_type(root, modules, imp, wanted, legacy_use_paths, scopes)?
            {
                let dependencies = api_dependencies(
                    source,
                    root,
                    modules,
                    scopes,
                    item,
                    legacy_use_paths,
                    (None, None),
                )?;
                let deprecation = expanded_deprecation(&imp.attrs, inherited_deprecation);
                let owner = text(source, imp.self_ty.span())?;
                let trait_name = imp
                    .trait_
                    .as_ref()
                    .map(|(_, path, _)| text(source, path.span()))
                    .transpose()?
                    .unwrap_or_default();
                let generics = if imp.generics.params.is_empty() {
                    String::new()
                } else {
                    format!("<{}>", text(source, imp.generics.params.span())?)
                };
                let where_clause = imp
                    .generics
                    .where_clause
                    .as_ref()
                    .map(|clause| text(source, clause.span()))
                    .transpose()?
                    .map_or_else(String::new, |clause| format!(" {clause}"));
                if let Some((polarity, trait_path, _)) = &imp.trait_ {
                    let derived = imp
                        .attrs
                        .iter()
                        .any(|attr| attr.path().is_ident("automatically_derived"));
                    let trait_name = if derived
                        && trait_path.leading_colon.is_some()
                        && trait_path.segments.first().is_some_and(|segment| {
                            segment.ident == "core" || segment.ident == "std"
                        }) {
                        trait_path.segments.last().unwrap().ident.to_string()
                    } else {
                        trait_name.to_string()
                    };
                    let safety = if imp.unsafety.is_some() {
                        "unsafe "
                    } else {
                        ""
                    };
                    let polarity = if polarity.is_some() { "!" } else { "" };
                    let prefix = if derived { "derived " } else { "" };
                    let shape = format!(
                        "{prefix}{safety}impl{generics} {polarity}{trait_name} for {owner}{where_clause}"
                    );
                    api.shapes.insert(shape.clone());
                    api.dependencies
                        .entry(shape.clone())
                        .or_default()
                        .push(dependencies.clone());
                    let canonical_trait = resolve_block_path(
                        root,
                        modules,
                        scopes,
                        &path_parts(trait_path, legacy_use_paths),
                        legacy_use_paths,
                        true,
                        &mut PathTrace::default(),
                    )?
                    .and_then(|path| path.last().cloned())
                    .unwrap_or_else(|| trait_path.segments.last().unwrap().ident.to_string());
                    api.members.push((
                        member_key(
                            if derived {
                                "derived trait impl"
                            } else {
                                "trait impl"
                            },
                            &canonical_trait,
                        ),
                        shape,
                    ));
                    if derived {
                        continue;
                    }
                }
                for member in &imp.items {
                    let signature = match member {
                        ImplItem::Fn(method) => Some(format!(
                            "{} {}",
                            visibility(source, &method.vis)?,
                            text(source, method.sig.span())?
                        )),
                        ImplItem::Const(constant) => Some(format!(
                            "const {}: {} = {}",
                            constant.ident,
                            text(source, constant.ty.span())?,
                            text(source, constant.expr.span())?
                        )),
                        ImplItem::Type(ty) => Some(format!(
                            "type {} = {}",
                            ty.ident,
                            text(source, ty.ty.span())?
                        )),
                        _ => None,
                    };
                    if let Some(signature) = signature {
                        let shape = format!(
                            "impl{generics} {trait_name} for {owner}{where_clause}: {signature}"
                        );
                        api.shapes.insert(shape.clone());
                        api.dependencies
                            .entry(shape.clone())
                            .or_default()
                            .push(dependencies.clone());
                        if imp.trait_.is_none() {
                            let key = match member {
                                ImplItem::Fn(method)
                                    if matches!(method.vis, Visibility::Public(_)) =>
                                {
                                    Some(member_key(
                                        "inherent method",
                                        &method.sig.ident.to_string(),
                                    ))
                                }
                                ImplItem::Const(constant)
                                    if matches!(constant.vis, Visibility::Public(_)) =>
                                {
                                    Some(member_key("inherent const", &constant.ident.to_string()))
                                }
                                _ => None,
                            };
                            if let Some(key) = key {
                                api.members.push((key.clone(), shape.clone()));
                                let attrs = match member {
                                    ImplItem::Fn(member) => &member.attrs,
                                    ImplItem::Const(member) => &member.attrs,
                                    _ => unreachable!(),
                                };
                                api.attributes.entry(key).or_default().push(
                                    expanded_member_attributes(&shape, attrs, deprecation.as_ref()),
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    let deprecation = expanded_deprecation(&file.attrs, None);
    let type_only = import.namespace == Some(crate::imports::NamespaceConstraint::Type);
    api.resolved_path = resolve_path(
        &file.items,
        &[],
        &wanted,
        legacy_use_paths,
        true,
        &mut PathTrace {
            query: true,
            type_only,
            ..PathTrace::default()
        },
    );
    // Resolve independently of Rustdoc's canonical path: intermediate aliases
    // can select another declaration while both declarations stay unchanged.
    let resolved = api.resolved_path.clone();
    let wanted = resolved
        .as_ref()
        .filter(|path| path.first().is_some_and(|part| part != "::"))
        .unwrap_or(&wanted);
    walk(
        source,
        &file.items,
        (&file.items, deprecation.as_ref(), &[]),
        &mut Vec::new(),
        (wanted, type_only),
        &mut api,
        legacy_use_paths,
    )?;
    Ok(api)
}

fn apply_non_doc_cfg(krate: &mut Crate, cfg: &RustcCfg) -> Result<(), String> {
    let mut unavailable = HashSet::new();
    for (id, item) in &krate.index {
        if !item_matches_cfg(item, cfg)? {
            unavailable.insert(*id);
        }
    }
    for item in krate.index.values() {
        let mut disabled_derives = cfg_attr_derive_difference(&item.attrs, cfg)?;
        if disabled_derives.contains("PartialEq") {
            disabled_derives.insert("StructuralPartialEq".into());
        }
        let impls = match &item.inner {
            rustdoc_types::ItemEnum::Struct(item) => &item.impls,
            rustdoc_types::ItemEnum::Enum(item) => &item.impls,
            rustdoc_types::ItemEnum::Union(item) => &item.impls,
            _ => continue,
        };
        for id in impls {
            let Some(impl_item) = krate.index.get(id) else {
                continue;
            };
            let rustdoc_types::ItemEnum::Impl(imp) = &impl_item.inner else {
                continue;
            };
            if impl_item
                .attrs
                .contains(&rustdoc_types::Attribute::AutomaticallyDerived)
                && imp.trait_.as_ref().is_some_and(|trait_| {
                    disabled_derives.contains(trait_.path.rsplit("::").next().unwrap_or(""))
                })
            {
                unavailable.insert(*id);
            }
        }
    }
    let mut pending = unavailable.iter().copied().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        let item = &krate.index[&id];
        let mut children = lexical_children(&item.inner);
        if matches!(item.inner, rustdoc_types::ItemEnum::Module(_)) {
            // Rustdoc omits impls from Module.items and records only the header
            // span for inline modules. Recover their body before pruning the
            // impl references attached to types declared elsewhere.
            let span = module_body_span(item)?;
            children.extend(krate.index.iter().filter_map(|(child_id, child)| {
                child
                    .span
                    .as_ref()
                    .filter(|child_span| {
                        child.crate_id == item.crate_id
                            && child_span.filename == span.filename
                            && child_span.begin >= span.begin
                            && child_span.end <= span.end
                    })
                    .map(|_| *child_id)
            }));
        }
        for child in children {
            if krate.index.contains_key(&child) && unavailable.insert(child) {
                pending.push(child);
            }
        }
    }
    for item in krate.index.values_mut() {
        reconcile_cfg_attr_deprecation(item, cfg)?;
        reconcile_cfg_attr_semantics(&mut item.attrs, cfg)?;
    }
    for id in &unavailable {
        if let Some(item) = krate.index.get_mut(id) {
            item.attrs.push(rustdoc_types::Attribute::Other(
                CFG_UNAVAILABLE_ATTRIBUTE.to_string(),
            ));
        }
    }
    for item in krate.index.values_mut() {
        prune_unavailable_references(&mut item.inner, &unavailable);
    }
    Ok(())
}

fn lexical_children(inner: &rustdoc_types::ItemEnum) -> Vec<rustdoc_types::Id> {
    use rustdoc_types::{ItemEnum, StructKind, VariantKind};
    match inner {
        ItemEnum::Module(module) => module.items.clone(),
        ItemEnum::Struct(struct_) => match &struct_.kind {
            StructKind::Plain { fields, .. } => fields.clone(),
            StructKind::Tuple(fields) => fields.iter().flatten().copied().collect(),
            StructKind::Unit => Vec::new(),
        },
        ItemEnum::Union(union_) => union_.fields.clone(),
        ItemEnum::Enum(enum_) => enum_.variants.clone(),
        ItemEnum::Variant(variant) => match &variant.kind {
            VariantKind::Struct { fields, .. } => fields.clone(),
            VariantKind::Tuple(fields) => fields.iter().flatten().copied().collect(),
            VariantKind::Plain => Vec::new(),
        },
        ItemEnum::Trait(trait_) => trait_.items.clone(),
        ItemEnum::Impl(impl_) => impl_.items.clone(),
        _ => Vec::new(),
    }
}

fn module_body_span(item: &rustdoc_types::Item) -> Result<rustdoc_types::Span, String> {
    let incomplete = |reason: String| {
        format!(
            "cannot establish non-doc lexical availability for module '{}': {reason}",
            item.name.as_deref().unwrap_or("<unnamed>")
        )
    };
    let mut span = item
        .span
        .clone()
        .ok_or_else(|| incomplete("source span missing".into()))?;
    let source = fs::read_to_string(&span.filename)
        .map_err(|error| incomplete(format!("{}: {error}", span.filename.display())))?;
    let file = syn::parse_file(&source).map_err(|error| incomplete(error.to_string()))?;
    struct ModuleBody<'a> {
        name: Option<&'a str>,
        end: (usize, usize),
        body_end: Option<(usize, usize)>,
    }
    impl<'ast> syn::visit::Visit<'ast> for ModuleBody<'_> {
        fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
            let end = module.ident.span().end();
            if self.name.is_some_and(|name| {
                crate::imports::identifier_key(&module.ident.to_string())
                    == crate::imports::identifier_key(name)
            }) && self.end == (end.line, end.column + 1)
                && let Some((brace, _)) = &module.content
            {
                let end = brace.span.close().end();
                self.body_end = Some((end.line, end.column + 1));
            }
            syn::visit::visit_item_mod(self, module);
        }
    }
    let mut visitor = ModuleBody {
        name: item.name.as_deref(),
        end: span.end,
        body_end: None,
    };
    syn::visit::Visit::visit_file(&mut visitor, &file);
    if let Some(end) = visitor.body_end {
        span.end = end;
    }
    Ok(span)
}

fn cfg_attr_outputs(
    attrs: &[rustdoc_types::Attribute],
    cfg: &RustcCfg,
) -> Result<Vec<syn::Meta>, String> {
    let mut outputs = Vec::new();
    for attr in attrs {
        let rustdoc_types::Attribute::Other(attribute) = attr else {
            continue;
        };
        if let Some(expression) = retained_attribute_expression(attribute, "cfg_attr") {
            collect_cfg_attr_outputs(expression, cfg, &mut outputs)?;
        }
    }
    Ok(outputs)
}

fn collect_cfg_attr_outputs(
    expression: &str,
    cfg: &RustcCfg,
    outputs: &mut Vec<syn::Meta>,
) -> Result<(), String> {
    let nested = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
        .parse_str(expression)
        .map_err(|error| format!("cannot parse cfg_attr '{expression}': {error}"))?;
    if nested.len() < 2 {
        return Err("cfg_attr requires a predicate and at least one output attribute".into());
    }
    let predicate = &nested[0];
    if !cfg_meta_matches(predicate, cfg)? {
        return Ok(());
    }
    for attribute in nested.into_iter().skip(1) {
        if let syn::Meta::List(list) = &attribute
            && cfg_path(&list.path) == "cfg_attr"
        {
            collect_cfg_attr_outputs(&list.tokens.to_string(), cfg, outputs)?;
        } else {
            outputs.push(attribute);
        }
    }
    Ok(())
}

fn derived_names(outputs: &[syn::Meta]) -> HashSet<String> {
    outputs
        .iter()
        .filter_map(|meta| match meta {
            syn::Meta::List(list) if cfg_path(&list.path) == "derive" => Some(&list.tokens),
            _ => None,
        })
        .filter_map(|tokens| {
            syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated
                .parse2(tokens.clone())
                .ok()
        })
        .flat_map(|paths| paths.iter().filter_map(derive_path).collect::<Vec<_>>())
        .collect()
}

fn cfg_attr_derive_difference(
    attrs: &[rustdoc_types::Attribute],
    cfg: &RustcCfg,
) -> Result<HashSet<String>, String> {
    let mut doc_cfg = cfg.clone();
    doc_cfg.flags.insert("doc".into());
    let normal = derived_names(&cfg_attr_outputs(attrs, cfg)?);
    Ok(derived_names(&cfg_attr_outputs(attrs, &doc_cfg)?)
        .difference(&normal)
        .map(|name| name.rsplit("::").next().unwrap_or(name).to_string())
        .collect())
}

fn reconcile_cfg_attr_deprecation(
    item: &mut rustdoc_types::Item,
    cfg: &RustcCfg,
) -> Result<(), String> {
    let mut doc_cfg = cfg.clone();
    doc_cfg.flags.insert("doc".into());
    let doc = cfg_attr_deprecation(&cfg_attr_outputs(&item.attrs, &doc_cfg)?);
    let normal = cfg_attr_deprecation(&cfg_attr_outputs(&item.attrs, cfg)?);
    if doc != normal && item.deprecation == doc {
        item.deprecation = normal;
    }
    Ok(())
}

fn cfg_attr_deprecation<'a>(
    outputs: impl IntoIterator<Item = &'a syn::Meta>,
) -> Option<rustdoc_types::Deprecation> {
    outputs.into_iter().find_map(|meta| match meta {
        syn::Meta::Path(path) if cfg_path(path) == "deprecated" => {
            Some(rustdoc_types::Deprecation {
                since: None,
                note: None,
            })
        }
        syn::Meta::NameValue(value) if cfg_path(&value.path) == "deprecated" => {
            let syn::Expr::Lit(value) = &value.value else {
                return None;
            };
            let syn::Lit::Str(note) = &value.lit else {
                return None;
            };
            Some(rustdoc_types::Deprecation {
                since: None,
                note: Some(note.value()),
            })
        }
        syn::Meta::List(list) if cfg_path(&list.path) == "deprecated" => {
            let fields = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
                .parse2(list.tokens.clone())
                .ok()?;
            let mut deprecation = rustdoc_types::Deprecation {
                since: None,
                note: None,
            };
            for field in fields {
                let syn::Meta::NameValue(field) = field else {
                    continue;
                };
                let syn::Expr::Lit(value) = field.value else {
                    continue;
                };
                let syn::Lit::Str(value) = value.lit else {
                    continue;
                };
                match cfg_path(&field.path).as_str() {
                    "since" => deprecation.since = Some(value.value()),
                    "note" => deprecation.note = Some(value.value()),
                    _ => {}
                }
            }
            Some(deprecation)
        }
        _ => None,
    })
}

fn reconcile_cfg_attr_semantics(
    attrs: &mut Vec<rustdoc_types::Attribute>,
    cfg: &RustcCfg,
) -> Result<(), String> {
    let mut doc_cfg = cfg.clone();
    doc_cfg.flags.insert("doc".into());
    let doc = cfg_attr_outputs(attrs, &doc_cfg)?;
    let normal = cfg_attr_outputs(attrs, cfg)?;

    let mut derives = derived_names(&normal).into_iter().collect::<Vec<_>>();
    derives.sort();
    if !derives.is_empty() {
        attrs.push(rustdoc_types::Attribute::Other(format!(
            "#[derive({})]",
            derives.join(", ")
        )));
    }

    let doc_semantics = semantic_cfg_attr_outputs(&doc);
    let normal_semantics = semantic_cfg_attr_outputs(&normal);
    for attribute in &doc_semantics {
        if !normal_semantics.contains(attribute)
            && let Some(position) = attrs.iter().position(|attr| attr == attribute)
        {
            attrs.remove(position);
        }
    }
    for attribute in normal_semantics {
        if !doc_semantics.contains(&attribute) && !attrs.contains(&attribute) {
            attrs.push(attribute);
        }
    }

    let doc_features = cfg_attr_target_features(&doc);
    let normal_features = cfg_attr_target_features(&normal);
    if doc_features != normal_features {
        let mut enable = attrs
            .iter()
            .filter_map(|attr| match attr {
                rustdoc_types::Attribute::TargetFeature { enable } => Some(enable),
                _ => None,
            })
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        for feature in &doc_features {
            // Rustdoc merges annotations but retains duplicate entries. Remove
            // one conditional occurrence so unconditional features survive.
            if let Some(position) = enable.iter().position(|entry| entry == feature) {
                enable.remove(position);
            }
        }
        enable.extend(normal_features);
        attrs.retain(|attr| !matches!(attr, rustdoc_types::Attribute::TargetFeature { .. }));
        if !enable.is_empty() {
            attrs.push(rustdoc_types::Attribute::TargetFeature { enable });
        }
    }

    let doc_repr = cfg_attr_repr(&doc);
    let normal_repr = cfg_attr_repr(&normal);
    if doc_repr != normal_repr {
        let position = attrs
            .iter()
            .position(|attr| matches!(attr, rustdoc_types::Attribute::Repr(_)));
        let mut repr = position
            .and_then(|position| match &attrs[position] {
                rustdoc_types::Attribute::Repr(repr) => Some(repr.clone()),
                _ => None,
            })
            .unwrap_or(rustdoc_types::AttributeRepr {
                kind: rustdoc_types::ReprKind::Rust,
                align: None,
                packed: None,
                int: None,
            });
        if doc_repr.kind != normal_repr.kind && repr.kind == doc_repr.kind {
            repr.kind = rustdoc_types::ReprKind::Rust;
        }
        if doc_repr.int != normal_repr.int && repr.int == doc_repr.int {
            repr.int = None;
        }
        if doc_repr.align != normal_repr.align && repr.align == doc_repr.align {
            repr.align = None;
        }
        if doc_repr.packed != normal_repr.packed && repr.packed == doc_repr.packed {
            repr.packed = None;
        }
        if normal_repr.kind != rustdoc_types::ReprKind::Rust {
            repr.kind = normal_repr.kind;
        }
        repr.int = normal_repr.int.or(repr.int);
        repr.align = normal_repr.align.or(repr.align);
        repr.packed = normal_repr.packed.or(repr.packed);
        if let Some(position) = position {
            attrs.remove(position);
        }
        if repr.kind != rustdoc_types::ReprKind::Rust
            || repr.int.is_some()
            || repr.align.is_some()
            || repr.packed.is_some()
        {
            attrs.push(rustdoc_types::Attribute::Repr(repr));
        }
    }
    Ok(())
}

fn semantic_cfg_attr_outputs<'a>(
    outputs: impl IntoIterator<Item = &'a syn::Meta>,
) -> Vec<rustdoc_types::Attribute> {
    outputs
        .into_iter()
        .filter_map(|meta| match meta {
            syn::Meta::Path(path) if cfg_path(path) == "non_exhaustive" => {
                Some(rustdoc_types::Attribute::NonExhaustive)
            }
            syn::Meta::Path(path) if cfg_path(path) == "must_use" => {
                Some(rustdoc_types::Attribute::MustUse { reason: None })
            }
            syn::Meta::NameValue(value) if cfg_path(&value.path) == "must_use" => {
                let syn::Expr::Lit(value) = &value.value else {
                    return None;
                };
                let syn::Lit::Str(reason) = &value.lit else {
                    return None;
                };
                Some(rustdoc_types::Attribute::MustUse {
                    reason: Some(reason.value()),
                })
            }
            _ => None,
        })
        .collect()
}

fn cfg_attr_target_features<'a>(outputs: impl IntoIterator<Item = &'a syn::Meta>) -> Vec<String> {
    outputs
        .into_iter()
        .filter_map(|meta| match meta {
            syn::Meta::List(list) if cfg_path(&list.path) == "target_feature" => {
                syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated
                    .parse2(list.tokens.clone())
                    .ok()
            }
            _ => None,
        })
        .flatten()
        .filter_map(|field| {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(value),
                ..
            }) = field.value
            else {
                return None;
            };
            (cfg_path(&field.path) == "enable").then(|| value.value())
        })
        .flat_map(|value| value.split(',').map(ToOwned::to_owned).collect::<Vec<_>>())
        .collect()
}

fn cfg_attr_repr<'a>(
    outputs: impl IntoIterator<Item = &'a syn::Meta>,
) -> rustdoc_types::AttributeRepr {
    let mut repr = rustdoc_types::AttributeRepr {
        kind: rustdoc_types::ReprKind::Rust,
        align: None,
        packed: None,
        int: None,
    };
    for meta in outputs {
        let syn::Meta::List(list) = meta else {
            continue;
        };
        if cfg_path(&list.path) != "repr" {
            continue;
        }
        let Ok(parts) = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
            .parse2(list.tokens.clone())
        else {
            continue;
        };
        for part in parts {
            match &part {
                syn::Meta::Path(path) => match cfg_path(path).as_str() {
                    "C" => repr.kind = rustdoc_types::ReprKind::C,
                    "transparent" => repr.kind = rustdoc_types::ReprKind::Transparent,
                    "simd" => repr.kind = rustdoc_types::ReprKind::Simd,
                    "Rust" => repr.kind = rustdoc_types::ReprKind::Rust,
                    "u8" | "u16" | "u32" | "u64" | "u128" | "usize" | "i8" | "i16" | "i32"
                    | "i64" | "i128" | "isize" => repr.int = Some(cfg_path(path)),
                    "packed" => repr.packed = Some(1),
                    _ => {}
                },
                syn::Meta::List(part) => {
                    let value = syn::parse2::<syn::LitInt>(part.tokens.clone())
                        .ok()
                        .and_then(|value| value.base10_parse().ok());
                    match cfg_path(&part.path).as_str() {
                        "align" => repr.align = value,
                        "packed" => repr.packed = value,
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
    repr
}

fn derive_path(path: &syn::Path) -> Option<String> {
    if path.segments.is_empty()
        || path
            .segments
            .iter()
            .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
    {
        return None;
    }
    Some(cfg_path(path))
}

fn cfg_all_matches(
    predicates: impl IntoIterator<Item = Result<bool, String>>,
) -> Result<bool, String> {
    // Check every operand, including those after a known false result.
    // Otherwise an unsupported predicate could become a boolean under negation.
    predicates
        .into_iter()
        .try_fold(true, |matches, next| Ok(next? && matches))
}

fn item_matches_cfg(item: &rustdoc_types::Item, cfg: &RustcCfg) -> Result<bool, String> {
    cfg_all_matches(item.attrs.iter().map(|attribute| {
        let rustdoc_types::Attribute::Other(attribute) = attribute else {
            return Ok(true);
        };
        if let Some(expression) = retained_attribute_expression(attribute, "cfg") {
            cfg_expression_matches(expression, cfg)
        } else if let Some(expression) = retained_attribute_expression(attribute, "cfg_attr") {
            cfg_attr_expression_matches(expression, cfg)
        } else {
            Ok(true)
        }
    }))
}

fn retained_attribute_expression<'a>(attribute: &'a str, name: &str) -> Option<&'a str> {
    let retained_prefix = format!("#[<{name}>(");
    let source_prefix = format!("#[{name}(");
    attribute
        .strip_prefix(&retained_prefix)
        .or_else(|| attribute.strip_prefix(&source_prefix))?
        .strip_suffix(")]")
}

fn cfg_expression_matches(expression: &str, cfg: &RustcCfg) -> Result<bool, String> {
    let meta = syn::parse_str::<syn::Meta>(expression)
        .map_err(|error| format!("cannot parse cfg predicate '{expression}': {error}"))?;
    cfg_meta_matches(&meta, cfg)
}

fn cfg_attr_expression_matches(expression: &str, cfg: &RustcCfg) -> Result<bool, String> {
    let nested = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
        .parse_str(expression)
        .map_err(|error| format!("cannot parse cfg_attr '{expression}': {error}"))?;
    if nested.len() < 2 {
        return Err("cfg_attr requires a predicate and at least one output attribute".into());
    }
    let predicate = &nested[0];
    if !cfg_meta_matches(predicate, cfg)? {
        return Ok(true);
    }
    cfg_all_matches(
        nested
            .iter()
            .skip(1)
            .map(|attribute| cfg_attr_output_matches(attribute, cfg)),
    )
}

fn cfg_attr_output_matches(attribute: &syn::Meta, cfg: &RustcCfg) -> Result<bool, String> {
    let syn::Meta::List(list) = attribute else {
        return Ok(true);
    };
    match cfg_path(&list.path).as_str() {
        "cfg" => cfg_expression_matches(&list.tokens.to_string(), cfg),
        "cfg_attr" => cfg_attr_expression_matches(&list.tokens.to_string(), cfg),
        _ => Ok(true),
    }
}

fn cfg_meta_matches(meta: &syn::Meta, cfg: &RustcCfg) -> Result<bool, String> {
    match meta {
        syn::Meta::Path(path) => Ok(cfg.contains_flag(&cfg_lookup_path(path))),
        syn::Meta::NameValue(name_value) => {
            let syn::Expr::Lit(expression) = &name_value.value else {
                return Err("cfg values must be string literals".into());
            };
            let syn::Lit::Str(value) = &expression.lit else {
                return Err("cfg values must be string literals".into());
            };
            Ok(cfg.contains_value(&cfg_lookup_path(&name_value.path), &value.value()))
        }
        syn::Meta::List(list) => {
            let name = cfg_path(&list.path);
            if name == "version" {
                return cfg.version_matches(&list.tokens);
            }
            if !matches!(name.as_str(), "all" | "any" | "not") {
                return Err(format!(
                    "cannot evaluate unsupported cfg predicate '{name}'"
                ));
            }
            let nested = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
                .parse2(list.tokens.clone())
                .map_err(|error| format!("cannot parse cfg predicate '{name}': {error}"))?;
            match name.as_str() {
                "all" => cfg_all_matches(nested.iter().map(|meta| cfg_meta_matches(meta, cfg))),
                "any" => cfg_all_matches(
                    nested
                        .iter()
                        .map(|meta| cfg_meta_matches(meta, cfg).map(|value| !value)),
                )
                .map(|value| !value),
                "not" if nested.len() == 1 => cfg_meta_matches(&nested[0], cfg).map(|value| !value),
                _ => Err("cfg(not(...)) requires exactly one predicate".into()),
            }
        }
    }
}

fn cfg_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn cfg_lookup_path(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| {
            let identifier = segment.ident.to_string();
            identifier
                .strip_prefix("r#")
                .unwrap_or(&identifier)
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("::")
}

fn prune_unavailable_references(
    inner: &mut rustdoc_types::ItemEnum,
    unavailable: &HashSet<rustdoc_types::Id>,
) {
    fn retain(ids: &mut Vec<rustdoc_types::Id>, unavailable: &HashSet<rustdoc_types::Id>) {
        ids.retain(|id| !unavailable.contains(id));
    }

    fn retain_optional(
        ids: &mut Vec<Option<rustdoc_types::Id>>,
        unavailable: &HashSet<rustdoc_types::Id>,
    ) {
        ids.retain(|id| id.is_none_or(|id| !unavailable.contains(&id)));
    }

    match inner {
        rustdoc_types::ItemEnum::Module(module) => retain(&mut module.items, unavailable),
        rustdoc_types::ItemEnum::Struct(struct_) => {
            retain(&mut struct_.impls, unavailable);
            match &mut struct_.kind {
                rustdoc_types::StructKind::Tuple(fields) => retain_optional(fields, unavailable),
                rustdoc_types::StructKind::Plain { fields, .. } => retain(fields, unavailable),
                rustdoc_types::StructKind::Unit => {}
            }
        }
        rustdoc_types::ItemEnum::Union(union_) => {
            retain(&mut union_.fields, unavailable);
            retain(&mut union_.impls, unavailable);
        }
        rustdoc_types::ItemEnum::Enum(enum_) => {
            retain(&mut enum_.variants, unavailable);
            retain(&mut enum_.impls, unavailable);
        }
        rustdoc_types::ItemEnum::Variant(variant) => match &mut variant.kind {
            rustdoc_types::VariantKind::Tuple(fields) => retain_optional(fields, unavailable),
            rustdoc_types::VariantKind::Struct { fields, .. } => retain(fields, unavailable),
            rustdoc_types::VariantKind::Plain => {}
        },
        rustdoc_types::ItemEnum::Trait(trait_) => {
            retain(&mut trait_.items, unavailable);
            retain(&mut trait_.implementations, unavailable);
        }
        rustdoc_types::ItemEnum::Impl(impl_) => retain(&mut impl_.items, unavailable),
        _ => {}
    }
}

fn generate_json(
    request: &RustdocRequest<'_>,
    target_dir: &Path,
    doc_dir: &Path,
) -> Result<(), String> {
    generate_json_with_toolchain(
        request,
        target_dir,
        doc_dir,
        &request.target_selection.toolchain,
    )
}

fn generate_json_with_toolchain(
    request: &RustdocRequest<'_>,
    target_dir: &Path,
    doc_dir: &Path,
    toolchain: &str,
) -> Result<(), String> {
    target_selector(request.target)?;
    let context_kind = exact_context_kind(request.contexts)?;
    let target_default_panic = target_default_panic(toolchain, request.unit.platform.as_deref())?;
    let original_rustc_wrapper =
        effective_general_rustc_wrapper(&request.manifest_path, toolchain)?;
    let mut command = Command::new("cargo");
    if let Some(invocation_dir) = request
        .manifest_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        command.current_dir(invocation_dir);
    }
    command.arg(format!("+{toolchain}"));
    if context_kind == DependencyKind::Development {
        command.args(["test", "--no-run"]);
        // Keep Cargo's test feature/profile graph, but compile placeholders for
        // consumer targets whose real source is not needed by the selected unit.
        // Cargo still receives artifacts for downstream consumer test targets.
        fs::create_dir_all(doc_dir).map_err(|error| error.to_string())?;
        let consumer_source = doc_dir.join("excra_consumer.rs");
        fs::write(
            &consumer_source,
            "//! Excra consumer placeholder.\n/// Unused consumer entry point.\n#[allow(dead_code)] fn main() {}\n",
        )
        .map_err(|error| error.to_string())?;
        let sources = request
            .root_package
            .targets
            .iter()
            .filter(|target| !target.kind.iter().any(|kind| kind == "custom-build"))
            .map(|target| target.src_path.as_std_path())
            .collect::<Vec<_>>();
        command
            .env("EXCRA_WRAPPER_CONSUMER_SOURCE", consumer_source)
            .env(
                "EXCRA_WRAPPER_ROOT_SOURCES",
                serde_json::to_string(&sources).expect("consumer sources serialize"),
            )
            .env(
                "EXCRA_WRAPPER_ROOT_PREREQUISITES",
                serde_json::to_string(request.root_prerequisites)
                    .expect("consumer prerequisites serialize"),
            );
    } else {
        command
            .arg("rustdoc")
            .arg(root_target_selector(request.root_package));
        // Cargo still compiles the dependencies in the selected root context.
        // Rustdoc's help mode skips consumer source and documentation checks.
        command
            .env("RUSTDOCFLAGS", "--help")
            .env_remove("CARGO_ENCODED_RUSTDOCFLAGS");
    }
    command
        .args(["--manifest-path"])
        .arg(&request.manifest_path)
        .args([
            "--locked",
            "-p",
            &package_spec(request.root_package),
            "--target-dir",
        ])
        .arg(target_dir);
    command.args(request.feature_selection.cargo_args());
    if let Some(target_triple) = &request.target_selection.command_line_override {
        command.args(["--target", target_triple]);
    }
    let current_exe = env::current_exe()
        .map_err(|err| format!("failed to locate excra executable for Rustdoc: {err}"))?;
    command
        .env("RUSTC_WRAPPER", current_exe)
        .env_remove("EXCRA_ORIGINAL_RUSTC_WRAPPER")
        .env("EXCRA_RUSTC_WRAPPER_MODE", "1")
        .env("EXCRA_WRAPPER_PACKAGE_NAME", &request.package.name)
        .env(
            "EXCRA_WRAPPER_PACKAGE_VERSION",
            request.package.version.to_string(),
        )
        .env(
            "EXCRA_WRAPPER_MANIFEST_DIR",
            request
                .package
                .manifest_path
                .parent()
                .expect("package manifest has parent")
                .as_std_path(),
        )
        .env("EXCRA_WRAPPER_TARGET_NAME", &request.target.name)
        .env(
            "EXCRA_WRAPPER_SOURCE_PATH",
            request.target.src_path.as_std_path(),
        )
        .env(
            "EXCRA_WRAPPER_FEATURES",
            serde_json::to_string(&request.unit.features).expect("feature names serialize"),
        )
        .env("EXCRA_WRAPPER_UNIT_MODE", &request.unit.mode)
        .env(
            "EXCRA_WRAPPER_UNIT_PLATFORM",
            serde_json::to_string(&request.unit.platform).expect("unit platform serializes"),
        )
        .env("EXCRA_WRAPPER_UNIT_PROFILE", &request.unit.profile)
        .env("EXCRA_WRAPPER_TARGET_DEFAULT_PANIC", target_default_panic)
        .env(
            "EXCRA_WRAPPER_CFG_PATH",
            doc_dir.join(format!("{}.cfg", request.target.name.replace('-', "_"))),
        )
        .env("EXCRA_WRAPPER_DOC_DIR", doc_dir);
    if let Some(wrapper) = original_rustc_wrapper {
        command.env("EXCRA_ORIGINAL_RUSTC_WRAPPER", wrapper);
    }
    let output = command
        .output()
        .map_err(|err| {
            format!(
                "failed to run cargo +{toolchain} rustdoc for {} {}: {err}; install it with `rustup toolchain install {toolchain}` or set EXCRA_TOOLCHAIN",
                request.package.name, request.package.version
            )
        })?;
    handle_generate_output(request.package, toolchain, output)?;
    let json_path = doc_dir.join(format!("{}.json", request.target.name.replace('-', "_")));
    if !json_path.is_file() {
        return Err(format!(
            "Cargo completed without producing Rustdoc JSON for the selected unit: {} {} target {} (mode {}, platform {}, features {:?}, profile {}); expected {}",
            request.package.name,
            request.package.version,
            request.target.name,
            request.unit.mode,
            request.unit.platform.as_deref().unwrap_or("host"),
            request.unit.features,
            request.unit.profile,
            json_path.display()
        ));
    }
    Ok(())
}

fn target_default_panic(toolchain: &str, platform: Option<&str>) -> Result<&'static str, String> {
    let mut command = Command::new("rustc");
    command.arg(format!("+{toolchain}"));
    if let Some(platform) = platform {
        command.args(["--target", platform]);
    }
    let output = command.arg("--print=cfg").output().map_err(|error| {
        format!("failed to inspect the default panic cfg with rustc +{toolchain}: {error}")
    })?;
    if !output.status.success() {
        return Err(format!(
            "failed to inspect the default panic cfg with rustc +{toolchain}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let cfg = RustcCfg::parse(&output.stdout);
    for strategy in ["abort", "unwind"] {
        if cfg.contains_value("panic", strategy) {
            return Ok(strategy);
        }
    }
    Err(format!(
        "failed to parse the default panic cfg from rustc +{toolchain} --print=cfg"
    ))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ImportProbe {
    compiler: Vec<OsString>,
    directory: PathBuf,
    arguments: Vec<OsString>,
}

fn load_import_probe(json_path: &Path) -> Result<ImportProbe, String> {
    serde_json::from_slice(
        &fs::read(json_path.with_extension("probe")).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn collect_cfg_versions(meta: &syn::Meta, versions: &mut HashSet<String>) {
    fn predicate(meta: &syn::Meta, versions: &mut HashSet<String>) {
        let syn::Meta::List(list) = meta else { return };
        if list.path.is_ident("version") {
            if let Ok(values) =
                syn::punctuated::Punctuated::<syn::LitStr, syn::Token![,]>::parse_terminated
                    .parse2(list.tokens.clone())
                && values.len() == 1
            {
                versions.insert(values[0].value());
            }
        } else if ["all", "any", "not"]
            .iter()
            .any(|name| list.path.is_ident(name))
            && let Ok(nested) =
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
                    .parse2(list.tokens.clone())
        {
            for meta in nested {
                predicate(&meta, versions);
            }
        }
    }
    let syn::Meta::List(list) = meta else { return };
    if list.path.is_ident("cfg") {
        if let Ok(meta) = syn::parse2(list.tokens.clone()) {
            predicate(&meta, versions);
        }
    } else if list.path.is_ident("cfg_attr")
        && let Ok(nested) =
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
                .parse2(list.tokens.clone())
    {
        if let Some(condition) = nested.first() {
            predicate(condition, versions);
        }
        for attribute in nested.iter().skip(1) {
            collect_cfg_versions(attribute, versions);
        }
    }
}

fn collect_source_cfg_versions(tokens: proc_macro2::TokenStream, versions: &mut HashSet<String>) {
    use proc_macro2::{Delimiter, TokenTree};
    let mut tokens = tokens.into_iter().peekable();
    while let Some(token) = tokens.next() {
        match token {
            TokenTree::Punct(pound) if pound.as_char() == '#' => {
                if matches!(tokens.peek(), Some(TokenTree::Punct(mark)) if mark.as_char() == '!') {
                    tokens.next();
                }
                if matches!(tokens.peek(), Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Bracket)
                    && let Some(TokenTree::Group(group)) = tokens.next()
                    && let Ok(meta) = syn::parse2::<syn::Meta>(group.stream())
                    && (meta.path().is_ident("cfg") || meta.path().is_ident("cfg_attr"))
                {
                    collect_cfg_versions(&meta, versions);
                }
            }
            TokenTree::Group(group) => collect_source_cfg_versions(group.stream(), versions),
            _ => {}
        }
    }
}

fn capture_version_cfg(
    compiler: &[OsString],
    arguments: &[OsString],
    cfg_path: &Path,
    source_path: &Path,
) -> Result<(), String> {
    let directory = env::current_dir().map_err(|error| error.to_string())?;
    let source_path = source_path
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let source_index = arguments.iter().position(|argument| {
        directory.join(argument).canonicalize().ok().as_ref() == Some(&source_path)
    });
    let krate: Crate = serde_json::from_slice(
        &fs::read(cfg_path.with_extension("json")).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let mut versions = HashSet::new();
    let mut sources = HashSet::new();
    if let Some(index) = source_index {
        sources.insert(directory.join(&arguments[index]));
    }
    for item in krate.index.values().filter(|item| item.crate_id == 0) {
        if let Some(span) = &item.span {
            sources.insert(directory.join(&span.filename));
        }
        for attribute in &item.attrs {
            if let rustdoc_types::Attribute::Other(attribute) = attribute {
                for name in ["cfg", "cfg_attr"] {
                    if let Some(expression) = retained_attribute_expression(attribute, name)
                        && let Ok(meta) = syn::parse_str(&format!("{name}({expression})"))
                    {
                        collect_cfg_versions(&meta, &mut versions);
                    }
                }
            }
        }
    }
    for source in sources {
        if let Ok(source) = fs::read_to_string(source)
            && let Ok(tokens) = source.parse()
        {
            collect_source_cfg_versions(tokens, &mut versions);
        }
    }
    let mut matches = HashMap::new();
    if !versions.is_empty() {
        let mut versions = versions.into_iter().collect::<Vec<_>>();
        versions.sort();
        let mut source = String::from(
            "#![no_std]\n#![feature(cfg_version)]\nconst EXCRA_CFG_VERSION_PROBE: () = ();\n",
        );
        for (index, version) in versions.iter().enumerate() {
            source.push_str(&format!(
                "#[cfg(version({version:?}))] const EXCRA_CFG_VERSION_{index}: () = ();\n"
            ));
        }
        // Keep Cargo's per-unit environment, crate name, arguments and wrapper chain.
        // The import-only probe deliberately omits wrappers and cannot evaluate cfgs.
        let source_path = cfg_path.with_extension("version-probe.rs");
        fs::write(&source_path, source).map_err(|error| error.to_string())?;
        let mut arguments = arguments.to_vec();
        arguments[source_index.ok_or("selected compiler invocation has no source input")?] =
            source_path.into_os_string();
        let output = Command::new(
            compiler
                .first()
                .ok_or("selected compiler invocation is empty")?,
        )
        .args(&compiler[1..])
        .args(arguments)
        .arg("-Zunpretty=expanded")
        .output()
        .map_err(|error| {
            format!("failed to evaluate cfg(version) with selected compiler: {error}")
        })?;
        if !output.status.success() {
            return Err(format!(
                "selected wrapped compiler could not evaluate cfg(version): {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let expanded = String::from_utf8_lossy(&output.stdout);
        if !expanded.contains("const EXCRA_CFG_VERSION_PROBE:") {
            return Err(
                "selected wrapped compiler did not produce the cfg(version) probe expansion".into(),
            );
        }
        for (index, version) in versions.into_iter().enumerate() {
            matches.insert(
                version,
                expanded.contains(&format!("const EXCRA_CFG_VERSION_{index}:")),
            );
        }
    }
    fs::write(
        cfg_path.with_extension("versions"),
        serde_json::to_vec(&matches).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn save_import_probe(
    artifact_compiler: Vec<OsString>,
    compiler: Vec<OsString>,
    arguments: &[OsString],
    path: &Path,
) -> Result<(), String> {
    let directory = env::current_dir().map_err(|error| error.to_string())?;
    let output = Command::new(&artifact_compiler[0])
        .args(&artifact_compiler[1..])
        .args(arguments)
        .arg("--print=file-names")
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let strings = arguments
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>();
    let out_dir = argument_value(&strings, "--out-dir")
        .ok_or("selected compiler invocation has no output directory")?;
    let artifact = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|name| directory.join(out_dir).join(name))
        .find_map(|path| {
            let metadata = path.with_extension("rmeta");
            if metadata.is_file() {
                Some(metadata)
            } else if path.is_file() {
                Some(path)
            } else {
                None
            }
        })
        .ok_or("selected compiler invocation produced no importable artifact")?;
    let mut probe_arguments = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let arg = arguments[index].to_string_lossy();
        if matches!(arg.as_ref(), "-L" | "--target" | "--sysroot") {
            probe_arguments.push(arguments[index].clone());
            index += 1;
            probe_arguments.push(arguments[index].clone());
        } else if arg.starts_with("-L")
            || arg.starts_with("--target=")
            || arg.starts_with("--sysroot=")
        {
            probe_arguments.push(arguments[index].clone());
        }
        index += 1;
    }
    probe_arguments.push("--extern".into());
    let mut external = OsString::from("excra_dependency=");
    external.push(artifact);
    probe_arguments.push(external);
    let probe = ImportProbe {
        compiler,
        directory,
        arguments: probe_arguments,
    };
    fs::write(
        path,
        serde_json::to_vec(&probe).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn validate_import(
    json_path: &Path,
    import: &crate::imports::ImportPath,
) -> Result<(), String> {
    let probe = load_import_probe(json_path)?;
    let source = json_path.with_extension("probe.rs");
    let mut parts = import.segments.clone();
    parts.push(import.item.clone());
    if import.namespace.is_some() {
        parts.push("{self}".into());
    }
    fs::write(
        &source,
        format!("#![no_std]\nuse excra_dependency::{};\n", parts.join("::")),
    )
    .map_err(|error| error.to_string())?;
    let output = Command::new(&probe.compiler[0])
        .args(&probe.compiler[1..])
        .current_dir(&probe.directory)
        .args(&probe.arguments)
        .args([
            "--edition=2024",
            "--crate-name=excra_import_probe",
            "--crate-type=lib",
            "--emit=metadata",
            "--cap-lints=allow",
        ])
        .arg(&source)
        .arg("-o")
        .arg(json_path.with_extension("probe.rmeta"))
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

pub(crate) fn run_rustc_wrapper() -> ! {
    let mut command_arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if let Some(source) = env::var_os("EXCRA_WRAPPER_CONSUMER_SOURCE")
        && !command_arguments.is_empty()
    {
        let invocation = rustc_invocation(&command_arguments);
        let root_sources: Vec<PathBuf> = env::var("EXCRA_WRAPPER_ROOT_SOURCES")
            .ok()
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        let prerequisites: Vec<PathBuf> = env::var("EXCRA_WRAPPER_ROOT_PREREQUISITES")
            .ok()
            .and_then(|value| serde_json::from_str(&value).ok())
            .unwrap_or_default();
        if let Some(index) = consumer_source_to_replace(
            invocation.arguments,
            &env::current_dir().unwrap_or_default(),
            &root_sources,
            &prerequisites,
        ) {
            let offset = command_arguments.len() - invocation.arguments.len();
            command_arguments[offset + index] = source;
        }
    }
    let Some(compiler) = command_arguments.first() else {
        eprintln!("excra Rust compiler wrapper was not given a compiler executable");
        std::process::exit(1);
    };
    let original_wrapper = env::var_os("EXCRA_ORIGINAL_RUSTC_WRAPPER");
    let mut compile = if let Some(wrapper) = &original_wrapper {
        let mut command = Command::new(wrapper);
        command.arg(compiler);
        command
    } else {
        Command::new(compiler)
    };
    let status = compile
        .args(&command_arguments[1..])
        .status()
        .unwrap_or_else(|err| {
            eprintln!("excra Rust compiler wrapper failed to run rustc: {err}");
            std::process::exit(1);
        });
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    let invocation = rustc_invocation(&command_arguments);
    if !wrapper_matches_selected_unit(invocation.arguments) {
        std::process::exit(0);
    }

    // Cargo emits its profile options before dependency search paths, and appends
    // user rustflags afterwards. Match the original profile so overrides cannot
    // hide the selected unit or collapse distinct dev/build profiles. Bypass both
    // compiler wrappers for this probe: they can also override profile options.
    let profile_arguments = cargo_profile_arguments(invocation.arguments);
    let print_cfg = |arguments: &[OsString], wrapped: bool| {
        let mut command = if wrapped {
            let mut command = if let Some(wrapper) = &original_wrapper {
                let mut command = Command::new(wrapper);
                command.arg(compiler);
                command
            } else {
                Command::new(compiler)
            };
            command
                .args(&command_arguments[1..command_arguments.len() - invocation.arguments.len()]);
            command
        } else {
            Command::new(invocation.rustc)
        };
        let cfg_output = command
            .args(arguments)
            .arg("--print=cfg")
            .output()
            .unwrap_or_else(|err| {
                eprintln!("excra Rust compiler wrapper failed to inspect rustc cfgs: {err}");
                std::process::exit(1);
            });
        if !cfg_output.status.success() {
            eprintln!(
                "excra Rust compiler wrapper failed to inspect rustc cfgs: {}",
                String::from_utf8_lossy(&cfg_output.stderr).trim()
            );
            std::process::exit(cfg_output.status.code().unwrap_or(1));
        }
        cfg_output
    };
    let profile_cfg = print_cfg(profile_arguments, false);
    if !profile_matches_selected_unit(&RustcCfg::parse(&profile_cfg.stdout), profile_arguments) {
        std::process::exit(0);
    }
    // Item filtering must still see the effective cfgs, including user overrides.
    let cfg_output = print_cfg(invocation.arguments, true);

    let Some(doc_dir) = env::var_os("EXCRA_WRAPPER_DOC_DIR").map(PathBuf::from) else {
        eprintln!("excra Rust compiler wrapper is missing its Rustdoc output directory");
        std::process::exit(1);
    };
    if let Err(err) = fs::create_dir_all(&doc_dir) {
        eprintln!(
            "excra Rust compiler wrapper failed to create {}: {err}",
            doc_dir.display()
        );
        std::process::exit(1);
    }
    let Some(cfg_path) = env::var_os("EXCRA_WRAPPER_CFG_PATH").map(PathBuf::from) else {
        eprintln!("excra Rust compiler wrapper is missing its rustc cfg output path");
        std::process::exit(1);
    };
    if let Err(err) = fs::write(&cfg_path, &cfg_output.stdout) {
        eprintln!(
            "excra Rust compiler wrapper failed to write {}: {err}",
            cfg_path.display()
        );
        std::process::exit(1);
    }
    for (label, doc) in [("normal", false), ("doc", true)] {
        let mut expanded = if let Some(wrapper) = &original_wrapper {
            let mut command = Command::new(wrapper);
            command.arg(compiler);
            command
        } else {
            Command::new(compiler)
        };
        expanded.args(&command_arguments[1..]);
        if doc {
            expanded.args(["--cfg", "doc"]);
        }
        let output = expanded
            .arg("-Zunpretty=expanded")
            .output()
            .unwrap_or_else(|err| {
                eprintln!("excra failed to expand the selected {label} unit: {err}");
                std::process::exit(1);
            });
        if !output.status.success() {
            eprintln!(
                "excra failed to expand the selected {label} unit: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            std::process::exit(output.status.code().unwrap_or(1));
        }
        if let Err(err) = fs::write(
            cfg_path.with_extension(format!("{label}.rs")),
            output.stdout,
        ) {
            eprintln!("excra failed to save the selected {label} expansion: {err}");
            std::process::exit(1);
        }
    }
    let mut artifact_compiler = original_wrapper.iter().cloned().collect::<Vec<_>>();
    artifact_compiler.extend_from_slice(
        &command_arguments[..command_arguments.len() - invocation.arguments.len()],
    );
    if let Err(error) = save_import_probe(
        artifact_compiler.clone(),
        import_probe_compiler(&invocation),
        invocation.arguments,
        &cfg_path.with_extension("probe"),
    ) {
        eprintln!("excra failed to retain the selected compiler artifact: {error}");
        std::process::exit(1);
    }
    let mut rustdoc = PathBuf::from(invocation.rustc);
    rustdoc.set_file_name(if cfg!(windows) {
        "rustdoc.exe"
    } else {
        "rustdoc"
    });
    let rustdoc_arguments = rustdoc_arguments(invocation.arguments);
    let mut command = match (original_wrapper, invocation.workspace_wrapper) {
        (Some(wrapper), Some(workspace_wrapper)) => {
            let mut command = Command::new(wrapper);
            command.arg(workspace_wrapper).arg(&rustdoc);
            command
        }
        (Some(wrapper), None) => {
            let mut command = Command::new(wrapper);
            command.arg(&rustdoc);
            command
        }
        (None, Some(workspace_wrapper)) => {
            let mut command = Command::new(workspace_wrapper);
            command.arg(&rustdoc);
            command
        }
        (None, None) => Command::new(&rustdoc),
    };
    let status = command
        .args(rustdoc_arguments)
        .args([
            "-Z",
            "unstable-options",
            "--output-format",
            "json",
            "--document-private-items",
            "--document-hidden-items",
            "-o",
        ])
        .arg(&doc_dir)
        .status()
        .unwrap_or_else(|err| {
            eprintln!(
                "excra Rust compiler wrapper failed to run {}: {err}",
                rustdoc.display()
            );
            std::process::exit(1);
        });
    if status.success() && cfg_path.with_extension("json").is_file() {
        let versions = env::var_os("EXCRA_WRAPPER_SOURCE_PATH")
            .ok_or_else(|| "selected compiler source path is unavailable".to_string())
            .and_then(|source| {
                capture_version_cfg(
                    &artifact_compiler,
                    invocation.arguments,
                    &cfg_path,
                    Path::new(&source),
                )
            });
        if let Err(error) = versions {
            eprintln!("excra failed to capture selected compiler version cfgs: {error}");
            std::process::exit(1);
        }
    }
    std::process::exit(status.code().unwrap_or(1));
}

fn consumer_source_to_replace(
    arguments: &[OsString],
    directory: &Path,
    root_sources: &[PathBuf],
    prerequisites: &[PathBuf],
) -> Option<usize> {
    let strings = arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>();
    if argument_value(&strings, "--crate-name") == Some("build_script_build") {
        return None;
    }
    let test_or_binary = strings.iter().any(|argument| argument == "--test")
        || argument_value(&strings, "--crate-type") == Some("bin");
    arguments.iter().position(|argument| {
        let path = directory.join(argument);
        root_sources.contains(&path) && (test_or_binary || !prerequisites.contains(&path))
    })
}

struct RustcInvocation<'a> {
    workspace_wrapper: Option<&'a OsStr>,
    rustc: &'a OsStr,
    arguments: &'a [OsString],
}

fn import_probe_compiler(invocation: &RustcInvocation<'_>) -> Vec<OsString> {
    // Cargo-scoped wrappers already produced the dependency artifact. The probe
    // only imports it, so invoking rustc directly avoids replaying wrappers
    // without Cargo's per-unit environment.
    vec![invocation.rustc.to_owned()]
}

fn rustc_invocation(arguments: &[OsString]) -> RustcInvocation<'_> {
    let compiler = arguments
        .first()
        .expect("compiler wrapper invocation has a compiler");
    // Cargo puts an option immediately after rustc for its direct invocations. When
    // RUSTC_WORKSPACE_WRAPPER is also active, Cargo's documented nesting instead puts
    // the actual rustc executable in this position:
    // `$RUSTC_WRAPPER $RUSTC_WORKSPACE_WRAPPER $RUSTC ...`.
    let nested = arguments
        .get(1)
        .is_some_and(|argument| !argument.to_string_lossy().starts_with('-'));
    if nested {
        RustcInvocation {
            workspace_wrapper: Some(compiler.as_os_str()),
            rustc: arguments[1].as_os_str(),
            arguments: &arguments[2..],
        }
    } else {
        RustcInvocation {
            workspace_wrapper: None,
            rustc: compiler.as_os_str(),
            arguments: &arguments[1..],
        }
    }
}

fn wrapper_matches_selected_unit(arguments: &[OsString]) -> bool {
    let expected_name = env::var("EXCRA_WRAPPER_PACKAGE_NAME").ok();
    let expected_version = env::var("EXCRA_WRAPPER_PACKAGE_VERSION").ok();
    let expected_manifest_dir = env::var_os("EXCRA_WRAPPER_MANIFEST_DIR");
    if expected_name.as_deref() != env::var("CARGO_PKG_NAME").ok().as_deref()
        || expected_version.as_deref() != env::var("CARGO_PKG_VERSION").ok().as_deref()
        || expected_manifest_dir.as_deref() != env::var_os("CARGO_MANIFEST_DIR").as_deref()
    {
        return false;
    }
    let arguments = arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>();
    let expected_target = env::var("EXCRA_WRAPPER_TARGET_NAME")
        .unwrap_or_default()
        .replace('-', "_");
    if argument_value(&arguments, "--crate-name") != Some(expected_target.as_str()) {
        return false;
    }

    let expected_mode = env::var("EXCRA_WRAPPER_UNIT_MODE").unwrap_or_default();
    if rustc_compile_mode(&arguments) != Some(expected_mode.as_str()) {
        return false;
    }
    let expected_platform = env::var("EXCRA_WRAPPER_UNIT_PLATFORM")
        .ok()
        .and_then(|platform| serde_json::from_str::<Option<String>>(&platform).ok())
        .flatten();
    if argument_value(&arguments, "--target") != expected_platform.as_deref() {
        return false;
    }

    let mut actual_features = Vec::new();
    for (index, argument) in arguments.iter().enumerate() {
        let cfg = if argument == "--cfg" {
            arguments.get(index + 1).map(|value| value.as_ref())
        } else {
            argument.strip_prefix("--cfg=")
        };
        if let Some(feature) = cfg
            .and_then(|cfg| cfg.strip_prefix("feature=\""))
            .and_then(|feature| feature.strip_suffix('"'))
        {
            actual_features.push(feature.to_string());
        }
    }
    actual_features.sort();
    actual_features.dedup();
    let mut expected_features = env::var("EXCRA_WRAPPER_FEATURES")
        .ok()
        .and_then(|features| serde_json::from_str::<Vec<String>>(&features).ok())
        .unwrap_or_default();
    expected_features.sort();
    expected_features.dedup();
    actual_features == expected_features
}

// ponytail: relies on pinned Cargo argument ordering; revisit when upgrading Cargo.
fn cargo_profile_arguments(arguments: &[OsString]) -> &[OsString] {
    let end = arguments
        .windows(2)
        .position(|pair| pair[0] == "-L" && pair[1].to_string_lossy().starts_with("dependency="));
    &arguments[..end.unwrap_or(arguments.len())]
}

fn profile_matches_selected_unit(cfg: &RustcCfg, arguments: &[OsString]) -> bool {
    let Some(profile) = env::var("EXCRA_WRAPPER_UNIT_PROFILE")
        .ok()
        .and_then(|profile| serde_json::from_str::<serde_json::Value>(&profile).ok())
    else {
        return false;
    };
    let Ok(target_default_panic) = env::var("EXCRA_WRAPPER_TARGET_DEFAULT_PANIC") else {
        return false;
    };
    let arguments = arguments
        .iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>();
    profile_matches_cfg(&profile, cfg, &arguments, &target_default_panic)
}

fn profile_matches_cfg(
    profile: &serde_json::Value,
    cfg: &RustcCfg,
    arguments: &[std::borrow::Cow<'_, str>],
    target_default_panic: &str,
) -> bool {
    let Some(opt_level) = profile.get("opt_level").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Some(lto) = profile.get("lto").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Some(codegen_backend) = optional_profile_string(profile, "codegen_backend") else {
        return false;
    };
    let Some(codegen_units) = optional_profile_u64(profile, "codegen_units") else {
        return false;
    };
    let Some(debuginfo) = profile_debuginfo(profile) else {
        return false;
    };
    let Some(split_debuginfo) = optional_profile_string(profile, "split_debuginfo") else {
        return false;
    };
    let Some(debug_assertions) = profile
        .get("debug_assertions")
        .and_then(serde_json::Value::as_bool)
    else {
        return false;
    };
    let Some(overflow_checks) = profile
        .get("overflow_checks")
        .and_then(serde_json::Value::as_bool)
    else {
        return false;
    };
    let Some(rpath) = profile.get("rpath").and_then(serde_json::Value::as_bool) else {
        return false;
    };
    let Some(incremental) = profile
        .get("incremental")
        .and_then(serde_json::Value::as_bool)
    else {
        return false;
    };
    let Some(panic) = profile.get("panic").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Some(strip) = profile_strip(profile) else {
        return false;
    };
    let explicit_panic = codegen_value(arguments, "panic");
    let effective_panic = explicit_panic.unwrap_or(target_default_panic);

    codegen_value(arguments, "opt-level").unwrap_or("0") == opt_level
        && profile_lto_matches(lto, arguments)
        && unstable_value(arguments, "codegen-backend") == codegen_backend
        && codegen_units.is_none_or(|expected| {
            codegen_value(arguments, "codegen-units").and_then(|actual| actual.parse::<u64>().ok())
                == Some(expected)
        })
        && (codegen_units.is_some() || !codegen_option_present(arguments, "codegen-units"))
        && codegen_value(arguments, "debuginfo").unwrap_or("0") == debuginfo
        && codegen_value(arguments, "split-debuginfo") == split_debuginfo
        && cfg.contains_flag("debug_assertions") == debug_assertions
        && cfg.contains_flag("overflow_checks") == overflow_checks
        && codegen_bool(arguments, "rpath") == rpath
        && codegen_option_present(arguments, "incremental") == incremental
        && explicit_panic.is_none_or(|actual| actual == panic)
        && cfg.contains_value("panic", effective_panic)
        && codegen_value(arguments, "strip").unwrap_or("none") == strip
}

fn optional_profile_string<'a>(
    profile: &'a serde_json::Value,
    field: &str,
) -> Option<Option<&'a str>> {
    match profile.get(field)? {
        serde_json::Value::Null => Some(None),
        serde_json::Value::String(value) => Some(Some(value)),
        _ => None,
    }
}

fn optional_profile_u64(profile: &serde_json::Value, field: &str) -> Option<Option<u64>> {
    match profile.get(field)? {
        serde_json::Value::Null => Some(None),
        serde_json::Value::Number(value) => value.as_u64().map(Some),
        _ => None,
    }
}

fn profile_debuginfo(profile: &serde_json::Value) -> Option<String> {
    match profile.get("debuginfo")? {
        serde_json::Value::Null => Some("0".into()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::String(value) => Some(value.clone()),
        _ => None,
    }
}

fn profile_strip(profile: &serde_json::Value) -> Option<&str> {
    let strip = profile.get("strip")?.as_object()?;
    let value = if let Some(value) = strip.get("deferred") {
        value.as_str()?
    } else {
        strip.get("resolved")?.as_object()?.get("Named")?.as_str()?
    };
    match value {
        "None" | "none" => Some("none"),
        "Debuginfo" | "debuginfo" => Some("debuginfo"),
        "Symbols" | "symbols" => Some("symbols"),
        _ => None,
    }
}

fn profile_lto_matches(lto: &str, arguments: &[std::borrow::Cow<'_, str>]) -> bool {
    let linker_plugin = codegen_option_present(arguments, "linker-plugin-lto");
    let explicit_lto = codegen_value(arguments, "lto");
    match lto {
        "false" => !linker_plugin && explicit_lto.is_none(),
        "off" => !linker_plugin && explicit_lto.is_none_or(|value| value == "off"),
        "true" | "fat" | "thin" => {
            linker_plugin
                || explicit_lto
                    .is_some_and(|value| value == lto || (lto == "true" && value == "fat"))
        }
        _ => false,
    }
}

fn argument_value<'a>(arguments: &'a [std::borrow::Cow<'a, str>], flag: &str) -> Option<&'a str> {
    arguments.iter().enumerate().find_map(|(index, argument)| {
        if argument == flag {
            return arguments.get(index + 1).map(AsRef::as_ref);
        }
        argument
            .strip_prefix(flag)
            .and_then(|value| value.strip_prefix('='))
    })
}

fn codegen_value<'a>(arguments: &'a [std::borrow::Cow<'a, str>], option: &str) -> Option<&'a str> {
    compiler_option_value(arguments, "-C", option)
}

fn unstable_value<'a>(arguments: &'a [std::borrow::Cow<'a, str>], option: &str) -> Option<&'a str> {
    compiler_option_value(arguments, "-Z", option)
}

fn compiler_option_value<'a>(
    arguments: &'a [std::borrow::Cow<'a, str>],
    prefix: &str,
    option: &str,
) -> Option<&'a str> {
    arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| {
            let value = if argument == prefix {
                arguments.get(index + 1).map(AsRef::as_ref)
            } else {
                argument.strip_prefix(prefix)
            }?;
            value
                .strip_prefix(option)
                .and_then(|value| value.strip_prefix('='))
        })
        .next_back()
}

fn codegen_option_present(arguments: &[std::borrow::Cow<'_, str>], option: &str) -> bool {
    arguments.iter().enumerate().any(|(index, argument)| {
        let value = if argument == "-C" {
            arguments.get(index + 1).map(AsRef::as_ref)
        } else {
            argument.strip_prefix("-C")
        };
        value.is_some_and(|value| {
            value == option
                || value
                    .strip_prefix(option)
                    .is_some_and(|value| value.starts_with('='))
        })
    })
}

fn codegen_bool(arguments: &[std::borrow::Cow<'_, str>], option: &str) -> bool {
    if let Some(value) = codegen_value(arguments, option) {
        matches!(value, "yes" | "on" | "true" | "y")
    } else {
        codegen_option_present(arguments, option)
    }
}

fn rustc_compile_mode(arguments: &[std::borrow::Cow<'_, str>]) -> Option<&'static str> {
    let emit = argument_value(arguments, "--emit")?;
    if emit.split(',').any(|kind| kind == "link") {
        Some("build")
    } else if emit.split(',').any(|kind| kind == "metadata") {
        Some("check")
    } else {
        None
    }
}

fn rustdoc_arguments(arguments: &[OsString]) -> Vec<OsString> {
    let mut filtered = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_string_lossy();
        if matches!(argument.as_ref(), "--emit" | "--out-dir" | "-o") {
            index += 2;
            continue;
        }
        if argument.starts_with("--emit=") || argument.starts_with("--out-dir=") {
            index += 1;
            continue;
        }
        if argument == "-C"
            && arguments.get(index + 1).is_some_and(|value| {
                matches!(
                    value.to_string_lossy().split('=').next(),
                    Some("incremental" | "metadata" | "extra-filename")
                )
            })
        {
            index += 2;
            continue;
        }
        filtered.push(arguments[index].clone());
        index += 1;
    }
    filtered
}

#[derive(Debug, Deserialize)]
struct UnitGraph {
    version: u32,
    roots: Vec<usize>,
    units: Vec<Unit>,
}

#[derive(Debug, Deserialize)]
struct Unit {
    pkg_id: String,
    target: UnitTarget,
    mode: String,
    platform: Option<String>,
    features: Vec<String>,
    profile: serde_json::Value,
    dependencies: Vec<UnitDependency>,
}

#[derive(Debug, Deserialize)]
struct UnitTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UnitDependency {
    index: usize,
}

fn exact_context_kind(contexts: &[DependencyContext]) -> Result<DependencyKind, String> {
    let Some(first) = contexts.first() else {
        return Err("cannot select a Cargo unit without a dependency context".to_string());
    };
    if contexts.iter().any(|context| context.kind != first.kind) {
        return Err(format!(
            "dependency contexts resolve to different Cargo units ({}); query each exact context separately",
            contexts
                .iter()
                .map(DependencyContext::label)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(first.kind)
}

#[derive(Debug, Deserialize)]
struct CargoConfig {
    build: CargoBuildConfig,
}

#[derive(Debug, Deserialize)]
struct CargoBuildConfig {
    target: ConfiguredCargoTargets,
}

#[derive(Debug, Deserialize)]
struct CargoRustcWrapperConfig {
    build: CargoRustcWrapperBuildConfig,
}

#[derive(Debug, Deserialize)]
struct CargoRustcWrapperBuildConfig {
    #[serde(rename = "rustc-wrapper")]
    rustc_wrapper: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ConfiguredCargoTargets {
    One(String),
    Many(Vec<String>),
}

fn effective_general_rustc_wrapper(
    manifest_path: &Path,
    toolchain: &str,
) -> Result<Option<OsString>, String> {
    for name in ["RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WRAPPER"] {
        if let Some(wrapper) = env::var_os(name) {
            return Ok((!wrapper.is_empty()).then_some(wrapper));
        }
    }

    configured_general_rustc_wrapper(manifest_path, toolchain, OsStr::new("cargo"))
}

fn configured_general_rustc_wrapper(
    manifest_path: &Path,
    toolchain: &str,
    cargo: &OsStr,
) -> Result<Option<OsString>, String> {
    let mut command = Command::new(cargo);
    if let Some(invocation_dir) = manifest_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        command.current_dir(invocation_dir);
    }
    let output = command
        .arg(format!("+{toolchain}"))
        .args([
            "-Z",
            "unstable-options",
            "config",
            "get",
            "build.rustc-wrapper",
            "--format",
            "json",
        ])
        .output()
        .map_err(|error| {
            format!("failed to ask Cargo +{toolchain} for the configured rustc wrapper: {error}")
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("config value `build.rustc-wrapper` is not set") {
            return Ok(None);
        }
        return Err(format!(
            "failed to read Cargo +{toolchain}'s configured rustc wrapper: {}",
            stderr.trim()
        ));
    }
    let config: CargoRustcWrapperConfig =
        serde_json::from_slice(&output.stdout).map_err(|error| {
            format!(
                "failed to parse Cargo's configured rustc wrapper: {error}: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            )
        })?;
    let wrapper = config.build.rustc_wrapper;
    if wrapper.is_empty() {
        return Ok(None);
    }
    let wrapper_path = Path::new(&wrapper);
    if wrapper_path.is_absolute() || wrapper_path.components().count() == 1 {
        return Ok(Some(wrapper.into()));
    }

    let origin = configured_value_origin(manifest_path, toolchain, cargo)?;
    let config_directory = origin.parent().ok_or_else(|| {
        format!(
            "Cargo rustc-wrapper configuration origin {} has no parent directory",
            origin.display()
        )
    })?;
    let relative_base = if config_directory.file_name() == Some(OsStr::new(".cargo")) {
        config_directory.parent().unwrap_or(config_directory)
    } else {
        config_directory
    };
    let relative_wrapper = wrapper_path
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect::<PathBuf>();
    Ok(Some(relative_base.join(relative_wrapper).into_os_string()))
}

fn configured_value_origin(
    manifest_path: &Path,
    toolchain: &str,
    cargo: &OsStr,
) -> Result<PathBuf, String> {
    let mut command = Command::new(cargo);
    if let Some(invocation_dir) = manifest_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        command.current_dir(invocation_dir);
    }
    let output = command
        .arg(format!("+{toolchain}"))
        .args([
            "-Z",
            "unstable-options",
            "config",
            "get",
            "build.rustc-wrapper",
            "--show-origin",
        ])
        .output()
        .map_err(|error| {
            format!(
                "failed to ask Cargo +{toolchain} for the rustc wrapper configuration origin: {error}"
            )
        })?;
    if !output.status.success() {
        return Err(format!(
            "failed to read Cargo +{toolchain}'s configured rustc wrapper origin: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let origin = stdout
        .lines()
        .find_map(|line| line.rsplit_once(" # ").map(|(_, origin)| origin.trim()))
        .filter(|origin| !origin.is_empty())
        .ok_or_else(|| {
            format!(
                "failed to parse Cargo's configured rustc wrapper origin: {}",
                stdout.trim()
            )
        })?;
    Ok(PathBuf::from(origin))
}

pub(crate) fn target_selection(
    manifest_path: &Path,
    command_line_target: Option<&str>,
    host_triple: &str,
    toolchain: &str,
) -> Result<CargoTargetSelection, String> {
    if let Some(target) = command_line_target {
        let effective_target = if target == "host" {
            host_triple
        } else {
            target
        };
        return Ok(CargoTargetSelection {
            toolchain: toolchain.to_string(),
            effective_triple: effective_target.to_string(),
            cargo_platform: Some(effective_target.to_string()),
            command_line_override: Some(target.to_string()),
        });
    }

    let Some(configured_target) = configured_build_target(manifest_path, toolchain)? else {
        return Ok(CargoTargetSelection {
            toolchain: toolchain.to_string(),
            effective_triple: host_triple.to_string(),
            cargo_platform: None,
            command_line_override: None,
        });
    };
    let effective_target = if configured_target == "host" {
        host_triple.to_string()
    } else {
        configured_target
    };
    Ok(CargoTargetSelection {
        toolchain: toolchain.to_string(),
        effective_triple: effective_target.clone(),
        cargo_platform: Some(effective_target),
        command_line_override: None,
    })
}

fn configured_build_target(
    manifest_path: &Path,
    toolchain: &str,
) -> Result<Option<String>, String> {
    let mut command = Command::new("cargo");
    if let Some(invocation_dir) = manifest_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        command.current_dir(invocation_dir);
    }
    let output = command
        .arg(format!("+{toolchain}"))
        .args([
            "-Z",
            "unstable-options",
            "config",
            "get",
            "build.target",
            "--format",
            "json",
        ])
        .output()
        .map_err(|err| {
            format!("failed to ask Cargo for the configured build target with +{toolchain}: {err}")
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("config value `build.target` is not set") {
            return Ok(None);
        }
        return Err(format!(
            "failed to read Cargo's configured build target: {}",
            stderr.trim()
        ));
    }
    let config: CargoConfig = serde_json::from_slice(&output.stdout).map_err(|err| {
        format!(
            "failed to parse Cargo's configured build target: {err}: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        )
    })?;
    select_configured_targets(config.build.target)
}

fn select_configured_targets(targets: ConfiguredCargoTargets) -> Result<Option<String>, String> {
    match targets {
        ConfiguredCargoTargets::One(target) => Ok(Some(target)),
        ConfiguredCargoTargets::Many(targets) if targets.is_empty() => Ok(None),
        ConfiguredCargoTargets::Many(mut targets) if targets.len() == 1 => Ok(targets.pop()),
        ConfiguredCargoTargets::Many(targets) => Err(format!(
            "Cargo build.target selects multiple targets ({}); pass --target TRIPLE to select one documentation target",
            targets.join(", ")
        )),
    }
}

pub(crate) fn resolved_unit(
    generation: &mut GenerationSession,
    request: CargoUnitRequest<'_>,
) -> Result<CargoUnitSelection, String> {
    let CargoUnitRequest {
        manifest_path,
        root_package,
        package,
        target,
        contexts,
        parent,
        target_selection,
        feature_selection,
    } = request;
    let toolchain = &target_selection.toolchain;
    let context_kind = exact_context_kind(contexts)?;
    let only_dev = context_kind == DependencyKind::Development;
    let host_unit = context_kind == DependencyKind::Build
        || target.kind.iter().any(|kind| kind == "proc-macro");
    let mut command = Command::new("cargo");
    if let Some(invocation_dir) = manifest_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        command.current_dir(invocation_dir);
    }
    command.arg(format!("+{toolchain}"));
    if only_dev {
        command.args(["test", "--no-run"]);
    } else {
        command
            .arg("rustdoc")
            .arg(root_target_selector(root_package));
    }
    command.args(["--manifest-path"]).arg(manifest_path).args([
        "--locked",
        "-p",
        &package_spec(root_package),
        "--unit-graph",
        "-Z",
        "unstable-options",
    ]);
    command.args(feature_selection.cargo_args());
    if let Some(target_triple) = &target_selection.command_line_override {
        command.args(["--target", target_triple]);
    }
    // The command captures root, toolchain, target, features, and dev/build graph mode.
    // Environment and Cargo configuration remain fixed for this query session.
    let graph_key = (
        command
            .get_current_dir()
            .unwrap_or(Path::new(""))
            .to_path_buf(),
        command.get_args().map(OsStr::to_os_string).collect(),
    );
    let graph = match generation.unit_graphs.entry(graph_key) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => {
            let output = command.output().map_err(|err| {
                format!(
                    "failed to run cargo +{toolchain} to resolve the exact feature unit for {} {}: {err}",
                    package.name, package.version
                )
            })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let hint = if is_lockfile_failure(&stderr) {
                    "; Cargo.lock is missing or stale; run `cargo check` or `cargo build` to refresh it, then retry"
                } else {
                    ""
                };
                return Err(format!(
                    "failed to resolve the exact Cargo feature unit for {} {} without changing Cargo.lock: {}{hint}",
                    package.name,
                    package.version,
                    stderr.trim()
                ));
            }
            let graph: UnitGraph = serde_json::from_slice(&output.stdout).map_err(|err| {
                format!(
                    "failed to parse Cargo unit graph while resolving features for {} {}: {err}",
                    package.name, package.version
                )
            })?;
            if graph.version != 1 {
                return Err(format!(
                    "Cargo unit graph version {} is unsupported; supported: 1",
                    graph.version
                ));
            }

            entry.insert(graph)
        }
    };

    let expected_mode = if only_dev || host_unit {
        "build"
    } else {
        "check"
    };
    let expected_platform = if host_unit {
        None
    } else {
        target_selection.cargo_platform.as_deref()
    };
    let mut candidates =
        units_for_context_edges(graph, root_package, package, target, &contexts[0], parent)
            .into_iter()
            .filter_map(|index| graph.units.get(index).map(|unit| (index, unit)))
            .filter(|unit| {
                unit.1.mode == expected_mode && unit.1.platform.as_deref() == expected_platform
            })
            .map(|(graph_index, unit)| CargoUnitSelection {
                identity: unit_identity(unit),
                graph_index,
                // Cargo also compiles the graph's other dependencies. A dev
                // cycle through any of them can require the real root library.
                root_prerequisites: graph
                    .units
                    .iter()
                    .filter(|unit| unit.pkg_id != root_package.id.to_string())
                    .flat_map(|unit| &unit.dependencies)
                    .filter_map(|dependency| graph.units.get(dependency.index))
                    .filter(|unit| unit.pkg_id == root_package.id.to_string())
                    .flat_map(|unit| {
                        root_package.targets.iter().filter(move |target| {
                            target.name == unit.target.name && target.kind == unit.target.kind
                        })
                    })
                    .map(|target| target.src_path.as_std_path().to_path_buf())
                    .collect(),
            })
            .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.identity
            .features
            .cmp(&right.identity.features)
            .then_with(|| left.identity.profile.cmp(&right.identity.profile))
            .then_with(|| left.graph_index.cmp(&right.graph_index))
    });
    candidates.dedup_by_key(|candidate| candidate.graph_index);
    match candidates.as_slice() {
        [unit] => Ok(unit.clone()),
        [] => Err(format!(
            "Cargo unit graph did not contain the selected {} unit for {} {} target {} on {}",
            expected_mode,
            package.name,
            package.version,
            target.name,
            expected_platform.unwrap_or("the host platform")
        )),
        _ => Err(format!(
            "Cargo unit graph contained multiple {} units for {} {} target {} on {}; query a dependency context with one exact Cargo unit",
            expected_mode,
            package.name,
            package.version,
            target.name,
            expected_platform.unwrap_or("the host platform")
        )),
    }
}

fn units_for_context_edges(
    graph: &UnitGraph,
    root_package: &Package,
    package: &Package,
    target: &Target,
    context: &DependencyContext,
    parent: Option<&CargoParentUnit>,
) -> Vec<usize> {
    let root_id = root_package.id.to_string();
    let roots = graph
        .roots
        .iter()
        .copied()
        .filter(|index| {
            graph
                .units
                .get(*index)
                .is_some_and(|unit| unit.pkg_id == root_id)
        })
        .collect::<Vec<_>>();
    let anchors = if context.kind == DependencyKind::Build {
        reachable_units(graph, &roots, false)
            .into_iter()
            .filter(|index| {
                graph
                    .units
                    .get(*index)
                    .is_some_and(|unit| unit.pkg_id == root_id && is_custom_build_unit(unit))
            })
            .collect::<Vec<_>>()
    } else {
        roots
    };

    let parents = if let Some(parent) = parent {
        graph
            .units
            .get(parent.graph_index)
            .filter(|unit| unit.pkg_id == parent.package_id)
            .map(|_| vec![parent.graph_index])
            .unwrap_or_default()
    } else if context.via.is_some() {
        Vec::new()
    } else {
        anchors
    };

    let package_id = package.id.to_string();
    let mut candidates = parents
        .into_iter()
        .filter_map(|index| graph.units.get(index))
        .flat_map(|unit| unit.dependencies.iter())
        .filter_map(|dependency| {
            let unit = graph.units.get(dependency.index)?;
            (unit.pkg_id == package_id && unit.target.name == target.name)
                .then_some(dependency.index)
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.dedup();
    candidates
}

fn reachable_units(graph: &UnitGraph, roots: &[usize], exclude_custom_build: bool) -> Vec<usize> {
    let mut visited = HashSet::new();
    let mut pending = roots.to_vec();
    while let Some(index) = pending.pop() {
        if !visited.insert(index) {
            continue;
        }
        let Some(unit) = graph.units.get(index) else {
            continue;
        };
        for dependency in &unit.dependencies {
            if exclude_custom_build
                && graph
                    .units
                    .get(dependency.index)
                    .is_some_and(is_custom_build_unit)
            {
                continue;
            }
            pending.push(dependency.index);
        }
    }
    let mut reachable = visited.into_iter().collect::<Vec<_>>();
    reachable.sort_unstable();
    reachable
}

fn is_custom_build_unit(unit: &Unit) -> bool {
    unit.target.kind.iter().any(|kind| kind == "custom-build")
}

fn unit_identity(unit: &Unit) -> CargoUnitIdentity {
    let mut features = unit.features.clone();
    features.sort();
    CargoUnitIdentity {
        features,
        mode: unit.mode.clone(),
        platform: unit.platform.clone(),
        profile: serde_json::to_string(&unit.profile).expect("Cargo profile serializes"),
    }
}

fn root_target_selector(package: &Package) -> &'static str {
    if package.targets.iter().any(is_library_target) {
        "--lib"
    } else {
        "--bins"
    }
}

fn target_selector(target: &Target) -> Result<Vec<String>, String> {
    if is_library_target(target) {
        return Ok(vec!["--lib".to_string()]);
    }
    if target.kind.iter().any(|kind| kind == "bin") {
        return Ok(vec!["--bin".to_string(), target.name.clone()]);
    }
    if target.kind.iter().any(|kind| kind == "example") {
        return Ok(vec!["--example".to_string(), target.name.clone()]);
    }
    if target.kind.iter().any(|kind| kind == "test") {
        return Ok(vec!["--test".to_string(), target.name.clone()]);
    }
    if target.kind.iter().any(|kind| kind == "bench") {
        return Ok(vec!["--bench".to_string(), target.name.clone()]);
    }
    Err(format!(
        "target {} has unsupported rustdoc target kind {:?}",
        target.name, target.kind
    ))
}

fn handle_generate_output(
    package: &Package,
    toolchain: &str,
    output: std::process::Output,
) -> Result<(), String> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format_generate_error(package, toolchain, &stderr));
    }
    Ok(())
}

fn format_generate_error(package: &Package, toolchain: &str, stderr: &str) -> String {
    let hint = if is_missing_toolchain_diagnostic(stderr) {
        format!(
            "; compatible nightly toolchain not found; install with `rustup toolchain install {toolchain}` or set EXCRA_TOOLCHAIN"
        )
    } else if stderr.contains("lock file") && stderr.contains("needs to be updated") {
        "; Cargo.lock is missing or stale; run `cargo check` or `cargo build` to refresh it, then retry".to_string()
    } else if stderr.contains("unstable-options") || stderr.contains("output-format") {
        "; rustdoc JSON requires nightly and `-Z unstable-options`".to_string()
    } else {
        String::new()
    };
    format!(
        "failed to generate rustdoc JSON for {} {}{hint}: {}",
        package.name,
        package.version,
        stderr.trim()
    )
}

fn is_missing_toolchain_diagnostic(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let line = line.to_ascii_lowercase();
        (line.contains("toolchain") && line.contains("is not installed"))
            || line.contains("no such installed toolchain")
    })
}

fn is_lockfile_failure(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("lock file")
        && (lower.contains("needs to be updated")
            || lower.contains("needs to be generated")
            || lower.contains("--locked"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cargo_metadata::MetadataCommand;
    use rustdoc_types::{Id, Item, ItemEnum, Module, Target as RustdocTarget, Visibility};
    use std::collections::HashMap;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(windows)]
    use std::os::windows::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    fn metadata() -> Metadata {
        MetadataCommand::new()
            .manifest_path("Cargo.toml")
            .exec()
            .unwrap()
    }

    fn package() -> Package {
        let metadata = metadata();
        metadata.root_package().unwrap().clone()
    }

    fn minimal_crate(version: Option<String>, format_version: u32) -> Crate {
        let root = Id(1);
        let item = Item {
            id: root,
            crate_id: 0,
            name: Some("root".into()),
            span: None,
            visibility: Visibility::Public,
            docs: None,
            links: HashMap::new(),
            attrs: Vec::new(),
            deprecation: None,
            inner: ItemEnum::Module(Module {
                is_crate: true,
                items: Vec::new(),
                is_stripped: false,
            }),
        };
        Crate {
            root,
            crate_version: version,
            includes_private: false,
            index: HashMap::from([(root, item)]),
            paths: HashMap::new(),
            external_crates: HashMap::new(),
            target: RustdocTarget {
                triple: "x86_64-unknown-linux-gnu".into(),
                target_features: Vec::new(),
            },
            format_version,
        }
    }

    fn status(code: i32) -> ExitStatus {
        #[cfg(unix)]
        {
            ExitStatus::from_raw(code << 8)
        }
        #[cfg(windows)]
        {
            ExitStatus::from_raw(code as u32)
        }
    }

    fn foreign_crate(inner: ItemEnum) -> Crate {
        let mut krate = minimal_crate(None, rustdoc_types::FORMAT_VERSION);
        let mut foreign = krate.index[&krate.root].clone();
        foreign.id = Id(2);
        foreign.name = Some("foreign".into());
        foreign.inner = inner;
        krate.index.insert(foreign.id, foreign);
        let ItemEnum::Module(root) = &mut krate.index.get_mut(&krate.root).unwrap().inner else {
            unreachable!();
        };
        root.items.push(Id(2));
        krate
    }

    fn foreign_function() -> ItemEnum {
        ItemEnum::Function(rustdoc_types::Function {
            sig: rustdoc_types::FunctionSignature {
                inputs: Vec::new(),
                output: None,
                is_c_variadic: false,
            },
            generics: rustdoc_types::Generics {
                params: Vec::new(),
                where_predicates: Vec::new(),
            },
            header: rustdoc_types::FunctionHeader {
                is_const: false,
                is_unsafe: true,
                is_async: false,
                abi: rustdoc_types::Abi::C { unwind: false },
            },
            has_body: false,
        })
    }

    #[test]
    fn foreign_declarations_collect_selected_dependencies_and_semantics() {
        let source = r#"
type Scalar = u8;
type Unrelated = u16;
#[deprecated(note = "old block")]
unsafe extern "C" {
    #[must_use = "inspect"] pub fn foreign(value: Scalar) -> Scalar;
    pub safe fn safe_foreign(value: Scalar) -> Scalar;
    pub static DATA: Scalar;
    pub safe static SAFE_DATA: Scalar;
    pub unsafe static UNSAFE_DATA: Scalar;
    pub static mut MUT_DATA: Scalar;
    pub type Opaque;
    pub fn unrelated(value: Unrelated);
}
"#;
        for name in [
            "foreign",
            "safe_foreign",
            "DATA",
            "SAFE_DATA",
            "UNSAFE_DATA",
            "MUT_DATA",
            "Opaque",
        ] {
            let import = crate::imports::parse_use_line(&format!("use dep::{name};")).unwrap();
            let api = expanded_api_shape(source, &import, false).unwrap();
            assert!(api.has_definition, "{name}");
            assert_eq!(api.shapes.len(), 1, "{name}: {:?}", api.shapes);
            let shape = api.shapes.iter().next().unwrap();
            assert!(shape.starts_with("foreign unsafe extern \"C\":"), "{shape}");
            assert!(
                shape.contains(if name.starts_with("safe_") || name == "SAFE_DATA" {
                    "pub safe "
                } else {
                    "pub unsafe "
                }),
                "{shape}"
            );
            assert!(
                api.attributes["item"][0]
                    .1
                    .iter()
                    .any(|attr| attr.contains("old block")),
                "{name}"
            );
            let dependencies = &api.dependencies["definition"][0];
            assert!(
                !dependencies
                    .iter()
                    .any(|dependency| dependency.contains("Unrelated")),
                "{name}: {dependencies:?}"
            );
            if name != "Opaque" {
                assert!(
                    dependencies.contains("type alias Scalar = u8"),
                    "{name}: {dependencies:?}"
                );
            }
        }
        let import = crate::imports::parse_use_line("use dep::foreign;").unwrap();
        let api = expanded_api_shape(source, &import, false).unwrap();
        assert_eq!(api.members[0].0, "function foreign");
        assert!(
            api.attributes["function foreign"][0]
                .1
                .iter()
                .any(|attr| attr.contains("inspect"))
        );
        let import = crate::imports::parse_use_line("use dep::Opaque::{self};").unwrap();
        assert!(
            expanded_api_shape(source, &import, false)
                .unwrap()
                .has_definition
        );
    }

    #[test]
    fn foreign_verification_rejects_changed_aliases_abi_safety_and_attributes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dep.json");
        let import = crate::imports::parse_use_line("use dep::foreign;").unwrap();
        let normal =
            "type Scalar = u8; unsafe extern \"C\" { pub fn foreign(value: Scalar) -> Scalar; }";
        fs::write(path.with_extension("normal.rs"), normal).unwrap();
        let krate = foreign_crate(foreign_function());
        for doc in [
            normal.replace("Scalar = u8", "Scalar = u16"),
            normal.replace("extern \"C\"", "extern \"C-unwind\""),
            normal.replace("pub fn", "pub safe fn"),
            normal.replace("pub fn", "#[must_use] pub fn"),
            normal.replace("pub fn", "#[link_name = \"different\"] pub fn"),
        ] {
            fs::write(path.with_extension("doc.rs"), &doc).unwrap();
            let mut reported = krate.clone();
            if doc.contains("#[must_use]") {
                reported
                    .index
                    .get_mut(&Id(2))
                    .unwrap()
                    .attrs
                    .push(rustdoc_types::Attribute::MustUse { reason: None });
            }
            let error = reject_non_doc_expansion(&reported, &path, &import, false).unwrap_err();
            assert!(
                error.contains("non-doc API extraction is incomplete"),
                "{doc}: {error}"
            );
        }
        fs::write(path.with_extension("doc.rs"), normal).unwrap();
        assert!(reject_non_doc_expansion(&krate, &path, &import, false).is_ok());

        let normal = "type Scalar = u8; unsafe extern \"C\" { pub static foreign: Scalar; }";
        let krate = foreign_crate(ItemEnum::Static(rustdoc_types::Static {
            type_: rustdoc_types::Type::Primitive("u8".into()),
            is_mutable: false,
            expr: String::new(),
            is_unsafe: true,
        }));
        fs::write(path.with_extension("normal.rs"), normal).unwrap();
        for doc in [
            normal.replace("Scalar = u8", "Scalar = u16"),
            normal.replace("pub static", "pub safe static"),
            normal.replace("pub static", "pub static mut"),
            normal.replace("pub static", "#[deprecated(note = \"changed\")] pub static"),
        ] {
            fs::write(path.with_extension("doc.rs"), &doc).unwrap();
            let mut reported = krate.clone();
            if doc.contains("#[deprecated") {
                reported.index.get_mut(&Id(2)).unwrap().deprecation =
                    Some(rustdoc_types::Deprecation {
                        since: None,
                        note: Some("changed".into()),
                    });
            }
            assert!(
                reject_non_doc_expansion(&reported, &path, &import, false).is_err(),
                "{doc}"
            );
        }
        fs::write(path.with_extension("doc.rs"), normal).unwrap();
        assert!(reject_non_doc_expansion(&krate, &path, &import, false).is_ok());
    }

    #[test]
    fn unexamined_foreign_declarations_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("dep.json");
        let import = crate::imports::parse_use_line("use dep::foreign;").unwrap();
        fs::write(path.with_extension("normal.rs"), "").unwrap();
        fs::write(path.with_extension("doc.rs"), "").unwrap();
        for inner in [
            foreign_function(),
            ItemEnum::Static(rustdoc_types::Static {
                type_: rustdoc_types::Type::Primitive("u8".into()),
                is_mutable: false,
                expr: String::new(),
                is_unsafe: true,
            }),
            ItemEnum::ExternType,
        ] {
            let error =
                reject_non_doc_expansion(&foreign_crate(inner), &path, &import, false).unwrap_err();
            assert!(
                error.contains("cannot verify foreign declaration")
                    || error.contains("cannot resolve requested path"),
                "{error}"
            );
        }
        let normal = "unsafe extern \"C\" { pub type foreign; }";
        fs::write(path.with_extension("normal.rs"), normal).unwrap();
        fs::write(path.with_extension("doc.rs"), normal).unwrap();
        assert!(
            reject_non_doc_expansion(&foreign_crate(ItemEnum::ExternType), &path, &import, false)
                .is_ok()
        );
    }

    #[test]
    fn expansion_declaration_dependencies_do_not_require_reported_members() {
        let directory = tempfile::TempDir::new().unwrap();
        let json_path = directory.path().join("dep.json");
        let krate = minimal_crate(None, FORMAT_VERSION);
        let normal = "type Number = u8; type Alias = Number; pub const C: Alias = 1; pub static S: Alias = 2; pub type Public = Alias;";
        fs::write(json_path.with_extension("normal.rs"), normal).unwrap();
        fs::write(json_path.with_extension("doc.rs"), normal).unwrap();
        for name in ["C", "S", "Public"] {
            let import = crate::imports::parse_use_line(&format!("use dep::{name};")).unwrap();
            assert!(reject_non_doc_expansion(&krate, &json_path, &import, false).is_ok());
        }
        fs::write(
            json_path.with_extension("doc.rs"),
            normal.replace("u8", "u16"),
        )
        .unwrap();
        for name in ["C", "S", "Public"] {
            let import = crate::imports::parse_use_line(&format!("use dep::{name};")).unwrap();
            let error = reject_non_doc_expansion(&krate, &json_path, &import, false).unwrap_err();
            assert!(
                error.contains("type aliases or resolved paths differ"),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn expansion_unicode_apis_preserve_complete_source_text() {
        for (source, query, shape) in [
            (
                "pub fn caféééé() -> [u8; 1] { [0; 1] }",
                "use dep::caféééé;",
                "fn pub fn caféééé() -> [u8; 1]",
            ),
            (
                "pub struct S { pub 字字字: u8 }",
                "use dep::S;",
                "struct field 字字字: pub u8",
            ),
            (
                "pub type 字字字 = u8; pub struct S { pub 字字字: 字字字 }",
                "use dep::S;",
                "struct field 字字字: pub 字字字",
            ),
            (
                "pub trait S { fn caféééé() -> [u8; 1]; }",
                "use dep::S;",
                "required trait method fn caféééé() -> [u8; 1]",
            ),
        ] {
            let import = crate::imports::parse_use_line(query).unwrap();
            for source in [source.to_string(), format!("// 字é\r\n{source}\r\n")] {
                let normal = expanded_api_shape(&source, &import, false).unwrap();
                let doc = expanded_api_shape(&source, &import, false).unwrap();
                assert!(normal.has_definition, "{query}");
                assert!(normal.shapes.contains(shape), "{:?}", normal.shapes);
                assert_eq!(normal.shapes, doc.shapes, "{query}");
                assert_eq!(normal.members, doc.members, "{query}");
                assert_eq!(normal.dependencies, doc.dependencies, "{query}");
            }
        }
    }

    #[test]
    fn expansion_unicode_signature_changes_are_detected() {
        for (source, query) in [
            (
                "pub fn caféééé() -> [u8; 1] { [0; 1] }",
                "use dep::caféééé;",
            ),
            ("pub trait S { fn caféééé() -> [u8; 1]; }", "use dep::S;"),
            (
                "pub struct S; impl S { pub fn caféééé() -> [u8; 1] { [0; 1] } }",
                "use dep::S;",
            ),
        ] {
            let import = crate::imports::parse_use_line(query).unwrap();
            let normal = expanded_api_shape(source, &import, false).unwrap();
            let changed = source.replace("[u8; 1]", "[u8; 2]");
            let doc = expanded_api_shape(&changed, &import, false).unwrap();
            assert!(
                normal.shapes.iter().any(|shape| shape.ends_with("[u8; 1]")),
                "{:?}",
                normal.shapes
            );
            assert_ne!(normal.shapes, doc.shapes, "{query}");
            assert_ne!(normal.members, doc.members, "{query}");
        }
    }

    #[test]
    fn expansion_rejects_changed_intermediate_routes() {
        let source = r#"
pub mod a { pub mod api { pub struct Thing { pub actual: u8 } } }
pub mod b { pub mod api { pub struct Thing { pub doc_only: u16 } } }
"#;
        let dir = tempfile::TempDir::new().unwrap();
        let json_path = dir.path().join("dep.json");
        let krate = minimal_crate(None, rustdoc_types::FORMAT_VERSION);
        for (normal_route, doc_route) in [
            ("pub use a::api as facade;", "pub use b::api as facade;"),
            (
                "use a::api as first; pub use first as facade;",
                "use b::api as first; pub use first as facade;",
            ),
            (
                "pub mod facade { pub use crate::a::api::*; }",
                "pub mod facade { pub use crate::b::api::*; }",
            ),
            (
                "pub use external_a::api as facade;",
                "pub use external_b::api as facade;",
            ),
        ] {
            fs::write(
                json_path.with_extension("normal.rs"),
                format!("{source}{normal_route}"),
            )
            .unwrap();
            fs::write(
                json_path.with_extension("doc.rs"),
                format!("{source}{doc_route}"),
            )
            .unwrap();
            let import = crate::imports::parse_use_line("use dep::facade::Thing;").unwrap();
            let error = reject_non_doc_expansion(&krate, &json_path, &import, false).unwrap_err();
            assert!(
                error.contains("resolves to different definitions under cfg(doc)"),
                "{normal_route}: {error}"
            );
        }
    }

    #[test]
    fn expansion_routes_preserve_aliases_namespaces_variants_and_external_paths() {
        let source = r#"
pub mod a { pub mod api {
    pub struct Thing { pub actual: u8 }
    pub type Alias = [u8; 1];
    pub fn value() {}
    pub const C: u8 = 1;
    pub static S: u8 = 1;
    pub enum E { Variant(u8) }
    unsafe extern "C" { pub fn foreign(); }
} }
pub fn facade() {}
pub use a::api as facade;
pub mod globbed { pub use crate::facade::*; }
pub mod typed {
    pub use crate::a::api::value as Thing;
    pub use crate::a::api::*;
}
pub use facade::E::Variant as Renamed;
pub extern crate self as me;
pub extern crate foreign as dependency;
pub use dependency::api as external;
"#;
        for (query, expected, has_definition) in [
            ("dep::facade::Thing", "a::api::Thing", true),
            ("dep::facade::{self}", "a::api", false),
            ("dep::globbed::Alias", "a::api::Alias", true),
            ("dep::globbed::value", "a::api::value", true),
            ("dep::globbed::C", "a::api::C", true),
            ("dep::globbed::S", "a::api::S", true),
            ("dep::Renamed", "a::api::E::Variant", true),
            ("dep::typed::Thing::{self}", "a::api::Thing", true),
            ("dep::me::facade::Thing", "a::api::Thing", true),
            ("dep::external::Thing", "::::foreign::api::Thing", false),
        ] {
            let import = crate::imports::parse_use_line(query).unwrap();
            let api = expanded_api_shape(source, &import, false).unwrap();
            assert_eq!(
                api.resolved_path.as_ref().map(|path| path.join("::")),
                Some(expected.to_string()),
                "{query}"
            );
            assert_eq!(api.has_definition, has_definition, "{query}");
        }
        let import = crate::imports::parse_use_line("dep::facade::foreign").unwrap();
        assert_eq!(
            expanded_api_shape(source, &import, false)
                .unwrap()
                .resolved_path,
            Some(vec!["a".into(), "api".into(), "foreign".into()])
        );
        let import = crate::imports::parse_use_line("dep::me::{self}").unwrap();
        assert_eq!(
            expanded_api_shape(source, &import, false)
                .unwrap()
                .resolved_path,
            Some(Vec::new())
        );
    }

    #[test]
    fn expansion_constant_dependencies_follow_imports_aliases_and_transitive_values() {
        let source = r#"
mod origin {
    pub const BASE: u8 = 1;
    pub const LEN: usize = BASE as usize;
}
use origin::LEN as SIZE;
pub type Buffer = [u8; SIZE];
pub const VALUE: usize = SIZE;
pub fn buffer(value: Buffer) -> [u8; SIZE] { value }
pub struct Packet { pub bytes: [u8; SIZE] }
pub struct Container<const N: usize>;
pub type SizedContainer = Container<SIZE>;
"#;
        for name in ["buffer", "Packet", "Buffer", "VALUE", "SizedContainer"] {
            let import = crate::imports::parse_use_line(&format!("use dep::{name};")).unwrap();
            let normal = expanded_api_shape(source, &import, false).unwrap();
            let changed = source.replace("BASE: u8 = 1", "BASE: u8 = 2");
            let doc = expanded_api_shape(&changed, &import, false).unwrap();
            assert_eq!(normal.shapes, doc.shapes, "{name}");
            assert_ne!(normal.dependencies, doc.dependencies, "{name}");
            let definitions = &normal.dependencies["definition"][0];
            assert!(
                definitions.contains("const origin::BASE: u8 = 1"),
                "{definitions:?}"
            );
            assert!(
                definitions.contains("const origin::LEN: usize = BASE as usize"),
                "{definitions:?}"
            );
        }
    }

    #[test]
    fn expansion_constant_evaluation_tracks_function_bodies_and_block_scope() {
        let source = r#"
const BASE: usize = 1;
const fn length(value: usize) -> usize {
    let (left, right) = (value, 0);
    const INCREMENT: usize = BASE;
    left + right + INCREMENT
}
const LEN: usize = {
    use crate::length as compute;
    let value = 0;
    compute(value)
};
pub fn buffer<const N: usize>(value: [u8; LEN]) -> [u8; N] { loop {} }
"#;
        let import = crate::imports::parse_use_line("use dep::buffer;").unwrap();
        let normal = expanded_api_shape(source, &import, false).unwrap();
        let definitions = &normal.dependencies["definition"][0];
        assert!(
            definitions.contains("const BASE: usize = 1"),
            "{definitions:?}"
        );
        assert!(
            definitions
                .iter()
                .any(|definition| definition.starts_with("function length:")),
            "{definitions:?}"
        );
        assert!(
            definitions
                .iter()
                .any(|definition| definition.contains("INCREMENT: usize = BASE")),
            "{definitions:?}"
        );
        for changed in [
            source.replace("BASE: usize = 1", "BASE: usize = 2"),
            source.replace("left + right + INCREMENT", "left + right + INCREMENT + 1"),
        ] {
            let doc = expanded_api_shape(&changed, &import, false).unwrap();
            assert_eq!(normal.shapes, doc.shapes);
            assert_ne!(normal.dependencies, doc.dependencies);
        }
    }

    #[test]
    fn constant_patterns_track_values_without_hiding_bindings() {
        let source = r#"
const BASE: u8 = 1;
pub const SIZE: usize = match 1_u8 { BASE => 1, value => value as usize + 1 };
pub type Buffer = [u8; SIZE];
pub fn buffer(value: Buffer) -> Buffer { value }
const fn length(value: u8) -> usize { match value { BASE => 1, other => other as usize + 1 } }
pub struct Computed { pub bytes: [u8; length(1)] }
"#;
        let changed = source.replace("BASE: u8 = 1", "BASE: u8 = 2");
        for name in ["SIZE", "Buffer", "buffer", "Computed"] {
            let import = crate::imports::parse_use_line(&format!("use dep::{name};")).unwrap();
            let normal = expanded_api_shape(source, &import, false).unwrap();
            let doc = expanded_api_shape(&changed, &import, false).unwrap();
            assert_ne!(normal.dependencies, doc.dependencies, "{name}");
            assert!(normal.dependencies["definition"][0].contains("const BASE: u8 = 1"));
        }
        let source = "const VALUE: u8 = 1; pub const SIZE: usize = { struct BASE { byte: u8 } const BASE: u8 = VALUE; match 1_u8 { BASE => 1, value => value as usize + 1 } };";
        let import = crate::imports::parse_use_line("use dep::SIZE;").unwrap();
        let normal = expanded_api_shape(source, &import, false).unwrap();
        let doc = expanded_api_shape(
            &source.replace("VALUE: u8 = 1", "VALUE: u8 = 2"),
            &import,
            false,
        )
        .unwrap();
        assert_ne!(normal.dependencies, doc.dependencies);
        assert!(normal.dependencies["definition"][0].contains("const VALUE: u8 = 1"));

        let source = "struct value { byte: u8 } const fn length(input: u8) -> usize { let value = input; if let 0 = value { 1 } else { match value { value => value as usize } } } pub const SIZE: usize = length(1);";
        let normal = expanded_api_shape(source, &import, false).unwrap();
        let doc =
            expanded_api_shape(&source.replace("byte: u8", "byte: u16"), &import, false).unwrap();
        assert_eq!(normal.dependencies, doc.dependencies);
    }

    #[test]
    fn block_modules_preserve_imports_aliases_and_shadowing() {
        let source = r#"
pub struct S;
pub struct Other;
fn helper() {
    use crate::Other as S;
    mod local {
        use crate::S as Selected;
        type Alias = self::Selected;
        impl Selected { pub fn imported(&self) {} }
        impl Alias { pub fn aliased(&self) {} }
        mod inner {
            use super::Alias;
            impl Alias { pub fn nested(&self) {} }
            struct Selected;
            impl Selected { pub fn hidden(&self) {} }
        }
    }
    impl local::Selected { pub fn qualified(&self) {} }
}
trait Defaults {
    fn helper() { mod local { use crate::S as Selected; impl Selected { pub fn default_body(&self) {} } } }
}
"#;
        let import = crate::imports::parse_use_line("use dep::S;").unwrap();
        let api = expanded_api_shape(source, &import, false).unwrap();
        let keys = api
            .members
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(
            keys,
            HashSet::from([
                "inherent method imported",
                "inherent method aliased",
                "inherent method nested",
                "inherent method qualified",
                "inherent method default_body",
            ])
        );
    }

    #[test]
    fn expansion_signature_dependencies_ignore_ordinary_function_bodies() {
        let source = r#"
const BASE: usize = 1;
pub fn buffer<const BASE: usize>() -> [u8; BASE] { [0; BASE] }
pub struct Packet;
impl Packet { pub fn value(&self) -> usize { BASE } }
pub trait Contract { fn value(&self) -> usize { BASE } }
pub struct Container<const N: usize>;
pub fn generic<const BASE: usize>() -> Container<BASE> { Container }
mod aliases { pub type Byte = u8; }
pub struct Typed<T>(T);
pub fn typed(value: Typed<aliases::Byte>) -> Typed<aliases::Byte> { value }
"#;
        let changed = source.replace("BASE: usize = 1", "BASE: usize = 2");
        for name in ["buffer", "Packet", "Contract", "generic", "typed"] {
            let import = crate::imports::parse_use_line(&format!("use dep::{name};")).unwrap();
            let normal = expanded_api_shape(source, &import, false).unwrap();
            let doc = expanded_api_shape(&changed, &import, false).unwrap();
            assert_eq!(normal.dependencies, doc.dependencies, "{name}");
            assert!(
                normal
                    .dependencies
                    .values()
                    .flatten()
                    .flatten()
                    .all(|definition| !definition.starts_with("const BASE:"))
            );
        }
    }

    #[test]
    fn expansion_constant_function_parameters_and_locals_shadow_value_paths() {
        let source = r#"
const fn value() -> usize { 1 }
const fn helper() -> usize { 1 }
const fn length(value: usize) -> usize {
    let helper = value;
    { let (value, offset) = (helper, 0); value + offset }
}
pub struct Packet { pub bytes: [u8; length(1)] }
"#;
        let import = crate::imports::parse_use_line("use dep::Packet;").unwrap();
        let normal = expanded_api_shape(source, &import, false).unwrap();
        let changed = source.replace("-> usize { 1 }", "-> usize { 2 }");
        let doc = expanded_api_shape(&changed, &import, false).unwrap();
        assert_eq!(normal.dependencies, doc.dependencies);
        let definitions = &normal.dependencies["definition"][0];
        assert!(
            definitions
                .iter()
                .any(|definition| definition.starts_with("function length:"))
        );
        assert!(
            definitions
                .iter()
                .all(|definition| !definition.starts_with("function value:")
                    && !definition.starts_with("function helper:"))
        );
    }

    #[test]
    fn expansion_unverified_associated_evaluation_is_rejected() {
        let import = crate::imports::parse_use_line("use dep::Packet;").unwrap();
        for source in [
            "struct Limits; impl Limits { const LEN: usize = 1; } pub struct Packet { pub bytes: [u8; Limits::LEN] }",
            "struct Limits; impl Limits { const LEN: usize = 1; } pub struct Container<const N: usize>; pub type Packet = Container<{ Limits::LEN }>;",
            "struct Limits; trait Size { const LEN: usize; } impl Size for Limits { const LEN: usize = 1; } pub struct Packet { pub bytes: [u8; <Limits as Size>::LEN] }",
            "struct Limits; impl Limits { const fn length(&self) -> usize { 1 } } const LEN: usize = Limits.length(); pub struct Packet { pub bytes: [u8; LEN] }",
        ] {
            let error = expanded_api_shape(source, &import, false).err().unwrap();
            assert!(
                error.contains("non-doc API extraction is incomplete"),
                "{error}"
            );
            assert!(error.contains("constant evaluation"), "{error}");
        }
    }

    #[test]
    fn expansion_impls_in_constant_blocks_preserve_scope_and_headers() {
        let import = crate::imports::parse_use_line("use dep::S::{self};").unwrap();
        let source = r#"
pub struct S;
pub struct Other;
pub trait Marker {}
pub trait Link<T> {}
const _: () = {
    extern crate core as _core;
    extern crate self as local;
    use crate::S as Selected;
    use Selected as Imported;
    use crate::Marker as LocalMarker;
    type Alias = Selected;
    #[automatically_derived] impl _core::clone::Clone for Alias { fn clone(&self) -> Self { Self } }
    impl LocalMarker for Selected {}
    impl Alias { pub fn through_alias(&self) {} }
    impl local::S { pub fn through_self(&self) {} }
    const _: () = {
        struct S;
        struct Selected;
        impl S { pub fn hidden(&self) {} }
        impl Imported { pub fn through_outer_import(&self) {} }
        impl crate::Link<crate::S> for S {}
    };
};
const _: () = {
    use crate::Other as S;
    impl S { pub fn unrelated(&self) {} }
};
"#;
        let api = expanded_api_shape(source, &import, false).unwrap();
        assert_eq!(api.members.len(), 6, "{:?}", api.members);
        for key in [
            "derived trait impl Clone",
            "trait impl Marker",
            "trait impl Link",
            "inherent method through_alias",
            "inherent method through_self",
            "inherent method through_outer_import",
        ] {
            assert!(api.members.iter().any(|(member, _)| member == key), "{key}");
        }
        assert!(api.has_definition);
        let local_module = "pub struct S; const _: () = { mod local { pub use crate::S; } impl local::S { pub fn through_module(&self) {} } };";
        let api = expanded_api_shape(local_module, &import, false).unwrap();
        assert!(
            api.members
                .iter()
                .any(|(key, _)| key == "inherent method through_module")
        );
    }

    #[test]
    fn expansion_impls_in_all_blocks_preserve_lexical_scope() {
        let import = crate::imports::parse_use_line("use dep::S::{self};").unwrap();
        let source = r#"
pub struct S;
pub struct Other;
pub trait Marker {}
fn private_uncalled() {
    use crate::S as Outer;
    impl Outer { pub fn function(&self) {} }
    let _ = || {
        struct S;
        impl S { pub fn hidden_local(&self) {} }
        impl Outer { pub fn closure(&self) {} }
        { use crate::Other as Outer; impl Outer { pub fn hidden_import(&self) {} } }
    };
    if true {
        type Alias = Outer;
        impl Alias { pub const NESTED: u8 = 1; }
    }
    let _ = [const { impl crate::S { pub fn inline_const(&self) {} } 0 }];
    { impl S { pub fn shadowed_later(&self) {} } struct S; }
}
impl Other {
    fn private_method() {
        impl crate::Marker for crate::S {}
        impl crate::S { pub fn method_body(&self) {} }
    }
    const ASSOCIATED: () = { impl crate::S { pub fn associated_const(&self) {} } };
}
trait Defaults {
    fn default_method() { impl crate::S { pub fn trait_default(&self) {} } }
    const VALUE: () = { impl crate::S { pub fn trait_const(&self) {} } };
}
static INITIALIZER: () = if true {
    impl crate::S { pub fn static_body(&self) {} }
};
const NESTED: () = ((), { impl crate::S { pub fn const_expression(&self) {} } }).1;
mod nested {
    fn private_uncalled() {
        use super::S as Imported;
        { impl Imported { pub fn nested_module(&self) {} } }
    }
}
"#;
        let api = expanded_api_shape(source, &import, false).unwrap();
        let keys = api
            .members
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(
            keys,
            HashSet::from([
                "inherent method function",
                "inherent method closure",
                "inherent const NESTED",
                "inherent method inline_const",
                "trait impl Marker",
                "inherent method method_body",
                "inherent method associated_const",
                "inherent method trait_default",
                "inherent method trait_const",
                "inherent method static_body",
                "inherent method const_expression",
                "inherent method nested_module",
            ])
        );
        assert_eq!(api.members.len(), keys.len(), "{:?}", api.members);
        assert!(api.has_definition);
        let doc =
            expanded_api_shape("pub struct S; fn private_uncalled() {}", &import, false).unwrap();
        assert!(
            api.shapes
                .difference(&doc.shapes)
                .any(|shape| shape.contains("fn function(&self)"))
        );
    }

    #[test]
    fn expansion_self_crate_aliases_preserve_lexical_and_external_identity() {
        let import = crate::imports::parse_use_line("use dep::S;").unwrap();
        let source = r#"
extern crate self as local;
extern crate external as foreign;
pub struct S;
pub mod origin { pub struct S; }
mod implementation {
    mod local { pub struct S; }
    impl local::S { pub fn lexical(&self) {} }
    impl ::local::S { pub fn real(&self) {} }
}
mod imports {
    use crate::origin as local;
    impl local::S { pub fn imported(&self) {} }
}
mod external_import {
    use foreign::S as local;
    impl local { pub fn explicit_external(&self) {} }
}
mod qualified_external {
    impl crate::foreign::S { pub fn renamed_external(&self) {} }
    impl ::origin::S { pub fn absolute_external(&self) {} }
}
"#;
        let shapes = expanded_api_shape(source, &import, false).unwrap();
        let methods = shapes
            .shapes
            .iter()
            .filter(|shape| shape.starts_with("impl"))
            .collect::<Vec<_>>();
        assert_eq!(methods.len(), 1, "{methods:?}");
        assert!(methods[0].contains("fn real(&self)"), "{methods:?}");
    }

    #[test]
    fn expansion_trait_impl_headers_preserve_semantic_constraints() {
        let import = crate::imports::parse_use_line("use dep::S;").unwrap();
        let source = "pub struct S<T>(T); impl<T: Copy> crate::Marker<(T,u8)> for crate::S<T> where T: Send {}";
        let normal = expanded_api_shape(source, &import, false).unwrap();
        assert!(
            normal
                .shapes
                .contains("impl<T: Copy> crate::Marker<(T,u8)> for crate::S<T> where T: Send"),
            "{:?}",
            normal.shapes
        );
        for changed in [
            source.replace("impl<T:", "unsafe impl<T:"),
            source.replace("crate::Marker", "!crate::Marker"),
            source.replace("T: Copy", "T: Clone"),
            source.replace("where T: Send", "where T: Sync"),
            source.replace("Marker<(T,u8)>", "Marker<(T,u16)>"),
            source.replace("for crate::S<T>", "for crate::S<u8>"),
        ] {
            let doc = expanded_api_shape(&changed, &import, false).unwrap();
            assert!(
                normal
                    .shapes
                    .difference(&doc.shapes)
                    .any(|shape| shape.contains("impl")),
                "{changed}"
            );
        }
    }

    #[test]
    fn expansion_trait_impl_keys_follow_local_and_external_aliases() {
        let import = crate::imports::parse_use_line("use dep::S;").unwrap();
        let source = r#"
pub struct S;
pub trait Marker {}
pub use Marker as First;
mod local {
    use crate::First as Second;
    impl Second for crate::S {}
}
use std::fmt::Debug as ImportedDebug;
impl ImportedDebug for S {}
extern crate core as foreign;
use foreign::fmt::Display as FirstDisplay;
mod external {
    use crate::FirstDisplay as SecondDisplay;
    impl SecondDisplay for crate::S {}
}
use ::std::hash::Hash as ImportedHash;
impl ImportedHash for S {}
"#;
        let api = expanded_api_shape(source, &import, false).unwrap();
        let keys = api
            .members
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(
            keys,
            HashSet::from([
                "trait impl Marker",
                "trait impl Debug",
                "trait impl Display",
                "trait impl Hash",
            ])
        );
    }

    #[test]
    fn expansion_trait_impls_include_references_and_trait_argument_types() {
        let import = crate::imports::parse_use_line("use dep::S;").unwrap();
        let source = r#"
pub struct S;
pub struct Other<T>(T);
pub trait Link<T> {}
impl Link<S> for u8 {}
impl Link<Option<S>> for u16 {}
impl Link<u8> for &S {}
impl Link<u16> for &mut S {}
impl Link<u32> for Other<S> {}
impl Link<u64> for (S, u8) {}
impl Link<u128> for Other<u8> {}
impl<S> Link<S> for Other<S> {}
impl<S> Link<crate::S> for Other<S> {}
impl Other<S> { pub fn unrelated(&self) {} }
"#;
        let api = expanded_api_shape(source, &import, false).unwrap();
        let impls = api
            .members
            .iter()
            .filter(|(key, _)| key == "trait impl Link")
            .map(|(_, shape)| shape.as_str())
            .collect::<Vec<_>>();
        assert_eq!(impls.len(), 4, "{impls:?}");
        assert!(
            !impls
                .iter()
                .any(|shape| shape.starts_with("impl<S> Link<S>"))
        );
        assert!(!api.shapes.iter().any(|shape| shape.contains("unrelated")));
    }

    #[test]
    fn unresolved_explicit_imports_never_select_glob_types() {
        let import = crate::imports::parse_use_line("use dep::S;").unwrap();
        for imports in [
            "use crate::*; use origin::S;",
            "use origin::S; use crate::*;",
            "use crate::*; use origin::S::{self};",
            "use origin::{S as r#S}; use crate::*;",
        ] {
            let source = format!(
                "pub struct S; impl S {{ pub fn live(&self) {{}} }} pub trait LocalTrait {{ fn external(&self); }} mod implementation {{ {imports} impl LocalTrait for S {{ fn external(&self) {{}} }} }}"
            );
            let shapes = expanded_api_shape(&source, &import, false).unwrap();
            let methods = shapes
                .shapes
                .iter()
                .filter(|shape| shape.starts_with("impl"))
                .collect::<Vec<_>>();
            assert_eq!(methods.len(), 1, "{imports}: {methods:?}");
            assert!(
                methods[0].contains("fn live(&self)"),
                "{imports}: {methods:?}"
            );
        }
    }

    #[test]
    fn load_valid_json_covers_success_and_validation_errors() {
        let dir = tempfile::TempDir::new().unwrap();
        let pkg = package();
        let good = dir.path().join("good.json");
        fs::write(
            &good,
            serde_json::to_vec(&minimal_crate(
                Some(pkg.version.to_string()),
                FORMAT_VERSION,
            ))
            .unwrap(),
        )
        .unwrap();
        let loaded = load_valid_json(&good, &pkg).unwrap();
        assert_eq!(loaded.format_version, FORMAT_VERSION);

        let no_version = dir.path().join("no_version.json");
        fs::write(
            &no_version,
            serde_json::to_vec(&minimal_crate(None, FORMAT_VERSION)).unwrap(),
        )
        .unwrap();
        assert!(load_valid_json(&no_version, &pkg).is_ok());

        let missing = load_valid_json(&dir.path().join("missing.json"), &pkg).unwrap_err();
        assert!(missing.contains("rustdoc JSON missing"));

        let bad_json = dir.path().join("bad.json");
        fs::write(&bad_json, b"not json").unwrap();
        assert!(
            load_valid_json(&bad_json, &pkg)
                .unwrap_err()
                .contains("failed to parse rustdoc JSON")
        );

        let bad_format = dir.path().join("bad_format.json");
        fs::write(
            &bad_format,
            serde_json::to_vec(&minimal_crate(
                Some(pkg.version.to_string()),
                FORMAT_VERSION + 1,
            ))
            .unwrap(),
        )
        .unwrap();
        assert!(
            load_valid_json(&bad_format, &pkg)
                .unwrap_err()
                .contains("unsupported")
        );

        let stale = dir.path().join("stale.json");
        fs::write(
            &stale,
            serde_json::to_vec(&minimal_crate(Some("0.0.0".into()), FORMAT_VERSION)).unwrap(),
        )
        .unwrap();
        assert!(load_valid_json(&stale, &pkg).unwrap_err().contains("stale"));
    }

    #[test]
    fn generate_output_error_hints_are_actionable() {
        let pkg = package();
        let base = Output {
            status: status(1),
            stdout: Vec::new(),
            stderr: b"toolchain 'nightly' is not installed".to_vec(),
        };
        let err = handle_generate_output(&pkg, "nightly", base).unwrap_err();
        assert_eq!(
            err,
            format!(
                "failed to generate rustdoc JSON for {} {}; compatible nightly toolchain not found; install with `rustup toolchain install nightly` or set EXCRA_TOOLCHAIN: toolchain 'nightly' is not installed",
                pkg.name, pkg.version
            )
        );

        let unrelated = format_generate_error(
            &pkg,
            "nightly",
            "error[E0425]: cannot find value `toolchain` in this scope",
        );
        assert_eq!(
            unrelated,
            format!(
                "failed to generate rustdoc JSON for {} {}: error[E0425]: cannot find value `toolchain` in this scope",
                pkg.name, pkg.version
            )
        );

        let unstable = Output {
            status: status(1),
            stdout: Vec::new(),
            stderr: b"the option `Z` is only accepted with unstable-options output-format".to_vec(),
        };
        let err = handle_generate_output(&pkg, "stable", unstable).unwrap_err();
        assert!(err.contains("rustdoc JSON requires nightly"));

        let plain = format_generate_error(&pkg, "nightly", "plain failure");
        assert!(plain.contains("plain failure"));
        assert!(!plain.contains("requires nightly"));

        let ok = Output {
            status: status(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        assert!(handle_generate_output(&pkg, "nightly", ok).is_ok());
    }

    #[test]
    fn default_toolchain_matches_the_repository_pin() {
        let toolchain_file = include_str!("../rust-toolchain.toml");
        assert!(toolchain_file.contains(&format!("channel = \"{PINNED_TOOLCHAIN}\"")));
    }

    #[test]
    fn target_selector_filters_cargo_rustdoc_to_one_target() {
        let pkg = package();
        let mut target = pkg.targets[0].clone();

        target.kind = vec!["lib".into()];
        assert_eq!(target_selector(&target).unwrap(), vec!["--lib"]);

        target.kind = vec!["proc-macro".into()];
        assert_eq!(target_selector(&target).unwrap(), vec!["--lib"]);

        for kind in ["rlib", "dylib", "cdylib", "staticlib"] {
            target.kind = vec![kind.into()];
            assert_eq!(target_selector(&target).unwrap(), vec!["--lib"]);
        }

        target.kind = vec!["bin".into()];
        target.name = "tool".into();
        assert_eq!(target_selector(&target).unwrap(), vec!["--bin", "tool"]);

        target.kind = vec!["custom-build".into()];
        assert!(
            target_selector(&target)
                .unwrap_err()
                .contains("unsupported")
        );
    }

    #[test]
    fn generate_json_reports_missing_toolchain() {
        let pkg = package();
        let target = pkg.targets[0].clone();
        let output = tempfile::TempDir::new().unwrap();
        let contexts = [DependencyContext {
            kind: DependencyKind::Normal,
            target: None,
            via: None,
        }];
        let target_selection = CargoTargetSelection {
            toolchain: PINNED_TOOLCHAIN.into(),
            effective_triple: "x86_64-unknown-linux-gnu".into(),
            cargo_platform: None,
            command_line_override: None,
        };
        let unit = CargoUnitIdentity {
            features: Vec::new(),
            mode: "check".into(),
            platform: None,
            profile: "{}".into(),
        };
        let feature_selection = FeatureSelection::default();
        let request = RustdocRequest {
            manifest_path: PathBuf::from("Cargo.toml"),
            metadata: &metadata(),
            root_package: &pkg,
            package: &pkg,
            target: &target,
            contexts: &contexts,
            target_selection: &target_selection,
            feature_selection: &feature_selection,
            unit: &unit,
            root_prerequisites: &[],
        };
        let err = generate_json_with_toolchain(
            &request,
            output.path(),
            output.path(),
            "definitely_missing_excra_toolchain",
        )
        .unwrap_err();
        assert!(err.contains("definitely_missing_excra_toolchain"), "{err}");
    }

    #[test]
    fn consumer_placeholders_preserve_required_libraries_and_build_scripts() {
        let directory = Path::new("/workspace");
        let sources = vec![
            directory.join("app/src/lib.rs"),
            directory.join("app/src/main.rs"),
        ];
        let required = vec![sources[0].clone()];
        let arguments = |source: &str, kind: &str, test: bool| {
            let mut arguments = vec![
                "--crate-name".into(),
                "app".into(),
                source.into(),
                "--crate-type".into(),
                kind.into(),
            ];
            if test {
                arguments.push("--test".into());
            }
            arguments
        };
        for (source, kind, test, expected) in [
            ("app/src/lib.rs", "lib", false, None),
            ("app/src/lib.rs", "lib", true, Some(2)),
            ("app/src/main.rs", "bin", false, Some(2)),
            ("app/build.rs", "bin", false, None),
            ("other/src/lib.rs", "lib", false, None),
        ] {
            assert_eq!(
                consumer_source_to_replace(
                    &arguments(source, kind, test),
                    directory,
                    &sources,
                    &required,
                ),
                expected,
            );
        }
        assert_eq!(
            consumer_source_to_replace(
                &arguments("app/src/lib.rs", "lib", false),
                directory,
                &sources,
                &[],
            ),
            Some(2),
        );
        let mut build_script = arguments("app/src/lib.rs", "bin", false);
        build_script[1] = "build_script_build".into();
        assert_eq!(
            consumer_source_to_replace(&build_script, directory, &sources, &[]),
            None,
        );
    }

    #[test]
    fn compiler_wrapper_parses_direct_and_nested_cargo_invocations() {
        let direct = vec![
            OsString::from("/toolchain/bin/rustc"),
            OsString::from("--crate-name"),
            OsString::from("fixture"),
        ];
        let invocation = rustc_invocation(&direct);
        assert!(invocation.workspace_wrapper.is_none());
        assert_eq!(invocation.rustc, OsStr::new("/toolchain/bin/rustc"));
        assert_eq!(invocation.arguments, &direct[1..]);

        let nested = vec![
            OsString::from("/workspace/recording-wrapper"),
            OsString::from("/toolchain/bin/rustc"),
            OsString::from("--crate-name"),
            OsString::from("fixture"),
        ];
        let invocation = rustc_invocation(&nested);
        assert_eq!(
            invocation.workspace_wrapper,
            Some(OsStr::new("/workspace/recording-wrapper"))
        );
        assert_eq!(invocation.rustc, OsStr::new("/toolchain/bin/rustc"));
        assert_eq!(invocation.arguments, &nested[2..]);
        assert_eq!(
            import_probe_compiler(&invocation),
            [OsString::from("/toolchain/bin/rustc")]
        );
    }

    #[test]
    fn compiler_wrapper_distinguishes_compile_mode_and_platform() {
        let target_check = [
            OsString::from("--crate-name"),
            OsString::from("shared"),
            OsString::from("--emit=dep-info,metadata"),
            OsString::from("--target"),
            OsString::from("wasm32-unknown-unknown"),
        ];
        let target_check = target_check
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        assert_eq!(rustc_compile_mode(&target_check), Some("check"));
        assert_eq!(
            argument_value(&target_check, "--target"),
            Some("wasm32-unknown-unknown")
        );

        let host_build = [
            OsString::from("--crate-name=shared"),
            OsString::from("--emit"),
            OsString::from("dep-info,metadata,link"),
        ];
        let host_build = host_build
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();
        assert_eq!(rustc_compile_mode(&host_build), Some("build"));
        assert_eq!(argument_value(&host_build, "--target"), None);
        assert_eq!(argument_value(&host_build, "--crate-name"), Some("shared"));
    }

    #[test]
    fn compiler_wrapper_distinguishes_complete_profile_identity() {
        let selected = serde_json::json!({
            "name": "test",
            "opt_level": "1",
            "lto": "thin",
            "codegen_backend": "llvm",
            "codegen_units": 1,
            "debuginfo": 1,
            "split_debuginfo": "packed",
            "debug_assertions": true,
            "overflow_checks": true,
            "rpath": true,
            "incremental": true,
            "panic": "unwind",
            "strip": { "resolved": { "Named": "debuginfo" } },
        });
        let selected_cfg = RustcCfg::parse(
            b"debug_assertions\noverflow_checks\npanic=\"unwind\"\ntarget_os=\"linux\"\n",
        );
        let arguments = [
            OsString::from("-Copt-level=1"),
            OsString::from("-Clinker-plugin-lto"),
            OsString::from("-Z"),
            OsString::from("codegen-backend=llvm"),
            OsString::from("-Ccodegen-units=1"),
            OsString::from("-C"),
            OsString::from("debuginfo=1"),
            OsString::from("-Csplit-debuginfo=packed"),
            OsString::from("-Crpath"),
            OsString::from("-Cincremental=/tmp/incremental"),
            OsString::from("-Cpanic=unwind"),
            OsString::from("-Cstrip=debuginfo"),
        ];
        let arguments = arguments
            .iter()
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>();

        assert!(profile_matches_cfg(
            &selected,
            &selected_cfg,
            &arguments,
            "unwind"
        ));

        for (field, value) in [
            ("opt_level", serde_json::json!("0")),
            ("lto", serde_json::json!("false")),
            ("codegen_backend", serde_json::Value::Null),
            ("codegen_units", serde_json::json!(2)),
            ("debuginfo", serde_json::json!(2)),
            ("split_debuginfo", serde_json::Value::Null),
            ("rpath", serde_json::json!(false)),
            ("incremental", serde_json::json!(false)),
            ("panic", serde_json::json!("abort")),
            (
                "strip",
                serde_json::json!({ "resolved": { "Named": "symbols" } }),
            ),
        ] {
            let mut different = selected.clone();
            different[field] = value;
            assert!(
                !profile_matches_cfg(&different, &selected_cfg, &arguments, "unwind"),
                "profile field {field} was not matched"
            );
        }

        let build_cfg = RustcCfg::parse(b"panic=\"unwind\"\ntarget_os=\"linux\"\n");
        assert!(!profile_matches_cfg(
            &selected, &build_cfg, &arguments, "unwind"
        ));
        let missing_panic = RustcCfg::parse(b"debug_assertions\noverflow_checks\n");
        assert!(!profile_matches_cfg(
            &selected,
            &missing_panic,
            &arguments,
            "unwind"
        ));

        let defaults = serde_json::json!({
            "name": "test",
            "opt_level": "0",
            "lto": "false",
            "codegen_backend": null,
            "codegen_units": null,
            "debuginfo": 0,
            "split_debuginfo": null,
            "debug_assertions": false,
            "overflow_checks": false,
            "rpath": false,
            "incremental": false,
            "panic": "unwind",
            "strip": { "deferred": "None" },
        });
        let default_cfg = RustcCfg::parse(b"panic=\"unwind\"\n");
        assert!(profile_matches_cfg(&defaults, &default_cfg, &[], "unwind"));
        let abort_default_cfg = RustcCfg::parse(b"panic=\"abort\"\n");
        assert!(profile_matches_cfg(
            &defaults,
            &abort_default_cfg,
            &[],
            "abort"
        ));
    }

    #[test]
    fn unavailable_module_without_source_fails_instead_of_leaking_impls() {
        let mut docs = minimal_crate(None, FORMAT_VERSION);
        let root = docs.index.get_mut(&docs.root).unwrap();
        root.attrs
            .push(rustdoc_types::Attribute::Other("#[cfg(doc)]".into()));
        root.span = None;
        let error = apply_non_doc_cfg(&mut docs, &RustcCfg::parse(b"")).unwrap_err();
        assert!(
            error.contains("cannot establish non-doc lexical availability"),
            "{error}"
        );
        assert!(error.contains("source span missing"), "{error}");
    }

    #[test]
    fn retained_cfgs_are_evaluated_without_rustdocs_doc_flag() {
        let mut docs = minimal_crate(None, FORMAT_VERSION);
        let root = docs.root;
        let mut doc_only = Item {
            id: Id(2),
            crate_id: 0,
            name: Some("DocOnly".into()),
            span: None,
            visibility: Visibility::Public,
            docs: None,
            links: HashMap::new(),
            attrs: vec![rustdoc_types::Attribute::Other(
                "#[<cfg>(any(doc, windows))]".into(),
            )],
            deprecation: None,
            inner: ItemEnum::Struct(rustdoc_types::Struct {
                kind: rustdoc_types::StructKind::Unit,
                generics: rustdoc_types::Generics {
                    params: Vec::new(),
                    where_predicates: Vec::new(),
                },
                impls: Vec::new(),
            }),
        };
        let unix_only = Item {
            id: Id(3),
            name: Some("UnixOnly".into()),
            attrs: vec![rustdoc_types::Attribute::Other(
                "#[<cfg>(all(unix, target_os = \"linux\"))]".into(),
            )],
            ..doc_only.clone()
        };
        let cfg_attr_only = Item {
            id: Id(4),
            name: Some("CfgAttrOnly".into()),
            attrs: vec![rustdoc_types::Attribute::Other(
                "#[<cfg_attr>(not(doc), cfg(windows))]".into(),
            )],
            ..doc_only.clone()
        };
        doc_only.id = Id(2);
        docs.index.insert(Id(2), doc_only);
        docs.index.insert(Id(3), unix_only);
        docs.index.insert(Id(4), cfg_attr_only);
        let ItemEnum::Module(root_module) = &mut docs.index.get_mut(&root).unwrap().inner else {
            panic!("crate root is a module");
        };
        root_module.items = vec![Id(2), Id(3), Id(4)];
        let cfg = RustcCfg::parse(b"unix\ntarget_os=\"linux\"\npanic=\"unwind\"\n");

        apply_non_doc_cfg(&mut docs, &cfg).unwrap();

        let ItemEnum::Module(root_module) = &docs.index[&root].inner else {
            panic!("crate root is a module");
        };
        assert_eq!(root_module.items, [Id(3)]);
        assert!(docs.index[&Id(2)].attrs.iter().any(|attribute| {
            matches!(attribute, rustdoc_types::Attribute::Other(attribute) if attribute == CFG_UNAVAILABLE_ATTRIBUTE)
        }));
        assert!(docs.index[&Id(4)].attrs.iter().any(|attribute| {
            matches!(attribute, rustdoc_types::Attribute::Other(attribute) if attribute == CFG_UNAVAILABLE_ATTRIBUTE)
        }));
    }

    #[test]
    fn retained_cfg_attr_derives_follow_the_captured_non_doc_cfg() {
        let mut attrs = vec![
            rustdoc_types::Attribute::Other(
                "#[<cfg_attr>(feature = \"enabled\", derive(Enabled, marker::Qualified))]".into(),
            ),
            rustdoc_types::Attribute::Other(
                "#[<cfg_attr>(feature = \"disabled\", derive(Disabled))]".into(),
            ),
            rustdoc_types::Attribute::Other(
                "#[<cfg_attr>(feature = \"enabled\", cfg_attr(unix, derive(Nested)))]".into(),
            ),
        ];
        let cfg = RustcCfg::parse(b"feature=\"enabled\"\nunix\n");

        reconcile_cfg_attr_semantics(&mut attrs, &cfg).unwrap();

        assert!(attrs.iter().any(|attribute| {
            matches!(
                attribute,
                rustdoc_types::Attribute::Other(attribute)
                    if attribute == "#[derive(Enabled, Nested, marker::Qualified)]"
            )
        }));
        assert!(!attrs.iter().any(|attribute| {
            matches!(
                attribute,
                rustdoc_types::Attribute::Other(attribute)
                    if attribute.starts_with("#[derive(") && attribute.contains("Disabled")
            )
        }));
    }

    #[test]
    fn unknown_cfg_predicates_never_become_false_or_true_under_negation() {
        let cfg = RustcCfg::parse(b"enabled\n");
        for expression in [
            "unsupported(\"argument\")",
            "not(unsupported())",
            "all(disabled, unsupported())",
            "any(enabled, unsupported())",
            "not(any(disabled, all(enabled, unsupported())))",
            "not()",
            "not(enabled, disabled)",
            "mode = 1",
            "mode = function()",
            "version()",
            "version(\"1.80\", \"1.90\")",
            "version(enabled)",
            "all(",
        ] {
            assert!(
                cfg_expression_matches(expression, &cfg).is_err(),
                "{expression}"
            );
        }
        assert!(
            cfg_expression_matches("version(\"1.80\")", &cfg)
                .unwrap_err()
                .contains("not captured from the selected wrapped compiler")
        );
        for (expression, expected) in [
            ("all()", true),
            ("any()", false),
            ("not(enabled)", false),
            ("all(enabled, not(disabled))", true),
            ("any(disabled, enabled)", true),
        ] {
            assert_eq!(cfg_expression_matches(expression, &cfg).unwrap(), expected);
        }
    }

    #[test]
    fn unknown_cfg_attr_conditions_propagate_through_filters_and_semantics() {
        let cfg = RustcCfg::parse(b"enabled\n");
        for expression in [
            "unsupported(), cfg(enabled)",
            "not(unsupported()), derive(Clone)",
            "enabled, cfg(not(unsupported()))",
            "enabled, cfg_attr(enabled, cfg(unsupported()))",
            "",
            "enabled,",
            "enabled, cfg_attr()",
            "enabled, cfg(invalid = 1)",
        ] {
            assert!(
                cfg_attr_expression_matches(expression, &cfg).is_err(),
                "{expression}"
            );
        }
        assert!(cfg_attr_expression_matches("disabled, cfg(unsupported())", &cfg).unwrap());
        let attrs = vec![rustdoc_types::Attribute::Other(
            "#[<cfg_attr>(enabled, cfg_attr(not(unsupported()), derive(Clone)))]".into(),
        )];
        assert!(cfg_attr_outputs(&attrs, &cfg).is_err());
        let malformed = vec![rustdoc_types::Attribute::Other("#[cfg_attr()]".into())];
        assert!(cfg_attr_outputs(&malformed, &cfg).is_err());
        assert!(cfg_attr_derive_difference(&attrs, &cfg).is_err());
        assert!(reconcile_cfg_attr_semantics(&mut attrs.clone(), &cfg).is_err());
        let mut docs = minimal_crate(None, FORMAT_VERSION);
        docs.index.get_mut(&docs.root).unwrap().attrs = attrs;
        assert!(
            reconcile_cfg_attr_deprecation(docs.index.get_mut(&docs.root).unwrap(), &cfg).is_err()
        );
        assert!(apply_non_doc_cfg(&mut docs, &cfg).is_err());
    }

    #[test]
    fn version_capture_collects_only_cfg_predicates_including_macro_templates() {
        let source = r#"
#![cfg_attr(version("1.80"), cfg(not(version("999.0"))))]
#[cfg(any(version("1.90"), all(not(version("1.91")), enabled)))] struct S;
#[cfg_attr(enabled, cfg_attr(version("1.92"), must_use), version("attribute"))] struct T;
#[other(version("unrelated"))] struct U;
macro_rules! generated { () => { #[cfg(version("1.93"))] struct Generated; } }
"#;
        let mut versions = HashSet::new();
        collect_source_cfg_versions(source.parse().unwrap(), &mut versions);
        assert_eq!(
            versions,
            HashSet::from(["1.80", "999.0", "1.90", "1.91", "1.92", "1.93"].map(String::from))
        );
    }

    #[test]
    fn version_probe_errors_are_not_false_cfg_results() {
        let directory = tempfile::tempdir().unwrap();
        let cfg_path = directory.path().join("dep.cfg");
        fs::write(
            cfg_path.with_extension("json"),
            serde_json::to_vec(&minimal_crate(None, FORMAT_VERSION)).unwrap(),
        )
        .unwrap();
        let source = directory.path().join("lib.rs");
        fs::write(&source, "#[cfg(version(\"1.80\"))] pub struct S;").unwrap();
        let arguments = vec![source.clone().into_os_string()];
        assert!(
            capture_version_cfg(&[], &arguments, &cfg_path, &source)
                .unwrap_err()
                .contains("selected compiler invocation is empty")
        );
        assert!(
            capture_version_cfg(
                &[directory.path().join("missing-rustc").into()],
                &arguments,
                &cfg_path,
                &source
            )
            .unwrap_err()
            .contains("failed to evaluate cfg(version) with selected compiler")
        );
        assert!(!cfg_path.with_extension("versions").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let wrapper = directory.path().join("skip-probe.sh");
            fs::write(&wrapper, "#!/bin/sh\nwhile IFS= read -r line; do :; done\n").unwrap();
            fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
            let error =
                capture_version_cfg(&[wrapper.into_os_string()], &arguments, &cfg_path, &source)
                    .unwrap_err();
            assert!(
                error.contains("did not produce the cfg(version) probe expansion"),
                "{error}"
            );
            assert!(!cfg_path.with_extension("versions").exists());
        }
    }

    #[test]
    fn version_results_remain_correct_when_nested_or_negated() {
        let mut cfg = RustcCfg::parse(b"enabled\n");
        cfg.version_matches
            .extend([("1.80".into(), true), ("999.0".into(), false)]);
        for (expression, expected) in [
            ("version(\"1.80\")", true),
            ("version(\"1.80\",)", true),
            ("version(\"999.0\")", false),
            ("not(version(\"1.80\"))", false),
            ("not(version(\"999.0\"))", true),
            (
                "all(enabled, any(version(\"999.0\"), version(\"1.80\")))",
                true,
            ),
        ] {
            assert_eq!(cfg_expression_matches(expression, &cfg).unwrap(), expected);
        }
        assert!(
            cfg_attr_expression_matches(
                "version(\"1.80\"), cfg_attr(not(version(\"999.0\")), cfg(enabled))",
                &cfg,
            )
            .unwrap()
        );
        let attrs = vec![rustdoc_types::Attribute::Other(
            "#[<cfg_attr>(version(\"1.80\"), cfg_attr(not(version(\"999.0\")), derive(Clone)))]"
                .into(),
        )];
        assert_eq!(
            derived_names(&cfg_attr_outputs(&attrs, &cfg).unwrap()),
            HashSet::from(["Clone".into()])
        );
    }

    #[test]
    fn cfg_lookup_normalizes_raw_identifiers_without_changing_rendered_paths() {
        let cfg = RustcCfg::parse("async\nmode=\"fast\"\ncafé\ncafé=\"oui\"\n".as_bytes());

        assert!(cfg_expression_matches("café", &cfg).unwrap());
        assert!(cfg_expression_matches("café = \"oui\"", &cfg).unwrap());

        assert!(cfg_expression_matches("r#async", &cfg).unwrap());
        assert!(cfg_expression_matches("r#mode = \"fast\"", &cfg).unwrap());
        let path = syn::parse_str::<syn::Path>("r#async::r#type").unwrap();
        assert_eq!(cfg_path(&path), "r#async::r#type");
        assert_eq!(cfg_lookup_path(&path), "async::type");

        let mut attrs = vec![rustdoc_types::Attribute::Other(
            "#[<cfg_attr>(r#async, derive(RawEnabled))]".into(),
        )];
        reconcile_cfg_attr_semantics(&mut attrs, &cfg).unwrap();
        assert!(attrs.iter().any(|attribute| {
            matches!(
                attribute,
                rustdoc_types::Attribute::Other(attribute)
                    if attribute == "#[derive(RawEnabled)]"
            )
        }));
    }

    #[test]
    fn configured_target_forms_follow_cargo_semantics() {
        assert_eq!(
            select_configured_targets(ConfiguredCargoTargets::One("host".into())).unwrap(),
            Some("host".into())
        );
        assert_eq!(
            select_configured_targets(ConfiguredCargoTargets::Many(vec!["host".into()])).unwrap(),
            Some("host".into())
        );
        assert_eq!(
            select_configured_targets(ConfiguredCargoTargets::Many(Vec::new())).unwrap(),
            None
        );
        let error = select_configured_targets(ConfiguredCargoTargets::Many(vec![
            "host".into(),
            "wasm32-unknown-unknown".into(),
        ]))
        .unwrap_err();
        assert!(error.contains("selects multiple targets (host, wasm32-unknown-unknown)"));
    }

    #[test]
    fn generation_directory_requires_and_refreshes_its_ownership_marker() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().join("excra");
        let first = reset_generation_target_dir(&root).unwrap();
        fs::write(first.join("stale"), "stale").unwrap();

        let second = reset_generation_target_dir(&root).unwrap();
        assert_eq!(first, second);
        assert!(!second.join("stale").exists());
        assert_eq!(
            fs::read_to_string(second.join(".excra-generation")).unwrap(),
            GENERATION_MARKER
        );

        fs::write(second.join(".excra-generation"), "not ours\n").unwrap();
        assert!(
            reset_generation_target_dir(&root)
                .unwrap_err()
                .contains("refusing to replace unowned")
        );
    }

    #[test]
    fn generation_directory_recovers_past_interrupted_staging_initialization() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().join("excra");
        fs::create_dir_all(&root).unwrap();
        let unmarked = root.join("generation.staging-0");
        fs::create_dir(&unmarked).unwrap();
        fs::write(unmarked.join("keep.txt"), "not owned\n").unwrap();
        let owned = root.join("generation.staging-1");
        fs::create_dir(&owned).unwrap();
        fs::write(owned.join(".excra-generation"), GENERATION_MARKER).unwrap();
        fs::write(owned.join("stale"), "stale\n").unwrap();

        let generation = reset_generation_target_dir(&root).unwrap();

        assert_eq!(
            fs::read_to_string(generation.join(".excra-generation")).unwrap(),
            GENERATION_MARKER
        );
        assert_eq!(
            fs::read_to_string(unmarked.join("keep.txt")).unwrap(),
            "not owned\n"
        );
        assert!(!owned.exists());
    }

    #[test]
    fn generation_directory_without_a_marker_remains_unowned() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().join("excra");
        let generation = root.join("generation");
        fs::create_dir_all(&generation).unwrap();
        fs::write(generation.join("keep.txt"), "keep me\n").unwrap();

        let error = reset_generation_target_dir(&root).unwrap_err();

        assert!(error.contains("refusing to replace unowned"));
        assert_eq!(
            fs::read_to_string(generation.join("keep.txt")).unwrap(),
            "keep me\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn generation_root_rejects_a_symlink_and_preserves_its_destination() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().unwrap();
        let target = temp.path().join("target");
        let victim = temp.path().join("victim");
        fs::create_dir(&target).unwrap();
        fs::create_dir(&victim).unwrap();
        fs::write(victim.join("keep.txt"), "keep me\n").unwrap();
        symlink(&victim, target.join("excra")).unwrap();

        let error = prepare_generation_root(&target).unwrap_err();

        assert!(error.contains("non-directory or symlink excra managed root"));
        assert_eq!(
            fs::read_to_string(victim.join("keep.txt")).unwrap(),
            "keep me\n"
        );
        assert!(
            fs::symlink_metadata(target.join("excra"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn generation_directory_rejects_a_symlink_ownership_marker() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path().join("excra");
        let generation = reset_generation_target_dir(&root).unwrap();
        let marker = generation.join(".excra-generation");
        let victim = temp.path().join("victim");
        fs::write(&victim, GENERATION_MARKER).unwrap();
        fs::remove_file(&marker).unwrap();
        symlink(&victim, &marker).unwrap();

        let error = reset_generation_target_dir(&root).unwrap_err();

        assert!(error.contains("ownership marker"));
        assert!(error.contains("not a regular non-symlink file"));
        assert_eq!(fs::read_to_string(&victim).unwrap(), GENERATION_MARKER);
        assert!(generation.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn configured_wrapper_uses_selected_toolchain_and_cargo_origin() {
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        let cargo_dir = temp.path().join(".cargo");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&cargo_dir).unwrap();
        let fake_cargo = temp.path().join("fake-cargo");
        let origin = cargo_dir.join("config.toml");
        fs::write(
            &fake_cargo,
            format!(
                "#!/bin/sh\nif [ \"$1\" != \"+alternate-nightly\" ]; then exit 41; fi\ncase \" $* \" in\n  *\" --show-origin \"*) printf '%s\\n' 'build.rustc-wrapper = \"./tools/wrapper\" # {}' ;;\n  *) printf '%s\\n' '{{\"build\":{{\"rustc-wrapper\":\"./tools/wrapper\"}}}}' ;;\nesac\n",
                origin.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_cargo).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_cargo, permissions).unwrap();

        let wrapper = configured_general_rustc_wrapper(
            &project.join("Cargo.toml"),
            "alternate-nightly",
            fake_cargo.as_os_str(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(wrapper, temp.path().join("tools/wrapper").into_os_string());
    }

    #[test]
    fn cargo_unit_selection_follows_contextual_dependency_edges() {
        fn unit(
            pkg_id: String,
            name: &str,
            kind: &str,
            features: &[&str],
            dependencies: &[usize],
        ) -> Unit {
            Unit {
                pkg_id,
                target: UnitTarget {
                    name: name.into(),
                    kind: vec![kind.into()],
                },
                mode: "build".into(),
                platform: None,
                features: features.iter().map(|feature| (*feature).into()).collect(),
                profile: serde_json::json!({"name": "test"}),
                dependencies: dependencies
                    .iter()
                    .map(|index| UnitDependency { index: *index })
                    .collect(),
            }
        }

        let metadata = metadata();
        let root = metadata.root_package().unwrap();
        let dependency = metadata
            .packages
            .iter()
            .find(|package| package.name == "cargo_metadata")
            .unwrap();
        let target = crate::resolver::library_target(dependency).unwrap();
        let graph = UnitGraph {
            version: 1,
            roots: vec![0],
            units: vec![
                unit(root.id.to_string(), "excra", "bin", &[], &[1, 2]),
                unit(
                    dependency.id.to_string(),
                    &target.name,
                    "lib",
                    &["dev-api"],
                    &[],
                ),
                unit(
                    root.id.to_string(),
                    "build-script-build",
                    "custom-build",
                    &[],
                    &[3],
                ),
                unit(
                    dependency.id.to_string(),
                    &target.name,
                    "lib",
                    &["build-api"],
                    &[],
                ),
            ],
        };
        let dev = DependencyContext {
            kind: DependencyKind::Development,
            target: None,
            via: None,
        };
        let build = DependencyContext {
            kind: DependencyKind::Build,
            target: None,
            via: None,
        };

        assert_eq!(
            units_for_context_edges(&graph, root, dependency, target, &dev, None),
            vec![1]
        );
        assert_eq!(
            units_for_context_edges(&graph, root, dependency, target, &build, None),
            vec![3]
        );

        let facade_one = "facade 1.0.0 (path+file:///one)".to_string();
        let facade_two = "facade 2.0.0 (path+file:///two)".to_string();
        let graph = UnitGraph {
            version: 1,
            roots: vec![0],
            units: vec![
                unit(root.id.to_string(), "excra", "bin", &[], &[1, 2]),
                unit(facade_one.clone(), "facade", "lib", &["one"], &[3]),
                unit(facade_two, "facade", "lib", &["two"], &[4]),
                unit(
                    dependency.id.to_string(),
                    &target.name,
                    "lib",
                    &["through-one"],
                    &[],
                ),
                unit(
                    dependency.id.to_string(),
                    &target.name,
                    "lib",
                    &["through-two"],
                    &[],
                ),
            ],
        };
        let transitive = DependencyContext {
            kind: DependencyKind::Development,
            target: None,
            via: Some("facade".into()),
        };
        let parent = CargoParentUnit {
            package_id: facade_one,
            graph_index: 1,
        };

        assert_eq!(
            units_for_context_edges(&graph, root, dependency, target, &transitive, Some(&parent),),
            vec![3]
        );
    }

    #[test]
    fn cargo_unit_selection_rejects_mixed_contexts() {
        let contexts = [
            DependencyContext {
                kind: DependencyKind::Normal,
                target: None,
                via: None,
            },
            DependencyContext {
                kind: DependencyKind::Development,
                target: None,
                via: None,
            },
        ];

        let error = exact_context_kind(&contexts).unwrap_err();
        assert!(error.contains("different Cargo units"));
        assert!(error.contains("normal, dev"));
    }

    #[test]
    fn lock_uses_an_empty_file_and_releases_exclusive_lock_on_drop() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock_path = dir.path().join("crate.json.lock");
        let first =
            JsonGenerationLock::acquire_with_timeout(lock_path.clone(), Duration::ZERO).unwrap();
        assert_eq!(fs::read_to_string(&lock_path).unwrap(), "");

        let err = JsonGenerationLock::acquire_with_timeout(lock_path.clone(), Duration::ZERO)
            .unwrap_err();
        assert_eq!(
            err,
            format!(
                "timed out waiting for the active rustdoc JSON lock {}",
                lock_path.display()
            )
        );

        drop(first);
        assert!(JsonGenerationLock::acquire_with_timeout(lock_path, Duration::ZERO).is_ok());
    }

    #[test]
    fn lock_preserves_existing_contents_when_no_owner_is_active() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock_path = dir.path().join("stale.json.lock");
        fs::write(&lock_path, "existing contents\n").unwrap();
        let _lock =
            JsonGenerationLock::acquire_with_timeout(lock_path.clone(), Duration::ZERO).unwrap();
        assert_eq!(
            fs::read_to_string(&lock_path).unwrap(),
            "existing contents\n"
        );
    }

    #[test]
    fn lock_waits_for_a_healthy_owner_to_release() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock_path = dir.path().join("concurrent.json.lock");
        let first = JsonGenerationLock::acquire(lock_path.clone()).unwrap();
        let holder = thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            drop(first);
        });

        let second = JsonGenerationLock::acquire(lock_path).unwrap();
        holder.join().unwrap();
        drop(second);
    }

    #[test]
    fn lock_rejects_non_regular_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let lock_path = dir.path().join("lock-directory");
        fs::create_dir(&lock_path).unwrap();
        let error = JsonGenerationLock::acquire_with_timeout(lock_path.clone(), Duration::ZERO)
            .unwrap_err();
        assert_eq!(error, invalid_lock_path_message(&lock_path));
    }

    #[cfg(unix)]
    #[test]
    fn lock_rejects_symlinks_without_modifying_the_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::TempDir::new().unwrap();
        let victim = dir.path().join("victim");
        let lock_path = dir.path().join("symlink.lock");
        fs::write(&victim, "keep me\n").unwrap();
        symlink(&victim, &lock_path).unwrap();

        let error = JsonGenerationLock::acquire_with_timeout(lock_path.clone(), Duration::ZERO)
            .unwrap_err();
        assert_eq!(error, invalid_lock_path_message(&lock_path));
        assert_eq!(fs::read_to_string(victim).unwrap(), "keep me\n");
    }

    #[test]
    fn relative_rustdoc_spans_are_normalized_against_invocation_directory() {
        let mut krate = minimal_crate(None, FORMAT_VERSION);
        krate.index.get_mut(&krate.root).unwrap().span = Some(rustdoc_types::Span {
            filename: PathBuf::from("dep/src/lib.rs"),
            begin: (7, 1),
            end: (7, 2),
        });
        normalize_span_paths(&mut krate, Path::new("/workspace"));
        assert_eq!(
            krate.index[&krate.root].span.as_ref().unwrap().filename,
            PathBuf::from("/workspace/dep/src/lib.rs")
        );
    }
}
