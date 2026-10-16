use cargo_metadata::{DependencyKind, Metadata, Package, PackageId, Target};
use std::collections::HashMap;
use std::path::Path;
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResolveError {
    NotDirectDependency(String),
    Other(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDirectDependency(message) | Self::Other(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl Error for ResolveError {}

#[cfg(test)]
impl ResolveError {
    fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }
}

#[derive(Debug)]
pub(crate) struct ResolvedDependency<'a> {
    pub(crate) package: &'a Package,
    pub(crate) target: &'a Target,
    pub(crate) contexts: Vec<DependencyContext>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DependencyFilter {
    pub(crate) include_dev: bool,
    pub(crate) include_build: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DependencyContext {
    pub(crate) kind: DependencyKind,
    pub(crate) target: Option<String>,
}

impl DependencyContext {
    pub(crate) fn label(&self) -> String {
        let kind = match self.kind {
            DependencyKind::Normal => "normal",
            DependencyKind::Development => "dev",
            DependencyKind::Build => "build",
            _ => "unknown",
        };
        match &self.target {
            Some(target) => format!("{kind} ({target})"),
            None => kind.to_string(),
        }
    }
}

fn package_by_id<'a>(packages: &'a [Package], package_id: &PackageId) -> Option<&'a Package> {
    packages.iter().find(|pkg| &pkg.id == package_id)
}

#[derive(Debug, Clone)]
pub(crate) struct DependencyEntry {
    pub(crate) package_id: PackageId,
    pub(crate) contexts: Vec<DependencyContext>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct DependencyIndex {
    entries: HashMap<String, Vec<DependencyEntry>>,
    excluded: HashMap<String, ExcludedDependencyKinds>,
}

#[derive(Debug, Clone, Copy, Default)]
struct ExcludedDependencyKinds {
    dev: bool,
    build: bool,
}

impl DependencyIndex {
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn contains_key(&self, crate_name: &str) -> bool {
        self.entries.contains_key(crate_name)
    }

    #[cfg(test)]
    pub(crate) fn insert(&mut self, crate_name: String, entry: DependencyEntry) {
        self.entries.entry(crate_name).or_default().push(entry);
    }
}

fn push_dependency_context(
    entries: &mut Vec<DependencyEntry>,
    package_id: PackageId,
    context: DependencyContext,
) {
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.package_id == package_id)
    {
        if !entry.contexts.contains(&context) {
            entry.contexts.push(context);
            sort_dependency_contexts(&mut entry.contexts);
        }
        return;
    }

    entries.push(DependencyEntry {
        package_id,
        contexts: vec![context],
    });
}

fn sort_dependency_contexts(contexts: &mut [DependencyContext]) {
    contexts.sort_by(|left, right| {
        dependency_kind_order(left.kind)
            .cmp(&dependency_kind_order(right.kind))
            .then_with(|| left.target.cmp(&right.target))
    });
}

fn dependency_kind_order(kind: DependencyKind) -> u8 {
    match kind {
        DependencyKind::Normal => 0,
        DependencyKind::Development => 1,
        DependencyKind::Build => 2,
        _ => 3,
    }
}

pub(crate) fn is_rust_library_crate(crate_name: &str) -> bool {
    matches!(
        crate::imports::identifier_key(crate_name).as_ref(),
        "std" | "core" | "alloc"
    )
}

pub(crate) fn select_package<'a>(
    metadata: &'a Metadata,
    manifest_path: &Path,
    package_selector: Option<&str>,
) -> Result<&'a Package, String> {
    if let Some(selector) = package_selector {
        let mut matches = metadata
            .workspace_members
            .iter()
            .filter_map(|id| package_by_id(&metadata.packages, id))
            .filter(|package| package.name == selector || package.id.to_string() == selector)
            .collect::<Vec<_>>();
        matches.sort_by(|left, right| left.id.repr.cmp(&right.id.repr));
        return match matches.as_slice() {
            [] => Err(format!("package '{selector}' not found in workspace")),
            [package] => Ok(*package),
            packages => {
                let candidates = packages
                    .iter()
                    .map(|package| package.id.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(format!(
                    "package '{selector}' matched multiple workspace packages: {candidates}"
                ))
            }
        };
    }

    if let Some(package) = metadata.root_package() {
        return Ok(package);
    }
    // Metadata without a resolve graph does not identify the selected member.
    if let Some(package) = metadata
        .packages
        .iter()
        .find(|package| package.manifest_path.as_std_path() == manifest_path)
    {
        return Ok(package);
    }

    let root = manifest_path
        .parent()
        .unwrap_or(manifest_path)
        .canonicalize()
        .unwrap_or_else(|_| {
            manifest_path
                .parent()
                .unwrap_or(manifest_path)
                .to_path_buf()
        });
    Err(format!(
        "virtual workspace root {} has no root package; pass --package <name-or-id>",
        root.display()
    ))
}

#[cfg(test)]
pub(crate) fn package_dependencies(
    metadata: &Metadata,
    package_id: &PackageId,
    filter: DependencyFilter,
) -> DependencyIndex {
    index_dependencies(metadata, package_id, filter, None)
}

pub(crate) fn package_dependencies_for_platform(
    metadata: &Metadata,
    package_id: &PackageId,
    filter: DependencyFilter,
    platform: &crate::cargo_graph::TargetPlatform,
    host: &crate::cargo_graph::TargetPlatform,
    features: &crate::cargo_graph::FeatureGraph,
) -> DependencyIndex {
    let mut dependencies = index_dependencies(
        metadata,
        package_id,
        filter,
        Some((platform, host, features.root_is_host())),
    );
    dependencies.entries.retain(|_, entries| {
        entries.retain_mut(|entry| {
            entry.contexts.retain(|context| {
                features
                    .direct_dependency(&entry.package_id, context.kind)
                    .is_some()
            });
            !entry.contexts.is_empty()
        });
        !entries.is_empty()
    });
    dependencies
}

fn index_dependencies(
    metadata: &Metadata,
    package_id: &PackageId,
    filter: DependencyFilter,
    platform: Option<(
        &crate::cargo_graph::TargetPlatform,
        &crate::cargo_graph::TargetPlatform,
        bool,
    )>,
) -> DependencyIndex {
    let mut deps = DependencyIndex::default();
    if let Some(resolve) = metadata.resolve.as_ref()
        && let Some(node) = resolve.nodes.iter().find(|node| &node.id == package_id)
    {
        for dep in &node.deps {
            for dep_kind in &dep.dep_kinds {
                if platform.is_some_and(|(target, host, root_is_host)| {
                    let platform = if dep_kind.kind == DependencyKind::Build
                        || (root_is_host && dep_kind.kind == DependencyKind::Normal)
                    {
                        host
                    } else {
                        target
                    };
                    !platform.matches(dep_kind.target.as_ref())
                }) {
                    continue;
                }
                if !dependency_kind_allowed(dep_kind.kind, filter) {
                    let excluded = deps.excluded.entry(dep.name.replace('-', "_")).or_default();
                    match dep_kind.kind {
                        DependencyKind::Development => excluded.dev = true,
                        DependencyKind::Build => excluded.build = true,
                        _ => {}
                    }
                    continue;
                }
                let crate_name = dep.name.replace('-', "_");
                let entries = deps.entries.entry(crate_name).or_default();
                push_dependency_context(
                    entries,
                    dep.pkg.clone(),
                    DependencyContext {
                        kind: dep_kind.kind,
                        target: dep_kind.target.as_ref().map(ToString::to_string),
                    },
                );
            }
        }
    }
    deps
}

#[cfg(test)]
pub(crate) fn merge_dependency_kind(
    dependencies: &mut DependencyIndex,
    additional: DependencyIndex,
    kind: DependencyKind,
) {
    for (crate_name, entries) in additional.entries {
        let destination = dependencies.entries.entry(crate_name).or_default();
        for entry in entries {
            for context in entry
                .contexts
                .into_iter()
                .filter(|context| context.kind == kind)
            {
                push_dependency_context(destination, entry.package_id.clone(), context);
            }
        }
    }
    for (crate_name, excluded) in additional.excluded {
        let destination = dependencies.excluded.entry(crate_name).or_default();
        match kind {
            DependencyKind::Development if excluded.dev => destination.dev = true,
            DependencyKind::Build if excluded.build => destination.build = true,
            _ => {}
        }
    }
}

fn dependency_kind_allowed(kind: DependencyKind, filter: DependencyFilter) -> bool {
    match kind {
        DependencyKind::Normal => true,
        DependencyKind::Development => filter.include_dev,
        DependencyKind::Build => filter.include_build,
        _ => false,
    }
}

pub(crate) fn resolve_dependency<'a>(
    packages: &'a [Package],
    dependencies: &DependencyIndex,
    crate_name: &str,
) -> Result<Vec<ResolvedDependency<'a>>, ResolveError> {
    let crate_name = crate::imports::identifier_key(crate_name);
    let Some(entries) = dependencies.entries.get(crate_name.as_ref()) else {
        if let Some(excluded) = dependencies.excluded.get(crate_name.as_ref()) {
            let mut flags = Vec::new();
            if excluded.dev {
                flags.push("`--include-dev`");
            }
            if excluded.build {
                flags.push("`--include-build`");
            }
            return Err(ResolveError::NotDirectDependency(format!(
                "crate '{crate_name}' is declared only in dependency contexts excluded by default; retry with {}",
                flags.join(" or ")
            )));
        }
        return Err(ResolveError::NotDirectDependency(format!(
            "crate '{crate_name}' is not a direct dependency of the selected package"
        )));
    };
    let overlapping = entries.iter().enumerate().any(|(left_index, left)| {
        entries.iter().skip(left_index + 1).any(|right| {
            left.package_id != right.package_id
                && left.contexts.iter().any(|left_context| {
                    right
                        .contexts
                        .iter()
                        .any(|right_context| left_context.kind == right_context.kind)
                })
        })
    });
    if overlapping {
        let candidates = entries
            .iter()
            .map(|entry| {
                let contexts = entry
                    .contexts
                    .iter()
                    .map(DependencyContext::label)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} [{contexts}]", entry.package_id)
            })
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ResolveError::Other(format!(
            "crate '{crate_name}' matched multiple direct dependencies in the same Cargo context: {candidates}"
        )));
    }
    entries
        .iter()
        .map(|entry| {
            let package = package_by_id(packages, &entry.package_id).ok_or_else(|| {
                ResolveError::Other(format!(
                    "direct dependency '{crate_name}' missing from cargo metadata"
                ))
            })?;
            let target = library_target(package).map_err(ResolveError::Other)?;
            Ok(ResolvedDependency {
                package,
                target,
                contexts: entry.contexts.clone(),
            })
        })
        .collect()
}

pub(crate) fn library_target(package: &Package) -> Result<&Target, String> {
    package
        .targets
        .iter()
        .find(|target| is_library_target(target))
        .ok_or_else(|| format!("package {} has no doc-able library target", package.name))
}

pub(crate) fn is_library_target(target: &Target) -> bool {
    target.kind.iter().any(|kind| {
        matches!(
            kind.as_str(),
            "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
        )
    })
}

pub(crate) fn package_spec(package: &Package) -> String {
    package.id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cargo_metadata::MetadataCommand;
    use std::fs;
    use tempfile::TempDir;

    fn metadata() -> Metadata {
        MetadataCommand::new()
            .manifest_path("Cargo.toml")
            .exec()
            .unwrap()
    }

    fn default_filter() -> DependencyFilter {
        DependencyFilter {
            include_dev: false,
            include_build: false,
        }
    }

    fn one_dependency<'a>(
        result: Result<Vec<ResolvedDependency<'a>>, ResolveError>,
    ) -> ResolvedDependency<'a> {
        let mut dependencies = result.unwrap();
        assert_eq!(dependencies.len(), 1);
        dependencies.pop().unwrap()
    }

    fn virtual_workspace() -> TempDir {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join("member_a/src")).unwrap();
        fs::create_dir_all(temp.path().join("member_b/src")).unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            r#"
[workspace]
members = ["member_a", "member_b"]
"#,
        )
        .unwrap();
        fs::write(
            temp.path().join("member_a/Cargo.toml"),
            r#"
[package]
name = "member_a"
version = "0.1.0"
edition = "2024"
"#,
        )
        .unwrap();
        fs::write(temp.path().join("member_a/src/lib.rs"), "").unwrap();
        fs::write(
            temp.path().join("member_b/Cargo.toml"),
            r#"
[package]
name = "member_b"
version = "0.1.0"
edition = "2024"
"#,
        )
        .unwrap();
        fs::write(temp.path().join("member_b/src/lib.rs"), "").unwrap();
        temp
    }

    fn metadata_for(manifest_path: &Path) -> Metadata {
        MetadataCommand::new()
            .manifest_path(manifest_path)
            .exec()
            .unwrap()
    }

    #[test]
    fn detects_unsupported_rust_library_crates() {
        assert!(is_rust_library_crate("std"));
        assert!(is_rust_library_crate("core"));
        assert!(is_rust_library_crate("alloc"));
        assert!(!is_rust_library_crate("syn"));
    }

    #[test]
    fn resolves_package_for_exact_manifest_and_its_dependencies() {
        let metadata = metadata();
        let package = select_package(&metadata, Path::new("Cargo.toml"), None).unwrap();
        assert_eq!(package.name, "excra");

        let deps = package_dependencies(&metadata, &package.id, default_filter());
        assert!(deps.contains_key("cargo_metadata"));

        let dep = one_dependency(resolve_dependency(
            &metadata.packages,
            &deps,
            "cargo_metadata",
        ));
        assert_eq!(dep.package.name, "cargo_metadata");
        assert!(library_target(dep.package).is_ok());
    }

    #[test]
    fn recognizes_every_cargo_library_crate_type() {
        let metadata = metadata();
        let mut target = metadata
            .packages
            .iter()
            .find(|package| package.name == "cargo_metadata")
            .unwrap()
            .targets[0]
            .clone();

        for kind in ["lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"] {
            target.kind = vec![kind.into()];
            assert!(is_library_target(&target), "{kind}");
        }
        target.kind = vec!["example".into()];
        target.crate_types = vec!["rlib".into()];
        assert!(!is_library_target(&target));
    }

    #[test]
    fn selects_workspace_member_by_package_name_or_id() {
        let temp = virtual_workspace();
        let manifest_path = temp.path().join("Cargo.toml");
        let metadata = metadata_for(&manifest_path);

        let member_a = select_package(&metadata, &manifest_path, Some("member_a")).unwrap();
        assert_eq!(member_a.name, "member_a");

        let by_id =
            select_package(&metadata, &manifest_path, Some(&member_a.id.to_string())).unwrap();
        assert_eq!(by_id.id, member_a.id);
    }

    #[test]
    fn virtual_workspace_requires_package_selector() {
        let temp = virtual_workspace();
        let manifest_path = temp.path().join("Cargo.toml");
        let metadata = metadata_for(&manifest_path);

        let err = select_package(&metadata, &manifest_path, None).unwrap_err();
        assert!(err.contains("virtual workspace root"));
        assert!(err.contains("pass --package <name-or-id>"));
    }

    #[test]
    fn unknown_package_selector_reports_workspace_miss() {
        let temp = virtual_workspace();
        let manifest_path = temp.path().join("Cargo.toml");
        let metadata = metadata_for(&manifest_path);

        let err = select_package(&metadata, &manifest_path, Some("missing")).unwrap_err();
        assert_eq!(err, "package 'missing' not found in workspace");
    }

    #[test]
    fn package_spec_preserves_resolved_package_identity() {
        let metadata = metadata();
        let package = select_package(&metadata, Path::new("Cargo.toml"), None).unwrap();
        let deps = package_dependencies(&metadata, &package.id, default_filter());
        let dep = one_dependency(resolve_dependency(
            &metadata.packages,
            &deps,
            "cargo_metadata",
        ));
        let spec = package_spec(dep.package);

        assert_eq!(spec, dep.package.id.to_string());
        assert_ne!(
            spec,
            format!("{}@{}", dep.package.name, dep.package.version)
        );
        assert!(spec.contains("registry+"));
        assert!(spec.contains("#cargo_metadata@"));
    }

    #[test]
    fn resolver_error_paths_are_explicit() {
        let metadata = metadata();
        let package = select_package(&metadata, Path::new("Cargo.toml"), None).unwrap();
        let deps = package_dependencies(&metadata, &package.id, default_filter());

        let missing =
            resolve_dependency(&metadata.packages, &deps, "definitely_missing_crate").unwrap_err();
        assert!(missing.contains("not a direct dependency"));

        let mut broken_deps = deps.clone();
        broken_deps.insert(
            "missing_dep".into(),
            DependencyEntry {
                package_id: deps.entries["cargo_metadata"][0].package_id.clone(),
                contexts: deps.entries["cargo_metadata"][0].contexts.clone(),
            },
        );
        let missing_metadata = resolve_dependency(&[], &broken_deps, "missing_dep").unwrap_err();
        assert!(missing_metadata.contains("missing from cargo metadata"));

        let no_lib = library_target(package).unwrap_err();
        assert!(no_lib.contains("no doc-able library target"));
    }

    #[test]
    fn dependency_lookup_is_empty_for_unknown_package_id() {
        let metadata = metadata();
        let fake = PackageId {
            repr: "path+file:///missing#0.0.0".to_string(),
        };
        assert!(package_dependencies(&metadata, &fake, default_filter()).is_empty());
    }

    #[test]
    fn filters_dev_dependencies_by_default() {
        let mut metadata = metadata();
        let root_id = metadata.root_package().unwrap().id.clone();
        let node = metadata
            .resolve
            .as_mut()
            .unwrap()
            .nodes
            .iter_mut()
            .find(|node| node.id == root_id)
            .unwrap();
        let dep = node
            .deps
            .iter_mut()
            .find(|dep| dep.name == "tempfile")
            .unwrap();
        for kind in &mut dep.dep_kinds {
            kind.kind = DependencyKind::Development;
        }
        let package = select_package(&metadata, Path::new("Cargo.toml"), None).unwrap();

        let default_deps = package_dependencies(&metadata, &package.id, default_filter());
        assert!(!default_deps.contains_key("tempfile"));
        assert_eq!(
            resolve_dependency(&metadata.packages, &default_deps, "tempfile")
                .unwrap_err()
                .to_string(),
            "crate 'tempfile' is declared only in dependency contexts excluded by default; retry with `--include-dev`"
        );

        let dev_deps = package_dependencies(
            &metadata,
            &package.id,
            DependencyFilter {
                include_dev: true,
                include_build: false,
            },
        );
        let dep = one_dependency(resolve_dependency(
            &metadata.packages,
            &dev_deps,
            "tempfile",
        ));
        assert_eq!(dep.contexts[0].kind, DependencyKind::Development);
    }

    #[test]
    fn merging_a_host_graph_preserves_excluded_build_guidance() {
        let mut dependencies = DependencyIndex::default();
        let mut host_dependencies = DependencyIndex::default();
        host_dependencies.excluded.insert(
            "host_build".into(),
            ExcludedDependencyKinds {
                dev: false,
                build: true,
            },
        );

        merge_dependency_kind(&mut dependencies, host_dependencies, DependencyKind::Build);

        assert_eq!(
            resolve_dependency(&[], &dependencies, "host_build")
                .unwrap_err()
                .to_string(),
            "crate 'host_build' is declared only in dependency contexts excluded by default; retry with `--include-build`"
        );
    }

    #[test]
    fn duplicate_dependency_contexts_for_same_package_are_merged() {
        let metadata = metadata();
        let package = select_package(&metadata, Path::new("Cargo.toml"), None).unwrap();
        let mut deps = package_dependencies(&metadata, &package.id, default_filter());
        let package_id = deps.entries["cargo_metadata"][0].package_id.clone();

        push_dependency_context(
            deps.entries.get_mut("cargo_metadata").unwrap(),
            package_id,
            DependencyContext {
                kind: DependencyKind::Development,
                target: None,
            },
        );

        let dep = one_dependency(resolve_dependency(
            &metadata.packages,
            &deps,
            "cargo_metadata",
        ));
        assert_eq!(dep.package.name, "cargo_metadata");
        assert_eq!(
            dep.contexts
                .iter()
                .map(DependencyContext::label)
                .collect::<Vec<_>>(),
            vec!["normal", "dev"]
        );
    }

    #[test]
    fn same_crate_name_can_resolve_to_different_packages_in_disjoint_contexts() {
        let metadata = metadata();
        let package = select_package(&metadata, Path::new("Cargo.toml"), None).unwrap();
        let mut deps = package_dependencies(&metadata, &package.id, default_filter());
        let html_id = metadata
            .packages
            .iter()
            .find(|package| package.name == "html-to-markdown-rs")
            .unwrap()
            .id
            .clone();
        deps.insert(
            "cargo_metadata".into(),
            DependencyEntry {
                package_id: html_id.clone(),
                contexts: vec![DependencyContext {
                    kind: DependencyKind::Development,
                    target: None,
                }],
            },
        );

        let resolved = resolve_dependency(&metadata.packages, &deps, "cargo_metadata").unwrap();
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].package.name, "cargo_metadata");
        assert_eq!(resolved[0].contexts[0].kind, DependencyKind::Normal);
        assert_eq!(resolved[1].package.id, html_id);
        assert_eq!(resolved[1].contexts[0].kind, DependencyKind::Development);
    }

    #[test]
    fn same_crate_name_with_different_package_ids_overlapping_a_context_is_ambiguous() {
        let metadata = metadata();
        let package = select_package(&metadata, Path::new("Cargo.toml"), None).unwrap();
        let mut deps = package_dependencies(&metadata, &package.id, default_filter());
        let html_id = metadata
            .packages
            .iter()
            .find(|package| package.name == "html-to-markdown-rs")
            .unwrap()
            .id
            .clone();
        deps.insert(
            "cargo_metadata".into(),
            DependencyEntry {
                package_id: html_id,
                contexts: vec![DependencyContext {
                    kind: DependencyKind::Normal,
                    target: Some("cfg(unix)".into()),
                }],
            },
        );

        let err = resolve_dependency(&metadata.packages, &deps, "cargo_metadata").unwrap_err();
        assert!(err.contains("matched multiple direct dependencies"));
        assert!(err.contains("normal"));
    }
}
