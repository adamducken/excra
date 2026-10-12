use crate::cargo_graph::{self, FeatureGraph};
use crate::cli::FeatureSelection;
use crate::imports::{ImportPath, NamespaceConstraint, identifier_key};
use crate::resolver::{ResolvedDependency, package_spec};
use cargo_metadata::{Metadata, MetadataCommand, Package};
use html_to_markdown_rs::visitor::{HtmlVisitor, NodeContext, VisitResult};
use html_to_markdown_rs::{CodeBlockStyle, ConversionOptions, HeadingStyle, convert};
use std::collections::HashSet;
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
    let mut configuration = manifest
        .parent()
        .expect("manifest has a parent")
        .ancestors()
        .map(|directory| directory.join(".cargo"))
        .collect::<Vec<_>>();
    if let Some(home) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
    {
        configuration.push(home);
    }
    for directory in configuration {
        for name in ["config", "config.toml"] {
            let path = directory.join(name);
            if let Ok(contents) = fs::read(&path) {
                path.hash(&mut identity);
                contents.hash(&mut identity);
            }
        }
    }
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
        .map(|unit| (unit.package.clone(), unit.host, unit.features.clone()))
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
            )
        })
        .collect::<HashSet<_>>();
    if actual != expected {
        return Err(format!(
            "documentation graph changed Cargo package, host, or feature context for {}",
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
            "--manifest-path",
        ])
        .arg(&generated_manifest);
    command
        .args(["--package", &package_spec(documented), "--target-dir"])
        .arg(generation.join("build"));
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    let status = command
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|error| format!("cannot run cargo doc: {error}"))?;
    if !status.success() {
        return Err(format!(
            "cargo doc failed for {} {}",
            package.name, package.version
        ));
    }
    let docs = documentation_dir(generation.join("build").as_std_path())?;
    convert_crate(&docs, &docs.join(dependency.target.name.replace('-', "_")))?;
    Ok(Documentation {
        directory: docs,
        _generation_lock: generation_lock,
    })
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

fn documentation_dir(generation: &Path) -> Result<PathBuf, String> {
    let generation = generation
        .canonicalize()
        .map_err(|error| error.to_string())?;
    if generation.join("doc").is_dir() {
        return Ok(generation.join("doc"));
    }
    let entries = fs::read_dir(&generation)
        .map_err(|error| format!("cannot read {}: {error}", generation.display()))?;
    let mut directories = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let docs = entry.path().join("doc");
        if docs.is_dir() {
            directories.push(docs);
        }
    }
    match directories.as_slice() {
        [docs] => Ok(docs.clone()),
        _ => Err("expected one rustdoc HTML output directory; use --target for one target".into()),
    }
}

pub(crate) fn find_page(
    docs: &Path,
    crate_name: &str,
    import: &ImportPath,
) -> Result<PathBuf, String> {
    let mut page = docs.join(crate_name).join("index.html");
    for (index, name) in import
        .segments
        .iter()
        .chain(std::iter::once(&import.item))
        .enumerate()
    {
        page = follow_redirect(docs, &page)?;
        let html = read_html(&page)?;
        let name = identifier_key(name);
        // Variants live on their enum's page.
        if page
            .file_name()
            .is_some_and(|file| file.to_string_lossy().starts_with("enum."))
            && index == import.segments.len()
            && html.contains(&format!("id=\"variant.{name}\""))
        {
            return Ok(page);
        }
        if page.file_name().is_none_or(|file| file != "index.html") {
            return Err(format!(
                "item '{}' has no rustdoc HTML page",
                import.full_path()
            ));
        }
        let directory = page.parent().expect("page has a parent");
        let type_only =
            index < import.segments.len() || import.namespace == Some(NamespaceConstraint::Type);
        let mut candidates = Vec::new();
        let module = directory.join(name).join("index.html");
        if module.is_file() && html.contains(&format!("href=\"{name}/index.html\"")) {
            candidates.push(module);
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
            let candidate = directory.join(format!("{kind}.{name}.html"));
            if candidate.is_file() && html.contains(&format!("href=\"{kind}.{name}.html\"")) {
                candidates.push(candidate);
            }
        }
        let reexport_id = format!("reexport.{name}");
        for row in html.split("<dt ").skip(1) {
            let row = row.split("</dt>").next().unwrap_or(row);
            let Some(id) = attribute(row, "id") else {
                continue;
            };
            if id != reexport_id
                && !id
                    .strip_prefix(&format!("{reexport_id}-"))
                    .is_some_and(|suffix| {
                        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                    })
            {
                continue;
            }
            if let Some(href) = attribute(row, "href") {
                candidates.push(local_link(docs, directory, href)?);
            }
        }
        let mut candidates = candidates
            .iter()
            .map(|candidate| follow_redirect(docs, candidate))
            .collect::<Result<Vec<_>, _>>()?;
        candidates.retain(|candidate| {
            !type_only
                || candidate
                    .file_name()
                    .and_then(|file| file.to_str())
                    .and_then(|file| file.split('.').next())
                    .is_some_and(type_kind)
        });
        candidates.sort();
        candidates.dedup();
        page = match candidates.as_slice() {
            [page] => page.clone(),
            [] => {
                return Err(format!(
                    "item '{}' not found in rustdoc HTML",
                    import.full_path()
                ));
            }
            _ => {
                return Err(format!(
                    "ambiguous item '{}' has multiple rustdoc HTML pages",
                    import.full_path()
                ));
            }
        };
    }
    follow_redirect(docs, &page)
}

fn type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "index" | "struct" | "enum" | "union" | "trait" | "type" | "traitalias" | "primitive"
    )
}

fn follow_redirect(docs: &Path, page: &Path) -> Result<PathBuf, String> {
    let mut page = page.to_path_buf();
    let mut visited = HashSet::new();
    loop {
        let html = read_html(&page)?;
        if html.contains("<main") {
            return Ok(page);
        }
        if !visited.insert(page.clone()) {
            return Err("cycle in rustdoc HTML redirects".into());
        }
        let href = html
            .split_once("URL=")
            .and_then(|(_, tail)| tail.split('"').next())
            .ok_or_else(|| format!("{} is not a rustdoc HTML page", page.display()))?;
        page = local_link(docs, page.parent().expect("page has a parent"), href)?;
    }
}

fn attribute<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    html.split_once(&format!("{name}=\""))?.1.split('"').next()
}

fn local_link(docs: &Path, directory: &Path, href: &str) -> Result<PathBuf, String> {
    let path = directory.join(href.split('#').next().unwrap_or(href));
    let path = path
        .canonicalize()
        .map_err(|error| format!("rustdoc link '{href}' is unavailable: {error}"))?;
    if !path.starts_with(docs) {
        return Err(format!(
            "rustdoc link '{href}' leaves the local documentation"
        ));
    }
    Ok(path)
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
        assert!(documentation_dir(&docs.join("missing")).is_err());
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
