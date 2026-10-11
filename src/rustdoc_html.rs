use crate::imports::{ImportPath, NamespaceConstraint, identifier_key};
use crate::resolver::{ResolvedDependency, package_spec};
use cargo_metadata::{Metadata, Package};
use html_to_markdown_rs::visitor::{HtmlVisitor, NodeContext, VisitResult};
use html_to_markdown_rs::{CodeBlockStyle, ConversionOptions, HeadingStyle, convert};
use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

pub(crate) fn generate(
    manifest: &Path,
    metadata: &Metadata,
    target: Option<&str>,
    dependency: &ResolvedDependency<'_>,
) -> Result<PathBuf, String> {
    let package = dependency.package;
    let features = resolved_features(metadata, package);
    let lock = fs::read(metadata.workspace_root.join("Cargo.lock"))
        .map_err(|error| format!("cannot read workspace Cargo.lock: {error}"))?;
    let mut identity = std::collections::hash_map::DefaultHasher::new();
    package_spec(package).hash(&mut identity);
    package.manifest_path.hash(&mut identity);
    features.hash(&mut identity);
    target.hash(&mut identity);
    std::env::var_os("CARGO_BUILD_TARGET").hash(&mut identity);
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
    let generated_manifest = generation.join("Cargo.toml");
    if !generated_manifest.is_file() {
        // Cargo disallows feature flags for selected non-workspace packages.
        // An isolated manifest enables only this crate's resolved features.
        let source = package
            .manifest_path
            .parent()
            .expect("manifest has a parent");
        let contents = format!(
            "[package]\nname = \"excra-doc\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
             [lib]\npath = \"lib.rs\"\n\
             [workspace]\nresolver = \"3\"\n\
             [dependencies]\n\
             documented = {{ package = {:?}, path = {:?}, default-features = false, features = {features:?} }}\n",
            package.name,
            source.as_str()
        );
        fs::write(generation.join("Cargo.lock"), lock)
            .and_then(|()| fs::write(generation.join("lib.rs"), ""))
            .and_then(|()| fs::write(&generated_manifest, contents))
            .map_err(|error| format!("cannot prepare documentation manifest: {error}"))?;
    }
    let mut command = Command::new("cargo");
    command.current_dir(manifest.parent().expect("manifest has a parent"));
    command
        .args(["doc", "--offline", "--no-deps", "--lib", "--manifest-path"])
        .arg(&generated_manifest);
    command
        .args([
            "--package",
            &format!("{}@{}", package.name, package.version),
            "--target-dir",
        ])
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
    Ok(docs)
}

fn resolved_features(metadata: &Metadata, package: &Package) -> Vec<String> {
    metadata
        .resolve
        .as_ref()
        .and_then(|resolve| resolve.nodes.iter().find(|node| node.id == package.id))
        .map(|node| node.features.clone())
        .unwrap_or_default()
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
            if (index < import.segments.len()
                || import.namespace == Some(NamespaceConstraint::Type))
                && matches!(
                    kind,
                    "fn" | "constant" | "static" | "macro" | "attr" | "derive"
                )
            {
                continue;
            }
            let candidate = directory.join(format!("{kind}.{name}.html"));
            if candidate.is_file() && html.contains(&format!("href=\"{kind}.{name}.html\"")) {
                candidates.push(candidate);
            }
        }
        if candidates.is_empty()
            && let Some((_, reexport)) = html.split_once(&format!("id=\"reexport.{name}\""))
            && let Some(href) = reexport
                .split("</dt>")
                .next()
                .and_then(|item| attribute(item, "href"))
        {
            candidates.push(local_link(docs, directory, href)?);
        }
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

pub(crate) fn write_markdown(docs: &Path, page: &Path) -> Result<PathBuf, String> {
    let source = follow_redirect(docs, page)?;
    let markdown = page.with_extension("md");
    if let (Ok(html), Ok(md)) = (fs::metadata(&source), fs::metadata(&markdown))
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
    fs::write(&markdown, content)
        .map_err(|error| format!("cannot write {}: {error}", markdown.display()))?;
    Ok(markdown)
}

// Rustdoc links type names inside signatures. Keep their text inside code.
#[derive(Debug, Default)]
struct CodeLinks {
    depth: usize,
}

impl HtmlVisitor for CodeLinks {
    fn visit_element_start(&mut self, ctx: &NodeContext<'_>) -> VisitResult {
        if code_node(ctx) {
            self.depth += 1;
        }
        VisitResult::Continue
    }

    fn visit_element_end(&mut self, ctx: &NodeContext<'_>, _: &str) -> VisitResult {
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
            VisitResult::Custom(text.into())
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
