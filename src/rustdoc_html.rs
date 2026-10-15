use crate::cargo_graph::{self, FeatureGraph};
use crate::cli::FeatureSelection;
use crate::imports::{ImportPath, identifier_key};
use crate::resolver::{ResolvedDependency, package_spec};
use cargo_metadata::{Metadata, MetadataCommand, Package};
use html_to_markdown_rs::visitor::{HtmlVisitor, NodeContext, VisitResult};
use html_to_markdown_rs::{CodeBlockStyle, ConversionOptions, HeadingStyle, convert};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

pub(crate) struct Documentation {
    pub(crate) directory: PathBuf,
    _generation_lock: fs::File,
}

pub(crate) fn generate(
    manifest: &Path,
    metadata: &Metadata,
    target: Option<&str>,
    dependency: &ResolvedDependency<'_>,
    graph: &FeatureGraph,
    host_context: bool,
) -> Result<Documentation, String> {
    let package = dependency.package;
    let configuration = cargo_configuration(manifest)?;
    let targeted = target.is_some()
        || std::env::var_os("CARGO_BUILD_TARGET").is_some()
        || configuration.iter().any(|(_, contents)| {
            contents
                .parse::<toml::Table>()
                .ok()
                .and_then(|config| config.get("build")?.get("target").cloned())
                .is_some_and(|target| match target {
                    toml::Value::Array(targets) => !targets.is_empty(),
                    _ => true,
                })
        });
    let host = if host_context && targeted {
        Some(cargo_graph::host_triple(manifest, metadata)?)
    } else {
        None
    };
    let target = host.as_deref().or(target);
    let suppress_target_flags = host_context && targeted;
    let effective_target = cargo_graph::platform(manifest, metadata, target)?;
    let contents = documentation_manifest(metadata, graph, package, host_context)?;
    let lock = fs::read(metadata.workspace_root.join("Cargo.lock"))
        .map_err(|error| format!("cannot read workspace Cargo.lock: {error}"))?;
    let mut identity = std::collections::hash_map::DefaultHasher::new();
    // Do not reuse Markdown produced by an older conversion implementation.
    (env!("CARGO_PKG_VERSION"), "markdown-v2").hash(&mut identity);
    package_spec(package).hash(&mut identity);
    package.manifest_path.hash(&mut identity);
    contents.hash(&mut identity);
    target.hash(&mut identity);
    effective_target.name.hash(&mut identity);
    targeted.hash(&mut identity);
    suppress_target_flags.hash(&mut identity);
    for name in [
        "CARGO_BUILD_TARGET",
        "RUSTFLAGS",
        "RUSTDOCFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_ENCODED_RUSTDOCFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_RUSTDOCFLAGS",
        "RUSTC",
        "RUSTDOC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC",
        "CARGO_BUILD_RUSTDOC",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "RUSTUP_TOOLCHAIN",
    ] {
        name.hash(&mut identity);
        std::env::var_os(name).hash(&mut identity);
    }
    let mut target_flags = std::env::vars_os()
        .filter(|(name, _)| {
            let name = name.to_string_lossy();
            name.starts_with("CARGO_TARGET_") && name.ends_with("_RUSTFLAGS")
        })
        .collect::<Vec<_>>();
    target_flags.sort();
    target_flags.hash(&mut identity);
    lock.hash(&mut identity);
    configuration.hash(&mut identity);
    let generation = metadata.target_directory.join("excra").join(format!(
        "docs-{}-{:016x}",
        package.name,
        identity.finish()
    ));
    fs::create_dir_all(&generation)
        .map_err(|error| format!("cannot create {generation}: {error}"))?;
    let generation_lock = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(generation.join(".lock"))
        .map_err(|error| format!("cannot open documentation cache lock: {error}"))?;
    generation_lock
        .lock()
        .map_err(|error| format!("cannot lock documentation cache: {error}"))?;
    let generated_manifest = generation.join("Cargo.toml");
    if fs::read_to_string(&generated_manifest).ok().as_deref() != Some(&contents) {
        atomic_write(generation.join("Cargo.lock").as_std_path(), |file| {
            file.write_all(&lock)
        })
        .and_then(|()| atomic_write(generation.join("lib.rs").as_std_path(), |_| Ok(())))
        .and_then(|()| {
            atomic_write(generated_manifest.as_std_path(), |file| {
                file.write_all(contents.as_bytes())
            })
        })
        .map_err(|error| format!("cannot prepare documentation manifest: {error}"))?;
    }
    // The copied lockfile pins the original registry versions and Git revisions.
    let generated_metadata = MetadataCommand::new()
        .manifest_path(&generated_manifest)
        .current_dir(manifest.parent().expect("manifest has a parent"))
        .other_options(vec!["--offline".into()])
        .exec()
        .map_err(|error| format!("cannot resolve documentation manifest: {error}"))?;
    let documented = generated_metadata
        .packages
        .iter()
        .find(|candidate| candidate.id == package.id)
        .ok_or_else(|| {
            format!(
                "documented package {} missing from isolated graph",
                package.id
            )
        })?;
    let generated_graph = cargo_graph::features(
        manifest,
        &generated_metadata,
        generated_metadata
            .root_package()
            .expect("documentation manifest has a root package"),
        &FeatureSelection::default(),
        target,
        false,
    )?;
    let expected = graph
        .closure((package.id.clone(), host_context))?
        .into_iter()
        .map(|unit| {
            (
                unit.package.clone(),
                unit.host,
                unit.features.clone(),
                !targeted || !unit.host,
            )
        })
        .collect::<HashSet<_>>();
    let proc_macro = dependency
        .target
        .kind
        .iter()
        .any(|kind| kind == "proc-macro");
    let actual = generated_graph
        .closure((documented.id.clone(), proc_macro))?
        .into_iter()
        .map(|unit| {
            (
                unit.package.clone(),
                host_context || unit.host,
                unit.features.clone(),
                !suppress_target_flags && (!targeted || !unit.host),
            )
        })
        .collect::<HashSet<_>>();
    if actual != expected {
        return Err(format!(
            "documentation graph changed Cargo package, host, feature, or compiler flag context for {}",
            package.id
        ));
    }
    let mut command = Command::new("cargo");
    command.current_dir(manifest.parent().expect("manifest has a parent"));
    command
        .args([
            "doc",
            "--offline",
            "--locked",
            "--no-deps",
            "--lib",
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(&generated_manifest);
    command
        .args(["--package", &package_spec(documented), "--target-dir"])
        .arg(generation.join("build"));
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    if suppress_target_flags {
        // The selected host package is a target unit in the isolated manifest.
        // An empty encoded value overrides every source of target Rust flags.
        command.env("CARGO_ENCODED_RUSTFLAGS", "");
    }
    let output = command
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("cannot run cargo doc: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo doc failed for {} {}",
            package.name, package.version
        ));
    }
    let docs = documentation_dir(
        &String::from_utf8_lossy(&output.stdout),
        &documented.id.repr,
    )?;
    convert_crate(&docs, &docs.join(dependency.target.name.replace('-', "_")))?;
    Ok(Documentation {
        directory: docs,
        _generation_lock: generation_lock,
    })
}

fn cargo_configuration(manifest: &Path) -> Result<Vec<(PathBuf, String)>, String> {
    let mut directories = manifest
        .parent()
        .expect("manifest has a parent")
        .ancestors()
        .map(|directory| directory.join(".cargo"))
        .collect::<Vec<_>>();
    if let Some(home) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
    {
        directories.push(home);
    }
    let mut configuration = Vec::new();
    let mut visited = HashSet::new();
    for directory in directories {
        let legacy = directory.join("config");
        let path = if legacy.is_file() {
            legacy
        } else {
            directory.join("config.toml")
        };
        read_configuration(&path, true, &mut visited, &mut configuration)?;
    }
    Ok(configuration)
}

fn read_configuration(
    path: &Path,
    optional: bool,
    visited: &mut HashSet<PathBuf>,
    configuration: &mut Vec<(PathBuf, String)>,
) -> Result<(), String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if optional && error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let canonical = path.canonicalize().map_err(|error| error.to_string())?;
    if !visited.insert(canonical) {
        return Ok(());
    }
    let table = contents
        .parse::<toml::Table>()
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    if let Some(includes) = table.get("include").and_then(toml::Value::as_array) {
        for include in includes {
            let name = include
                .as_str()
                .or_else(|| include.get("path").and_then(toml::Value::as_str))
                .ok_or_else(|| {
                    format!("invalid Cargo configuration include in {}", path.display())
                })?;
            let optional = include
                .get("optional")
                .and_then(toml::Value::as_bool)
                .unwrap_or(false);
            read_configuration(
                &path
                    .parent()
                    .expect("configuration has a parent")
                    .join(name),
                optional,
                visited,
                configuration,
            )?;
        }
    }
    configuration.push((path.to_path_buf(), contents));
    Ok(())
}

fn documentation_manifest(
    metadata: &Metadata,
    graph: &FeatureGraph,
    package: &Package,
    host: bool,
) -> Result<String, String> {
    let units = graph.closure((package.id.clone(), host))?;
    let selected = units
        .iter()
        .find(|unit| unit.package == package.id && unit.host == host)
        .expect("selected unit is in closure");
    let mut manifest = format!(
        "[package]\nname = \"excra-doc\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         [lib]\npath = \"lib.rs\"\n[workspace]\nresolver = {}\n[dependencies]\n\
         documented = {{ package = {}, {}, default-features = false, features = {} }}\n",
        quoted(&resolver_version(metadata)?),
        quoted(&package.name),
        dependency_source(package)?,
        serde_json::to_string(&selected.features).expect("feature names serialize"),
    );
    for build in [false, true] {
        if build {
            manifest.push_str("[build-dependencies]\n");
        }
        for unit in &units {
            if unit.package == package.id && unit.host == host
                || if build {
                    !unit.host
                } else {
                    !host && unit.host
                }
            {
                continue;
            }
            let dependency = metadata
                .packages
                .iter()
                .find(|package| package.id == unit.package)
                .ok_or_else(|| {
                    format!(
                        "feature context package {} missing from metadata",
                        unit.package
                    )
                })?;
            // Cargo requires one alias per source identity across dependency kinds.
            let alias = if unit.package == package.id {
                "documented".to_string()
            } else {
                format!(
                    "node_{}",
                    units
                        .iter()
                        .position(|other| other.package == unit.package)
                        .expect("unit is in closure")
                )
            };
            let source = dependency_source(dependency)?;
            manifest.push_str(&format!(
                "{alias} = {{ package = {}, {source}, default-features = false, features = {} }}\n",
                quoted(&dependency.name),
                serde_json::to_string(&unit.features).expect("feature names serialize"),
            ));
        }
    }
    Ok(manifest)
}

fn resolver_version(metadata: &Metadata) -> Result<String, String> {
    let manifest = metadata.workspace_root.join("Cargo.toml");
    let contents = fs::read_to_string(&manifest)
        .map_err(|error| format!("cannot read workspace manifest: {error}"))?;
    let manifest_table = contents
        .parse::<toml::Table>()
        .map_err(|error| format!("cannot parse workspace manifest: {error}"))?;
    for section in ["workspace", "package"] {
        if let Some(resolver) = manifest_table
            .get(section)
            .and_then(|section| section.get("resolver"))
            .and_then(toml::Value::as_str)
        {
            return Ok(resolver.to_string());
        }
    }
    let edition = metadata
        .packages
        .iter()
        .find(|package| package.manifest_path == manifest)
        .map(|package| package.edition.as_str());
    Ok(match edition {
        Some("2024") => "3",
        Some("2021") => "2",
        _ => "1",
    }
    .to_string())
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}

fn dependency_source(package: &Package) -> Result<String, String> {
    let Some(source) = &package.source else {
        return Ok(format!(
            "path = {}",
            quoted(
                package
                    .manifest_path
                    .parent()
                    .expect("manifest has a parent")
                    .as_str()
            )
        ));
    };
    let version = format!("version = {}", quoted(&format!("={}", package.version)));
    if let Some(index) = source.repr.strip_prefix("registry+").or_else(|| {
        source
            .repr
            .starts_with("sparse+")
            .then_some(source.repr.as_str())
    }) {
        return Ok(format!("{version}, registry-index = {}", quoted(index)));
    }
    if let Some(git) = source.repr.strip_prefix("git+") {
        let mut url =
            url::Url::parse(git).map_err(|error| format!("invalid Cargo git source: {error}"))?;
        let query = url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        url.set_fragment(None);
        url.set_query(None);
        let mut source = format!("{version}, git = {}", quoted(url.as_str()));
        for (key, value) in query {
            if !matches!(key.as_str(), "branch" | "tag" | "rev") {
                return Err(format!("unsupported Cargo git source parameter: {key}"));
            }
            source.push_str(&format!(", {key} = {}", quoted(&value)));
        }
        return Ok(source);
    }
    Err(format!("unsupported Cargo source: {source}"))
}

fn convert_crate(docs: &Path, directory: &Path) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        if path.is_dir() {
            convert_crate(docs, &path)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "html")
        {
            write_markdown(docs, &path)?;
        }
    }
    Ok(())
}

fn documentation_dir(output: &str, package: &str) -> Result<PathBuf, String> {
    let mut directories = Vec::new();
    for line in output.lines() {
        let artifact: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("cannot read Cargo documentation artifact: {error}"))?;
        if artifact["reason"] != "compiler-artifact" || artifact["package_id"] != package {
            continue;
        }
        if let Some(files) = artifact["filenames"].as_array() {
            for path in files.iter().filter_map(serde_json::Value::as_str) {
                let path = Path::new(path);
                if path.file_name().is_some_and(|name| name == "index.html") {
                    directories.push(
                        path.parent()
                            .and_then(Path::parent)
                            .ok_or_else(|| {
                                format!("invalid documentation artifact: {}", path.display())
                            })?
                            .canonicalize()
                            .map_err(|error| error.to_string())?,
                    );
                }
            }
        }
    }
    directories.sort();
    directories.dedup();
    match directories.as_slice() {
        [docs] => Ok(docs.clone()),
        _ => Err("expected one rustdoc HTML output directory; use --target for one target".into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct HtmlItem {
    page: PathBuf,
    fragment: Option<String>,
}

#[derive(PartialEq, Eq, Hash)]
enum ItemIdentity {
    Page(HtmlItem),
    Source(HtmlItem, String, Option<String>),
}

fn item_identity(docs: &Path, item: &HtmlItem) -> Result<ItemIdentity, String> {
    let html = read_html(&item.page)?;
    let directory = item.page.parent().expect("page has a parent");
    let kind = page_kind(&item.page).unwrap_or_default();
    // Inlined nominal types keep their canonical self-type links in auto impls.
    if let Some((_, implementations)) = html.split_once("id=\"synthetic-implementations-list\"") {
        for link in implementations
            .split("</div>")
            .next()
            .unwrap_or_default()
            .split("<a ")
            .skip(1)
        {
            let tag = link.split('>').next().unwrap_or_default();
            if attribute(tag, "class") == Some(kind)
                && let Some(href) = attribute(tag, "href")
                && let Ok(mut definition) = local_link(docs, directory, href)
            {
                definition.fragment = item.fragment.clone();
                return Ok(ItemIdentity::Page(definition));
            }
        }
    }
    // Trait implementation scripts likewise belong to the definition's path.
    if kind == "trait" {
        for script in html.split("<script ").skip(1) {
            let tag = script.split('>').next().unwrap_or_default();
            if let Some(src) = attribute(tag, "src")
                && (src.contains("/trait.impl/") || src.contains("/implementors/"))
                && let Ok(definition) = local_link(docs, directory, src)
            {
                return Ok(ItemIdentity::Page(definition));
            }
        }
    }
    for link in html.split("<a ").skip(1) {
        let tag = link.split('>').next().unwrap_or_default();
        if attribute(tag, "class") == Some("src")
            && let Some(href) = attribute(tag, "href")
            && let Ok(source) = local_link(docs, directory, href)
            && unique_source_definition(&source, kind) == Some(true)
        {
            return Ok(ItemIdentity::Source(
                source,
                kind.to_string(),
                item.fragment.clone(),
            ));
        }
    }
    Ok(ItemIdentity::Page(item.clone()))
}

fn unique_source_definition(source: &HtmlItem, kind: &str) -> Option<bool> {
    let range = source.fragment.as_deref()?;
    let (start, end) = range.split_once('-').unwrap_or((range, range));
    let start = start.parse::<usize>().ok()?;
    let end = end.parse::<usize>().ok()?;
    let html = read_html(&source.page).ok()?;
    let code = html.split_once("<code>")?.1.split_once("</code>")?.0;
    let snippet = code
        .lines()
        .skip(start.checked_sub(1)?)
        .take(end.checked_sub(start)?.checked_add(1)?)
        .map(|line| line.split_once("</a>").map_or(line, |(_, code)| code))
        .collect::<Vec<_>>()
        .join("\n");
    let markdown = convert(
        &format!("<pre><code>{snippet}</code></pre>"),
        ConversionOptions {
            code_block_style: CodeBlockStyle::Backticks,
            visitor: Some(Arc::new(Mutex::new(CodeLinks::default()))),
            ..Default::default()
        },
    )
    .ok()?
    .content?;
    let code = markdown.trim().strip_prefix("```")?.split_once('\n')?.1;
    let code = code.strip_suffix("```")?;
    let syntax = syn::parse_file(code).ok()?;
    Some(source_definition_count(&syntax.items, kind) == 1)
}

fn source_definition_count(items: &[syn::Item], kind: &str) -> usize {
    items
        .iter()
        .map(|item| {
            let matches = matches!(
                (kind, item),
                ("constant", syn::Item::Const(_))
                    | ("enum", syn::Item::Enum(_))
                    | ("fn" | "attr" | "derive", syn::Item::Fn(_))
                    | ("index", syn::Item::Mod(_))
                    | ("static", syn::Item::Static(_))
                    | ("struct", syn::Item::Struct(_))
                    | ("trait", syn::Item::Trait(_))
                    | ("traitalias", syn::Item::TraitAlias(_))
                    | ("type", syn::Item::Type(_))
                    | ("union", syn::Item::Union(_))
            ) || matches!((kind, item), ("macro", syn::Item::Macro(item)) if item.ident.is_some());
            let nested = if let syn::Item::Mod(module) = item {
                module
                    .content
                    .as_ref()
                    .map_or(0, |(_, items)| source_definition_count(items, kind))
            } else {
                0
            };
            usize::from(matches) + nested
        })
        .sum()
}

impl HtmlItem {
    fn new(page: PathBuf) -> Self {
        Self {
            page,
            fragment: None,
        }
    }

    fn kind(&self) -> Option<&str> {
        self.fragment
            .as_deref()
            .and_then(|fragment| fragment.split('.').next())
            .or_else(|| page_kind(&self.page))
    }
}

pub(crate) fn find_page(
    docs: &Path,
    crate_name: &str,
    import: &ImportPath,
) -> Result<PathBuf, String> {
    let mut lookup = HtmlLookup::new(docs);
    let mut item = HtmlItem::new(docs.join(crate_name).join("index.html"));
    for (index, name) in import
        .segments
        .iter()
        .chain(std::iter::once(&import.item))
        .enumerate()
    {
        item = lookup.follow_redirect(&item)?;
        let name = identifier_key(name);
        if item.fragment.is_some() || !matches!(item.kind(), Some("index" | "enum")) {
            return Err(format!(
                "item '{}' has no rustdoc HTML page",
                import.full_path()
            ));
        }
        let type_only = index < import.segments.len() || import.self_parent;
        let candidates = lookup.candidates(&item, name, type_only)?;
        item = match candidates.as_slice() {
            [item] => item.clone(),
            [] => {
                return Err(format!(
                    "item '{}' not found in rustdoc HTML",
                    import.full_path()
                ));
            }
            _ => {
                return Err(format!(
                    "ambiguous item '{}' has multiple rustdoc HTML matches",
                    import.full_path()
                ));
            }
        };
    }
    let item = lookup.follow_redirect(&item)?;
    if import.self_parent && !matches!(item.kind(), Some("index" | "enum" | "trait")) {
        return Err(format!(
            "item '{}' cannot be a self import parent; expected a module, enum, or trait",
            import.full_path()
        ));
    }
    Ok(item.page)
}

struct ParsedPage {
    html: String,
    links: HashSet<String>,
    reexports: Vec<(String, String)>,
    globs: Vec<String>,
}

struct Bindings {
    explicit: Vec<(HtmlItem, u8)>,
    globs: Vec<String>,
}

impl ParsedPage {
    fn new(html: String) -> Self {
        let links = html
            .split("href=\"")
            .skip(1)
            .filter_map(|tail| tail.split('"').next().map(str::to_string))
            .collect();
        let mut reexports = Vec::new();
        let mut globs = Vec::new();
        for row in html.split("<dt").skip(1) {
            if !row.starts_with([' ', '>']) {
                continue;
            }
            let row = row.split("</dt>").next().unwrap_or(row);
            let Some(href) = attribute(row, "href") else {
                continue;
            };
            if row.contains("::*;") {
                globs.push(href.to_string());
            } else if let Some(id) = attribute(row, "id") {
                reexports.push((id.to_string(), href.to_string()));
            }
        }
        Self {
            html,
            links,
            reexports,
            globs,
        }
    }
}

struct HtmlLookup<'a> {
    docs: &'a Path,
    pages: HashMap<PathBuf, ParsedPage>,
    completed: HashMap<(HtmlItem, String, bool), Result<Vec<HtmlItem>, String>>,
    #[cfg(test)]
    expansions: usize,
}

impl<'a> HtmlLookup<'a> {
    fn new(docs: &'a Path) -> Self {
        Self {
            docs,
            pages: HashMap::new(),
            completed: HashMap::new(),
            #[cfg(test)]
            expansions: 0,
        }
    }

    fn page(&mut self, path: &Path) -> Result<&ParsedPage, String> {
        if !self.pages.contains_key(path) {
            self.pages
                .insert(path.to_path_buf(), ParsedPage::new(read_html(path)?));
        }
        Ok(&self.pages[path])
    }

    fn follow_redirect(&mut self, item: &HtmlItem) -> Result<HtmlItem, String> {
        let mut item = item.clone();
        let mut visited = HashSet::new();
        loop {
            let page = self.page(&item.page)?;
            if page.html.contains("<main") {
                return Ok(item);
            }
            if !visited.insert(item.page.clone()) {
                return Err("cycle in rustdoc HTML redirects".into());
            }
            let href = page
                .html
                .split_once("URL=")
                .and_then(|(_, tail)| tail.split('"').next())
                .ok_or_else(|| format!("{} is not a rustdoc HTML page", item.page.display()))?
                .to_string();
            let next = local_link(
                self.docs,
                item.page.parent().expect("page has a parent"),
                &href,
            )?;
            item = HtmlItem {
                page: next.page,
                fragment: next.fragment.or(item.fragment),
            };
        }
    }

    fn candidates(
        &mut self,
        item: &HtmlItem,
        name: &str,
        type_only: bool,
    ) -> Result<Vec<HtmlItem>, String> {
        let item = self.follow_redirect(item)?;
        let key = (item.clone(), name.to_string(), type_only);
        if let Some(result) = self.completed.get(&key) {
            return result.clone();
        }
        let result = self
            .collect_candidates(&item, name, type_only)
            .and_then(|mut items| {
                items.sort();
                if items.len() > 1 {
                    let mut identities = HashSet::new();
                    let mut distinct = Vec::new();
                    for item in items {
                        if identities.insert(item_identity(self.docs, &item)?) {
                            distinct.push(item);
                        }
                    }
                    items = distinct;
                }
                Ok(items)
            });
        self.completed.insert(key, result.clone());
        result
    }

    fn collect_candidates(
        &mut self,
        root: &HtmlItem,
        name: &str,
        type_only: bool,
    ) -> Result<Vec<HtmlItem>, String> {
        let mut pending = vec![(root.clone(), if type_only { 1 } else { 1 | 2 | 4 })];
        let mut visited: HashMap<PathBuf, u8> = HashMap::new();
        let mut candidates = HashSet::new();
        let mut unavailable = None;
        while let Some((item, namespaces)) = pending.pop() {
            let item = match self.follow_redirect(&item) {
                Ok(item) => item,
                Err(error) => {
                    unavailable.get_or_insert(error);
                    continue;
                }
            };
            if item.fragment.is_some() {
                continue;
            }
            let seen = visited.entry(item.page.clone()).or_default();
            let namespaces = namespaces & !*seen;
            if namespaces == 0 {
                continue;
            }
            *seen |= namespaces;
            #[cfg(test)]
            {
                self.expansions += 1;
            }
            let Bindings { explicit, globs } = match self.bindings(&item, name, type_only) {
                Ok(bindings) => bindings,
                Err(error) if item == *root => return Err(error),
                Err(error) => {
                    unavailable.get_or_insert(error);
                    continue;
                }
            };
            let mut shadowed = 0;
            for (item, binding_namespaces) in explicit {
                shadowed |= binding_namespaces;
                if binding_namespaces & namespaces != 0 {
                    candidates.insert(item);
                }
            }
            let remaining = namespaces & !shadowed;
            if remaining == 0 {
                continue;
            }
            for href in globs {
                match local_link(
                    self.docs,
                    item.page.parent().expect("page has a parent"),
                    &href,
                ) {
                    Ok(page) => pending.push((page, remaining)),
                    Err(error) => {
                        unavailable.get_or_insert(error);
                    }
                }
            }
        }
        if candidates.is_empty()
            && let Some(error) = unavailable
        {
            return Err(error);
        }
        Ok(candidates.into_iter().collect())
    }

    fn bindings(
        &mut self,
        item: &HtmlItem,
        name: &str,
        type_only: bool,
    ) -> Result<Bindings, String> {
        let docs = self.docs;
        let page = self.page(&item.page)?;
        let directory = item.page.parent().expect("page has a parent");
        let mut candidates = Vec::new();
        if page_kind(&item.page) == Some("enum") {
            if page.html.contains(&format!("id=\"variant.{name}\"")) {
                candidates.push(HtmlItem {
                    page: item.page.clone(),
                    fragment: Some(format!("variant.{name}")),
                });
            }
        } else {
            let module = directory.join(name).join("index.html");
            if module.is_file() && page.links.contains(&format!("{name}/index.html")) {
                candidates.push(HtmlItem::new(module));
            }
            for kind in [
                "struct",
                "enum",
                "union",
                "trait",
                "type",
                "traitalias",
                "fn",
                "constant",
                "static",
                "macro",
                "attr",
                "derive",
                "primitive",
            ] {
                if type_only && !type_kind(kind) {
                    continue;
                }
                let file = format!("{kind}.{name}.html");
                let candidate = directory.join(&file);
                if candidate.is_file() && page.links.contains(&file) {
                    candidates.push(HtmlItem::new(candidate));
                }
            }
            let reexport_id = format!("reexport.{name}");
            for (id, href) in &page.reexports {
                if id != &reexport_id
                    && !id
                        .strip_prefix(&format!("{reexport_id}-"))
                        .is_some_and(|suffix| {
                            !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                        })
                {
                    continue;
                }
                let kind = page_kind(Path::new(href.split('#').next().unwrap_or(href)));
                if type_only && kind.is_some_and(|kind| !type_kind(kind)) {
                    continue;
                }
                candidates.push(local_link(docs, directory, href)?);
            }
        }
        let globs = page.globs.clone();
        let mut bindings = Vec::new();
        for candidate in candidates {
            let candidate = self.follow_redirect(&candidate)?;
            if !type_only || candidate.kind().is_some_and(type_kind) {
                let namespaces = if matches!(candidate.kind(), Some("struct" | "variant")) {
                    item_namespaces(&candidate, &self.page(&candidate.page)?.html)
                } else {
                    item_namespaces(&candidate, "")
                };
                bindings.push((candidate, namespaces));
            }
        }
        Ok(Bindings {
            explicit: bindings,
            globs,
        })
    }
}

fn item_namespaces(item: &HtmlItem, html: &str) -> u8 {
    match item.kind() {
        Some("struct" | "variant") => {
            let declaration = html
                .split_once("<pre class=\"rust item-decl\">")
                .and_then(|(_, declaration)| declaration.split_once("</pre>"))
                .map(|(declaration, _)| declaration)
                .unwrap_or_default();
            let constructor = if let Some(name) = item
                .fragment
                .as_deref()
                .and_then(|fragment| fragment.strip_prefix("variant."))
            {
                !declaration.contains(&format!("\n    {name} {{"))
                    && !declaration.contains(&format!("\n    r#{name} {{"))
            } else {
                declaration.trim_end().ends_with(";</code>")
            };
            // Unit and tuple constructors also occupy the value namespace.
            if constructor { 1 | 2 } else { 1 }
        }
        Some(kind) if type_kind(kind) => 1,
        Some("macro" | "attr" | "derive") => 4,
        _ => 2,
    }
}

fn page_kind(page: &Path) -> Option<&str> {
    page.file_name()?.to_str()?.split('.').next()
}

fn type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "index"
            | "struct"
            | "enum"
            | "union"
            | "trait"
            | "type"
            | "traitalias"
            | "primitive"
            | "variant"
    )
}

fn follow_redirect(docs: &Path, page: &Path) -> Result<PathBuf, String> {
    Ok(follow_item_redirect(docs, &HtmlItem::new(page.to_path_buf()))?.page)
}

fn follow_item_redirect(docs: &Path, item: &HtmlItem) -> Result<HtmlItem, String> {
    let mut item = item.clone();
    let mut visited = HashSet::new();
    loop {
        let html = read_html(&item.page)?;
        if html.contains("<main") {
            return Ok(item);
        }
        if !visited.insert(item.page.clone()) {
            return Err("cycle in rustdoc HTML redirects".into());
        }
        let href = html
            .split_once("URL=")
            .and_then(|(_, tail)| tail.split('"').next())
            .ok_or_else(|| format!("{} is not a rustdoc HTML page", item.page.display()))?;
        let next = local_link(docs, item.page.parent().expect("page has a parent"), href)?;
        item = HtmlItem {
            page: next.page,
            fragment: next.fragment.or(item.fragment),
        };
    }
}

fn attribute<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    html.split_once(&format!("{name}=\""))?.1.split('"').next()
}

fn local_link(docs: &Path, directory: &Path, href: &str) -> Result<HtmlItem, String> {
    let (path, fragment) = match href.split_once('#') {
        Some((path, fragment)) => (path, Some(fragment.to_string())),
        None => (href, None),
    };
    let path = directory.join(path);
    let path = path
        .canonicalize()
        .map_err(|error| format!("rustdoc link '{href}' is unavailable: {error}"))?;
    if !path.starts_with(docs) {
        return Err(format!(
            "rustdoc link '{href}' leaves the local documentation"
        ));
    }
    Ok(HtmlItem {
        page: path,
        fragment,
    })
}

fn read_html(page: &Path) -> Result<String, String> {
    fs::read_to_string(page)
        .map_err(|error| format!("cannot read rustdoc HTML {}: {error}", page.display()))
}

fn atomic_write(
    path: &Path,
    write: impl FnOnce(&mut fs::File) -> io::Result<()>,
) -> io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().expect("file has a parent"))?;
    write(temporary.as_file_mut())?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn write_markdown(docs: &Path, page: &Path) -> Result<PathBuf, String> {
    let source = follow_redirect(docs, page)?;
    let markdown = page.with_extension("md");
    if let (Ok(html), Ok(md)) = (fs::metadata(&source), fs::metadata(&markdown))
        && md.is_file()
        && md.len() > 0
        && let (Ok(html), Ok(md)) = (html.modified(), md.modified())
        && md >= html
    {
        return Ok(markdown);
    }
    let html = read_html(&source)?;
    let main = html
        .split_once("<main>")
        .and_then(|(_, main)| main.rsplit_once("</main>").map(|(content, _)| content))
        .ok_or_else(|| format!("{} has no rustdoc main content", page.display()))?;
    let options = ConversionOptions {
        heading_style: HeadingStyle::Atx,
        code_block_style: CodeBlockStyle::Backticks,
        code_language: "rust".into(),
        extract_metadata: false,
        visitor: Some(Arc::new(Mutex::new(CodeLinks::default()))),
        exclude_selectors: [
            "nav",
            "rustdoc-topbar",
            ".sidebar-resizer",
            ".skip-main-content",
            "button",
            "rustdoc-toolbar",
            "a.anchor",
            ".hideme",
        ]
        .into_iter()
        .map(str::to_string)
        .collect(),
        ..Default::default()
    };
    let content = convert(main, options)
        .map_err(|error| format!("cannot convert {}: {error}", page.display()))?
        .content
        .ok_or_else(|| format!("no Markdown returned for {}", page.display()))?;
    if content.trim().is_empty() {
        return Err(format!("empty Markdown returned for {}", page.display()));
    }
    atomic_write(&markdown, |file| file.write_all(content.as_bytes()))
        .map_err(|error| format!("cannot write {}: {error}", markdown.display()))?;
    Ok(markdown)
}

// Rustdoc links type names inside signatures. Keep their text inside code.
#[derive(Debug, Default)]
struct CodeLinks {
    depth: usize,
    link_text: Option<String>,
}

impl HtmlVisitor for CodeLinks {
    fn visit_heading(
        &mut self,
        ctx: &NodeContext<'_>,
        level: u32,
        text: &str,
        _: Option<&str>,
    ) -> VisitResult {
        if code_node(ctx) {
            let delimiter = "`".repeat(
                text.split(|character| character != '`')
                    .map(str::len)
                    .max()
                    .unwrap_or(0)
                    + 1,
            );
            VisitResult::Custom(format!(
                "\n\n{} {delimiter}{text}{delimiter}\n\n",
                "#".repeat(level as usize)
            ))
        } else {
            VisitResult::Continue
        }
    }

    fn visit_element_start(&mut self, ctx: &NodeContext<'_>) -> VisitResult {
        if code_node(ctx) {
            self.depth += 1;
        }
        if self.depth > 0 && ctx.tag_name == "a" {
            self.link_text = Some(String::new());
        }
        VisitResult::Continue
    }

    fn visit_text(&mut self, _: &NodeContext<'_>, text: &str) -> VisitResult {
        if let Some(link_text) = &mut self.link_text {
            link_text.push_str(text);
        }
        VisitResult::Continue
    }

    fn visit_element_end(&mut self, ctx: &NodeContext<'_>, _: &str) -> VisitResult {
        if ctx.tag_name == "a" {
            self.link_text = None;
        }
        if code_node(ctx) {
            self.depth -= 1;
        }
        VisitResult::Continue
    }

    fn visit_link(
        &mut self,
        _: &NodeContext<'_>,
        _: &str,
        text: &str,
        _: Option<&str>,
    ) -> VisitResult {
        if self.depth > 0 {
            VisitResult::Custom(self.link_text.take().unwrap_or_else(|| text.into()))
        } else {
            VisitResult::Continue
        }
    }
}

fn code_node(ctx: &NodeContext<'_>) -> bool {
    matches!(ctx.tag_name.as_ref(), "pre" | "code")
        || ctx.attributes().get("class").is_some_and(|classes| {
            classes
                .split_whitespace()
                .any(|class| class == "code-header")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_glob_graphs_expand_each_page_once_and_reuse_completed_lookups() {
        for levels in [15, 30, 60] {
            let dir = tempfile::TempDir::new().unwrap();
            let docs = dir.path();
            let write_module = |name: &str, body: String| {
                let directory = docs.join(name);
                fs::create_dir_all(&directory).unwrap();
                fs::write(directory.join("index.html"), format!("<main>{body}</main>")).unwrap();
            };
            let glob = |href: &str| format!("<dt><a href=\"{href}\">module</a>::*;</dt>");
            write_module(
                "leaf",
                "<a href=\"struct.Item.html\">Item</a>".to_string() + &glob("../index.html"),
            );
            let definition = docs.join("leaf/struct.Item.html");
            fs::write(
                &definition,
                "<main><pre class=\"rust item-decl\"><code>pub struct Item;</code></pre></main>",
            )
            .unwrap();
            for level in 0..levels {
                let previous = if level == 0 {
                    vec!["leaf".to_string()]
                } else {
                    vec![format!("a{}", level - 1), format!("b{}", level - 1)]
                };
                let exports = previous
                    .iter()
                    .map(|name| glob(&format!("../{name}/index.html")))
                    .collect::<String>();
                for side in ["a", "b"] {
                    write_module(&format!("{side}{level}"), exports.clone());
                }
            }
            write_module(
                "",
                glob(&format!("a{}/index.html", levels - 1))
                    + &glob(&format!("b{}/index.html", levels - 1)),
            );
            let root = HtmlItem::new(docs.join("index.html"));
            let mut lookup = HtmlLookup::new(docs);
            let result = lookup.candidates(&root, "Item", false).unwrap();
            assert_eq!(result, vec![HtmlItem::new(definition)]);
            assert_eq!(lookup.expansions, 2 * levels + 2);
            let parsed = lookup.pages.len();
            assert_eq!(parsed, 2 * levels + 3);
            assert_eq!(lookup.candidates(&root, "Item", false).unwrap(), result);
            assert_eq!(lookup.expansions, 2 * levels + 2);
            assert_eq!(lookup.candidates(&root, "Item", true).unwrap(), result);
            assert_eq!(lookup.expansions, 2 * (2 * levels + 2));
            assert!(
                lookup
                    .candidates(&root, "Missing", false)
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(lookup.expansions, 3 * (2 * levels + 2));
            assert_eq!(lookup.pages.len(), parsed);
        }
    }

    #[test]
    fn variant_fragments_survive_rustdoc_redirects() {
        let dir = tempfile::TempDir::new().unwrap();
        let docs = dir.path();
        let page = docs.join("enum.E.html");
        fs::write(&page, "<main></main>").unwrap();
        let redirect = docs.join("enum.Alias.html");
        for (href, destination) in [
            ("enum.Alias.html", "enum.E.html#variant.First"),
            ("enum.Alias.html#variant.First", "enum.E.html"),
        ] {
            fs::write(&redirect, format!("<meta content=\"URL={destination}\">")).unwrap();
            let item = local_link(docs, docs, href).unwrap();
            let item = follow_item_redirect(docs, &item).unwrap();
            assert_eq!(item.page, page);
            assert_eq!(item.fragment.as_deref(), Some("variant.First"));
            assert_eq!(item.kind(), Some("variant"));
        }
    }

    #[test]
    fn readers_keep_the_generation_lock_during_source_regeneration() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("api")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"consumer\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"lib.rs\"\n[dependencies]\napi = { path = \"api\" }\n[workspace]\nresolver = \"3\"\n",
        )
        .unwrap();
        fs::write(root.join("lib.rs"), "").unwrap();
        fs::write(
            root.join("api/Cargo.toml"),
            "[package]\nname = \"api\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"lib.rs\"\n",
        )
        .unwrap();
        let source = root.join("api/lib.rs");
        fs::write(&source, "pub fn item(_: u8) {}").unwrap();
        let manifest = root.join("Cargo.toml");
        let metadata = MetadataCommand::new()
            .manifest_path(&manifest)
            .other_options(vec!["--offline".into()])
            .exec()
            .unwrap();
        let graph = cargo_graph::features(
            &manifest,
            &metadata,
            metadata.root_package().unwrap(),
            &FeatureSelection::default(),
            None,
            false,
        )
        .unwrap();
        let package = metadata
            .packages
            .iter()
            .find(|package| package.name == "api")
            .unwrap();
        let dependency = ResolvedDependency {
            package,
            target: crate::resolver::library_target(package).unwrap(),
            contexts: Vec::new(),
        };
        let docs = generate(&manifest, &metadata, None, &dependency, &graph, false).unwrap();
        let import = crate::imports::parse_use_lines("use api::item;")
            .unwrap()
            .remove(0);
        let page = find_page(&docs.directory, "api", &import).unwrap();
        let lock = docs
            .directory
            .ancestors()
            .find(|path| path.join("Cargo.toml").is_file())
            .unwrap()
            .join(".lock");
        fs::write(&source, "pub fn item(_: u16) {}").unwrap();
        let (ready, waiting) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| {
                let contender = fs::File::options()
                    .read(true)
                    .write(true)
                    .open(&lock)
                    .unwrap();
                assert!(contender.try_lock().is_err(), "reader released its lock");
                ready.send(()).unwrap();
                let next =
                    generate(&manifest, &metadata, None, &dependency, &graph, false).unwrap();
                assert_eq!(next.directory, page.parent().unwrap().parent().unwrap());
                fs::read_to_string(page.with_extension("md")).unwrap()
            });
            waiting
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap();
            let content = fs::read_to_string(page.with_extension("md")).unwrap();
            assert!(content.contains("pub fn item(_: u8)"), "{content}");
            drop(docs);
            let content = writer.join().unwrap();
            assert!(content.contains("pub fn item(_: u16)"), "{content}");
        });
    }

    #[test]
    fn failed_atomic_writes_leave_no_partial_cache_or_replace_valid_content() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("item.md");
        for previous in [None, Some("previous valid Markdown")] {
            if let Some(previous) = previous {
                fs::write(&path, previous).unwrap();
            }
            let error = atomic_write(&path, |file| {
                file.write_all(b"partial")?;
                Err(io::Error::other("simulated storage failure"))
            });
            assert!(error.is_err());
            assert_eq!(fs::read_to_string(&path).ok().as_deref(), previous);
            atomic_write(&path, |file| file.write_all(b"complete Markdown")).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "complete Markdown");
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn damaged_html_and_failed_publication_do_not_create_valid_markdown() {
        let dir = tempfile::TempDir::new().unwrap();
        let docs = dir.path();
        let page = docs.join("struct.Item.html");
        assert!(
            write_markdown(docs, &page)
                .unwrap_err()
                .contains("cannot read rustdoc HTML")
        );
        for (html, message) in [
            ("invalid HTML", "is not a rustdoc HTML page"),
            ("<main>", "has no rustdoc main content"),
            ("<main></main>", "empty Markdown returned"),
            (
                "<meta content=\"URL=struct.Item.html\">",
                "cycle in rustdoc HTML redirects",
            ),
            ("<meta content=\"URL=missing.html\">", "is unavailable"),
        ] {
            fs::write(&page, html).unwrap();
            let error = write_markdown(docs, &page).unwrap_err();
            assert!(error.contains(message), "{error}");
            assert!(!page.with_extension("md").exists());
        }
        let outside = tempfile::NamedTempFile::new().unwrap();
        assert!(
            local_link(docs, docs, outside.path().to_str().unwrap())
                .unwrap_err()
                .contains("leaves the local documentation")
        );
        fs::write(&page, "<main><pre>pub struct Item;</pre></main>").unwrap();
        fs::create_dir(page.with_extension("md")).unwrap();
        assert!(
            write_markdown(docs, &page)
                .unwrap_err()
                .contains("cannot write")
        );
        assert!(page.with_extension("md").is_dir());
        assert!(
            convert_crate(docs, &docs.join("missing"))
                .unwrap_err()
                .contains("cannot read")
        );
        assert!(documentation_dir("", "missing").is_err());
    }

    #[test]
    fn resolver_defaults_follow_workspace_root_edition_and_virtual_workspaces() {
        let dir = tempfile::TempDir::new().unwrap();
        let manifest = dir.path().join("Cargo.toml");
        fs::write(dir.path().join("lib.rs"), "").unwrap();
        for (edition, resolver) in [("2018", "1"), ("2021", "2"), ("2024", "3")] {
            fs::write(&manifest, format!(
                "[package]\nname = \"root\"\nversion = \"0.0.0\"\nedition = \"{edition}\"\n[lib]\npath = \"lib.rs\"\n[workspace]\n"
            )).unwrap();
            let mut metadata = MetadataCommand::new()
                .manifest_path(&manifest)
                .no_deps()
                .exec()
                .unwrap();
            assert_eq!(resolver_version(&metadata).unwrap(), resolver);
            metadata.packages.clear();
            assert_eq!(resolver_version(&metadata).unwrap(), "1");
            fs::write(&manifest, "[broken").unwrap();
            assert!(
                resolver_version(&metadata)
                    .unwrap_err()
                    .contains("cannot parse workspace manifest")
            );
            metadata.workspace_root = dir.path().join("missing").to_str().unwrap().into();
            assert!(
                resolver_version(&metadata)
                    .unwrap_err()
                    .contains("cannot read workspace manifest")
            );
        }
    }
}
