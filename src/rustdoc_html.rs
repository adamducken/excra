use crate::cargo_graph::{self, FeatureGraph};
use crate::cli::FeatureSelection;
use crate::imports::{ImportPath, identifier_key};
use crate::resolver::{ResolvedDependency, library_target, package_spec};
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, NodeDep, Package, PackageId};
use cargo_platform::{Cfg, CfgExpr};
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
    external_crates: Vec<ExternalCrate>,
    source_context: SourceContext,
    _generation_lock: fs::File,
}

struct ExternalCrate {
    dependency: NodeDep,
    source: PathBuf,
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
    let configuration = cargo_graph::cargo_configuration(manifest)?;
    let mut compiler = crate::compiler::Context::new(manifest, metadata, &configuration)?;
    let targeted = cargo_graph::targeted(target, &configuration);
    let host = if host_context && targeted {
        Some(cargo_graph::host_triple(manifest, metadata)?)
    } else {
        None
    };
    let target = host.as_deref().or(target);
    let suppress_target_flags = host_context && targeted;
    compiler.suppress_target_flags = suppress_target_flags;
    let effective_target =
        cargo_graph::platform_with_context(manifest, metadata, target, &compiler)?;
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
    compiler.hash(&mut identity);
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
        .cargo_path("cargo")
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
    let generated_graph = cargo_graph::features_with_context(
        manifest,
        &generated_metadata,
        generated_metadata
            .root_package()
            .expect("documentation manifest has a root package"),
        &FeatureSelection::default(),
        target,
        false,
        &compiler,
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
                compiler.workspace_enabled() && metadata.workspace_members.contains(&unit.package),
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
                generated_metadata
                    .packages
                    .iter()
                    .find(|package| package.id == unit.package)
                    .is_some_and(|package| compiler.wraps(package)),
            )
        })
        .collect::<HashSet<_>>();
    if actual != expected {
        return Err(format!(
            "documentation graph changed Cargo package, host, feature, or compiler flag context (including workspace wrappers) for {}",
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
    compiler.apply(&mut command);
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
        source_context: SourceContext::new(
            effective_target.cfg,
            &graph
                .closure((package.id.clone(), host_context))?
                .into_iter()
                .find(|unit| unit.package == package.id && unit.host == host_context)
                .expect("documented unit is in closure")
                .features,
            package,
            &configuration,
        ),
        external_crates: generated_metadata
            .resolve
            .as_ref()
            .and_then(|resolve| resolve.nodes.iter().find(|node| node.id == documented.id))
            .map(|node| {
                node.deps
                    .iter()
                    .filter_map(|dependency| {
                        let package = generated_metadata
                            .packages
                            .iter()
                            .find(|package| package.id == dependency.pkg)?;
                        Some(ExternalCrate {
                            dependency: dependency.clone(),
                            source: library_target(package)
                                .ok()?
                                .src_path
                                .clone()
                                .into_std_path_buf(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
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

#[derive(Clone, PartialEq, Eq, Hash)]
enum ItemIdentity {
    Page(HtmlItem),
    External {
        package: PackageId,
        path: Vec<String>,
        kind: String,
        fragment: Option<String>,
    },
}

fn item_identity(
    docs: &Path,
    external_crates: &[ExternalCrate],
    context: &SourceContext,
    item: &HtmlItem,
    visited: &mut HashSet<HtmlItem>,
) -> Result<ItemIdentity, String> {
    if !visited.insert(item.clone()) {
        return Ok(ItemIdentity::Page(item.clone()));
    }
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
    if let Some(definition) = declared_reexport(docs, external_crates, context, item) {
        return match definition {
            ItemIdentity::Page(definition) => {
                item_identity(docs, external_crates, context, &definition, visited)
            }
            external => Ok(external),
        };
    }
    Ok(ItemIdentity::Page(item.clone()))
}

fn source_link(docs: &Path, page: &Path) -> Option<HtmlItem> {
    let html = read_html(page).ok()?;
    for link in html.split("<a ").skip(1) {
        let tag = link.split('>').next().unwrap_or_default();
        if attribute(tag, "class") == Some("src")
            && let Some(href) = attribute(tag, "href")
            && let Ok(source) = local_link(docs, page.parent()?, href)
        {
            return Some(source);
        }
    }
    None
}

fn source_file(page: &Path) -> Option<syn::File> {
    let html = read_html(page).ok()?;
    let code = html.split_once("<code>")?.1.split_once("</code>")?.0;
    let snippet = code
        .lines()
        .map(|line| line.split_once("</a>").map_or(line, |(_, code)| code))
        .collect::<Vec<_>>()
        .join("\n");
    syn::parse_file(&rendered_code(&snippet)?).ok()
}

fn rendered_code(html: &str) -> Option<String> {
    let markdown = convert(
        &format!("<pre><code>{html}</code></pre>"),
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
    Some(code.to_string())
}

#[derive(Default)]
struct SourceContext {
    cfg: Option<Vec<Cfg>>,
    edition_2015: bool,
}

impl SourceContext {
    fn new(
        mut cfg: Vec<Cfg>,
        features: &[String],
        package: &Package,
        configuration: &[(PathBuf, String)],
    ) -> Self {
        // The platform probe supplies compiler cfg, not arbitrary rustdoc or
        // build-script cfg. Such contexts must remain unknown to this pass.
        let custom = package
            .targets
            .iter()
            .any(|target| target.kind.iter().any(|kind| kind == "custom-build"))
            || std::env::vars_os().any(|(name, value)| {
                if value.is_empty() {
                    return false;
                }
                let name = name.to_string_lossy();
                matches!(
                    name.as_ref(),
                    "RUSTC"
                        | "RUSTDOC"
                        | "RUSTC_WRAPPER"
                        | "RUSTC_WORKSPACE_WRAPPER"
                        | "CARGO_BUILD_RUSTC"
                        | "CARGO_BUILD_RUSTDOC"
                        | "CARGO_BUILD_RUSTC_WRAPPER"
                        | "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"
                ) || ((matches!(name.as_ref(), "RUSTFLAGS" | "RUSTDOCFLAGS")
                    || name.ends_with("_RUSTFLAGS")
                    || name.ends_with("_RUSTDOCFLAGS"))
                    && flags_change_cfg(&value.to_string_lossy()))
            })
            || configuration.iter().any(|(_, contents)| {
                let Ok(table) = contents.parse::<toml::Table>() else {
                    return true;
                };
                let build = table.get("build");
                [
                    "rustc",
                    "rustdoc",
                    "rustc-wrapper",
                    "rustc-workspace-wrapper",
                ]
                .iter()
                .any(|key| {
                    build
                        .and_then(|build| build.get(key))
                        .and_then(toml::Value::as_str)
                        .is_some_and(|value| !value.is_empty())
                }) || build.is_some_and(config_flags_change_cfg)
                    || table
                        .get("target")
                        .and_then(toml::Value::as_table)
                        .is_some_and(|targets| targets.values().any(config_flags_change_cfg))
            });
        let cfg = if custom {
            None
        } else {
            cfg.push(Cfg::Name("doc".into()));
            cfg.extend(
                features
                    .iter()
                    .map(|feature| Cfg::KeyPair("feature".into(), feature.clone())),
            );
            Some(cfg)
        };
        Self {
            cfg,
            edition_2015: package.edition.as_str() == "2015",
        }
    }

    fn matches(&self, expression: &CfgExpr) -> Option<bool> {
        match expression {
            CfgExpr::Value(value) => {
                let name = match value {
                    Cfg::Name(name) | Cfg::KeyPair(name, _) => name,
                };
                modeled_cfg(name)
                    .then(|| self.cfg.as_ref().map(|cfg| cfg.contains(value)))
                    .flatten()
            }
            CfgExpr::Not(expression) => self.matches(expression).map(|value| !value),
            CfgExpr::All(expressions) | CfgExpr::Any(expressions) => {
                let all = matches!(expression, CfgExpr::All(_));
                let mut result = Some(all);
                for expression in expressions {
                    match self.matches(expression) {
                        Some(value) if value != all => return Some(value),
                        None => result = None,
                        _ => {}
                    }
                }
                result
            }
        }
    }

    fn enabled(&self, attributes: &[syn::Attribute]) -> Option<bool> {
        let mut result = Some(true);
        for attribute in attributes
            .iter()
            .filter(|attribute| attribute.path().is_ident("cfg"))
        {
            let enabled = match &attribute.meta {
                syn::Meta::List(list) => list
                    .tokens
                    .to_string()
                    .parse::<CfgExpr>()
                    .ok()
                    .and_then(|expression| self.matches(&expression)),
                _ => None,
            };
            match enabled {
                Some(false) => return Some(false),
                None => result = None,
                _ => {}
            }
        }
        result
    }

    fn active_items(&self, items: Vec<syn::Item>) -> Vec<syn::Item> {
        items
            .into_iter()
            .filter(|item| {
                let attrs = match item {
                    syn::Item::Macro(item) => Some(item.attrs.as_slice()),
                    _ => item_attributes(item),
                };
                attrs.is_none_or(|attrs| self.enabled(attrs) != Some(false))
            })
            .map(|mut item| {
                if let syn::Item::ForeignMod(block) = &mut item {
                    block.items.retain(|item| {
                        foreign_attributes(item)
                            .is_none_or(|attrs| self.enabled(attrs) != Some(false))
                    });
                }
                item
            })
            .collect()
    }

    fn complete_items(&self, items: &[syn::Item]) -> bool {
        plain_items(items)
            && items.iter().all(|item| {
                item_attributes(item).is_some_and(|attrs| self.enabled(attrs) == Some(true))
                    && match item {
                        syn::Item::ForeignMod(block) => block.items.iter().all(|item| {
                            foreign_attributes(item)
                                .is_some_and(|attrs| self.enabled(attrs) == Some(true))
                        }),
                        _ => true,
                    }
            })
    }
}

fn modeled_cfg(name: &str) -> bool {
    matches!(
        name,
        "doc"
            | "unix"
            | "windows"
            | "feature"
            | "target_arch"
            | "target_os"
            | "target_env"
            | "target_vendor"
            | "target_family"
            | "target_endian"
            | "target_pointer_width"
    )
}

fn flags_change_cfg(flags: &str) -> bool {
    flags.contains("--target")
        || flags.contains('@')
        || (flags.contains("--cfg")
            && flags
                .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
                .any(modeled_cfg))
}

fn config_flags_change_cfg(section: &toml::Value) -> bool {
    ["rustflags", "rustdocflags"].iter().any(|key| {
        section.get(key).is_some_and(|flags| match flags {
            toml::Value::String(flags) => flags_change_cfg(flags),
            toml::Value::Array(flags) => flags
                .iter()
                .map(toml::Value::as_str)
                .collect::<Option<Vec<_>>>()
                .is_none_or(|flags| flags_change_cfg(&flags.join("\x1f"))),
            _ => true,
        })
    })
}

fn plain_attributes(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().all(|attribute| {
        [
            "doc",
            "cfg",
            "allow",
            "warn",
            "deny",
            "forbid",
            "deprecated",
            "path",
            "macro_export",
            "no_std",
            "no_main",
            "recursion_limit",
            "type_length_limit",
            "expect",
            "inline",
            "cold",
            "must_use",
            "repr",
            "non_exhaustive",
            "derive",
        ]
        .iter()
        .any(|name| attribute.path().is_ident(name))
    })
}

fn item_attributes(item: &syn::Item) -> Option<&[syn::Attribute]> {
    Some(match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::ForeignMod(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) if item.ident.is_some() => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::TraitAlias(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        // Macro expansion can introduce bindings absent from this source.
        _ => return None,
    })
}

fn plain_items(items: &[syn::Item]) -> bool {
    items.iter().all(|item| {
        item_attributes(item).is_some_and(|attrs| {
            // Derives retain the annotated declaration, but an imported
            // procedural derive can introduce additional bindings.
            plain_attributes(attrs)
                && !attrs
                    .iter()
                    .any(|attribute| attribute.path().is_ident("derive"))
                && match item {
                    syn::Item::ForeignMod(block) => block.items.iter().all(|item| {
                        foreign_binding(item).is_some()
                            && foreign_attributes(item).is_some_and(plain_attributes)
                    }),
                    _ => true,
                }
        })
    })
}

fn foreign_attributes(item: &syn::ForeignItem) -> Option<&[syn::Attribute]> {
    Some(match item {
        syn::ForeignItem::Fn(item) => &item.attrs,
        syn::ForeignItem::Static(item) => &item.attrs,
        syn::ForeignItem::Type(item) => &item.attrs,
        syn::ForeignItem::Macro(item) => &item.attrs,
        _ => return None,
    })
}

fn foreign_binding(item: &syn::ForeignItem) -> Option<(&syn::Ident, u8, &syn::Visibility)> {
    Some(match item {
        syn::ForeignItem::Fn(item) => (&item.sig.ident, 2, &item.vis),
        syn::ForeignItem::Static(item) => (&item.ident, 2, &item.vis),
        syn::ForeignItem::Type(item) => (&item.ident, 1, &item.vis),
        _ => return None,
    })
}

fn unconditional_attributes(attributes: &[syn::Attribute]) -> bool {
    plain_attributes(attributes)
        && !attributes
            .iter()
            .any(|attribute| attribute.path().is_ident("cfg"))
}

fn declaration(item: &syn::Item) -> Option<(&syn::Ident, &'static str)> {
    Some(match item {
        syn::Item::Const(item) => (&item.ident, "constant"),
        syn::Item::Enum(item) => (&item.ident, "enum"),
        syn::Item::Fn(item) => (&item.sig.ident, "fn"),
        syn::Item::Macro(item) => (item.ident.as_ref()?, "macro"),
        syn::Item::Mod(item) => (&item.ident, "index"),
        syn::Item::Static(item) => (&item.ident, "static"),
        syn::Item::Struct(item) => (&item.ident, "struct"),
        syn::Item::Trait(item) => (&item.ident, "trait"),
        syn::Item::TraitAlias(item) => (&item.ident, "traitalias"),
        syn::Item::Type(item) => (&item.ident, "type"),
        syn::Item::Union(item) => (&item.ident, "union"),
        _ => return None,
    })
}

// Without dependency HTML, only unconditional declarations can prove that an
// external re-export contributes to this page's namespace and kind.
fn external_has_kind(source: &Path, path: &[String], kind: &str) -> Option<bool> {
    let syntax = syn::parse_file(&fs::read_to_string(source).ok()?).ok()?;
    if !unconditional_attributes(&syntax.attrs) {
        return None;
    }
    let mut items = syntax.items;
    let mut directory = source.parent()?.to_path_buf();
    let (name, parents) = path.split_last()?;
    for name in parents {
        let mut modules = items.into_iter().filter_map(|item| match item {
            syn::Item::Mod(module) if identifier_key(&module.ident.to_string()) == *name => {
                Some(module)
            }
            _ => None,
        });
        let module = modules.next()?;
        if modules.next().is_some()
            || !unconditional_attributes(&module.attrs)
            || module
                .attrs
                .iter()
                .any(|attribute| attribute.path().is_ident("path"))
        {
            return None;
        }
        if let Some((_, contents)) = module.content {
            items = contents;
        } else {
            let file = directory.join(format!("{name}.rs"));
            let nested = directory.join(name).join("mod.rs");
            let file = match (file.is_file(), nested.is_file()) {
                (true, false) => file,
                (false, true) => nested,
                _ => return None,
            };
            let syntax = syn::parse_file(&fs::read_to_string(file).ok()?).ok()?;
            if !unconditional_attributes(&syntax.attrs) {
                return None;
            }
            items = syntax.items;
        }
        directory.push(name);
    }
    let mut kinds = Vec::new();
    for item in &items {
        if let Some((ident, kind)) = declaration(item)
            && identifier_key(&ident.to_string()) == *name
        {
            if !unconditional_attributes(item_attributes(item)?) {
                return None;
            }
            kinds.push(kind);
        }
    }
    (!kinds.is_empty()).then(|| kinds.contains(&kind))
}

fn module_items(
    docs: &Path,
    context: &SourceContext,
    module: &Path,
) -> Option<(PathBuf, Vec<syn::Item>)> {
    module_items_with_visited(docs, context, module, &mut HashSet::new())
}

fn module_items_with_visited(
    docs: &Path,
    context: &SourceContext,
    module: &Path,
    visited: &mut HashSet<(PathBuf, String)>,
) -> Option<(PathBuf, Vec<syn::Item>)> {
    let components = module
        .iter()
        .map(|part| part.to_str())
        .collect::<Option<Vec<_>>>()?;
    let mut directory = docs.join(components.first()?);
    let source = source_link(docs, &directory.join("index.html"))?;
    let syntax = source_file(&source.page)?;
    if !plain_attributes(&syntax.attrs) {
        return None;
    }
    let mut items = context.active_items(syntax.items);
    for (index, name) in components[1..].iter().enumerate() {
        let aliased = items.iter().any(|item| {
            let syn::Item::Use(item) = item else {
                return false;
            };
            let mut targets = Vec::new();
            reexport_targets(&item.tree, &mut Vec::new(), name, &mut targets);
            targets.iter().any(|(glob, _)| !glob)
        });
        let mut modules = items.into_iter().filter_map(|item| match item {
            syn::Item::Mod(module) if identifier_key(&module.ident.to_string()) == *name => {
                Some(module)
            }
            _ => None,
        });
        let Some(module) = modules.next() else {
            if !aliased {
                return None;
            }
            let scope = directory.strip_prefix(docs).ok()?;
            let SourcePath::Local(mut path) =
                resolve_source_path(docs, context, scope, false, vec![name.to_string()], visited)?
            else {
                return None;
            };
            path.extend(components[index + 2..].iter().map(|part| part.to_string()));
            return module_items_with_visited(
                docs,
                context,
                &path.iter().collect::<PathBuf>(),
                visited,
            );
        };
        if modules.next().is_some()
            || !plain_attributes(&module.attrs)
            || context.enabled(&module.attrs) != Some(true)
        {
            return None;
        }
        directory.push(name);
        items = context.active_items(if let Some((_, items)) = module.content {
            items
        } else {
            let source = source_link(docs, &directory.join("index.html"))?;
            let syntax = source_file(&source.page)?;
            if !plain_attributes(&syntax.attrs) {
                return None;
            }
            syntax.items
        });
    }
    Some((directory.strip_prefix(docs).ok()?.to_path_buf(), items))
}

fn declaration_binding(item: &syn::Item) -> Option<(&syn::Ident, u8, &syn::Visibility)> {
    Some(match item {
        syn::Item::Const(item) => (&item.ident, 2, &item.vis),
        syn::Item::Enum(item) => (&item.ident, 1, &item.vis),
        syn::Item::ExternCrate(item) => (
            item.rename.as_ref().map_or(&item.ident, |(_, ident)| ident),
            1,
            &item.vis,
        ),
        syn::Item::Fn(item) => (&item.sig.ident, 2, &item.vis),
        syn::Item::Mod(item) => (&item.ident, 1, &item.vis),
        syn::Item::Static(item) => (&item.ident, 2, &item.vis),
        syn::Item::Struct(item) => (
            &item.ident,
            if matches!(item.fields, syn::Fields::Named(_)) {
                1
            } else {
                1 | 2
            },
            &item.vis,
        ),
        syn::Item::Trait(item) => (&item.ident, 1, &item.vis),
        syn::Item::TraitAlias(item) => (&item.ident, 1, &item.vis),
        syn::Item::Type(item) => (&item.ident, 1, &item.vis),
        syn::Item::Union(item) => (&item.ident, 1, &item.vis),
        _ => return None,
    })
}

struct ExportedMacros<'a> {
    docs: &'a Path,
    context: &'a SourceContext,
    module: PathBuf,
    name: &'a str,
    found: bool,
    complete: bool,
}

fn exported_macros<'a>(
    docs: &'a Path,
    context: &'a SourceContext,
    module: &Path,
    items: &[syn::Item],
    name: &'a str,
) -> ExportedMacros<'a> {
    let mut macros = ExportedMacros {
        docs,
        context,
        module: module.to_path_buf(),
        name,
        found: false,
        complete: true,
    };
    for item in items {
        syn::visit::Visit::visit_item(&mut macros, item);
    }
    macros
}

impl<'ast> syn::visit::Visit<'ast> for ExportedMacros<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attrs = match item {
            syn::Item::Macro(item) => Some(item.attrs.as_slice()),
            _ => item_attributes(item),
        };
        let Some(attrs) = attrs else {
            self.complete = false;
            return;
        };
        if self.context.enabled(attrs) == Some(false) {
            return;
        }
        if !plain_attributes(attrs)
            || self.context.enabled(attrs) != Some(true)
            || attrs.iter().any(|attr| attr.path().is_ident("derive"))
        {
            self.complete = false;
            return;
        }
        match item {
            // Foreign declarations cannot contain macro definitions or bodies.
            syn::Item::ForeignMod(_) => {}
            syn::Item::Macro(item) => {
                if let Some(ident) = &item.ident {
                    self.found |= identifier_key(&ident.to_string()) == self.name
                        && attrs
                            .iter()
                            .any(|attr| attr.path().is_ident("macro_export"));
                } else {
                    self.complete = false;
                }
            }
            syn::Item::Mod(item) => {
                self.module
                    .push(identifier_key(&item.ident.to_string()).as_ref());
                if let Some((_, items)) = &item.content {
                    for item in items {
                        self.visit_item(item);
                    }
                } else if let Some((_, items)) = module_items(self.docs, self.context, &self.module)
                {
                    for item in &items {
                        self.visit_item(item);
                    }
                } else {
                    self.complete = false;
                }
                self.module.pop();
            }
            _ => syn::visit::visit_item(self, item),
        }
    }

    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        // Attributes on expressions or associated items can also hide or
        // generate nested macro exports. Keep unmodeled contexts uncertain.
        let attrs = std::slice::from_ref(attr);
        if !plain_attributes(attrs) || self.context.enabled(attrs) != Some(true) {
            self.complete = false;
        }
    }
}

fn source_import_namespaces(
    docs: &Path,
    context: &SourceContext,
    module: &Path,
    absolute: bool,
    mut path: Vec<String>,
) -> Option<u8> {
    let mut variant = None;
    let target = resolve_source_path(
        docs,
        context,
        module,
        absolute,
        path.clone(),
        &mut HashSet::new(),
    )
    .or_else(|| {
        // Enum variants are not modules, so resolve their enum separately.
        variant = path.pop();
        resolve_source_path(docs, context, module, absolute, path, &mut HashSet::new())
    })?;
    let SourcePath::Local(path) = target else {
        return None;
    };
    let (name, parents) = path.split_last()?;
    let (_, items) = module_items(docs, context, &parents.iter().collect::<PathBuf>())?;
    if !context.complete_items(&items) {
        return None;
    }
    let mut namespaces = 0;
    for item in &items {
        if let Some(variant) = &variant {
            if let syn::Item::Enum(item) = item
                && identifier_key(&item.ident.to_string()) == *name
            {
                for item in &item.variants {
                    if identifier_key(&item.ident.to_string()) == *variant {
                        if !plain_attributes(&item.attrs)
                            || context.enabled(&item.attrs) != Some(true)
                        {
                            return None;
                        }
                        namespaces |= if matches!(item.fields, syn::Fields::Named(_)) {
                            1
                        } else {
                            1 | 2
                        };
                    }
                }
            }
        } else if let Some((ident, binding_namespaces, _)) = declaration_binding(item)
            && identifier_key(&ident.to_string()) == *name
        {
            namespaces |= binding_namespaces;
        } else if let syn::Item::ForeignMod(block) = item {
            for item in &block.items {
                if let Some((ident, binding_namespaces, _)) = foreign_binding(item)
                    && identifier_key(&ident.to_string()) == *name
                {
                    namespaces |= binding_namespaces;
                }
            }
        } else if let syn::Item::Macro(item) = item
            && item
                .ident
                .as_ref()
                .is_some_and(|ident| identifier_key(&ident.to_string()) == *name)
        {
            namespaces |= 4;
        }
    }
    if parents.len() == 1 && variant.is_none() {
        let macros = exported_macros(docs, context, Path::new(&parents[0]), &items, name);
        if !macros.complete {
            return None;
        }
        if macros.found {
            namespaces |= 4;
        }
    }
    (namespaces != 0).then_some(namespaces)
}

// Source bindings still shadow globs when rustdoc omits their pages. Imports
// whose target namespaces cannot be proved conservatively block all three.
fn source_bindings(
    docs: &Path,
    context: &SourceContext,
    module: &Path,
    items: &[syn::Item],
    name: &str,
) -> (u8, u8, u8) {
    let mut shadowed = 0;
    let mut public = 0;
    for item in items {
        if !item_attributes(item).is_some_and(plain_attributes) {
            continue;
        }
        if let syn::Item::ForeignMod(block) = item {
            for item in &block.items {
                if !foreign_attributes(item).is_some_and(plain_attributes) {
                    continue;
                }
                if let Some((ident, namespaces, visibility)) = foreign_binding(item)
                    && identifier_key(&ident.to_string()) == name
                {
                    shadowed |= namespaces;
                    if matches!(visibility, syn::Visibility::Public(_)) {
                        public |= namespaces;
                    }
                }
            }
            continue;
        }
        if let syn::Item::Use(item) = item {
            let mut targets = Vec::new();
            reexport_targets(&item.tree, &mut Vec::new(), name, &mut targets);
            for (_, path) in targets.into_iter().filter(|(glob, _)| !glob) {
                let namespaces = source_import_namespaces(
                    docs,
                    context,
                    module,
                    item.leading_colon.is_some(),
                    path,
                )
                .unwrap_or(1 | 2 | 4);
                shadowed |= namespaces;
                if matches!(item.vis, syn::Visibility::Public(_)) {
                    public |= namespaces;
                }
            }
            continue;
        }
        let Some((ident, namespaces, visibility)) = declaration_binding(item) else {
            continue;
        };
        if identifier_key(&ident.to_string()) == name {
            shadowed |= namespaces;
            if matches!(visibility, syn::Visibility::Public(_)) {
                public |= namespaces;
            }
        }
    }
    let mut complete = if context.complete_items(items) {
        1 | 2 | 4
    } else {
        0
    };
    if module.components().count() == 1 {
        let macros = exported_macros(docs, context, module, items, name);
        if macros.found {
            shadowed |= 4;
            public |= 4;
        }
        if !macros.complete {
            complete &= !4;
        }
    }
    (shadowed, public, complete)
}

fn declared_reexport(
    docs: &Path,
    external_crates: &[ExternalCrate],
    context: &SourceContext,
    item: &HtmlItem,
) -> Option<ItemIdentity> {
    let relative = item.page.strip_prefix(docs).ok()?;
    let kind = page_kind(&item.page)?;
    let (module, name) = if kind == "index" {
        let module = relative.parent()?;
        (module.parent()?, module.file_name()?.to_str()?)
    } else {
        (
            relative.parent()?,
            item.page.file_stem()?.to_str()?.split_once('.')?.1,
        )
    };
    let (module, items) = module_items(docs, context, module)?;
    // A declaration shadows a glob in its own namespace.
    if items.iter().any(|item| {
        declaration(item).is_some_and(|(ident, declared)| {
            declared == kind && identifier_key(&ident.to_string()) == name
        })
    }) {
        return None;
    }
    let mut targets = Vec::new();
    for item in &items {
        if let syn::Item::Use(item) = item
            && matches!(item.vis, syn::Visibility::Public(_))
        {
            let mut origins = Vec::new();
            reexport_targets(&item.tree, &mut Vec::new(), name, &mut origins);
            targets.extend(origins.into_iter().map(|(glob, path)| {
                (
                    glob,
                    item.leading_colon.is_some(),
                    path,
                    plain_attributes(&item.attrs) && context.enabled(&item.attrs) == Some(true),
                )
            }));
        }
    }
    targets.sort_by_key(|(glob, _, _, _)| *glob);
    let mut resolved = Vec::new();
    for (glob, absolute, path, unconditional) in targets {
        if glob && resolved.iter().any(|(glob, _)| !glob) {
            continue;
        }
        if !unconditional {
            return None;
        }
        if glob && !context.complete_items(&items) {
            // Expansions may introduce an explicit binding that shadows a glob.
            return None;
        }
        let target =
            resolve_source_path(docs, context, &module, absolute, path, &mut HashSet::new())?;
        let definition = match target {
            SourcePath::External(path) => {
                // --no-deps may omit both the external page and its source.
                let (crate_name, external_path) = path.split_first()?;
                let mut packages = external_crates.iter().filter(|dep| {
                    dep.dependency.name.replace('-', "_") == *crate_name
                        && dep
                            .dependency
                            .dep_kinds
                            .iter()
                            .any(|kind| kind.kind == DependencyKind::Normal)
                });
                let package = packages.next()?;
                if packages.any(|other| other.dependency.pkg != package.dependency.pkg) {
                    return None;
                }
                if !external_has_kind(&package.source, external_path, kind)? {
                    continue;
                }
                ItemIdentity::External {
                    package: package.dependency.pkg.clone(),
                    path: external_path.to_vec(),
                    kind: kind.to_string(),
                    fragment: item.fragment.clone(),
                }
            }
            SourcePath::Local(path) => {
                let (name, parents) = path.split_last()?;
                let mut page = docs.to_path_buf();
                page.extend(parents);
                page = if kind == "index" {
                    page.join(name).join("index.html")
                } else {
                    page.join(format!("{kind}.{name}.html"))
                };
                if !page.is_file() {
                    return None;
                }
                ItemIdentity::Page(HtmlItem {
                    page,
                    fragment: item.fragment.clone(),
                })
            }
        };
        resolved.push((glob, definition));
    }
    let explicit = resolved.iter().any(|(glob, _)| !glob);
    let definitions = resolved
        .into_iter()
        .filter(|(glob, _)| !explicit || !glob)
        .map(|(_, definition)| definition)
        .collect::<HashSet<_>>();
    if definitions.len() == 1 {
        let definition = definitions.into_iter().next()?;
        if definition != ItemIdentity::Page(item.clone()) {
            return Some(definition);
        }
    }
    None
}

#[derive(Debug, PartialEq, Eq)]
enum SourcePath {
    Local(Vec<String>),
    External(Vec<String>),
}

fn resolve_source_path(
    docs: &Path,
    context: &SourceContext,
    module: &Path,
    absolute: bool,
    mut path: Vec<String>,
    visited: &mut HashSet<(PathBuf, String)>,
) -> Option<SourcePath> {
    let mut base = module
        .iter()
        .map(|part| part.to_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()?;
    let external_only = absolute && !context.edition_2015;
    let mut implicit_external = !absolute
        && !context.edition_2015
        && !matches!(path.first()?.as_str(), "crate" | "self" | "super");
    if absolute
        || (context.edition_2015 && !matches!(path.first()?.as_str(), "crate" | "self" | "super"))
    {
        base.truncate(1);
    }
    match path.first()?.as_str() {
        "crate" => {
            base.truncate(1);
            path.remove(0);
        }
        "self" => {
            path.remove(0);
        }
        "super" => {
            while path.first().is_some_and(|part| part == "super") {
                if base.len() <= 1 {
                    return None;
                }
                base.pop();
                path.remove(0);
            }
        }
        _ => {}
    }
    while let Some(name) = path.first() {
        let scope = base.iter().collect::<PathBuf>();
        let (scope, items) = module_items_with_visited(docs, context, &scope, visited)?;
        base = scope
            .iter()
            .map(|part| part.to_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()?;
        let mut crates = items.iter().filter_map(|item| match item {
            syn::Item::ExternCrate(item)
                if identifier_key(
                    &item
                        .rename
                        .as_ref()
                        .map_or(&item.ident, |(_, ident)| ident)
                        .to_string(),
                ) == *name =>
            {
                Some(item)
            }
            _ => None,
        });
        if let Some(external) = crates.next() {
            if crates.next().is_some()
                || !plain_attributes(&external.attrs)
                || context.enabled(&external.attrs) != Some(true)
            {
                return None;
            }
            if external.ident == "self" {
                path[0] = "crate".into();
                return resolve_source_path(docs, context, &scope, false, path, visited);
            }
            path[0] = identifier_key(&external.ident.to_string()).into_owned();
            return Some(SourcePath::External(path));
        }
        if external_only {
            return context
                .complete_items(&items)
                .then_some(SourcePath::External(path));
        }
        let mut aliases = Vec::new();
        for item in &items {
            if let syn::Item::Use(item) = item {
                let mut targets = Vec::new();
                reexport_targets(&item.tree, &mut Vec::new(), name, &mut targets);
                for (_, target) in targets.into_iter().filter(|(glob, _)| !glob) {
                    if !plain_attributes(&item.attrs) || context.enabled(&item.attrs) != Some(true)
                    {
                        return None;
                    }
                    aliases.push((item.leading_colon.is_some(), target));
                }
            }
        }
        if !aliases.is_empty() {
            if aliases.len() != 1 || !visited.insert((scope.clone(), name.clone())) {
                return None;
            }
            let (absolute, target) = aliases.pop()?;
            let target = resolve_source_path(docs, context, &scope, absolute, target, visited)?;
            let suffix = &path[1..];
            return match target {
                SourcePath::External(mut target) => {
                    target.extend_from_slice(suffix);
                    Some(SourcePath::External(target))
                }
                SourcePath::Local(target) if suffix.is_empty() => Some(SourcePath::Local(target)),
                SourcePath::Local(mut target) => {
                    let root = PathBuf::from(target.first()?);
                    target[0] = "crate".into();
                    target.extend_from_slice(suffix);
                    resolve_source_path(docs, context, &root, false, target, visited)
                }
            };
        }
        if implicit_external
            && !scope_shadows_external(&items, name, path.len() > 1)
            && !(path.len() == 1
                && base.len() == 1
                && exported_macros(docs, context, &scope, &items, name).found)
        {
            return context
                .complete_items(&items)
                .then_some(SourcePath::External(path));
        }
        base.push(path.remove(0));
        implicit_external = false;
    }
    Some(SourcePath::Local(base))
}

fn scope_shadows_external(items: &[syn::Item], name: &str, type_only: bool) -> bool {
    items.iter().any(|item| match item {
        syn::Item::ExternCrate(item) => {
            let ident = item.rename.as_ref().map_or(&item.ident, |(_, ident)| ident);
            identifier_key(&ident.to_string()) == name
        }
        syn::Item::Use(item) => {
            let mut targets = Vec::new();
            reexport_targets(&item.tree, &mut Vec::new(), name, &mut targets);
            targets.iter().any(|(glob, _)| !glob)
        }
        _ => declaration(item).is_some_and(|(ident, kind)| {
            (!type_only || type_kind(kind)) && identifier_key(&ident.to_string()) == name
        }),
    })
}

fn reexport_targets(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    name: &str,
    targets: &mut Vec<(bool, Vec<String>)>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(identifier_key(&path.ident.to_string()).into_owned());
            reexport_targets(&path.tree, prefix, name, targets);
            prefix.pop();
        }
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                reexport_targets(tree, prefix, name, targets);
            }
        }
        syn::UseTree::Name(item) => {
            let ident = item.ident.to_string();
            let ident = identifier_key(&ident);
            if ident == name || (ident == "self" && prefix.last().is_some_and(|part| part == name))
            {
                let mut path = prefix.clone();
                if ident != "self" {
                    path.push(ident.to_string());
                }
                targets.push((false, path));
            }
        }
        syn::UseTree::Rename(item) if identifier_key(&item.rename.to_string()) == name => {
            let mut path = prefix.clone();
            if item.ident != "self" {
                path.push(identifier_key(&item.ident.to_string()).into_owned());
            }
            targets.push((false, path));
        }
        syn::UseTree::Glob(_) => {
            let mut path = prefix.clone();
            path.push(name.to_string());
            targets.push((true, path));
        }
        _ => {}
    }
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
    documentation: &Documentation,
    crate_name: &str,
    import: &ImportPath,
) -> Result<PathBuf, String> {
    let docs = &documentation.directory;
    let mut lookup = HtmlLookup::new(
        docs,
        &documentation.external_crates,
        &documentation.source_context,
    );
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
        let candidates = lookup.candidates(&item, name.as_ref(), type_only)?;
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
                    "ambiguous rustdoc HTML matches for item '{}'; cannot establish one canonical definition",
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
    variants: HashSet<String>,
}

struct Bindings {
    explicit: Vec<(HtmlItem, u8)>,
    globs: Vec<String>,
    shadowed: u8,
    unavailable: u8,
    source_complete: u8,
}

impl ParsedPage {
    fn new(html: String) -> Result<Self, String> {
        let visitor = Arc::new(Mutex::new(ExportBindings::default()));
        convert(
            &html,
            ConversionOptions {
                visitor: Some(visitor.clone()),
                ..Default::default()
            },
        )
        .map_err(|error| format!("cannot parse rustdoc bindings: {error}"))?;
        let bindings = std::mem::take(
            &mut *visitor
                .lock()
                .map_err(|error| format!("cannot read rustdoc bindings: {error}"))?,
        );
        Ok(Self {
            html,
            links: bindings.links,
            reexports: bindings.reexports,
            globs: bindings.globs,
            variants: bindings.variants,
        })
    }
}

#[derive(Debug, Default)]
struct ExportBindings {
    content_depth: Option<usize>,
    table_depth: Option<usize>,
    variants_depth: Option<usize>,
    row_id: Option<String>,
    row_href: Option<String>,
    links: HashSet<String>,
    reexports: Vec<(String, String)>,
    globs: Vec<String>,
    variants: HashSet<String>,
}

impl HtmlVisitor for ExportBindings {
    fn visit_element_start(&mut self, ctx: &NodeContext<'_>) -> VisitResult {
        let class = ctx
            .attributes()
            .get("class")
            .map(String::as_str)
            .unwrap_or("");
        let has_class = |name| class.split_whitespace().any(|part| part == name);
        // Documentation may contain arbitrary HTML, including forged tables.
        if has_class("docblock") || matches!(ctx.tag_name.as_ref(), "dd" | "nav" | "aside") {
            return VisitResult::Skip;
        }
        if ctx.tag_name == "section"
            && ctx
                .attributes()
                .get("id")
                .is_some_and(|id| id == "main-content")
        {
            self.content_depth = Some(ctx.depth);
        } else if self
            .content_depth
            .is_some_and(|depth| ctx.depth == depth + 1)
        {
            if ctx.tag_name == "dl" && has_class("item-table") {
                self.table_depth = Some(ctx.depth);
            } else if ctx.tag_name == "div" && has_class("variants") {
                self.variants_depth = Some(ctx.depth);
            }
        } else if ctx.tag_name == "dt"
            && self.table_depth.is_some_and(|depth| ctx.depth == depth + 1)
        {
            self.row_id = ctx.attributes().get("id").cloned();
            self.row_href = None;
        } else if ctx.tag_name == "section"
            && has_class("variant")
            && self
                .variants_depth
                .is_some_and(|depth| ctx.depth == depth + 1)
            && let Some(name) = ctx
                .attributes()
                .get("id")
                .and_then(|id| id.strip_prefix("variant."))
        {
            self.variants.insert(name.to_string());
        }
        VisitResult::Continue
    }

    fn visit_link(
        &mut self,
        ctx: &NodeContext<'_>,
        href: &str,
        _: &str,
        _: Option<&str>,
    ) -> VisitResult {
        if self.table_depth.is_some_and(|depth| ctx.depth > depth + 1) {
            self.row_href.get_or_insert_with(|| href.to_string());
        }
        VisitResult::Continue
    }

    fn visit_element_end(&mut self, ctx: &NodeContext<'_>, output: &str) -> VisitResult {
        if ctx.tag_name == "dt"
            && self.table_depth.is_some_and(|depth| ctx.depth == depth + 1)
            && let Some(href) = self.row_href.take()
        {
            if output.contains("::*;") {
                self.globs.push(href);
            } else if let Some(id) = self.row_id.take().filter(|id| id.starts_with("reexport.")) {
                self.reexports.push((id, href));
            } else {
                self.links.insert(href);
            }
        }
        if self.table_depth == Some(ctx.depth) {
            self.table_depth = None;
        }
        if self.variants_depth == Some(ctx.depth) {
            self.variants_depth = None;
        }
        if self.content_depth == Some(ctx.depth) {
            self.content_depth = None;
        }
        VisitResult::Continue
    }
}

struct HtmlLookup<'a> {
    docs: &'a Path,
    external_crates: &'a [ExternalCrate],
    source_context: &'a SourceContext,
    pages: HashMap<PathBuf, ParsedPage>,
    completed: HashMap<(HtmlItem, String, bool), Result<Vec<HtmlItem>, String>>,
    #[cfg(test)]
    expansions: usize,
}

impl<'a> HtmlLookup<'a> {
    fn new(
        docs: &'a Path,
        external_crates: &'a [ExternalCrate],
        source_context: &'a SourceContext,
    ) -> Self {
        Self {
            docs,
            external_crates,
            source_context,
            pages: HashMap::new(),
            completed: HashMap::new(),
            #[cfg(test)]
            expansions: 0,
        }
    }

    fn page(&mut self, path: &Path) -> Result<&ParsedPage, String> {
        if !self.pages.contains_key(path) {
            self.pages
                .insert(path.to_path_buf(), ParsedPage::new(read_html(path)?)?);
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
            .and_then(|(mut items, uncertain, unavailable)| {
                items.sort();
                if items.len() > 1 {
                    let mut identities = HashSet::new();
                    let mut distinct = Vec::new();
                    for item in items {
                        if identities.insert(item_identity(
                            self.docs,
                            self.external_crates,
                            self.source_context,
                            &item,
                            &mut HashSet::new(),
                        )?) {
                            distinct.push(item);
                        }
                    }
                    items = distinct;
                }
                if items.len() <= 1
                    && let Some(error) = unavailable
                {
                    return Err(error);
                }
                if uncertain && items.len() == 1 {
                    return Err(format!(
                        "cannot establish binding '{name}' through a glob: source bindings are incomplete"
                    ));
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
    ) -> Result<(Vec<HtmlItem>, bool, Option<String>), String> {
        let mut pending = vec![(root.clone(), if type_only { 1 } else { 1 | 2 | 4 }, 0)];
        let mut visited: HashMap<(PathBuf, u8), u8> = HashMap::new();
        let mut candidates = HashSet::new();
        let mut matched_namespaces = 0;
        let mut uncertain_match = false;
        let mut unavailable = None;
        let mut unavailable_namespaces = 0;
        while let Some((item, namespaces, uncertain)) = pending.pop() {
            let item = match self.follow_redirect(&item) {
                Ok(item) => item,
                Err(error) => {
                    unavailable.get_or_insert(error);
                    unavailable_namespaces |= namespaces;
                    continue;
                }
            };
            if item.fragment.is_some() {
                continue;
            }
            let seen = visited.entry((item.page.clone(), uncertain)).or_default();
            let namespaces = namespaces & !*seen;
            if namespaces == 0 {
                continue;
            }
            *seen |= namespaces;
            #[cfg(test)]
            {
                self.expansions += 1;
            }
            let Bindings {
                explicit,
                globs,
                mut shadowed,
                unavailable: omitted_public,
                source_complete,
            } = match self.bindings(&item, name, type_only) {
                Ok(bindings) => bindings,
                Err(error) if item == *root => return Err(error),
                Err(error) => {
                    unavailable.get_or_insert(error);
                    unavailable_namespaces |= namespaces;
                    continue;
                }
            };
            let visible = explicit
                .iter()
                .fold(0, |all, (_, namespaces)| all | namespaces);
            let missing = if item == *root {
                shadowed
            } else {
                omitted_public
            } & !visible;
            if missing & namespaces != 0 {
                return Err(format!(
                    "binding '{name}' has no available rustdoc HTML page; cannot establish a glob match"
                ));
            }
            for (item, binding_namespaces) in explicit {
                shadowed |= binding_namespaces;
                if binding_namespaces & namespaces != 0 {
                    matched_namespaces |= binding_namespaces & namespaces;
                    uncertain_match |= binding_namespaces & namespaces & uncertain != 0;
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
                    Ok(page) => {
                        pending.push((page, remaining, uncertain | (remaining & !source_complete)))
                    }
                    Err(error) => {
                        unavailable.get_or_insert(error);
                        unavailable_namespaces |= remaining;
                    }
                }
            }
        }
        let unavailable = unavailable
            .filter(|_| candidates.is_empty() || unavailable_namespaces & matched_namespaces != 0);
        Ok((
            candidates.into_iter().collect(),
            uncertain_match,
            unavailable,
        ))
    }

    fn bindings(
        &mut self,
        item: &HtmlItem,
        name: &str,
        type_only: bool,
    ) -> Result<Bindings, String> {
        let docs = self.docs;
        let context = self.source_context;
        let page = self.page(&item.page)?;
        let directory = item.page.parent().expect("page has a parent");
        let mut candidates = Vec::new();
        if page_kind(&item.page) == Some("enum") {
            if page.variants.contains(name) {
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
        let (shadowed, unavailable, source_complete) = if globs.is_empty() {
            (0, 0, 1 | 2 | 4)
        } else {
            module_items(
                docs,
                context,
                directory.strip_prefix(docs).expect("page is inside docs"),
            )
            .map(|(module, items)| source_bindings(docs, context, &module, &items, name))
            .unwrap_or_default()
        };
        let mut bindings = Vec::new();
        for candidate in candidates {
            let candidate = self.follow_redirect(&candidate)?;
            if !type_only || candidate.kind().is_some_and(type_kind) {
                let namespaces = if matches!(candidate.kind(), Some("struct" | "variant")) {
                    item_namespaces(&candidate, &self.page(&candidate.page)?.html)?
                } else {
                    item_namespaces(&candidate, "")?
                };
                bindings.push((candidate, namespaces));
            }
        }
        Ok(Bindings {
            explicit: bindings,
            globs,
            shadowed,
            unavailable,
            source_complete,
        })
    }
}

fn item_namespaces(item: &HtmlItem, html: &str) -> Result<u8, String> {
    Ok(match item.kind() {
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
                let code = rendered_code(
                    declaration
                        .trim()
                        .strip_prefix("<code>")
                        .and_then(|code| code.strip_suffix("</code>"))
                        .unwrap_or(declaration),
                )
                .ok_or_else(|| {
                    format!(
                        "cannot parse rustdoc enum declaration in {}",
                        item.page.display()
                    )
                })?;
                // Rustdoc may render raw identifiers without `r#`. Attribute
                // markup is decoded before inspecting the variant's shape.
                !code.lines().any(|line| {
                    let line = line.trim_start();
                    line.strip_prefix(name)
                        .or_else(|| line.strip_prefix(&format!("r#{name}")))
                        .is_some_and(|tail| tail.trim_start().starts_with('{'))
                })
            } else {
                declaration.trim_end().ends_with(";</code>")
            };
            // Unit and tuple constructors also occupy the value namespace.
            if constructor { 1 | 2 } else { 1 }
        }
        Some(kind) if type_kind(kind) => 1,
        Some("macro" | "attr" | "derive") => 4,
        _ => 2,
    })
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
    fn export_metadata_excludes_documentation_and_navigation_links() {
        let page = ParsedPage::new(
            r#"
<aside><section id="main-content"><dl class="item-table"><dt><a href="struct.Navigation.html">Navigation</a></dt></dl></section></aside>
<main><section id="main-content">
<div class="docblock"><dl class="item-table"><dt><a href="struct.Prose.html">Prose</a></dt></dl><div class="variants"><section class="variant" id="variant.Fake"></section></div></div>
<dl class="item-table">
<dt><a href="struct.Real.html">Real</a></dt>
<dd><dl class="item-table"><dt id="reexport.Fake"><a href="struct.Summary.html">Summary</a></dt></dl></dd>
<dt id="reexport.Alias"><code>pub use <a href="struct.Other.html">Other</a> as Alias;</code></dt>
<dt><code>pub use <a href="exports/index.html">exports</a>::*;</code></dt>
</dl>
<div class="variants"><section id="variant.Present" class="variant"></section><div class="docblock"><section id="variant.Forged" class="variant"></section></div></div>
</section></main>
"#
            .to_string(),
        )
        .unwrap();
        assert_eq!(page.links, HashSet::from(["struct.Real.html".to_string()]));
        assert_eq!(
            page.reexports,
            [(
                "reexport.Alias".to_string(),
                "struct.Other.html".to_string()
            )]
        );
        assert_eq!(page.globs, ["exports/index.html"]);
        assert_eq!(page.variants, HashSet::from(["Present".to_string()]));
    }

    #[test]
    fn failed_glob_redirects_and_bindings_do_not_certify_another_candidate() {
        let dir = tempfile::TempDir::new().unwrap();
        let docs = dir.path();
        let crate_dir = docs.join("fixture");
        for module in ["local", "broken"] {
            fs::create_dir_all(crate_dir.join(module)).unwrap();
        }
        fs::write(
            docs.join("source.html"),
            "<code>pub mod local { pub struct Item; } pub mod broken {}</code>",
        )
        .unwrap();
        fs::write(
            crate_dir.join("index.html"),
            r#"<main><a class="src" href="../source.html">Source</a><section id="main-content"><dl class="item-table">
<dt><code>pub use <a href="local/index.html">local</a>::*;</code></dt>
<dt><code>pub use <a href="broken/index.html">broken</a>::*;</code></dt>
</dl></section></main>"#,
        )
        .unwrap();
        fs::write(
            crate_dir.join("local/index.html"),
            r#"<main><section id="main-content"><dl class="item-table"><dt><a href="struct.Item.html">Item</a></dt></dl></section></main>"#,
        )
        .unwrap();
        fs::write(
            crate_dir.join("local/struct.Item.html"),
            r#"<main><pre class="rust item-decl"><code>pub struct Item;</code></pre></main>"#,
        )
        .unwrap();
        for (html, message) in [
            (r#"<meta content="URL=missing.html">"#, "unavailable"),
            (r#"<meta content="URL=index.html">"#, "cycle"),
            (
                r#"<main><section id="main-content"><dl class="item-table"><dt id="reexport.Item"><code>pub use <a href="missing.html">Missing</a> as Item;</code></dt></dl></section></main>"#,
                "unavailable",
            ),
        ] {
            fs::write(crate_dir.join("broken/index.html"), html).unwrap();
            let context = SourceContext::default();
            let mut lookup = HtmlLookup::new(docs, &[], &context);
            let error = lookup
                .candidates(&HtmlItem::new(crate_dir.join("index.html")), "Item", false)
                .unwrap_err();
            assert!(error.contains(message), "{error}");
        }
    }

    #[test]
    fn shared_glob_graphs_expand_each_page_once_and_reuse_completed_lookups() {
        for levels in [15, 30, 60] {
            let dir = tempfile::TempDir::new().unwrap();
            let docs = dir.path();
            let crate_dir = docs.join("fixture");
            let write_module = |name: &str, body: String| {
                let directory = crate_dir.join(name);
                fs::create_dir_all(&directory).unwrap();
                let source = if name.is_empty() {
                    "../source.html"
                } else {
                    "../../source.html"
                };
                fs::write(directory.join("index.html"), format!("<main><a class=\"src\" href=\"{source}\">Source</a><section id=\"main-content\"><dl class=\"item-table\">{body}</dl></section></main>")).unwrap();
            };
            let glob = |href: &str| {
                format!("<dt><code>pub use <a href=\"{href}\">module</a>::*;</code></dt>")
            };
            write_module(
                "leaf",
                "<dt><a href=\"struct.Item.html\">Item</a></dt>".to_string()
                    + &glob("../index.html"),
            );
            let definition = crate_dir.join("leaf/struct.Item.html");
            fs::write(
                &definition,
                "<main><pre class=\"rust item-decl\"><code>pub struct Item;</code></pre></main>",
            )
            .unwrap();
            let mut source = "pub mod leaf { pub struct Item; }".to_string();
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
                    source.push_str(&format!("pub mod {side}{level} {{}}"));
                }
            }
            fs::write(docs.join("source.html"), format!("<code>{source}</code>")).unwrap();
            write_module(
                "",
                glob(&format!("a{}/index.html", levels - 1))
                    + &glob(&format!("b{}/index.html", levels - 1)),
            );
            let root = HtmlItem::new(crate_dir.join("index.html"));
            let context = SourceContext::default();
            let mut lookup = HtmlLookup::new(docs, &[], &context);
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
            fs::remove_file(docs.join("source.html")).unwrap();
            let mut lookup = HtmlLookup::new(docs, &[], &context);
            assert!(
                lookup
                    .candidates(&root, "Item", false)
                    .unwrap_err()
                    .contains("source bindings are incomplete")
            );
            assert_eq!(
                lookup
                    .candidates(
                        &HtmlItem::new(crate_dir.join("leaf/index.html")),
                        "Item",
                        false
                    )
                    .unwrap(),
                result
            );
        }
    }

    #[test]
    fn conditional_source_selection_preserves_unknown_predicates() {
        let context = SourceContext {
            cfg: Some(vec![
                Cfg::Name("doc".into()),
                Cfg::Name("unix".into()),
                Cfg::KeyPair("feature".into(), "enabled".into()),
                Cfg::KeyPair("target_os".into(), "linux".into()),
            ]),
            ..SourceContext::default()
        };
        for (predicate, expected) in [
            ("any()", Some(false)),
            ("all()", Some(true)),
            ("feature = \"disabled\"", Some(false)),
            ("feature = \"enabled\"", Some(true)),
            ("target_os = \"windows\"", Some(false)),
            ("not(doc)", Some(false)),
            ("not(windows)", Some(true)),
            ("unknown_flag", None),
            ("target_feature = \"sse2\"", None),
            ("not(unknown_flag)", None),
            ("all(unknown_flag, any())", Some(false)),
            ("any(unknown_flag, all())", Some(true)),
            ("any(unknown_flag, unix)", Some(true)),
            ("all(unknown_flag, unix)", None),
        ] {
            let syntax = syn::parse_file(&format!("#[cfg({predicate})] pub struct Item;")).unwrap();
            assert_eq!(
                context.enabled(item_attributes(&syntax.items[0]).unwrap()),
                expected,
                "{predicate}"
            );
        }
        let syntax =
            syn::parse_file("#[cfg(unknown_flag)] #[cfg(any())] pub struct Item;").unwrap();
        assert!(context.active_items(syntax.items).is_empty());
        let unknown = SourceContext::default();
        assert_eq!(
            unknown.matches(&"feature = \"disabled\"".parse().unwrap()),
            None
        );
        assert_eq!(unknown.matches(&"all()".parse().unwrap()), Some(true));
        assert!(!flags_change_cfg(
            "--cfg coverage --cfg=unknown_flag -C instrument-coverage"
        ));
        for flags in [
            "--cfg feature=\"extra\"",
            "--cfg=target_os=\"other\"",
            "@flags.txt",
            "--target other",
        ] {
            assert!(flags_change_cfg(flags), "{flags}");
        }
        for config in [
            "rustdocflags = ['--cfg', 'feature=\"extra\"']",
            "rustdocflags = '--cfg feature=\"extra\"'",
        ] {
            let table = config.parse::<toml::Table>().unwrap();
            assert!(config_flags_change_cfg(&toml::Value::Table(table)));
        }
    }

    #[test]
    fn cyclic_source_aliases_and_parent_paths_stop_without_identity_evidence() {
        let dir = tempfile::TempDir::new().unwrap();
        let docs = dir.path();
        fs::create_dir(docs.join("fixture")).unwrap();
        fs::write(
            docs.join("fixture/index.html"),
            "<a class=\"src\" href=\"../source.html\">Source</a>",
        )
        .unwrap();
        fs::write(
            docs.join("source.html"),
            "<code>use b as a; use a as b;</code>",
        )
        .unwrap();
        let context = SourceContext::default();
        for path in [vec!["a", "item"], vec!["super", "item"]] {
            assert_eq!(
                resolve_source_path(
                    docs,
                    &context,
                    Path::new("fixture"),
                    false,
                    path.into_iter().map(str::to_string).collect(),
                    &mut HashSet::new()
                ),
                None
            );
        }
        for module in ["fixture/a", "fixture/a/item"] {
            assert!(module_items(docs, &context, Path::new(module)).is_none());
        }
        for declaration in [
            "pub struct Original; pub use Original as alias;",
            "pub mod original {} #[cfg(unknown_flag)] pub use original as alias;",
            "pub mod original {} #[cfg_attr(all(), doc(inline))] pub use original as alias;",
        ] {
            fs::write(
                docs.join("source.html"),
                format!("<code>{declaration}</code>"),
            )
            .unwrap();
            assert!(module_items(docs, &context, Path::new("fixture/alias")).is_none());
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
        let page = find_page(&docs, "api", &import).unwrap();
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
