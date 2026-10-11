mod cli;
mod imports;
mod resolver;
mod rustdoc_html;

use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, Package};
use cli::{FeatureSelection, ParsedCommand};
use resolver::{DependencyFilter, ResolvedDependency, package_dependencies, select_package};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("excra: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = match cli::parse_command()? {
        ParsedCommand::Run(args) => args,
        ParsedCommand::Help => {
            println!("{}", cli::usage());
            return Ok(());
        }
    };
    let imports = imports::parse_use_lines(&args.use_line)?;
    for import in &imports {
        if resolver::is_rust_library_crate(&import.crate_name) {
            return Err(format!(
                "{} is part of the Rust standard library; use https://doc.rust-lang.org/std/",
                import.crate_name
            ));
        }
    }

    let requested_manifest = args.root.join("Cargo.toml");
    let manifest = requested_manifest
        .canonicalize()
        .map_err(|error| format!("cannot read {}: {error}", requested_manifest.display()))?;
    let base = metadata_for(
        &manifest,
        &manifest,
        args.target.as_deref(),
        &FeatureSelection::default(),
        true,
    )?;
    let selected = select_package(&base, &manifest, args.package.as_deref())?;
    let metadata_features = package_features(selected, &args.feature_selection);
    let metadata = metadata_for(
        selected.manifest_path.as_std_path(),
        &manifest,
        args.target.as_deref(),
        &metadata_features,
        false,
    )?;
    let root = select_package(&metadata, &manifest, args.package.as_deref())?;
    let mut dependencies = package_dependencies(
        &metadata,
        &root.id,
        DependencyFilter {
            include_dev: args.include_dev,
            include_build: false,
        },
    );
    let host = if args.include_build {
        Some(host_triple(&manifest)?)
    } else {
        None
    };
    let host_metadata = if let Some(host) = &host {
        let host_metadata = metadata_for(
            root.manifest_path.as_std_path(),
            &manifest,
            Some(host),
            &metadata_features,
            false,
        )?;
        resolver::merge_dependency_kind(
            &mut dependencies,
            package_dependencies(
                &host_metadata,
                &root.id,
                DependencyFilter {
                    include_dev: false,
                    include_build: true,
                },
            ),
            DependencyKind::Build,
        );
        Some(host_metadata)
    } else {
        None
    };

    let queries = imports
        .iter()
        .map(|import| {
            resolver::resolve_dependency(&metadata.packages, &dependencies, &import.crate_name)
                .map(|resolved| {
                    resolved
                        .into_iter()
                        .flat_map(|dep| {
                            [false, true].into_iter().filter_map(move |build| {
                                let contexts = dep
                                    .contexts
                                    .iter()
                                    .filter(|context| {
                                        (context.kind == DependencyKind::Build) == build
                                    })
                                    .cloned()
                                    .collect::<Vec<_>>();
                                (!contexts.is_empty()).then_some((
                                    import,
                                    ResolvedDependency {
                                        package: dep.package,
                                        target: dep.target,
                                        contexts,
                                    },
                                ))
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut reports = Vec::new();
    for build in [false, true] {
        let group = queries
            .iter()
            .filter(|(_, dep)| (dep.contexts[0].kind == DependencyKind::Build) == build)
            .collect::<Vec<_>>();
        if group.is_empty() {
            continue;
        }
        let context_metadata = if build {
            host_metadata.as_ref().expect("build metadata loaded")
        } else {
            &metadata
        };
        let target = if build {
            host.as_deref()
        } else {
            args.target.as_deref()
        };
        let mut generated = HashMap::new();
        for (import, dependency) in group {
            let docs = if let Some(docs) = generated.get(&dependency.package.id) {
                docs
            } else {
                let docs = rustdoc_html::generate(&manifest, context_metadata, target, dependency)?;
                generated
                    .entry(dependency.package.id.clone())
                    .or_insert(docs)
            };
            let page =
                rustdoc_html::find_page(docs, &dependency.target.name.replace('-', "_"), import)?;
            let markdown = page.with_extension("md");
            reports.push(render_report(
                root,
                dependency,
                import,
                &args.feature_selection,
                &page,
                &markdown,
            )?);
        }
    }
    println!("{}", reports.join("\n"));
    Ok(())
}

fn host_triple(manifest: &Path) -> Result<String, String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .current_dir(manifest.parent().expect("manifest has a parent"))
        .output()
        .map_err(|error| format!("cannot run rustc: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "rustc failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| "rustc did not report a host target".into())
}

fn package_features(package: &Package, selection: &FeatureSelection) -> FeatureSelection {
    // Cargo metadata otherwise enables defaults on every workspace member.
    let mut features = selection
        .features
        .iter()
        .map(|feature| {
            if feature.contains('/') {
                feature.clone()
            } else {
                format!("{}/{feature}", package.name)
            }
        })
        .collect::<Vec<_>>();
    if selection.all_features {
        features.extend(
            package
                .features
                .keys()
                .map(|name| format!("{}/{name}", package.name)),
        );
    } else if !selection.no_default_features && package.features.contains_key("default") {
        features.push(format!("{}/default", package.name));
    }
    FeatureSelection {
        features,
        no_default_features: true,
        all_features: false,
    }
}

fn metadata_for(
    manifest: &Path,
    invocation_manifest: &Path,
    target: Option<&str>,
    features: &FeatureSelection,
    no_dependencies: bool,
) -> Result<Metadata, String> {
    let mut command = MetadataCommand::new();
    command.manifest_path(manifest);
    command.current_dir(invocation_manifest.parent().expect("manifest has a parent"));
    if no_dependencies {
        command.no_deps();
    }
    let mut options = vec!["--locked".to_string()];
    if let Some(target) = target {
        options.extend(["--filter-platform".into(), target.into()]);
    }
    options.extend(features.cargo_args());
    command.other_options(options);
    command
        .exec()
        .map_err(|error| cargo_failure("failed to read Cargo metadata", &error.to_string()))
}

pub(crate) fn cargo_failure(context: &str, error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    let hint = if lower.contains("lock file")
        && (lower.contains("needs to be updated")
            || lower.contains("needs to be generated")
            || lower.contains("--locked"))
    {
        "; Cargo.lock is missing or stale; run `cargo check` or `cargo build`, then retry"
    } else {
        ""
    };
    format!("{context}: {error}{hint}")
}

fn render_report(
    root: &Package,
    dependency: &ResolvedDependency<'_>,
    import: &imports::ImportPath,
    features: &FeatureSelection,
    page: &Path,
    markdown: &Path,
) -> Result<String, String> {
    let content = std::fs::read_to_string(markdown)
        .map_err(|error| format!("cannot read {}: {error}", markdown.display()))?;
    let contexts = dependency
        .contexts
        .iter()
        .map(|context| context.label())
        .collect::<Vec<_>>()
        .join(", ");
    let path = if import.namespace.is_some() {
        format!("{}::{{self}}", import.full_path())
    } else {
        import.full_path()
    };
    Ok(format!(
        "crate: {}\nversion: {}\npackage: {}\ndependency: {contexts}\nroot features: {}\nimport: use {path};\nsource: {}\nmarkdown: {}\n\n{content}",
        dependency.target.name.replace('-', "_"),
        dependency.package.version,
        root.name,
        features.label(),
        page.display(),
        markdown.display(),
    ))
}
