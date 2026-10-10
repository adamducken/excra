mod cli;
mod imports;
mod resolver;
mod rustdoc_json;
mod symbols;

use cargo_metadata::{Metadata, MetadataCommand, Package, PackageId, Target};
use cli::{FeatureSelection, ParsedCommand, parse_command};
use imports::ImportPath;
use resolver::{
    DependencyContext, DependencyFilter, ResolveError, is_rust_library_crate,
    merge_dependency_kind, package_dependencies, resolve_dependency,
    resolve_dependency_from_package, select_package,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::Arc;
use symbols::{NestedDoc, SymbolDoc, SymbolError, SymbolReport};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RustdocCacheKey {
    package_id: PackageId,
    target_name: String,
    unit: rustdoc_json::CargoUnitIdentity,
}

type LoadedDocs = (Arc<rustdoc_types::Crate>, PathBuf);
type RustdocCache = HashMap<RustdocCacheKey, LoadedDocs>;

struct SelectedDocs {
    krate: Arc<rustdoc_types::Crate>,
    json_path: PathBuf,
    unit: rustdoc_json::CargoUnitSelection,
}

struct DependencyDocsRequest<'a> {
    manifest_path: &'a Path,
    metadata: &'a Metadata,
    root_package: &'a Package,
    package: &'a Package,
    target: &'a Target,
    contexts: &'a [DependencyContext],
    parent: Option<&'a rustdoc_json::CargoParentUnit>,
    target_selection: &'a rustdoc_json::CargoTargetSelection,
    feature_selection: &'a FeatureSelection,
}

struct MetadataSet {
    target: Metadata,
    host: Metadata,
}

impl MetadataSet {
    fn for_contexts(&self, contexts: &[DependencyContext]) -> &Metadata {
        if contexts
            .first()
            .is_some_and(|context| context.kind == cargo_metadata::DependencyKind::Build)
        {
            &self.host
        } else {
            &self.target
        }
    }
}

#[derive(Debug)]
struct OutputReport {
    crate_name: String,
    version: Option<String>,
    dependency: String,
    target_triple: String,
    root_features: String,
    source: PathBuf,
    import_line: String,
    symbols: SymbolReport,
}

#[derive(Debug)]
struct ResolvedQuery {
    symbols: SymbolReport,
    crate_name: String,
    version: String,
    contexts: Vec<DependencyContext>,
    target_triple: String,
    json_path: PathBuf,
}

#[derive(Debug)]
enum QueryError {
    Absent(String),
    Incomplete(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent(message) | Self::Incomplete(message) => formatter.write_str(message),
        }
    }
}

fn main() -> ExitCode {
    if std::env::var_os("EXCRA_RUSTC_WRAPPER_MODE").is_some() {
        rustdoc_json::run_rustc_wrapper();
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("excra: {message}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<(), String> {
    let args = match parse_command()? {
        ParsedCommand::Run(args) => args,
        ParsedCommand::Help => {
            println!("{}", cli::usage());
            return Ok(());
        }
    };
    let imports = imports::parse_use_lines(&args.use_line)?;

    for import in &imports {
        if is_rust_library_crate(&import.crate_name) {
            return Err(format!(
                "{} is part of the Rust standard library and is not supported; use the official Rust docs: https://doc.rust-lang.org/std/",
                import.crate_name
            ));
        }
    }

    let requested_manifest = args.root.join("Cargo.toml");
    if !requested_manifest.exists() {
        return Err(format!(
            "no Cargo.toml found at {}",
            requested_manifest.display()
        ));
    }
    let manifest_path = requested_manifest.canonicalize().map_err(|error| {
        format!(
            "failed to resolve manifest path {}: {error}",
            requested_manifest.display()
        )
    })?;
    let toolchain = rustdoc_json::selected_toolchain();
    let host_target = host_target_triple(&toolchain)?;
    let target_selection = rustdoc_json::target_selection(
        &manifest_path,
        args.target.as_deref(),
        &host_target,
        &toolchain,
    )?;
    let base_metadata = cargo_metadata_for(
        &manifest_path,
        &manifest_path,
        &target_selection.effective_triple,
        &FeatureSelection::default(),
        &toolchain,
    )?;
    let base_root = select_package(&base_metadata, &manifest_path, args.package.as_deref())?;
    let metadata_manifest = if args.feature_selection.is_default() {
        manifest_path.clone()
    } else {
        base_root.manifest_path.as_std_path().to_path_buf()
    };
    let target_metadata = if args.feature_selection.is_default() {
        base_metadata
    } else {
        cargo_metadata_for(
            &metadata_manifest,
            &manifest_path,
            &target_selection.effective_triple,
            &args.feature_selection,
            &toolchain,
        )?
    };
    let host_metadata = if args.include_build && host_target != target_selection.effective_triple {
        cargo_metadata_for(
            &metadata_manifest,
            &manifest_path,
            &host_target,
            &args.feature_selection,
            &toolchain,
        )?
    } else {
        target_metadata.clone()
    };
    let metadata = MetadataSet {
        target: target_metadata,
        host: host_metadata,
    };
    let root_package = select_package(&metadata.target, &manifest_path, args.package.as_deref())?;
    let dependency_filter = DependencyFilter {
        include_dev: args.include_dev,
        include_build: args.include_build,
    };
    let mut root_dependencies = package_dependencies(
        &metadata.target,
        &root_package.id,
        DependencyFilter {
            include_dev: dependency_filter.include_dev,
            include_build: false,
        },
    );
    if dependency_filter.include_build {
        let host_dependencies = package_dependencies(
            &metadata.host,
            &root_package.id,
            DependencyFilter {
                include_dev: false,
                include_build: true,
            },
        );
        merge_dependency_kind(
            &mut root_dependencies,
            host_dependencies,
            cargo_metadata::DependencyKind::Build,
        );
    }
    let mut host_classification_loaded =
        dependency_filter.include_build || host_target == target_selection.effective_triple;
    let mut generation = rustdoc_json::GenerationSession::start(&metadata.target)?;
    let mut rustdoc_cache = RustdocCache::new();
    let mut printed_report = false;
    for import in &imports {
        let mut dependency_result = resolve_dependency(
            &metadata.target.packages,
            &root_dependencies,
            &import.crate_name,
        );
        if matches!(dependency_result, Err(ResolveError::NotDirectDependency(_)))
            && !host_classification_loaded
        {
            let diagnostic_host_metadata = cargo_metadata_for(
                &metadata_manifest,
                &manifest_path,
                &host_target,
                &args.feature_selection,
                &toolchain,
            )?;
            let host_dependencies = package_dependencies(
                &diagnostic_host_metadata,
                &root_package.id,
                DependencyFilter {
                    include_dev: false,
                    include_build: false,
                },
            );
            merge_dependency_kind(
                &mut root_dependencies,
                host_dependencies,
                cargo_metadata::DependencyKind::Build,
            );
            host_classification_loaded = true;
            dependency_result = resolve_dependency(
                &metadata.target.packages,
                &root_dependencies,
                &import.crate_name,
            );
        }
        let dependencies = dependency_result.map_err(|err| err.to_string())?;
        let mut resolved_contexts = Vec::new();
        let mut absent_contexts = Vec::new();
        let mut incomplete_contexts = Vec::new();
        for dependency in dependencies {
            for context in dependency.contexts {
                let context_label = context.label();
                match resolve_query(
                    &mut rustdoc_cache,
                    &mut generation,
                    &manifest_path,
                    &metadata,
                    root_package,
                    dependency.package,
                    dependency.target,
                    vec![context],
                    None,
                    &target_selection,
                    &args.feature_selection,
                    import,
                    &mut HashSet::new(),
                ) {
                    Ok(resolved) => resolved_contexts.push(resolved),
                    Err(QueryError::Absent(error)) => {
                        absent_contexts.push(format!("{context_label}: {error}"));
                    }
                    Err(QueryError::Incomplete(error)) => {
                        incomplete_contexts.push(format!("{context_label}: {error}"));
                    }
                }
            }
        }
        if !incomplete_contexts.is_empty() {
            return Err(format!(
                "query '{}' is incomplete because selected dependency contexts failed: {}",
                import.full_path(),
                incomplete_contexts.join("; ")
            ));
        }
        if resolved_contexts.is_empty() {
            return Err(format!(
                "failed to resolve '{}' in any selected dependency context: {}",
                import.full_path(),
                absent_contexts.join("; ")
            ));
        }

        for resolved in resolved_contexts {
            if printed_report {
                println!();
            }
            let output = OutputReport {
                crate_name: resolved.crate_name,
                version: Some(resolved.version),
                dependency: format_dependency_contexts(&resolved.contexts),
                target_triple: resolved.target_triple,
                root_features: args.feature_selection.label(),
                source: resolved.json_path,
                import_line: format_use(import),
                symbols: resolved.symbols,
            };
            print_report(&output);
            printed_report = true;
        }
    }

    Ok(())
}

fn cargo_metadata_for(
    manifest_path: &Path,
    invocation_manifest: &Path,
    platform: &str,
    features: &FeatureSelection,
    toolchain: &str,
) -> Result<Metadata, String> {
    let mut command = MetadataCommand::new();
    command.cargo_path("cargo");
    command.manifest_path(manifest_path);
    command.env("RUSTUP_TOOLCHAIN", toolchain);
    // The member manifest scopes features; the original invocation directory
    // must still govern Cargo configuration, just as it does for unit graphs.
    if let Some(invocation_dir) = invocation_manifest.parent() {
        command.current_dir(invocation_dir);
    }
    let mut options = vec![
        "--locked".to_string(),
        "--filter-platform".to_string(),
        platform.to_string(),
    ];
    options.extend(features.cargo_args());
    command.other_options(options);
    command.exec().map_err(|error| {
        cargo_failure_message(
            "failed to read cargo metadata without changing Cargo.lock",
            &error.to_string(),
        )
    })
}

fn cargo_failure_message(context: &str, error: &str) -> String {
    let lock_hint = if is_lockfile_failure(error) {
        "; Cargo.lock is missing or stale; run `cargo check` or `cargo build` to refresh it, then retry"
    } else {
        ""
    };
    format!("{context}: {error}{lock_hint}")
}

fn is_lockfile_failure(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("lock file")
        && (lower.contains("needs to be updated")
            || lower.contains("needs to be generated")
            || lower.contains("--locked"))
}

#[allow(clippy::too_many_arguments)]
fn resolve_query(
    cache: &mut RustdocCache,
    generation: &mut rustdoc_json::GenerationSession,
    manifest_path: &Path,
    metadata: &MetadataSet,
    root_package: &Package,
    package: &Package,
    target: &Target,
    contexts: Vec<DependencyContext>,
    parent: Option<rustdoc_json::CargoParentUnit>,
    target_selection: &rustdoc_json::CargoTargetSelection,
    feature_selection: &FeatureSelection,
    import: &ImportPath,
    visited: &mut HashSet<(PackageId, String)>,
) -> Result<ResolvedQuery, QueryError> {
    let visit_key = (package.id.clone(), import.full_path());
    if !visited.insert(visit_key) {
        return Err(QueryError::Incomplete(format!(
            "cycle while resolving external re-export {} from {}",
            import.full_path(),
            package.name
        )));
    }
    let context_metadata = metadata.for_contexts(&contexts);
    let selected_docs = load_docs_cached(
        cache,
        generation,
        DependencyDocsRequest {
            manifest_path,
            metadata: context_metadata,
            root_package,
            package,
            target,
            contexts: &contexts,
            parent: parent.as_ref(),
            target_selection,
            feature_selection,
        },
    )
    .map_err(QueryError::Incomplete)?;
    let krate = selected_docs.krate;
    let json_path = selected_docs.json_path;
    let child_parent = rustdoc_json::CargoParentUnit {
        package_id: package.id.to_string(),
        graph_index: selected_docs.unit.graph_index,
    };
    // Only the user's path must be externally importable. Recursive routes may
    // contain private canonical modules exposed by a public re-export.
    let is_root_query = visited.len() == 1;
    let validate_candidate = || {
        if !is_root_query {
            return Ok(());
        }
        rustdoc_json::validate_import(&json_path, import).map_err(|error| {
            if error.contains("error[E0603]")
                || error.contains("error[E0432]")
                || error.contains("error[E0433]")
            {
                QueryError::Absent(format!(
                    "item '{}' not found in {} {}: compiler rejected import '{}': {error}",
                    import.item,
                    package.name,
                    package.version,
                    import.full_path()
                ))
            } else {
                QueryError::Incomplete(format!(
                    "could not validate import '{}': {error}",
                    import.full_path()
                ))
            }
        })
    };
    let reject_missing_valid_import = || {
        if !is_root_query {
            return Ok(());
        }
        match rustdoc_json::validate_import(&json_path, import) {
            Ok(()) => Err(QueryError::Incomplete(format!(
                "non-doc API extraction is incomplete: compiler accepted import '{}' but Rustdoc JSON omitted it",
                import.full_path()
            ))),
            Err(error)
                if error.contains("error[E0432]")
                    || error.contains("error[E0433]")
                    || error.contains("error[E0603]") =>
            {
                Ok(())
            }
            Err(error) => Err(QueryError::Incomplete(format!(
                "could not validate missing import '{}': {error}",
                import.full_path()
            ))),
        }
    };
    let mut local_result = symbols::find_symbol_report(&krate, import);
    if let Err(SymbolError::Ambiguous(message)) = &local_result {
        return Err(QueryError::Incomplete(format!(
            "ambiguous import '{}' in {} {}: {message}",
            import.full_path(),
            package.name,
            package.version
        )));
    }
    let missing_impls = rustdoc_json::reject_non_doc_only_source(
        &krate, target, &json_path, import,
    )
    .map_err(|error| {
        // A rejected import is absent; an accepted name with an
        // unverifiable definition is incomplete.
        validate_candidate()
            .err()
            .unwrap_or(QueryError::Incomplete(error))
    })?;
    if let Ok(symbols) = &mut local_result {
        let doc = symbols.resolved.as_mut().unwrap_or(&mut symbols.imported);
        doc.impls.extend(missing_impls);
        doc.impls.sort();
        doc.impls.dedup();
    }
    let external_candidates = symbols::external_reexports(
        &krate,
        import,
        target.edition == cargo_metadata::Edition::E2015,
    )
    .map_err(|error| {
        QueryError::Incomplete(match &local_result {
            Ok(_) => format!("failed to inspect external re-export graph: {error}"),
            Err(local_error) => {
                format!("{local_error}; failed to inspect external re-export graph: {error}")
            }
        })
    })?;
    if external_candidates.is_empty() {
        return match local_result {
            Ok(symbols) => {
                validate_candidate()?;
                Ok(ResolvedQuery {
                    symbols,
                    crate_name: package.name.clone(),
                    version: package.version.to_string(),
                    contexts,
                    target_triple: krate.target.triple.clone(),
                    json_path,
                })
            }
            Err(local_error) => {
                reject_missing_valid_import()?;
                let message = not_found_message(
                    import,
                    &package.name,
                    Some(&package.version.to_string()),
                    &json_path,
                    Some(&local_error),
                );
                if matches!(local_error, SymbolError::NotFound(_)) {
                    Err(QueryError::Absent(message))
                } else {
                    Err(QueryError::Incomplete(message))
                }
            }
        };
    }

    let mut successes = Vec::new();
    let mut absent_branch_errors = Vec::new();
    let mut incomplete_branch_errors = Vec::new();
    for external in external_candidates {
        let preferred = resolve_dependency_from_package(
            context_metadata,
            &context_metadata.packages,
            package,
            &contexts,
            &external.crate_name,
        );
        let (external_dep, external_import, external_crate_name) = match preferred {
            Ok(dep) => (dep, external.import_path(), external.crate_name.clone()),
            Err(preferred_error) => {
                let Some(fallback) = external.canonical_fallback.as_ref() else {
                    incomplete_branch_errors
                        .push(format!("{}: {preferred_error}", external.crate_name));
                    continue;
                };
                if !matches!(preferred_error, ResolveError::NotDirectDependency(_)) {
                    incomplete_branch_errors
                        .push(format!("{}: {preferred_error}", external.crate_name));
                    continue;
                }
                match resolve_dependency_from_package(
                    context_metadata,
                    &context_metadata.packages,
                    package,
                    &contexts,
                    &fallback.crate_name,
                ) {
                    Ok(dep) => (
                        dep,
                        external.canonical_import_path(),
                        fallback.crate_name.clone(),
                    ),
                    Err(error) => {
                        incomplete_branch_errors.push(format!("{}: {error}", fallback.crate_name));
                        continue;
                    }
                }
            }
        };
        let mut branch_visited = visited.clone();
        let result = if let Some(external_import) = external_import {
            resolve_query(
                cache,
                generation,
                manifest_path,
                metadata,
                root_package,
                external_dep.package,
                external_dep.target,
                external_dep.contexts,
                Some(child_parent.clone()),
                target_selection,
                feature_selection,
                &external_import,
                &mut branch_visited,
            )
        } else {
            load_docs_cached(
                cache,
                generation,
                DependencyDocsRequest {
                    manifest_path,
                    metadata: context_metadata,
                    root_package,
                    package: external_dep.package,
                    target: external_dep.target,
                    contexts: &external_dep.contexts,
                    parent: Some(&child_parent),
                    target_selection,
                    feature_selection,
                },
            )
            .map_err(QueryError::Incomplete)
            .and_then(|external_docs| {
                let external_krate = external_docs.krate;
                let external_json_path = external_docs.json_path;
                Ok(ResolvedQuery {
                    symbols: SymbolReport {
                        resolved_id: external_krate.root,
                        imported: symbols::format_crate_root(&external_krate)
                            .map_err(|error| QueryError::Incomplete(error.to_string()))?,
                        resolved: None,
                    },
                    crate_name: external_dep.package.name.clone(),
                    version: external_dep.package.version.to_string(),
                    contexts: external_dep.contexts,
                    target_triple: external_krate.target.triple.clone(),
                    json_path: external_json_path,
                })
            })
        };
        match result {
            Ok(resolved) => successes.push((external.via_glob, resolved)),
            Err(QueryError::Absent(error)) => absent_branch_errors.push(format!(
                "{} ({}): {error}",
                external_crate_name, external_dep.package.id
            )),
            Err(QueryError::Incomplete(error)) => incomplete_branch_errors.push(format!(
                "{} ({}): {error}",
                external_crate_name, external_dep.package.id
            )),
        }
    }

    if !incomplete_branch_errors.is_empty() {
        return Err(QueryError::Incomplete(format!(
            "external re-export branches could not be verified: {}",
            incomplete_branch_errors.join("; ")
        )));
    }

    if let Ok(local_symbols) = local_result {
        let conflicts = successes
            .iter()
            .filter(|(via_glob, resolved)| {
                !*via_glob
                    || symbols::report_has_unshadowed_namespace(
                        &local_symbols,
                        &resolved.symbols,
                        import.namespace,
                    )
            })
            .map(|(_, resolved)| symbols::report_item_label(&resolved.symbols))
            .collect::<Vec<_>>();
        if !conflicts.is_empty() {
            return Err(QueryError::Incomplete(format!(
                "ambiguous import '{}' in {} {}: imported name '{}' is ambiguous across Rust namespaces ({}, {}); query a namespace-specific canonical path",
                import.full_path(),
                package.name,
                package.version,
                import.item,
                symbols::report_item_label(&local_symbols),
                conflicts.join(", ")
            )));
        }
        validate_candidate()?;
        return Ok(ResolvedQuery {
            symbols: local_symbols,
            crate_name: package.name.clone(),
            version: package.version.to_string(),
            contexts,
            target_triple: krate.target.triple.clone(),
            json_path,
        });
    }

    let local_error = local_result.expect_err("local result was checked above");
    let imported_reexport = match symbols::imported_reexport(&krate, import) {
        Ok(imported) => imported,
        Err(SymbolError::NotFound(_) | SymbolError::ExternalReexport(_)) => None,
        Err(error) => return Err(QueryError::Incomplete(error.to_string())),
    };
    // Prefer named routes when the same definition is also reached via a glob.
    // Each cached JSON path belongs to one exact package/Cargo unit. Item IDs
    // identify definitions only within that graph, never across JSON files.
    successes.sort_by_key(|(via_glob, _)| *via_glob);
    let mut definitions = HashSet::new();
    successes.retain(|(_, resolved)| {
        definitions.insert((resolved.json_path.clone(), resolved.symbols.resolved_id))
    });
    let named = successes
        .iter()
        .enumerate()
        .filter(|(_, (via_glob, _))| !*via_glob)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if named.len() == 1 {
        let named_index = named[0];
        let named_report = &successes[named_index].1.symbols;
        let conflicts = successes
            .iter()
            .enumerate()
            .filter(|(index, (via_glob, resolved))| {
                *index != named_index
                    && (!*via_glob
                        || symbols::report_has_unshadowed_namespace(
                            named_report,
                            &resolved.symbols,
                            import.namespace,
                        ))
            })
            .map(|(_, (_, resolved))| symbols::report_item_label(&resolved.symbols))
            .collect::<Vec<_>>();
        if conflicts.is_empty() {
            let selected = successes.swap_remove(named_index);
            successes.clear();
            successes.push(selected);
        }
    }
    match successes.len() {
        1 => {
            validate_candidate()?;
            let (_, mut resolved) = successes.pop().expect("one successful branch");
            if let Some(imported) = imported_reexport {
                let resolved_item = resolved
                    .symbols
                    .resolved
                    .take()
                    .unwrap_or(resolved.symbols.imported);
                resolved.symbols = SymbolReport {
                    resolved_id: resolved.symbols.resolved_id,
                    imported,
                    resolved: Some(resolved_item),
                };
            }
            Ok(resolved)
        }
        count if count > 1 => Err(QueryError::Incomplete(format!(
            "ambiguous external re-export for '{}': {count} branches resolved successfully",
            import.full_path()
        ))),
        _ => {
            reject_missing_valid_import()?;
            let mut message = not_found_message(
                import,
                &package.name,
                Some(&package.version.to_string()),
                &json_path,
                Some(&local_error),
            );
            if !absent_branch_errors.is_empty() {
                message.push_str("; external branches failed: ");
                message.push_str(&absent_branch_errors.join("; "));
            }
            Err(QueryError::Absent(message))
        }
    }
}

fn host_target_triple(toolchain: &str) -> Result<String, String> {
    let output = Command::new("rustc")
        .arg(format!("+{toolchain}"))
        .arg("-vV")
        .output()
        .map_err(|err| {
            format!("failed to run rustc +{toolchain} -vV to detect host target: {err}")
        })?;
    if !output.status.success() {
        return Err(format!(
            "failed to detect host target with rustc +{toolchain} -vV: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| "failed to parse host target from rustc -vV output".to_string())
}

fn load_docs_cached(
    cache: &mut RustdocCache,
    generation: &mut rustdoc_json::GenerationSession,
    request: DependencyDocsRequest<'_>,
) -> Result<SelectedDocs, String> {
    let unit = rustdoc_json::resolved_unit(
        generation,
        rustdoc_json::CargoUnitRequest {
            manifest_path: request.manifest_path,
            root_package: request.root_package,
            package: request.package,
            target: request.target,
            contexts: request.contexts,
            parent: request.parent,
            target_selection: request.target_selection,
            feature_selection: request.feature_selection,
        },
    )?;
    let key = rustdoc_cache_key(request.package, request.target, &unit.identity);
    if let Some(cached) = cache.get(&key) {
        return Ok(SelectedDocs {
            krate: Arc::clone(&cached.0),
            json_path: cached.1.clone(),
            unit,
        });
    }

    let (krate, json_path) = rustdoc_json::load_or_generate(
        generation,
        rustdoc_json::RustdocRequest {
            manifest_path: request.manifest_path.to_path_buf(),
            metadata: request.metadata,
            root_package: request.root_package,
            package: request.package,
            target: request.target,
            contexts: request.contexts,
            target_selection: request.target_selection,
            feature_selection: request.feature_selection,
            unit: &unit.identity,
            root_prerequisites: &unit.root_prerequisites,
        },
    )?;
    let loaded = (Arc::new(krate), json_path);
    cache.insert(key, loaded.clone());
    Ok(SelectedDocs {
        krate: loaded.0,
        json_path: loaded.1,
        unit,
    })
}

fn rustdoc_cache_key(
    package: &Package,
    target: &Target,
    unit: &rustdoc_json::CargoUnitIdentity,
) -> RustdocCacheKey {
    RustdocCacheKey {
        package_id: package.id.clone(),
        target_name: target.name.clone(),
        unit: unit.clone(),
    }
}

#[cfg(test)]
fn find_external_symbol(
    krate: &rustdoc_types::Crate,
    import: &ImportPath,
    crate_name: &str,
    version: &str,
    json_path: &Path,
) -> Result<SymbolReport, String> {
    symbols::find_symbol_report(krate, import)
        .map_err(|err| not_found_message(import, crate_name, Some(version), json_path, Some(&err)))
}

fn format_use(import: &ImportPath) -> String {
    let mut parts = vec![import.crate_name.clone()];
    parts.extend(import.segments.clone());
    parts.push(import.item.clone());
    if import.namespace.is_some() {
        parts.push("{self}".to_string());
    }
    format!("use {};", parts.join("::"))
}

fn print_report(report: &OutputReport) {
    print!("{}", render_report(report));
}

fn render_report(report: &OutputReport) -> String {
    let mut output = String::new();
    if let Some(version) = &report.version {
        output.push_str(&format!("crate: {} {version}\n", report.crate_name));
    } else {
        output.push_str(&format!("crate: {}\n", report.crate_name));
    }
    output.push_str(&format!("dependency: {}\n", report.dependency));
    output.push_str(&format!("target: {}\n", report.target_triple));
    output.push_str(&format!("root features: {}\n", report.root_features));
    output.push_str(&format!("source: {}\n", report.source.display()));
    output.push_str(&format!("import: {}\n", report.import_line.trim()));
    push_doc(&mut output, "item", &report.symbols.imported);
    if let Some(resolved) = &report.symbols.resolved {
        push_doc(&mut output, "resolved item", resolved);
    }
    output
}

fn format_dependency_contexts(contexts: &[DependencyContext]) -> String {
    contexts
        .iter()
        .map(DependencyContext::label)
        .collect::<Vec<_>>()
        .join(", ")
}

fn push_doc(output: &mut String, label: &str, found: &SymbolDoc) {
    output.push_str(&format!("{label}: {} {}\n", found.kind, found.name));
    if found.path.as_os_str().is_empty() {
        output.push_str("location: (unknown)\n");
    } else {
        output.push_str(&format!(
            "location: {}:{}\n",
            found.path.display(),
            found.line
        ));
    }
    output.push_str(&format!("definition: {}\n", found.definition));
    if let Some(deprecation) = &found.deprecation {
        output.push_str("deprecation:\n");
        if let Some(since) = &deprecation.since {
            output.push_str(&format!("  since: {since}\n"));
        }
        if let Some(note) = &deprecation.note {
            output.push_str(&format!("  note: {note}\n"));
        }
        if deprecation.since.is_none() && deprecation.note.is_none() {
            output.push_str("  (no details)\n");
        }
    }
    if !found.attributes.is_empty() {
        output.push_str("attributes:\n");
        for attribute in &found.attributes {
            output.push_str(&format!("  {}\n", attribute.render()));
        }
    }
    if !found.derives.is_empty() {
        output.push_str(&format!("derives: {}\n", found.derives.join(", ")));
    }
    if !found.details.is_empty() {
        output.push_str("details:\n");
        for detail in &found.details {
            push_nested_doc(output, detail, 2);
        }
    }
    if !found.methods.is_empty() {
        output.push_str("methods:\n");
        for method in &found.methods {
            push_nested_doc(output, method, 2);
        }
    }
    if !found.associated_constants.is_empty() {
        output.push_str("associated constants:\n");
        for constant in &found.associated_constants {
            push_nested_doc(output, constant, 2);
        }
    }
    if !found.impls.is_empty() {
        output.push_str("impls:\n");
        for line in &found.impls {
            output.push_str(&format!("  {line}\n"));
        }
    }
    if found.docs.is_empty() {
        output.push_str("docs: (none)\n");
    } else {
        output.push_str("docs:\n");
        for line in &found.docs {
            output.push_str(&format!("  {line}\n"));
        }
    }
}

fn push_nested_doc(output: &mut String, doc: &NestedDoc, indent: usize) {
    let padding = " ".repeat(indent);
    output.push_str(&format!("{padding}{}\n", doc.definition));
    if let Some(deprecation) = &doc.deprecation {
        output.push_str(&format!("{padding}  deprecation:\n"));
        if let Some(since) = &deprecation.since {
            output.push_str(&format!("{padding}    since: {since}\n"));
        }
        if let Some(note) = &deprecation.note {
            output.push_str(&format!("{padding}    note: {note}\n"));
        }
        if deprecation.since.is_none() && deprecation.note.is_none() {
            output.push_str(&format!("{padding}    (no details)\n"));
        }
    }
    if !doc.attributes.is_empty() {
        output.push_str(&format!("{padding}  attributes:\n"));
        for attribute in &doc.attributes {
            output.push_str(&format!("{padding}    {}\n", attribute.render()));
        }
    }
    if !doc.docs.is_empty() {
        output.push_str(&format!("{padding}  docs:\n"));
        for line in &doc.docs {
            output.push_str(&format!("{padding}    {line}\n"));
        }
    }
    let documented_children = doc
        .children
        .iter()
        .filter(|child| child.has_metadata())
        .collect::<Vec<_>>();
    if !documented_children.is_empty() {
        output.push_str(&format!("{padding}  members:\n"));
        for child in documented_children {
            push_nested_doc(output, child, indent + 4);
        }
    }
}

fn not_found_message(
    import: &ImportPath,
    crate_name: &str,
    version: Option<&str>,
    source: &Path,
    context: Option<&SymbolError>,
) -> String {
    let crate_label = if let Some(version) = version {
        format!("{crate_name} {version}")
    } else {
        crate_name.to_string()
    };
    let Some(context) = context else {
        return format!(
            "item '{}' not found in {} ({}): no matching public rustdoc item; check the path, visibility, and selected feature set",
            import.item,
            crate_label,
            source.display(),
        );
    };

    format!(
        "item '{}' not found in {} ({}): {}; check the path, visibility, and selected feature set",
        import.item,
        crate_label,
        source.display(),
        context
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cargo_metadata::MetadataCommand;
    use rustdoc_types::{Crate, Id, Item, ItemEnum, Module, Struct, StructKind, Visibility};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn displayed_import_preserves_namespace_on_round_trip() {
        for line in ["use dep::foo::{self};", "use dep::foo;"] {
            let original = imports::parse_use_line(line).unwrap();
            let displayed = format_use(&original);
            assert_eq!(displayed, line);
            let reparsed = imports::parse_use_line(&displayed).unwrap();
            assert_eq!(reparsed.full_path(), original.full_path());
            assert_eq!(reparsed.namespace, original.namespace);
        }
    }

    fn doc(name: &str, docs: Vec<String>) -> SymbolDoc {
        let nested = |definition: &str| NestedDoc {
            definition: definition.into(),
            deprecation: None,
            attributes: Vec::new(),
            docs: Vec::new(),
            children: Vec::new(),
        };
        SymbolDoc {
            path: PathBuf::from("/tmp/src/lib.rs"),
            line: 7,
            kind: "struct",
            name: name.into(),
            definition: format!("pub struct {name};"),
            deprecation: None,
            attributes: Vec::new(),
            details: vec![nested("field: usize")],
            docs,
            derives: vec!["Debug".into()],
            methods: vec![nested("pub fn new() -> Self")],
            associated_constants: Vec::new(),
            impls: vec!["impl Clone".into()],
            namespaces: 1,
        }
    }

    fn report(doc: SymbolDoc) -> SymbolReport {
        SymbolReport {
            resolved_id: Id(0),
            imported: doc,
            resolved: None,
        }
    }

    fn dependency_context() -> DependencyContext {
        DependencyContext {
            kind: cargo_metadata::DependencyKind::Normal,
            target: None,
            via: None,
        }
    }

    fn rustdoc_item(id: u32, name: &str, inner: ItemEnum) -> Item {
        Item {
            id: Id(id),
            crate_id: 0,
            name: Some(name.into()),
            span: None,
            visibility: Visibility::Public,
            docs: None,
            links: HashMap::new(),
            attrs: Vec::new(),
            deprecation: None,
            inner,
        }
    }

    #[test]
    fn rustdoc_cache_key_uses_package_identity_and_target_name() {
        let metadata = MetadataCommand::new()
            .manifest_path("Cargo.toml")
            .exec()
            .unwrap();
        let package = select_package(&metadata, Path::new("Cargo.toml"), None).unwrap();
        let deps = package_dependencies(
            &metadata,
            &package.id,
            DependencyFilter {
                include_dev: false,
                include_build: false,
            },
        );
        let dependencies = resolve_dependency(&metadata.packages, &deps, "cargo_metadata").unwrap();
        let dep = dependencies.first().unwrap();
        let unit = rustdoc_json::CargoUnitIdentity {
            features: vec!["serde".into()],
            mode: "check".into(),
            platform: None,
            profile: "{}".into(),
        };
        let key = rustdoc_cache_key(dep.package, dep.target, &unit);

        assert_eq!(&key.package_id, &dep.package.id);
        assert_eq!(&key.target_name, &dep.target.name);
        assert_eq!(key.unit.features, ["serde"]);
    }

    #[test]
    fn not_found_message_does_not_guess_item_kind_from_spelling() {
        let module = ImportPath {
            crate_name: "tokio".into(),
            segments: vec!["sync".into()],
            item: "mpsc".into(),
            namespace: None,
        };
        let message = not_found_message(&module, "tokio", Some("1.0.0"), Path::new("/src"), None);
        assert!(message.contains("check the path, visibility, and selected feature set"));
        assert!(!message.contains("appears to be a module"));

        let item = ImportPath {
            crate_name: "x".into(),
            segments: vec![],
            item: "Thing".into(),
            namespace: None,
        };
        let message = not_found_message(&item, "x", None, Path::new("/src"), None);
        assert!(!message.contains("appears to be a module"));
    }

    #[test]
    fn output_report_preserves_structured_fields_and_renders_text() {
        let mut symbol = doc("Thing", vec!["docs".into()]);
        symbol.deprecation = Some(symbols::DeprecationDoc {
            since: Some("1.2.3".into()),
            note: Some("use NewThing".into()),
        });
        symbol.attributes = vec![
            symbols::ReportedAttribute::NonExhaustive,
            symbols::ReportedAttribute::MustUse {
                reason: Some("inspect the value".into()),
            },
        ];
        let output = OutputReport {
            crate_name: "x".into(),
            version: Some("1.2.3".into()),
            dependency: dependency_context().label(),
            target_triple: "x86_64-unknown-linux-gnu".into(),
            root_features: "default".into(),
            source: PathBuf::from("/tmp/src"),
            import_line: "use x::Thing;".into(),
            symbols: report(symbol),
        };

        assert_eq!(output.crate_name, "x");
        assert_eq!(output.version.as_deref(), Some("1.2.3"));
        assert_eq!(output.dependency, "normal");
        assert_eq!(output.target_triple, "x86_64-unknown-linux-gnu");
        assert_eq!(output.root_features, "default");
        assert_eq!(output.import_line, "use x::Thing;");

        let rendered = render_report(&output);
        assert!(rendered.contains("crate: x 1.2.3\n"));
        assert!(rendered.contains("dependency: normal\n"));
        assert!(rendered.contains("root features: default\n"));
        assert!(rendered.contains("item: struct Thing\n"));
        assert!(rendered.contains(
            "deprecation:\n  since: 1.2.3\n  note: use NewThing\nattributes:\n  #[non_exhaustive]\n  #[must_use = \"inspect the value\"]\n"
        ));
        assert!(rendered.contains("docs:\n  docs\n"));
    }

    #[test]
    fn output_report_renders_resolved_items_and_empty_docs() {
        let output = OutputReport {
            crate_name: "x".into(),
            version: None,
            dependency: dependency_context().label(),
            target_triple: "x86_64-unknown-linux-gnu".into(),
            root_features: "default".into(),
            source: PathBuf::from("/tmp/src"),
            import_line: "use x::Thing;".into(),
            symbols: SymbolReport {
                resolved_id: Id(0),
                imported: doc("Thing", Vec::new()),
                resolved: Some(doc("ResolvedThing", Vec::new())),
            },
        };

        let rendered = render_report(&output);
        assert!(rendered.starts_with("crate: x\n"));
        assert!(rendered.contains("docs: (none)\n"));
        assert!(rendered.contains("resolved item: struct ResolvedThing\n"));
    }

    #[test]
    fn output_preserves_markdown_indentation_and_blank_lines() {
        let output = OutputReport {
            crate_name: "x".into(),
            version: None,
            dependency: dependency_context().label(),
            target_triple: "x86_64-unknown-linux-gnu".into(),
            root_features: "default".into(),
            source: PathBuf::from("/tmp/src"),
            import_line: "use x::Thing;".into(),
            symbols: report(doc(
                "Thing",
                vec![
                    "First paragraph.".into(),
                    "".into(),
                    "- parent".into(),
                    "  - child".into(),
                ],
            )),
        };

        assert!(
            render_report(&output)
                .contains("docs:\n  First paragraph.\n  \n  - parent\n    - child\n")
        );
    }

    #[test]
    fn typed_not_found_classification_does_not_promote_glob_diagnostics() {
        let import = ImportPath {
            crate_name: "facade".into(),
            segments: vec![],
            item: "Missing".into(),
            namespace: None,
        };
        let error = SymbolError::NotFound(
            "glob branches failed: external re-export target unavailable".into(),
        );
        let message = not_found_message(
            &import,
            "facade",
            Some("1.0.0"),
            Path::new("/tmp/facade.json"),
            Some(&error),
        );
        assert!(message.starts_with("item 'Missing' not found"));
        assert!(!message.contains("is a public re-export"));
    }

    #[test]
    fn cache_entries_share_the_rustdoc_graph() {
        let docs = Arc::new(root_mac_crate());
        let key = RustdocCacheKey {
            package_id: PackageId {
                repr: "path+file:///fixture#1.0.0".into(),
            },
            target_name: "fixture".into(),
            unit: rustdoc_json::CargoUnitIdentity {
                features: vec!["selected".into()],
                mode: "check".into(),
                platform: None,
                profile: "{}".into(),
            },
        };
        let mut cache = RustdocCache::new();
        cache.insert(
            key.clone(),
            (Arc::clone(&docs), PathBuf::from("fixture.json")),
        );
        let (cached, _) = cache.get(&key).unwrap().clone();

        assert!(Arc::ptr_eq(&docs, &cached));
        assert_eq!(Arc::strong_count(&docs), 3);
    }

    #[test]
    fn dependency_context_output_supports_multiple_contexts() {
        let contexts = [
            DependencyContext {
                kind: cargo_metadata::DependencyKind::Normal,
                target: None,
                via: None,
            },
            DependencyContext {
                kind: cargo_metadata::DependencyKind::Development,
                target: Some("cfg(test)".into()),
                via: None,
            },
        ];

        assert_eq!(
            format_dependency_contexts(&contexts),
            "normal, dev (cfg(test))"
        );
    }

    fn root_mac_crate() -> Crate {
        let root = rustdoc_item(
            1,
            "digest",
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2)],
                is_stripped: false,
            }),
        );
        let mac = rustdoc_item(
            2,
            "Mac",
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: rustdoc_types::Generics {
                    params: Vec::new(),
                    where_predicates: Vec::new(),
                },
                impls: Vec::new(),
            }),
        );
        Crate {
            root: Id(1),
            crate_version: Some("1.0.0".into()),
            includes_private: false,
            index: HashMap::from([(Id(1), root), (Id(2), mac)]),
            paths: HashMap::new(),
            external_crates: HashMap::new(),
            target: rustdoc_types::Target {
                triple: "x86_64-unknown-linux-gnu".into(),
                target_features: Vec::new(),
            },
            format_version: rustdoc_types::FORMAT_VERSION,
        }
    }

    #[test]
    fn external_lookup_does_not_fall_back_to_root_reexport_name() {
        let krate = root_mac_crate();
        let import = ImportPath {
            crate_name: "digest".into(),
            segments: vec!["mac".into()],
            item: "Mac".into(),
            namespace: None,
        };

        let err =
            find_external_symbol(&krate, &import, "digest", "1.0.0", Path::new("/x")).unwrap_err();

        assert!(err.contains("'mac' not found under digest"));
        assert!(!err.contains("root re-export fallback"));
    }

    #[test]
    fn external_lookup_reports_exact_root_item() {
        let krate = root_mac_crate();
        let import = ImportPath {
            crate_name: "digest".into(),
            segments: Vec::new(),
            item: "Mac".into(),
            namespace: None,
        };

        let found =
            find_external_symbol(&krate, &import, "digest", "1.0.0", Path::new("/x")).unwrap();

        assert_eq!(found.imported.name, "Mac");
    }
}
