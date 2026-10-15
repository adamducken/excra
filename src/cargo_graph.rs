use crate::cli::FeatureSelection;
use crate::resolver::package_spec;
use cargo_metadata::{DependencyKind, Metadata, Package, PackageId};
use cargo_platform::{Cfg, Platform};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;

pub(crate) struct TargetPlatform {
    pub(crate) name: String,
    cfg: Vec<Cfg>,
}

impl TargetPlatform {
    pub(crate) fn matches(&self, platform: Option<&Platform>) -> bool {
        platform.is_none_or(|platform| platform.matches(&self.name, &self.cfg))
    }
}

pub(crate) fn platform(
    manifest: &Path,
    metadata: &Metadata,
    target: Option<&str>,
) -> Result<TargetPlatform, String> {
    parse_platform(&probe(
        manifest,
        metadata,
        target,
        &["--print", "cfg", "--print", "target-libdir"],
    )?)
}

pub(crate) fn host_triple(manifest: &Path, metadata: &Metadata) -> Result<String, String> {
    probe(manifest, metadata, None, &["-vV"])?
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| "Cargo's compiler did not report a host target".into())
}

fn probe(
    manifest: &Path,
    metadata: &Metadata,
    target: Option<&str>,
    rustc_args: &[&str],
) -> Result<String, String> {
    let directory = metadata.target_directory.join("excra");
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let probe = tempfile::TempDir::new_in(&directory).map_err(|error| error.to_string())?;
    fs::write(
        probe.path().join("Cargo.toml"),
        "[package]\nname = \"excra-platform\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[lib]\npath = \"lib.rs\"\n[workspace]\n",
    )
    .and_then(|()| fs::write(probe.path().join("lib.rs"), ""))
    .map_err(|error| format!("cannot prepare Cargo platform probe: {error}"))?;
    // Cargo supplies the effective target, compiler, configuration, and flags.
    // --print does not compile the probe or any consuming application code.
    let mut command = Command::new("cargo");
    command
        .current_dir(manifest.parent().expect("manifest has a parent"))
        .args(["rustc", "--offline", "--lib", "--manifest-path"])
        .arg(probe.path().join("Cargo.toml"))
        .arg("--target-dir")
        .arg(directory.join("platform"));
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    let output = command
        .arg("--")
        .args(rustc_args)
        .output()
        .map_err(|error| format!("cannot read Cargo target configuration: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot read Cargo target configuration: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) type UnitKey = (PackageId, bool);

pub(crate) struct Unit {
    pub(crate) package: PackageId,
    pub(crate) host: bool,
    pub(crate) features: Vec<String>,
    dependencies: Vec<(DependencyKind, UnitKey)>,
}

pub(crate) struct FeatureGraph {
    root: UnitKey,
    units: HashMap<UnitKey, Unit>,
    #[cfg(test)]
    output_rows: usize,
}

impl FeatureGraph {
    pub(crate) fn root_is_host(&self) -> bool {
        self.root.1
    }

    pub(crate) fn direct_dependency(
        &self,
        package: &PackageId,
        kind: DependencyKind,
    ) -> Option<UnitKey> {
        self.units[&self.root]
            .dependencies
            .iter()
            .find(|(edge_kind, (id, _))| *edge_kind == kind && id == package)
            .map(|(_, key)| key.clone())
    }

    pub(crate) fn closure(&self, key: UnitKey) -> Result<Vec<&Unit>, String> {
        let mut pending = vec![key];
        let mut visited = HashSet::new();
        let mut units = Vec::new();
        while let Some(key) = pending.pop() {
            if !visited.insert(key.clone()) {
                continue;
            }
            let unit = self
                .units
                .get(&key)
                .ok_or_else(|| format!("package {} missing from Cargo feature context", key.0))?;
            pending.extend(
                unit.dependencies
                    .iter()
                    .filter(|(kind, _)| *kind != DependencyKind::Development)
                    .map(|(_, key)| key.clone()),
            );
            units.push(unit);
        }
        units.sort_by(|left, right| {
            left.package
                .repr
                .cmp(&right.package.repr)
                .then(left.host.cmp(&right.host))
        });
        Ok(units)
    }
}

pub(crate) fn features(
    manifest: &Path,
    metadata: &Metadata,
    root: &Package,
    selection: &FeatureSelection,
    target: Option<&str>,
    include_dev: bool,
) -> Result<FeatureGraph, String> {
    // An explicit target keeps Cargo's host and target nodes distinct even when
    // their own features match. Otherwise Cargo may deduplicate across contexts.
    let target = match target {
        Some(target) => target.to_string(),
        None => platform(manifest, metadata, None)?.name,
    };
    let command = || {
        let mut command = Command::new("cargo");
        command
            .current_dir(manifest.parent().expect("manifest has a parent"))
            .args([
                "tree",
                "--offline",
                "--locked",
                "--color",
                "never",
                "--manifest-path",
            ])
            .arg(&root.manifest_path)
            .args(["--package", &package_spec(root), "--edges"])
            .arg(if include_dev {
                "normal,build,dev"
            } else {
                "normal,build"
            })
            .args(selection.cargo_args())
            .args(["--target", &target]);
        command
    };
    // Repeated rows still carry edges and features; expand each Cargo unit once.
    let output = tree_output(command().args(["--charset", "ascii", "--format", "\t{p}\t{f}"]))?;
    let mut by_name: HashMap<String, Vec<&Package>> = HashMap::new();
    for package in &metadata.packages {
        by_name
            .entry(format!("{} v{}", package.name, package.version))
            .or_default()
            .push(package);
    }
    let mut labels = HashMap::new();
    for line in output
        .lines()
        .filter_map(|line| line.split_once('\t').map(|(_, tail)| tail))
    {
        let label = line.split('\t').next().unwrap_or(line);
        let name = label.split(" (").next().unwrap_or(label);
        let candidates = by_name
            .get(name)
            .ok_or_else(|| format!("Cargo tree package '{label}' missing from metadata"))?;
        if let [package] = candidates.as_slice() {
            labels.insert(label.to_string(), *package);
        } else if !labels.contains_key(label) {
            // Let Cargo identify its display labels for source collisions.
            for package in candidates {
                let mut inverted = command();
                inverted.args([
                    "--invert",
                    &package_spec(package),
                    "--depth",
                    "0",
                    "--prefix",
                    "none",
                    "--format",
                    "{p}",
                ]);
                let result = inverted.output().map_err(|error| error.to_string())?;
                if result.status.success() {
                    for label in String::from_utf8_lossy(&result.stdout).lines() {
                        labels.insert(label.to_string(), *package);
                    }
                }
            }
        }
    }
    parse_tree(&output, &labels)
}

fn tree_output(command: &mut Command) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|error| format!("cannot run cargo tree: {error}"))?;
    if !output.status.success() {
        return Err(crate::cargo_failure(
            "cannot read Cargo feature contexts",
            &String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_tree(output: &str, labels: &HashMap<String, &Package>) -> Result<FeatureGraph, String> {
    let mut units: HashMap<UnitKey, Unit> = HashMap::new();
    let mut stack: Vec<UnitKey> = Vec::new();
    let mut kinds = Vec::new();
    let mut root = None;
    for line in output.lines() {
        let Some((prefix, body)) = line.split_once('\t') else {
            let (offset, kind) = if let Some(offset) = line.find("[build-dependencies]") {
                (offset, DependencyKind::Build)
            } else if let Some(offset) = line.find("[dev-dependencies]") {
                (offset, DependencyKind::Development)
            } else {
                return Err(format!("unexpected cargo tree output: {line}"));
            };
            let entry = kinds
                .get_mut(offset / 4)
                .ok_or_else(|| format!("invalid cargo tree dependency section: {line}"))?;
            *entry = kind;
            continue;
        };
        let depth = prefix.len() / 4;
        if prefix.len() % 4 != 0 || depth > stack.len() {
            return Err(format!("invalid cargo tree depth: {line}"));
        }
        stack.truncate(depth);
        kinds.truncate(depth);
        let (label, feature_names) = body
            .split_once('\t')
            .ok_or_else(|| format!("missing cargo tree features: {line}"))?;
        let package = labels
            .get(label)
            .ok_or_else(|| format!("cannot identify Cargo package '{label}'"))?;
        let kind = kinds.last().copied().unwrap_or(DependencyKind::Normal);
        let host = stack.last().is_some_and(|(_, host)| *host)
            || kind == DependencyKind::Build
            || package
                .targets
                .iter()
                .any(|target| target.kind.iter().any(|kind| kind == "proc-macro"));
        let key = (package.id.clone(), host);
        let features = feature_names
            .strip_suffix(" (*)")
            .unwrap_or(feature_names)
            .split(',')
            .filter(|feature| !feature.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        let unit = units.entry(key.clone()).or_insert_with(|| Unit {
            package: package.id.clone(),
            host,
            features: features.clone(),
            dependencies: Vec::new(),
        });
        if unit.features != features {
            return Err(format!(
                "inconsistent Cargo feature context for {}",
                package.id
            ));
        }
        if let Some(parent) = stack.last() {
            let dependencies = &mut units
                .get_mut(parent)
                .expect("parent unit exists")
                .dependencies;
            if !dependencies.contains(&(kind, key.clone())) {
                dependencies.push((kind, key.clone()));
            }
        } else if root.replace(key.clone()).is_some() {
            return Err("expected one selected Cargo package".into());
        }
        stack.push(key);
        kinds.push(DependencyKind::Normal);
    }
    Ok(FeatureGraph {
        root: root.ok_or_else(|| "Cargo tree did not report a selected package".to_string())?,
        units,
        #[cfg(test)]
        output_rows: output.lines().count(),
    })
}

fn parse_platform(output: &str) -> Result<TargetPlatform, String> {
    let mut cfg = Vec::new();
    let mut name = None;
    for line in output.lines() {
        if let Ok(value) = line.parse::<Cfg>() {
            cfg.push(value);
        } else if let Some(target) = Path::new(line)
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
        {
            if name.replace(target.to_string()).is_some() {
                return Err("expected one Cargo target; use --target for one target".into());
            }
        } else {
            return Err(format!("unexpected Cargo target configuration: {line}"));
        }
    }
    let name = name.ok_or_else(|| "Cargo did not report a target library directory".to_string())?;
    Ok(TargetPlatform { name, cfg })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layered_graphs_keep_all_edges_with_linear_output() {
        for levels in [14, 24] {
            let dir = tempfile::TempDir::new().unwrap();
            let root = dir.path();
            fs::write(
                root.join("Cargo.toml"),
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"lib.rs\"\n[workspace]\nresolver = \"3\"\n[dependencies]\nn0a = { path = \"n0a\" }\nn0b = { path = \"n0b\" }\n",
            )
            .unwrap();
            fs::write(root.join("lib.rs"), "").unwrap();
            for level in 0..levels {
                for side in ["a", "b"] {
                    let name = format!("n{level}{side}");
                    let directory = root.join(&name);
                    fs::create_dir(&directory).unwrap();
                    let mut manifest = format!(
                        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"lib.rs\"\n"
                    );
                    if level + 1 < levels {
                        let next = level + 1;
                        manifest.push_str(&format!(
                            "[dependencies]\nn{next}a = {{ path = \"../n{next}a\" }}\nn{next}b = {{ path = \"../n{next}b\" }}\n"
                        ));
                    }
                    fs::write(directory.join("Cargo.toml"), manifest).unwrap();
                    fs::write(directory.join("lib.rs"), "").unwrap();
                }
            }
            let manifest = root.join("Cargo.toml");
            let metadata = cargo_metadata::MetadataCommand::new()
                .manifest_path(&manifest)
                .other_options(vec!["--offline".into()])
                .exec()
                .unwrap();
            let graph = features(
                &manifest,
                &metadata,
                metadata.root_package().unwrap(),
                &FeatureSelection::default(),
                None,
                false,
            )
            .unwrap();
            assert_eq!(graph.units.len(), 2 * levels + 1);
            assert_eq!(
                graph.closure(graph.root.clone()).unwrap().len(),
                2 * levels + 1
            );
            let edges = graph
                .units
                .values()
                .map(|unit| unit.dependencies.len())
                .sum::<usize>();
            assert_eq!(edges, 4 * levels - 2);
            assert_eq!(graph.output_rows, edges + 1);
        }
    }

    #[test]
    fn deduplicated_trees_preserve_shared_host_and_target_subtrees() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        fs::write(root.join("lib.rs"), "").unwrap();
        for (name, dependencies) in [
            (
                "bridge",
                "[dependencies]\nshared = { path = \"../shared\" }\n",
            ),
            (
                "shared",
                "[dependencies]\nleaf = { path = \"../leaf\" }\n[features]\nhost = [\"leaf/host\"]\ntarget = [\"leaf/target\"]\n",
            ),
            ("leaf", "[features]\nhost = []\ntarget = []\n"),
        ] {
            fs::create_dir(root.join(name)).unwrap();
            fs::write(root.join(name).join("lib.rs"), "").unwrap();
            fs::write(root.join(name).join("Cargo.toml"), format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"lib.rs\"\n{dependencies}"
            )).unwrap();
        }
        let manifest = root.join("Cargo.toml");
        for resolver in ["1", "2", "3"] {
            fs::write(&manifest, format!(
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"lib.rs\"\n[workspace]\nresolver = \"{resolver}\"\n[dependencies]\nbridge = {{ path = \"bridge\" }}\nshared = {{ path = \"shared\", features = [\"target\"] }}\n[build-dependencies]\nbridge = {{ path = \"bridge\" }}\nshared = {{ path = \"shared\", features = [\"host\"] }}\n"
            )).unwrap();
            let metadata = cargo_metadata::MetadataCommand::new()
                .manifest_path(&manifest)
                .other_options(vec!["--offline".into()])
                .exec()
                .unwrap();
            for target in [None, Some("x86_64-pc-windows-gnu")] {
                let graph = features(
                    &manifest,
                    &metadata,
                    metadata.root_package().unwrap(),
                    &FeatureSelection::default(),
                    target,
                    false,
                )
                .unwrap();
                for host in [false, true] {
                    let bridge = metadata
                        .packages
                        .iter()
                        .find(|package| package.name == "bridge")
                        .unwrap();
                    let units = graph.closure((bridge.id.clone(), host)).unwrap();
                    assert_eq!(
                        units.len(),
                        3,
                        "resolver {resolver}, host {host}, {target:?}"
                    );
                    for name in ["shared", "leaf"] {
                        let package = metadata
                            .packages
                            .iter()
                            .find(|package| package.name == name)
                            .unwrap();
                        let unit = units
                            .iter()
                            .find(|unit| unit.package == package.id)
                            .unwrap();
                        assert_eq!(unit.host, host);
                        let expected = if resolver == "1" {
                            vec!["host", "target"]
                        } else if host {
                            vec!["host"]
                        } else {
                            vec!["target"]
                        };
                        assert_eq!(unit.features, expected);
                    }
                }
            }
        }
    }

    #[test]
    fn platform_output_matches_names_and_cfg_and_rejects_multiple_targets() {
        let platform = parse_platform(
            "unix\ntarget_os=\"linux\"\n/opt/rust/lib/rustlib/x86_64-unknown-linux-gnu/lib\n",
        )
        .unwrap();
        assert!(platform.matches(None));
        assert!(platform.matches(Some(&"cfg(unix)".parse().unwrap())));
        assert!(platform.matches(Some(&"x86_64-unknown-linux-gnu".parse().unwrap())));
        assert!(!platform.matches(Some(&"cfg(windows)".parse().unwrap())));
        assert!(parse_platform("").is_err());
        assert!(parse_platform("invalid output").is_err());
        assert!(parse_platform("/rust/lib/triple/lib\n/rust/lib/other/lib\n").is_err());
    }

    #[test]
    fn malformed_feature_trees_and_missing_units_fail_explicitly() {
        let metadata = cargo_metadata::MetadataCommand::new()
            .no_deps()
            .exec()
            .unwrap();
        let package = metadata.root_package().unwrap();
        let labels = HashMap::from([("root".to_string(), package)]);
        for (tree, message) in [
            ("", "did not report a selected package"),
            ("unexpected", "unexpected cargo tree output"),
            (
                "[build-dependencies]",
                "invalid cargo tree dependency section",
            ),
            ("\troot", "missing cargo tree features"),
            ("\tunknown\t", "cannot identify Cargo package"),
            ("    \troot\t", "invalid cargo tree depth"),
            ("x\troot\t", "invalid cargo tree depth"),
            ("\troot\t\n\troot\t", "expected one selected Cargo package"),
            (
                "\troot\ta\n`-- \troot\tb",
                "inconsistent Cargo feature context",
            ),
        ] {
            let error = parse_tree(tree, &labels).err().unwrap();
            assert!(error.contains(message), "{error}");
        }
        let graph = parse_tree("\troot\ta\n`-- \troot\ta", &labels).unwrap();
        assert_eq!(graph.closure((package.id.clone(), false)).unwrap().len(), 1);
        assert!(graph.closure((package.id.clone(), true)).is_err());
    }
}
