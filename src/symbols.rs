use crate::imports::{ImportPath, NamespaceConstraint, identifier_key};
use rustdoc_types::{
    AssocItemConstraintKind, Attribute, AttributeRepr, Crate, GenericArg, GenericArgs,
    GenericBound, GenericParamDefKind, Id, Item, ItemEnum, MacroKind, ReprKind, StructKind, Term,
    TraitBoundModifier, Type, VariantKind, Visibility, WherePredicate,
};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use syn::parse::Parser;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SymbolError {
    NotFound(String),
    Ambiguous(String),
    ExternalReexport(String),
    InvalidRustdoc(String),
}

impl fmt::Display for SymbolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(message)
            | Self::Ambiguous(message)
            | Self::ExternalReexport(message)
            | Self::InvalidRustdoc(message) => formatter.write_str(message),
        }
    }
}

impl Error for SymbolError {}

#[cfg(test)]
impl SymbolError {
    fn contains(&self, needle: &str) -> bool {
        self.to_string().contains(needle)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SymbolDoc {
    pub(crate) path: PathBuf,
    pub(crate) line: usize,
    pub(crate) kind: &'static str,
    pub(crate) name: String,
    pub(crate) definition: String,
    pub(crate) deprecation: Option<DeprecationDoc>,
    pub(crate) attributes: Vec<ReportedAttribute>,
    pub(crate) details: Vec<NestedDoc>,
    pub(crate) docs: Vec<String>,
    pub(crate) derives: Vec<String>,
    pub(crate) methods: Vec<NestedDoc>,
    pub(crate) associated_constants: Vec<NestedDoc>,
    pub(crate) impls: Vec<String>,
    pub(crate) namespaces: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NestedDoc {
    pub(crate) definition: String,
    pub(crate) deprecation: Option<DeprecationDoc>,
    pub(crate) attributes: Vec<ReportedAttribute>,
    pub(crate) docs: Vec<String>,
    pub(crate) children: Vec<NestedDoc>,
}

impl NestedDoc {
    pub(crate) fn has_metadata(&self) -> bool {
        self.deprecation.is_some()
            || !self.attributes.is_empty()
            || !self.docs.is_empty()
            || self.children.iter().any(Self::has_metadata)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeprecationDoc {
    pub(crate) since: Option<String>,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReportedAttribute {
    Repr(AttributeRepr),
    NonExhaustive,
    MustUse { reason: Option<String> },
    TargetFeature { enable: Vec<String> },
}

impl ReportedAttribute {
    pub(crate) fn render(&self) -> String {
        match self {
            Self::Repr(repr) => repr_attribute(repr),
            Self::NonExhaustive => "#[non_exhaustive]".to_string(),
            Self::MustUse { reason: None } => "#[must_use]".to_string(),
            Self::MustUse {
                reason: Some(reason),
            } => format!("#[must_use = {reason:?}]"),
            Self::TargetFeature { enable } => {
                format!("#[target_feature(enable = {:?})]", enable.join(","))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SymbolReport {
    pub(crate) resolved_id: Id,
    pub(crate) imported: SymbolDoc,
    pub(crate) resolved: Option<SymbolDoc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalReexport {
    pub(crate) crate_name: String,
    pub(crate) path: Vec<String>,
    pub(crate) canonical_fallback: Option<ExternalTarget>,
    pub(crate) via_glob: bool,
    pub(crate) namespace: Option<NamespaceConstraint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExternalTarget {
    pub(crate) crate_name: String,
    pub(crate) path: Vec<String>,
}

impl ExternalReexport {
    pub(crate) fn import_path(&self) -> Option<ImportPath> {
        import_path(&self.crate_name, &self.path, self.namespace)
    }

    pub(crate) fn canonical_import_path(&self) -> Option<ImportPath> {
        let fallback = self.canonical_fallback.as_ref()?;
        import_path(&fallback.crate_name, &fallback.path, self.namespace)
    }

    fn extend_path(&mut self, parts: &[String]) {
        self.path.extend(parts.iter().cloned());
        if let Some(fallback) = &mut self.canonical_fallback {
            fallback.path.extend(parts.iter().cloned());
        }
    }
}

fn import_path(
    crate_name: &str,
    path: &[String],
    namespace: Option<NamespaceConstraint>,
) -> Option<ImportPath> {
    let (item, segments) = path.split_last()?;
    ImportPath {
        crate_name: crate_name.to_string(),
        segments: segments.to_vec(),
        item: item.clone(),
        namespace,
    }
    .into()
}

#[cfg(test)]
pub(crate) fn find_symbol(krate: &Crate, import: &ImportPath) -> Result<SymbolDoc, SymbolError> {
    let mut current = krate.root;
    let mut parts = import.segments.clone();
    parts.push(import.item.clone());

    for (index, part) in parts.iter().enumerate() {
        let is_last = index + 1 == parts.len();
        current = find_child(
            krate,
            current,
            part,
            if is_last {
                import.namespace
            } else {
                Some(NamespaceConstraint::Type)
            },
            &mut HashSet::new(),
        )?;
        current = follow_use(krate, current, &mut HashSet::new())?;
        if !is_last && !is_path_container(item(krate, current)?) {
            return Err(SymbolError::NotFound(format!(
                "path segment '{part}' resolved to an item that cannot contain imported names"
            )));
        }
    }

    let item = item(krate, current)?;
    Ok(format_item(krate, item))
}

pub(crate) fn find_symbol_report(
    krate: &Crate,
    import: &ImportPath,
) -> Result<SymbolReport, SymbolError> {
    let mut current = krate.root;
    let mut parts = import.segments.clone();
    parts.push(import.item.clone());

    for (index, part) in parts.iter().enumerate() {
        let is_last = index + 1 == parts.len();
        let child_id = find_child(
            krate,
            current,
            part,
            if is_last {
                import.namespace
            } else {
                Some(NamespaceConstraint::Type)
            },
            &mut HashSet::new(),
        )?;
        let child = item(krate, child_id)?;
        if is_last {
            if matches!(child.inner, ItemEnum::Use(_)) {
                let imported = format_item(krate, child);
                if let Some(resolved) = primitive_reexport_doc(child) {
                    return Ok(SymbolReport {
                        resolved_id: child_id,
                        imported,
                        resolved: Some(resolved),
                    });
                }
                let resolved_id = follow_use(krate, child_id, &mut HashSet::new())?;
                let resolved = format_item(krate, item(krate, resolved_id)?);
                return Ok(SymbolReport {
                    resolved_id,
                    imported,
                    resolved: Some(resolved),
                });
            }
            let id = follow_use(krate, child_id, &mut HashSet::new())?;
            return Ok(SymbolReport {
                resolved_id: id,
                imported: format_item(krate, item(krate, id)?),
                resolved: None,
            });
        }

        current = follow_use(krate, child_id, &mut HashSet::new())?;
        if !is_path_container(item(krate, current)?) {
            return Err(SymbolError::NotFound(format!(
                "path segment '{part}' resolved to an item that cannot contain imported names"
            )));
        }
    }

    Err(SymbolError::NotFound("empty import path".to_string()))
}

pub(crate) fn imported_reexport(
    krate: &Crate,
    import: &ImportPath,
) -> Result<Option<SymbolDoc>, SymbolError> {
    let mut current = krate.root;
    let mut parts = import.segments.clone();
    parts.push(import.item.clone());

    for (index, part) in parts.iter().enumerate() {
        let is_last = index + 1 == parts.len();
        let child_id = find_child(
            krate,
            current,
            part,
            if is_last {
                import.namespace
            } else {
                Some(NamespaceConstraint::Type)
            },
            &mut HashSet::new(),
        )?;
        let child = item(krate, child_id)?;
        if is_last {
            return Ok(matches!(child.inner, ItemEnum::Use(_)).then(|| format_item(krate, child)));
        }
        current = follow_use(krate, child_id, &mut HashSet::new())?;
        if !is_path_container(item(krate, current)?) {
            return Ok(None);
        }
    }

    Ok(None)
}

pub(crate) fn external_reexports(
    krate: &Crate,
    import: &ImportPath,
    root_relative_bare_uses: bool,
) -> Result<Vec<ExternalReexport>, SymbolError> {
    let mut parts = import.segments.clone();
    parts.push(import.item.clone());
    external_candidates(
        krate,
        krate.root,
        &parts,
        import.namespace,
        root_relative_bare_uses,
        &mut HashSet::new(),
    )
}

#[cfg(test)]
pub(crate) fn external_reexport(
    krate: &Crate,
    import: &ImportPath,
) -> Result<Option<ExternalReexport>, SymbolError> {
    let candidates = external_reexports(krate, import, false)?;
    match candidates.as_slice() {
        [] => Ok(None),
        [candidate] => Ok(Some(candidate.clone())),
        _ => Err(SymbolError::Ambiguous(format!(
            "imported name '{}' has multiple external re-export candidates",
            import.item
        ))),
    }
}

fn external_candidates(
    krate: &Crate,
    container_id: Id,
    parts: &[String],
    namespace: Option<NamespaceConstraint>,
    root_relative_bare_uses: bool,
    visited: &mut HashSet<(Id, usize)>,
) -> Result<Vec<ExternalReexport>, SymbolError> {
    let Some((part, tail)) = parts.split_first() else {
        return Ok(Vec::new());
    };
    if !visited.insert((container_id, parts.len())) {
        return Ok(Vec::new());
    }
    let container = item(krate, container_id)?;
    let children = container_children(container).ok_or_else(|| {
        SymbolError::NotFound(format!(
            "item '{}' cannot contain imported names",
            container.name.clone().unwrap_or_default()
        ))
    })?;

    let direct = direct_matching_children(
        krate,
        children,
        part,
        if tail.is_empty() {
            namespace
        } else {
            Some(NamespaceConstraint::Type)
        },
    )?;
    if !tail.is_empty() && has_private_type_binding(krate, children, part)? {
        return Ok(Vec::new());
    }
    let shadows_intermediate = !tail.is_empty() && !direct.is_empty();
    let mut candidates = Vec::new();
    for child_id in direct {
        match follow_use_or_external(
            krate,
            child_id,
            root_relative_bare_uses,
            &mut HashSet::new(),
        )? {
            Followed::External(mut external) => {
                external.extend_path(tail);
                external.namespace = namespace;
                push_external_candidate(&mut candidates, external);
            }
            Followed::Local(id) if !tail.is_empty() => {
                if item(krate, id).is_ok_and(is_path_container) {
                    let mut branch_visited = visited.clone();
                    for external in external_candidates(
                        krate,
                        id,
                        tail,
                        namespace,
                        root_relative_bare_uses,
                        &mut branch_visited,
                    )? {
                        push_external_candidate(&mut candidates, external);
                    }
                }
            }
            Followed::Local(_) => {}
        }
    }

    if shadows_intermediate {
        return Ok(candidates);
    }

    for child_id in children {
        let child = item(krate, *child_id)?;
        let ItemEnum::Use(use_item) = &child.inner else {
            continue;
        };
        if !is_public(child) || !use_item.is_glob {
            continue;
        }
        let Some(_) = use_item.id else {
            continue;
        };
        match follow_use_or_external(
            krate,
            *child_id,
            root_relative_bare_uses,
            &mut HashSet::new(),
        )? {
            Followed::External(mut external) => {
                external.extend_path(parts);
                external.via_glob = true;
                external.namespace = namespace;
                push_external_candidate(&mut candidates, external);
            }
            Followed::Local(id) => {
                if item(krate, id).is_ok_and(is_path_container) {
                    let mut branch_visited = visited.clone();
                    for mut external in external_candidates(
                        krate,
                        id,
                        parts,
                        namespace,
                        root_relative_bare_uses,
                        &mut branch_visited,
                    )? {
                        external.via_glob = true;
                        push_external_candidate(&mut candidates, external);
                    }
                }
            }
        }
    }
    Ok(candidates)
}

fn push_external_candidate(candidates: &mut Vec<ExternalReexport>, candidate: ExternalReexport) {
    if !candidates.contains(&candidate) {
        candidates.push(candidate);
    }
}

enum Followed {
    Local(Id),
    External(ExternalReexport),
}

fn follow_use_or_external(
    krate: &Crate,
    mut id: Id,
    root_relative_bare_uses: bool,
    visited: &mut HashSet<Id>,
) -> Result<Followed, SymbolError> {
    loop {
        if krate
            .index
            .get(&id)
            .is_some_and(|item| !is_cfg_available(item))
        {
            return Err(SymbolError::NotFound(
                "item is not available in the selected non-doc compilation configuration"
                    .to_string(),
            ));
        }
        if !visited.insert(id) {
            return Err(SymbolError::InvalidRustdoc(
                "cycle while following rustdoc use item".to_string(),
            ));
        }
        if krate.index.get(&id).and_then(extern_crate_name) == Some("self") {
            id = krate.root;
            continue;
        }
        if let Some(external) = krate.index.get(&id).and_then(external_from_extern_crate) {
            return Ok(Followed::External(external));
        }
        if let Some(external) = external_from_id(krate, id) {
            return Ok(Followed::External(external));
        }
        let current = item(krate, id)?;
        let ItemEnum::Use(use_item) = &current.inner else {
            return Ok(Followed::Local(id));
        };
        if use_item
            .id
            .and_then(|target| external_from_id(krate, target))
            .is_some()
        {
            match local_use_source_target(krate, id, use_item, root_relative_bare_uses, visited) {
                Ok(Some(Followed::Local(local_target))) => {
                    id = local_target;
                    continue;
                }
                Ok(Some(Followed::External(external))) => {
                    return Ok(Followed::External(external));
                }
                Ok(None) => {}
                Err(SymbolError::InvalidRustdoc(message))
                    if message.contains("is missing path segment") =>
                {
                    if use_source_starts_with_external_crate(krate, use_item) {
                        return external_from_use(krate, use_item)
                            .map(Followed::External)
                            .ok_or_else(|| {
                                SymbolError::InvalidRustdoc(format!(
                                    "use '{}' has an invalid external source path",
                                    use_item.source
                                ))
                            });
                    }
                    // Public Rustdoc strips private modules even when they form the
                    // syntactic path of a valid public external re-export. A missing
                    // first segment can also be a Cargo-renamed extern crate, so keep
                    // the source-level edge and retain the canonical id as fallback.
                    return external_from_use_with_canonical_fallback(krate, use_item)
                        .map(Followed::External)
                        .ok_or(SymbolError::InvalidRustdoc(message));
                }
                Err(SymbolError::InvalidRustdoc(message))
                    if message.contains("traverses extern crate alias") =>
                {
                    // A source-level `extern crate` alias is local syntax rather than
                    // Cargo's effective dependency name. Follow its canonical id.
                    return use_item
                        .id
                        .and_then(|target| external_from_id(krate, target))
                        .map(Followed::External)
                        .ok_or(SymbolError::InvalidRustdoc(message));
                }
                Err(error) => return Err(error),
            }
        }
        if let Some(external) = external_from_use(krate, use_item) {
            return Ok(Followed::External(external));
        }
        let Some(next) = use_item.id else {
            if primitive_reexport_name(use_item).is_some() {
                return Ok(Followed::Local(id));
            }
            return Err(SymbolError::InvalidRustdoc(format!(
                "use '{}' has no resolved id",
                use_item.source
            )));
        };
        id = next;
    }
}

fn use_source_starts_with_external_crate(krate: &Crate, use_item: &rustdoc_types::Use) -> bool {
    let Some(first) = use_item.source.split("::").next() else {
        return false;
    };
    krate
        .external_crates
        .values()
        .any(|external| identifier_key(&external.name) == identifier_key(first))
}

fn local_use_source_target(
    krate: &Crate,
    use_id: Id,
    use_item: &rustdoc_types::Use,
    root_relative_bare_uses: bool,
    visited: &mut HashSet<Id>,
) -> Result<Option<Followed>, SymbolError> {
    let mut parts = use_item.source.split("::").collect::<Vec<_>>();
    if use_item.is_glob && parts.last() == Some(&"*") {
        parts.pop();
    }
    let Some(first) = parts.first().copied() else {
        return Ok(None);
    };

    let mut container = match first {
        "crate" => krate.root,
        "self" | "super" => containing_module(krate, use_id).ok_or_else(|| {
            SymbolError::InvalidRustdoc(format!(
                "use '{}' is missing its containing module",
                use_item.source
            ))
        })?,
        _ if root_relative_bare_uses => krate.root,
        _ => containing_module(krate, use_id).ok_or_else(|| {
            SymbolError::InvalidRustdoc(format!(
                "use '{}' is missing its containing module",
                use_item.source
            ))
        })?,
    };
    let mut offset = usize::from(matches!(first, "crate" | "self" | "super"));
    if first == "super" {
        while parts.get(offset) == Some(&"super") {
            container = containing_module(krate, container).ok_or_else(|| {
                SymbolError::InvalidRustdoc(format!(
                    "use '{}' traverses beyond the crate root",
                    use_item.source
                ))
            })?;
            offset += 1;
        }
        container = containing_module(krate, container).ok_or_else(|| {
            SymbolError::InvalidRustdoc(format!(
                "use '{}' traverses beyond the crate root",
                use_item.source
            ))
        })?;
    }

    let path = &parts[offset..];
    if path.is_empty() {
        return Ok(Some(Followed::Local(container)));
    }
    for (index, name) in path.iter().enumerate() {
        let current = item(krate, container)?;
        let ItemEnum::Module(module) = &current.inner else {
            if matches!(current.inner, ItemEnum::ExternCrate { .. }) {
                return Err(SymbolError::InvalidRustdoc(format!(
                    "local use source '{}' traverses extern crate alias '{}'",
                    use_item.source,
                    current.name.as_deref().unwrap_or("<unnamed>")
                )));
            }
            return Err(SymbolError::InvalidRustdoc(format!(
                "local use path '{}' traverses non-module item '{}'",
                use_item.source,
                current.name.as_deref().unwrap_or("<unnamed>")
            )));
        };
        let mut matches = module
            .items
            .iter()
            .copied()
            .filter(|child_id| {
                *child_id != use_id
                    && krate.index.get(child_id).is_some_and(|child| {
                        exported_name(child).as_deref().is_some_and(|exported| {
                            identifier_key(exported) == identifier_key(name)
                        })
                    })
            })
            .collect::<Vec<_>>();
        if index + 1 != path.len() {
            matches.retain(|child_id| {
                item_namespaces(krate, *child_id)
                    .is_ok_and(|namespaces| namespaces & TYPE_NAMESPACE != 0)
            });
        }
        let matched = match matches.as_slice() {
            [matched] => *matched,
            [] => {
                return Err(SymbolError::InvalidRustdoc(format!(
                    "local use source '{}' is missing path segment '{}'",
                    use_item.source, name
                )));
            }
            _ => return Err(ambiguous_symbol(krate, name, &matches)),
        };
        if index + 1 == path.len() {
            return Ok(Some(Followed::Local(matched)));
        }
        let mut branch_visited = visited.clone();
        match follow_use_or_external(krate, matched, root_relative_bare_uses, &mut branch_visited)?
        {
            Followed::Local(id) if item(krate, id).is_ok_and(is_path_container) => container = id,
            Followed::External(mut external) => {
                external.extend_path(
                    &path[index + 1..]
                        .iter()
                        .map(|part| (*part).to_string())
                        .collect::<Vec<_>>(),
                );
                return Ok(Some(Followed::External(external)));
            }
            Followed::Local(id) => {
                return Err(SymbolError::InvalidRustdoc(format!(
                    "local use path '{}' traverses non-module item '{}'",
                    use_item.source,
                    item(krate, id)?.name.as_deref().unwrap_or("<unnamed>")
                )));
            }
        }
    }
    Ok(Some(Followed::Local(container)))
}

fn containing_module(krate: &Crate, child_id: Id) -> Option<Id> {
    krate.index.iter().find_map(|(id, item)| {
        let ItemEnum::Module(module) = &item.inner else {
            return None;
        };
        module.items.contains(&child_id).then_some(*id)
    })
}

fn external_from_id(krate: &Crate, id: Id) -> Option<ExternalReexport> {
    let summary = krate.paths.get(&id)?;
    let external = krate.external_crates.get(&summary.crate_id)?;
    let path = if summary.path.len() == 1 && summary.path.first() == Some(&external.name) {
        Vec::new()
    } else if summary.path.first() == Some(&external.name) {
        summary.path[1..].to_vec()
    } else {
        summary.path.clone()
    };
    Some(ExternalReexport {
        crate_name: external.name.clone(),
        path,
        canonical_fallback: None,
        via_glob: false,
        namespace: None,
    })
}

fn extern_crate_name(item: &Item) -> Option<&str> {
    let ItemEnum::ExternCrate { name, rename } = &item.inner else {
        return None;
    };
    let binding = item.name.as_deref().map(identifier_key);
    let crate_name = if binding == Some(identifier_key(name)) {
        rename.as_deref().unwrap_or(name)
    } else {
        name
    };
    Some(identifier_key(crate_name))
}

fn external_from_extern_crate(item: &Item) -> Option<ExternalReexport> {
    Some(ExternalReexport {
        crate_name: extern_crate_name(item)?.to_string(),
        path: Vec::new(),
        canonical_fallback: None,
        via_glob: false,
        namespace: None,
    })
}

fn external_from_use(krate: &Crate, use_item: &rustdoc_types::Use) -> Option<ExternalReexport> {
    use_item.id.and_then(|id| external_from_id(krate, id))?;
    let mut source = use_item
        .source
        .split("::")
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    if use_item.is_glob && source.last().is_some_and(|part| part == "*") {
        source.pop();
    }
    let crate_name = identifier_key(source.first()?).to_string();
    Some(ExternalReexport {
        crate_name,
        path: source.into_iter().skip(1).collect(),
        canonical_fallback: None,
        via_glob: use_item.is_glob,
        namespace: None,
    })
}

fn external_from_use_with_canonical_fallback(
    krate: &Crate,
    use_item: &rustdoc_types::Use,
) -> Option<ExternalReexport> {
    let canonical = use_item.id.and_then(|id| external_from_id(krate, id))?;
    let mut source = external_from_use(krate, use_item)?;
    if source.crate_name != canonical.crate_name || source.path != canonical.path {
        source.canonical_fallback = Some(ExternalTarget {
            crate_name: canonical.crate_name,
            path: canonical.path,
        });
    }
    Some(source)
}

pub(crate) fn format_crate_root(krate: &Crate) -> Result<SymbolDoc, SymbolError> {
    let root = item(krate, krate.root)?;
    let mut formatted = format_item(krate, root);
    formatted.kind = "crate";
    formatted.definition = format!(
        "definition rendering unsupported for crate root {}",
        rust_identifier(&formatted.name)
    );
    Ok(formatted)
}

fn find_child(
    krate: &Crate,
    module_id: Id,
    name: &str,
    namespace: Option<NamespaceConstraint>,
    visited: &mut HashSet<Id>,
) -> Result<Id, SymbolError> {
    find_child_with_shadowed_namespaces(krate, module_id, name, namespace, 0, visited)
        .map(|(id, _)| id)
}

fn find_child_with_shadowed_namespaces(
    krate: &Crate,
    module_id: Id,
    name: &str,
    namespace: Option<NamespaceConstraint>,
    shadowed_namespaces: u8,
    visited: &mut HashSet<Id>,
) -> Result<(Id, u8), SymbolError> {
    if !visited.insert(module_id) {
        return Err(SymbolError::InvalidRustdoc(format!(
            "cycle while resolving '{name}'"
        )));
    }
    let module_item = item(krate, module_id)?;
    let children = container_children(module_item).ok_or_else(|| {
        SymbolError::NotFound(format!(
            "item '{}' cannot contain imported names",
            module_item.name.clone().unwrap_or_default()
        ))
    })?;

    let local_private_namespaces = constrained_namespaces(
        private_binding_namespaces(krate, children, name)?,
        namespace,
    );
    let shadowed_namespaces = shadowed_namespaces | local_private_namespaces;
    let mut direct_matches = Vec::new();
    for id in direct_matching_children(krate, children, name, namespace)? {
        let namespaces =
            constrained_namespaces(item_namespaces(krate, id)?, namespace) & !shadowed_namespaces;
        if namespaces != 0 {
            direct_matches.push((id, namespaces));
        }
    }
    match direct_matches.as_slice() {
        [_] => {}
        [] => {}
        _ => {
            return Err(ambiguous_symbol(
                krate,
                name,
                &direct_matches.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ));
        }
    }

    let mut glob_errors = Vec::new();
    let mut glob_matches: Vec<(Id, u8)> = Vec::new();
    let mut glob_identities: HashMap<Id, usize> = HashMap::new();
    for child_id in children {
        let child = item(krate, *child_id)?;
        if !is_public(child) {
            continue;
        }
        let ItemEnum::Use(use_item) = &child.inner else {
            continue;
        };
        if use_item.is_glob {
            let Some(glob_id) = use_item.id else {
                glob_errors.push(format!(
                    "glob import '{}' has no resolved id",
                    use_item.source
                ));
                continue;
            };
            match follow_use(krate, glob_id, &mut HashSet::new()) {
                Ok(target) => match item(krate, target) {
                    Ok(target_item) if is_path_container(target_item) => {
                        let mut branch_visited = visited.clone();
                        match find_child_with_shadowed_namespaces(
                            krate,
                            target,
                            name,
                            namespace,
                            shadowed_namespaces,
                            &mut branch_visited,
                        ) {
                            Ok((found, namespaces)) => {
                                let identity = canonical_item_id(krate, found)?;
                                if let Some(index) = glob_identities.get(&identity) {
                                    glob_matches[*index].1 |= namespaces;
                                } else {
                                    glob_identities.insert(identity, glob_matches.len());
                                    glob_matches.push((found, namespaces));
                                }
                            }
                            Err(err) => glob_errors.push(format!(
                                "glob import '{}' did not resolve '{name}': {err}",
                                use_item.source
                            )),
                        }
                    }
                    Ok(target_item) => glob_errors.push(format!(
                        "glob import '{}' resolved to non-module '{}'",
                        use_item.source,
                        target_item.name.clone().unwrap_or_default()
                    )),
                    Err(err) => glob_errors.push(format!(
                        "glob import '{}' resolved to missing target: {err}",
                        use_item.source
                    )),
                },
                Err(err) => glob_errors.push(format!(
                    "glob import '{}' could not be followed: {err}",
                    use_item.source
                )),
            }
        }
    }

    if let [(direct, direct_namespaces)] = direct_matches.as_slice() {
        let distinct_globs = glob_matches
            .into_iter()
            .filter(|(_, namespaces)| namespaces & !direct_namespaces != 0)
            .collect::<Vec<_>>();
        if distinct_globs.is_empty() {
            return Ok((*direct, *direct_namespaces));
        }
        let mut candidates = vec![*direct];
        candidates.extend(distinct_globs.iter().map(|(id, _)| *id));
        return Err(ambiguous_symbol(krate, name, &candidates));
    }

    match glob_matches.as_slice() {
        [candidate] => return Ok(*candidate),
        [] => {}
        _ => {
            return Err(ambiguous_symbol(
                krate,
                name,
                &glob_matches.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ));
        }
    }

    if local_private_namespaces != 0 {
        return Err(SymbolError::NotFound(format!(
            "'{name}' is private under {}",
            path_label(krate, module_id)
        )));
    }

    let mut message = format!("'{name}' not found under {}", path_label(krate, module_id));
    if !glob_errors.is_empty() {
        message.push_str("; glob branches failed: ");
        message.push_str(&glob_errors.join("; "));
    }
    Err(SymbolError::NotFound(message))
}

const TYPE_NAMESPACE: u8 = 1;
const VALUE_NAMESPACE: u8 = 2;
const MACRO_NAMESPACE: u8 = 4;

fn item_namespaces(krate: &Crate, id: Id) -> Result<u8, SymbolError> {
    item_namespaces_inner(krate, id, &mut HashSet::new())
}

fn item_namespaces_inner(
    krate: &Crate,
    id: Id,
    visited: &mut HashSet<Id>,
) -> Result<u8, SymbolError> {
    if !visited.insert(id) {
        return Err(SymbolError::InvalidRustdoc(
            "cycle while determining Rust namespace".to_string(),
        ));
    }
    if let Some(item) = krate.index.get(&id) {
        return Ok(match &item.inner {
            ItemEnum::Module(_)
            | ItemEnum::Union(_)
            | ItemEnum::Enum(_)
            | ItemEnum::Trait(_)
            | ItemEnum::TraitAlias(_)
            | ItemEnum::TypeAlias(_)
            | ItemEnum::ExternType
            | ItemEnum::Primitive(_)
            | ItemEnum::AssocType { .. } => TYPE_NAMESPACE,
            ItemEnum::Struct(struct_) => struct_namespaces(krate, struct_, &item.attrs),
            ItemEnum::Function(_)
            | ItemEnum::StructField(_)
            | ItemEnum::Constant { .. }
            | ItemEnum::Static(_)
            | ItemEnum::AssocConst { .. } => VALUE_NAMESPACE,
            ItemEnum::Variant(variant) => variant_namespaces(variant, &item.attrs),
            ItemEnum::Macro(_) | ItemEnum::ProcMacro(_) => MACRO_NAMESPACE,
            ItemEnum::Use(use_item) => use_item
                .id
                .map(|id| item_namespaces_inner(krate, id, visited))
                .transpose()?
                .unwrap_or_else(|| {
                    if primitive_reexport_name(use_item).is_some() {
                        TYPE_NAMESPACE
                    } else {
                        TYPE_NAMESPACE | VALUE_NAMESPACE | MACRO_NAMESPACE
                    }
                }),
            ItemEnum::ExternCrate { .. } => TYPE_NAMESPACE,
            ItemEnum::Impl(_) => 0,
        });
    }
    krate
        .paths
        .get(&id)
        .map(|summary| item_kind_namespaces(summary.kind))
        .ok_or_else(|| {
            SymbolError::InvalidRustdoc(format!(
                "rustdoc item id {:?} missing from index and paths",
                id
            ))
        })
}

fn struct_namespaces(krate: &Crate, struct_: &rustdoc_types::Struct, attrs: &[Attribute]) -> u8 {
    let has_public_constructor = !is_non_exhaustive(attrs)
        && match &struct_.kind {
            StructKind::Unit => true,
            StructKind::Tuple(fields) => fields.iter().all(|field| {
                field.is_some_and(|id| {
                    krate
                        .index
                        .get(&id)
                        .is_some_and(|field| matches!(field.visibility, Visibility::Public))
                })
            }),
            StructKind::Plain { .. } => false,
        };
    TYPE_NAMESPACE
        | if has_public_constructor {
            VALUE_NAMESPACE
        } else {
            0
        }
}

fn variant_namespaces(variant: &rustdoc_types::Variant, attrs: &[Attribute]) -> u8 {
    TYPE_NAMESPACE
        | if !is_non_exhaustive(attrs)
            && matches!(variant.kind, VariantKind::Plain | VariantKind::Tuple(_))
        {
            VALUE_NAMESPACE
        } else {
            0
        }
}

fn is_non_exhaustive(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .any(|attr| matches!(attr, Attribute::NonExhaustive))
}

fn item_kind_namespaces(kind: rustdoc_types::ItemKind) -> u8 {
    use rustdoc_types::ItemKind;
    match kind {
        ItemKind::Module
        | ItemKind::ExternCrate
        | ItemKind::Union
        | ItemKind::Enum
        | ItemKind::Trait
        | ItemKind::TraitAlias
        | ItemKind::TypeAlias
        | ItemKind::ExternType
        | ItemKind::Primitive
        | ItemKind::AssocType => TYPE_NAMESPACE,
        ItemKind::Struct | ItemKind::Variant => TYPE_NAMESPACE | VALUE_NAMESPACE,
        ItemKind::StructField
        | ItemKind::Function
        | ItemKind::Constant
        | ItemKind::Static
        | ItemKind::AssocConst => VALUE_NAMESPACE,
        ItemKind::Macro | ItemKind::ProcAttribute | ItemKind::ProcDerive | ItemKind::Attribute => {
            MACRO_NAMESPACE
        }
        ItemKind::Use | ItemKind::Impl | ItemKind::Keyword => 0,
    }
}

fn symbol_namespaces(symbol: &SymbolDoc) -> u8 {
    symbol.namespaces
}

pub(crate) fn report_has_unshadowed_namespace(
    named: &SymbolReport,
    glob: &SymbolReport,
    namespace: Option<NamespaceConstraint>,
) -> bool {
    let named = named.resolved.as_ref().unwrap_or(&named.imported);
    let glob = glob.resolved.as_ref().unwrap_or(&glob.imported);
    constrained_namespaces(symbol_namespaces(glob), namespace)
        & !constrained_namespaces(symbol_namespaces(named), namespace)
        != 0
}

fn constrained_namespaces(namespaces: u8, namespace: Option<NamespaceConstraint>) -> u8 {
    namespaces & namespace.map_or(u8::MAX, namespace_mask)
}

pub(crate) fn report_item_label(report: &SymbolReport) -> String {
    let item = report.resolved.as_ref().unwrap_or(&report.imported);
    format!("{} {}", item.kind, item.name)
}

fn has_private_type_binding(
    krate: &Crate,
    children: &[Id],
    name: &str,
) -> Result<bool, SymbolError> {
    Ok(private_binding_namespaces(krate, children, name)? & TYPE_NAMESPACE != 0)
}

fn private_binding_namespaces(
    krate: &Crate,
    children: &[Id],
    name: &str,
) -> Result<u8, SymbolError> {
    let mut namespaces = 0;
    for id in children {
        let child = item(krate, *id)?;
        if is_cfg_available(child)
            && !is_public(child)
            && !matches!(&child.inner, ItemEnum::Use(use_item) if use_item.is_glob)
            && exported_name(child)
                .as_deref()
                .is_some_and(|exported| identifier_key(exported) == identifier_key(name))
        {
            namespaces |= item_namespaces(krate, *id)?;
        }
    }
    Ok(namespaces)
}

fn direct_matching_children(
    krate: &Crate,
    children: &[Id],
    name: &str,
    namespace: Option<NamespaceConstraint>,
) -> Result<Vec<Id>, SymbolError> {
    let mut matches = Vec::new();
    for child_id in children {
        let child = item(krate, *child_id)?;
        if is_public(child)
            && !matches!(&child.inner, ItemEnum::Use(use_item) if use_item.is_glob)
            && exported_name(child)
                .as_deref()
                .is_some_and(|exported| identifier_key(exported) == identifier_key(name))
            && namespace.is_none_or(|namespace| {
                item_namespaces(krate, *child_id)
                    .is_ok_and(|item_namespaces| item_namespaces & namespace_mask(namespace) != 0)
            })
        {
            matches.push(*child_id);
        }
    }
    Ok(matches)
}

fn namespace_mask(namespace: NamespaceConstraint) -> u8 {
    match namespace {
        NamespaceConstraint::Type => TYPE_NAMESPACE,
    }
}

fn ambiguous_symbol(krate: &Crate, name: &str, ids: &[Id]) -> SymbolError {
    let candidates = ids
        .iter()
        .filter_map(|id| krate.index.get(id))
        .map(|item| {
            format!(
                "{} {}",
                ambiguity_kind(krate, item),
                exported_name(item).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    SymbolError::Ambiguous(format!(
        "imported name '{name}' is ambiguous across Rust namespaces ({candidates}); query a namespace-specific canonical path"
    ))
}

fn ambiguity_kind(krate: &Crate, item: &Item) -> String {
    if let ItemEnum::Use(use_item) = &item.inner
        && let Some(summary) = use_item.id.and_then(|id| krate.paths.get(&id))
    {
        return format!("{:?}", summary.kind);
    }
    kind_name(&item.inner).to_string()
}

fn follow_use(krate: &Crate, mut id: Id, visited: &mut HashSet<Id>) -> Result<Id, SymbolError> {
    loop {
        if !visited.insert(id) {
            return Err(SymbolError::InvalidRustdoc(
                "cycle while following rustdoc use item".to_string(),
            ));
        }
        let current = item(krate, id)?;
        if !is_cfg_available(current) {
            return Err(SymbolError::NotFound(
                "item is not available in the selected non-doc compilation configuration"
                    .to_string(),
            ));
        }
        if extern_crate_name(current) == Some("self") {
            id = krate.root;
            continue;
        }
        let ItemEnum::Use(use_item) = &current.inner else {
            return Ok(id);
        };
        let Some(next) = use_item.id else {
            return Err(SymbolError::InvalidRustdoc(format!(
                "use '{}' has no resolved id",
                use_item.source
            )));
        };
        id = next;
    }
}

fn canonical_item_id(krate: &Crate, mut id: Id) -> Result<Id, SymbolError> {
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(id) {
            return Err(SymbolError::InvalidRustdoc(
                "cycle while following rustdoc use item".to_string(),
            ));
        }
        let Some(current) = krate.index.get(&id) else {
            return krate.paths.contains_key(&id).then_some(id).ok_or_else(|| {
                SymbolError::InvalidRustdoc(format!(
                    "rustdoc item id {:?} missing from index and paths",
                    id
                ))
            });
        };
        if !is_cfg_available(current) {
            return Err(SymbolError::NotFound(
                "item is not available in the selected non-doc compilation configuration"
                    .to_string(),
            ));
        }
        if extern_crate_name(current) == Some("self") {
            id = krate.root;
            continue;
        }
        let ItemEnum::Use(use_item) = &current.inner else {
            return Ok(id);
        };
        let Some(next) = use_item.id else {
            return Ok(id);
        };
        id = next;
    }
}

fn item(krate: &Crate, id: Id) -> Result<&Item, SymbolError> {
    krate.index.get(&id).ok_or_else(|| {
        if let Some(summary) = krate.paths.get(&id) {
            let external = krate
                .external_crates
                .get(&summary.crate_id)
                .map(|krate| krate.name.as_str())
                .unwrap_or("unknown");
            SymbolError::ExternalReexport(format!(
                "rustdoc item id {:?} references external re-export from crate '{}' (path {}); full item data is not present in this crate's rustdoc JSON",
                id,
                external,
                summary.path.join("::")
            ))
        } else {
            SymbolError::InvalidRustdoc(format!(
                "rustdoc item id {:?} missing from index and paths",
                id
            ))
        }
    })
}

fn container_children(item: &Item) -> Option<&[Id]> {
    match &item.inner {
        ItemEnum::Module(module) => Some(&module.items),
        ItemEnum::Enum(enum_) => Some(&enum_.variants),
        _ => None,
    }
}

fn is_path_container(item: &Item) -> bool {
    container_children(item).is_some()
}

fn exported_name(item: &Item) -> Option<String> {
    match &item.inner {
        ItemEnum::Use(use_item) => Some(use_item.name.clone()),
        _ => item.name.clone(),
    }
}

fn is_public(item: &Item) -> bool {
    is_cfg_available(item) && matches!(item.visibility, Visibility::Public | Visibility::Default)
}

fn is_cfg_available(item: &Item) -> bool {
    !item.attrs.iter().any(|attribute| {
        matches!(
            attribute,
            Attribute::Other(attribute)
                if attribute == crate::rustdoc_json::CFG_UNAVAILABLE_ATTRIBUTE
        )
    })
}

fn path_label(krate: &Crate, id: Id) -> String {
    krate
        .paths
        .get(&id)
        .map(|summary| summary.path.join("::"))
        .or_else(|| krate.index.get(&id).and_then(|item| item.name.clone()))
        .unwrap_or_else(|| format!("{:?}", id))
}

fn format_item(krate: &Crate, item: &Item) -> SymbolDoc {
    let name = item
        .name
        .clone()
        .unwrap_or_else(|| exported_name(item).unwrap_or_default());
    let rendered_name = rust_identifier(&name);
    let (kind, definition, details) = match &item.inner {
        ItemEnum::Struct(s) => (
            "struct",
            struct_def(krate, &rendered_name, s),
            struct_details(krate, s),
        ),
        ItemEnum::Enum(e) => (
            "enum",
            enum_def(krate, &rendered_name, e),
            enum_details(krate, e),
        ),
        ItemEnum::Trait(t) => (
            "trait",
            trait_def(krate, &rendered_name, t),
            trait_details(krate, t),
        ),
        ItemEnum::TraitAlias(alias) => (
            "trait alias",
            trait_alias_def(&rendered_name, alias),
            Vec::new(),
        ),
        ItemEnum::Function(f) => ("fn", fn_def(&rendered_name, f), Vec::new()),
        ItemEnum::TypeAlias(t) => (
            "type",
            format!(
                "pub type {}{}{} = {};",
                rendered_name,
                generics(&t.generics),
                where_clause(&t.generics),
                type_str(&t.type_)
            ),
            Vec::new(),
        ),
        ItemEnum::Constant { type_, const_ } => (
            "const",
            constant_def(&rendered_name, type_, const_),
            Vec::new(),
        ),
        ItemEnum::Static(s) => ("static", static_def(&rendered_name, s), Vec::new()),
        ItemEnum::Union(u) => (
            "union",
            union_def(krate, &rendered_name, u),
            union_details(krate, u),
        ),
        ItemEnum::Variant(variant) => (
            "variant",
            variant_def(krate, &rendered_name, variant),
            variant_field_docs(krate, variant),
        ),
        ItemEnum::Macro(source) => (
            "macro",
            if source.trim().is_empty() {
                "definition rendering unsupported for empty declarative macro source".to_string()
            } else {
                source.clone()
            },
            Vec::new(),
        ),
        ItemEnum::ProcMacro(pm) => match pm.kind {
            MacroKind::Bang => (
                "macro",
                format!(
                    "definition rendering unsupported for function-like procedural macro; usage: {rendered_name}!(...)"
                ),
                Vec::new(),
            ),
            MacroKind::Attr => (
                "proc-attribute",
                format!(
                    "definition rendering unsupported for attribute procedural macro; usage: #[{rendered_name}]"
                ),
                Vec::new(),
            ),
            MacroKind::Derive => {
                let helpers = if pm.helpers.is_empty() {
                    String::new()
                } else {
                    format!("; helper attributes: {}", pm.helpers.join(", "))
                };
                (
                    "proc-derive",
                    format!(
                        "definition rendering unsupported for derive procedural macro; usage: #[derive({rendered_name})]{helpers}"
                    ),
                    Vec::new(),
                )
            }
        },
        ItemEnum::Use(u) => (
            "use",
            format!(
                "pub use {} as {};",
                rust_path(&u.source),
                rust_identifier(&u.name)
            ),
            Vec::new(),
        ),
        ItemEnum::Module(_) => ("module", format!("pub mod {rendered_name};"), Vec::new()),
        ItemEnum::ExternCrate {
            name: external_name,
            rename,
        } => (
            "extern crate",
            format!(
                "pub extern crate {}{};",
                rust_identifier(external_name),
                rename
                    .as_ref()
                    .map(|rename| format!(" as {}", rust_identifier(rename)))
                    .unwrap_or_default()
            ),
            Vec::new(),
        ),
        ItemEnum::AssocConst { type_, value } => (
            "assoc const",
            assoc_const_def(&rendered_name, type_, value.as_deref()),
            Vec::new(),
        ),
        ItemEnum::AssocType {
            generics,
            bounds,
            type_,
        } => (
            "assoc type",
            assoc_type_def(&rendered_name, generics, bounds, type_.as_ref()),
            Vec::new(),
        ),
        ItemEnum::StructField(_)
        | ItemEnum::Impl(_)
        | ItemEnum::ExternType
        | ItemEnum::Primitive(_) => (
            kind_name(&item.inner),
            format!(
                "definition rendering unsupported for {}",
                kind_name(&item.inner)
            ),
            Vec::new(),
        ),
    };
    let (methods, associated_constants) = inherent_members(krate, item);
    SymbolDoc {
        path: item
            .span
            .as_ref()
            .map(|span| span.filename.clone())
            .unwrap_or_default(),
        line: item.span.as_ref().map(|span| span.begin.0).unwrap_or(0),
        kind,
        name,
        definition,
        deprecation: item.deprecation.as_ref().map(|deprecation| DeprecationDoc {
            since: deprecation.since.clone(),
            note: deprecation.note.clone(),
        }),
        attributes: reported_attributes(&item.attrs),
        details,
        docs: item
            .docs
            .as_deref()
            .map(|docs| docs.split('\n').map(ToOwned::to_owned).collect())
            .unwrap_or_default(),
        derives: derives(krate, item),
        methods,
        associated_constants,
        impls: impls(krate, item),
        namespaces: item_namespaces(krate, item.id).unwrap_or(0),
    }
}

fn primitive_reexport_name(use_item: &rustdoc_types::Use) -> Option<&str> {
    if use_item.id.is_some() || use_item.is_glob {
        return None;
    }
    let name = use_item.source.rsplit("::").next()?;
    matches!(
        name,
        "bool"
            | "char"
            | "str"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f16"
            | "f32"
            | "f64"
            | "f128"
    )
    .then_some(name)
}

fn primitive_reexport_doc(item: &Item) -> Option<SymbolDoc> {
    let ItemEnum::Use(use_item) = &item.inner else {
        return None;
    };
    let name = primitive_reexport_name(use_item)?;
    Some(SymbolDoc {
        path: PathBuf::new(),
        line: 0,
        kind: "primitive",
        name: name.to_string(),
        definition: format!("definition rendering unsupported for built-in primitive {name}"),
        deprecation: None,
        attributes: Vec::new(),
        details: Vec::new(),
        docs: Vec::new(),
        derives: Vec::new(),
        methods: Vec::new(),
        associated_constants: Vec::new(),
        impls: Vec::new(),
        namespaces: TYPE_NAMESPACE,
    })
}

fn rust_identifier(name: &str) -> String {
    if name.starts_with("r#")
        || matches!(name, "crate" | "self" | "Self" | "super" | "_")
        || !matches!(
            name,
            "as" | "async"
                | "await"
                | "break"
                | "const"
                | "continue"
                | "dyn"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "fn"
                | "for"
                | "gen"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "static"
                | "struct"
                | "trait"
                | "true"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
                | "abstract"
                | "become"
                | "box"
                | "do"
                | "final"
                | "macro"
                | "override"
                | "priv"
                | "try"
                | "typeof"
                | "unsized"
                | "virtual"
                | "yield"
        )
    {
        name.to_string()
    } else {
        format!("r#{name}")
    }
}

fn rust_path(path: &str) -> String {
    path.split("::")
        .map(rust_identifier)
        .collect::<Vec<_>>()
        .join("::")
}

fn rust_binding(binding: &str) -> String {
    let key = binding.strip_prefix("r#").unwrap_or(binding);
    if !key.is_empty()
        && key
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
    {
        rust_identifier(binding)
    } else {
        binding.to_string()
    }
}

fn reported_attributes(attrs: &[Attribute]) -> Vec<ReportedAttribute> {
    attrs
        .iter()
        .filter_map(|attr| match attr {
            Attribute::Repr(repr) => Some(ReportedAttribute::Repr(repr.clone())),
            Attribute::NonExhaustive => Some(ReportedAttribute::NonExhaustive),
            Attribute::MustUse { reason } => Some(ReportedAttribute::MustUse {
                reason: reason.clone(),
            }),
            Attribute::TargetFeature { enable } => Some(ReportedAttribute::TargetFeature {
                enable: enable.clone(),
            }),
            _ => None,
        })
        .collect()
}

fn repr_attribute(repr: &AttributeRepr) -> String {
    let mut arguments = Vec::new();
    match repr.kind {
        ReprKind::Rust if repr.int.is_none() && repr.align.is_none() && repr.packed.is_none() => {
            arguments.push("Rust".to_string());
        }
        ReprKind::Rust => {}
        ReprKind::C => arguments.push("C".to_string()),
        ReprKind::Transparent => arguments.push("transparent".to_string()),
        ReprKind::Simd => arguments.push("simd".to_string()),
    }
    if let Some(int) = &repr.int {
        arguments.push(int.clone());
    }
    if let Some(packed) = repr.packed {
        arguments.push(format!("packed({packed})"));
    }
    if let Some(align) = repr.align {
        arguments.push(format!("align({align})"));
    }
    format!("#[repr({})]", arguments.join(", "))
}

fn derives(krate: &Crate, item: &Item) -> Vec<String> {
    let mut derives = derive_attrs(&item.attrs);
    for impl_id in impl_ids(item) {
        let Some(impl_item) = krate.index.get(&impl_id) else {
            continue;
        };
        let ItemEnum::Impl(imp) = &impl_item.inner else {
            continue;
        };
        if !impl_item
            .attrs
            .iter()
            .any(|attr| matches!(attr, Attribute::AutomaticallyDerived))
        {
            continue;
        }
        let Some(trait_) = &imp.trait_ else {
            continue;
        };
        let name = trait_.path.rsplit("::").next().unwrap_or(&trait_.path);
        if name != "StructuralPartialEq" && !derives.iter().any(|existing| existing == name) {
            derives.push(name.to_string());
        }
    }
    derives.sort();
    derives
}

fn derive_attrs(attrs: &[Attribute]) -> Vec<String> {
    let mut derives = Vec::new();
    for attr in attrs {
        let Attribute::Other(text) = attr else {
            continue;
        };
        let Some(expression) = top_level_derive_expression(text) else {
            continue;
        };
        let Ok(paths) = syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated
            .parse_str(expression)
        else {
            continue;
        };
        derives.extend(
            paths
                .iter()
                .filter(|path| {
                    !path.segments.is_empty()
                        && path
                            .segments
                            .iter()
                            .all(|segment| matches!(segment.arguments, syn::PathArguments::None))
                })
                .map(|path| {
                    path.segments
                        .iter()
                        .map(|segment| segment.ident.to_string())
                        .collect::<Vec<_>>()
                        .join("::")
                }),
        );
    }
    derives.sort();
    derives.dedup();
    derives
}

fn top_level_derive_expression(attribute: &str) -> Option<&str> {
    attribute
        .strip_prefix("#[derive(")
        .or_else(|| attribute.strip_prefix("#[<derive>("))?
        .strip_suffix(")]")
}

fn nested_doc(item: &Item, definition: String, children: Vec<NestedDoc>) -> NestedDoc {
    NestedDoc {
        definition,
        deprecation: item.deprecation.as_ref().map(|deprecation| DeprecationDoc {
            since: deprecation.since.clone(),
            note: deprecation.note.clone(),
        }),
        attributes: reported_attributes(&item.attrs),
        docs: item
            .docs
            .as_deref()
            .map(|docs| docs.split('\n').map(ToOwned::to_owned).collect())
            .unwrap_or_default(),
        children,
    }
}

fn inherent_members(krate: &Crate, item: &Item) -> (Vec<NestedDoc>, Vec<NestedDoc>) {
    let mut methods = Vec::new();
    let mut associated_constants = Vec::new();
    for impl_id in impl_ids(item) {
        let Some(impl_item) = krate.index.get(&impl_id) else {
            continue;
        };
        let ItemEnum::Impl(imp) = &impl_item.inner else {
            continue;
        };
        if imp.trait_.is_some() || imp.is_negative || imp.is_synthetic {
            continue;
        }
        for item_id in &imp.items {
            let Some(method) = krate.index.get(item_id) else {
                continue;
            };
            if !is_public(method) {
                continue;
            }
            let Some(name) = method.name.as_deref() else {
                continue;
            };
            let definition = match &method.inner {
                ItemEnum::Function(function) => fn_def(&rust_identifier(name), function),
                ItemEnum::AssocConst { type_, value } => format!(
                    "pub {}",
                    assoc_const_def(&rust_identifier(name), type_, value.as_deref())
                ),
                _ => continue,
            };
            let definition = format!(
                "impl{} {}{} {{ {definition} }}",
                generics(&imp.generics),
                type_str(&imp.for_),
                where_clause(&imp.generics)
            );
            let rendered = nested_doc(method, definition, Vec::new());
            match method.inner {
                ItemEnum::Function(_) => methods.push(rendered),
                ItemEnum::AssocConst { .. } => associated_constants.push(rendered),
                _ => unreachable!(),
            }
        }
    }
    methods.sort_by(|left, right| left.definition.cmp(&right.definition));
    associated_constants.sort_by(|left, right| left.definition.cmp(&right.definition));
    (methods, associated_constants)
}

fn impls(krate: &Crate, item: &Item) -> Vec<String> {
    let mut impls = Vec::new();
    for impl_id in impl_ids(item) {
        let Some(impl_item) = krate.index.get(&impl_id) else {
            continue;
        };
        let ItemEnum::Impl(imp) = &impl_item.inner else {
            continue;
        };
        if imp.is_synthetic || imp.blanket_impl.is_some() {
            continue;
        }
        let Some(trait_) = &imp.trait_ else {
            continue;
        };
        let safety = if imp.is_unsafe { "unsafe " } else { "" };
        let generics = generics(&imp.generics);
        let where_clause = where_clause(&imp.generics);
        if imp.is_negative {
            impls.push(format!(
                "{safety}impl{generics} !{} for {}{where_clause}",
                format_path(trait_),
                type_str(&imp.for_)
            ));
        } else {
            impls.push(format!(
                "{safety}impl{generics} {} for {}{where_clause}",
                format_path(trait_),
                type_str(&imp.for_)
            ));
        }
    }
    impls.sort();
    impls.dedup();
    impls
}

fn impl_ids(item: &Item) -> Vec<Id> {
    match &item.inner {
        ItemEnum::Struct(s) => s.impls.clone(),
        ItemEnum::Enum(e) => e.impls.clone(),
        ItemEnum::Union(u) => u.impls.clone(),
        _ => Vec::new(),
    }
}

fn kind_name(inner: &ItemEnum) -> &'static str {
    match inner {
        ItemEnum::Module(_) => "module",
        ItemEnum::ExternCrate { .. } => "extern crate",
        ItemEnum::Use(_) => "use",
        ItemEnum::Struct(_) => "struct",
        ItemEnum::Union(_) => "union",
        ItemEnum::Enum(_) => "enum",
        ItemEnum::Function(_) => "fn",
        ItemEnum::Trait(_) => "trait",
        ItemEnum::StructField(_) => "field",
        ItemEnum::Variant(_) => "variant",
        ItemEnum::TraitAlias(_) => "trait alias",
        ItemEnum::Impl(_) => "impl",
        ItemEnum::TypeAlias(_) => "type",
        ItemEnum::Constant { .. } => "const",
        ItemEnum::Static(_) => "static",
        ItemEnum::Macro(_) => "macro",
        ItemEnum::ProcMacro(proc_macro) => match proc_macro.kind {
            MacroKind::Bang => "proc macro",
            MacroKind::Attr => "attribute macro",
            MacroKind::Derive => "derive macro",
        },
        ItemEnum::ExternType => "extern type",
        ItemEnum::Primitive(_) => "primitive",
        ItemEnum::AssocConst { .. } => "assoc const",
        ItemEnum::AssocType { .. } => "assoc type",
    }
}

fn struct_def(krate: &Crate, name: &str, s: &rustdoc_types::Struct) -> String {
    match &s.kind {
        StructKind::Unit => format!(
            "pub struct {name}{}{};",
            generics(&s.generics),
            where_clause(&s.generics)
        ),
        StructKind::Tuple(fields) => format!(
            "pub struct {name}{}({}){};",
            generics(&s.generics),
            fields
                .iter()
                .map(|id| {
                    id.and_then(|id| field_type(krate, id, FieldContext::TypeDefinition))
                        .unwrap_or_else(|| "/* private/stripped field */".into())
                })
                .collect::<Vec<_>>()
                .join(", "),
            where_clause(&s.generics)
        ),
        StructKind::Plain {
            fields,
            has_stripped_fields,
        } => {
            let mut fields = fields
                .iter()
                .filter_map(|id| field_line(krate, *id, FieldContext::TypeDefinition))
                .collect::<Vec<_>>();
            if *has_stripped_fields {
                fields.push("/* private/stripped fields */".to_string());
            }
            format!(
                "pub struct {name}{}{} {{ {} }}",
                generics(&s.generics),
                where_clause(&s.generics),
                fields.join(", ")
            )
        }
    }
}

fn struct_details(krate: &Crate, s: &rustdoc_types::Struct) -> Vec<NestedDoc> {
    match &s.kind {
        StructKind::Plain {
            fields,
            has_stripped_fields,
        } => {
            let mut details = fields
                .iter()
                .filter_map(|id| field_doc(krate, *id, FieldContext::TypeDefinition))
                .collect::<Vec<_>>();
            if *has_stripped_fields {
                details.push(plain_nested_doc("fields: private/stripped"));
            }
            details
        }
        StructKind::Tuple(fields) => {
            let mut details = fields
                .iter()
                .enumerate()
                .filter_map(|(i, id)| {
                    id.and_then(|id| {
                        let field = krate.index.get(&id)?;
                        field_type(krate, id, FieldContext::TypeDefinition)
                            .map(|ty| nested_doc(field, format!("#{i}: {ty}"), Vec::new()))
                    })
                })
                .collect::<Vec<_>>();
            if fields.iter().any(Option::is_none) {
                details.push(plain_nested_doc("fields: private/stripped"));
            }
            details
        }
        StructKind::Unit => Vec::new(),
    }
}

fn enum_def(krate: &Crate, name: &str, e: &rustdoc_types::Enum) -> String {
    let mut variants = enum_variant_defs(krate, e);
    if e.has_stripped_variants {
        variants.push("/* private/stripped variants */".to_string());
    }
    format!(
        "pub enum {name}{}{} {{ {} }}",
        generics(&e.generics),
        where_clause(&e.generics),
        variants.join(", ")
    )
}

fn enum_variant_defs(krate: &Crate, e: &rustdoc_types::Enum) -> Vec<String> {
    e.variants
        .iter()
        .filter_map(|id| {
            let item = krate.index.get(id)?;
            let name = item.name.clone()?;
            let ItemEnum::Variant(v) = &item.inner else {
                return Some(rust_identifier(&name));
            };
            Some(variant_def(krate, &rust_identifier(&name), v))
        })
        .collect()
}

fn enum_details(krate: &Crate, e: &rustdoc_types::Enum) -> Vec<NestedDoc> {
    let mut details: Vec<NestedDoc> = e
        .variants
        .iter()
        .filter_map(|id| {
            let item = krate.index.get(id)?;
            let name = rust_identifier(item.name.as_deref()?);
            let ItemEnum::Variant(variant) = &item.inner else {
                return Some(nested_doc(item, name, Vec::new()));
            };
            Some(nested_doc(
                item,
                variant_def(krate, &name, variant),
                variant_field_docs(krate, variant),
            ))
        })
        .collect();
    if e.has_stripped_variants {
        details.push(plain_nested_doc("variants: private/stripped"));
    }
    details
}

fn trait_def(_krate: &Crate, name: &str, t: &rustdoc_types::Trait) -> String {
    let prefix = match (t.is_unsafe, t.is_auto) {
        (true, true) => "pub unsafe auto trait",
        (true, false) => "pub unsafe trait",
        (false, true) => "pub auto trait",
        (false, false) => "pub trait",
    };
    let bounds = if t.bounds.is_empty() {
        String::new()
    } else {
        format!(": {}", bounds_str(&t.bounds))
    };
    format!(
        "{prefix} {name}{}{bounds}{} {{ ... }}",
        generics(&t.generics),
        where_clause(&t.generics)
    )
}

fn trait_alias_def(name: &str, alias: &rustdoc_types::TraitAlias) -> String {
    format!(
        "pub trait {name}{} = {}{};",
        generics(&alias.generics),
        bounds_str(&alias.params),
        where_clause(&alias.generics)
    )
}

fn variant_def(krate: &Crate, name: &str, variant: &rustdoc_types::Variant) -> String {
    let definition = match &variant.kind {
        VariantKind::Plain => name.to_string(),
        VariantKind::Tuple(fields) => format!(
            "{name}({})",
            fields
                .iter()
                .map(|field| {
                    field
                        .and_then(|id| field_type(krate, id, FieldContext::EnumVariant))
                        .unwrap_or_else(|| "/* private/stripped field */".to_string())
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        VariantKind::Struct {
            fields,
            has_stripped_fields,
        } => {
            let mut fields = fields
                .iter()
                .filter_map(|id| field_line(krate, *id, FieldContext::EnumVariant))
                .collect::<Vec<_>>();
            if *has_stripped_fields {
                fields.push("/* private/stripped fields */".to_string());
            }
            format!("{name} {{ {} }}", fields.join(", "))
        }
    };
    match &variant.discriminant {
        Some(discriminant) => format!(
            "{definition} = {}",
            constant_expression(&discriminant.expr, Some(&discriminant.value))
                .expect("discriminants always have an evaluated value")
        ),
        None => definition,
    }
}

fn trait_details(krate: &Crate, t: &rustdoc_types::Trait) -> Vec<NestedDoc> {
    t.items
        .iter()
        .filter_map(|id| krate.index.get(id))
        .filter_map(|item| {
            let name = rust_identifier(item.name.as_deref().unwrap_or_default());
            match &item.inner {
                ItemEnum::Function(f) => Some(trait_fn_def(&name, f)),
                ItemEnum::AssocType {
                    generics,
                    bounds,
                    type_,
                } => Some(assoc_type_def(&name, generics, bounds, type_.as_ref())),
                ItemEnum::AssocConst { type_, value } => {
                    Some(assoc_const_def(&name, type_, value.as_deref()))
                }
                _ => None,
            }
            .map(|definition| nested_doc(item, definition, Vec::new()))
        })
        .collect()
}

fn fn_def(name: &str, f: &rustdoc_types::Function) -> String {
    function_def(name, f, FunctionContext::PublicItem)
}

fn trait_fn_def(name: &str, f: &rustdoc_types::Function) -> String {
    function_def(name, f, FunctionContext::TraitItem)
}

#[derive(Debug, Clone, Copy)]
enum FunctionContext {
    PublicItem,
    TraitItem,
}

fn function_def(name: &str, f: &rustdoc_types::Function, context: FunctionContext) -> String {
    let mut prefix = String::new();
    if matches!(context, FunctionContext::PublicItem) {
        prefix.push_str("pub ");
    }
    if f.header.is_const {
        prefix.push_str("const ");
    }
    if f.header.is_async {
        prefix.push_str("async ");
    }
    if f.header.is_unsafe {
        prefix.push_str("unsafe ");
    }
    prefix.push_str(&abi_str(&f.header.abi));
    let inputs = f
        .sig
        .inputs
        .iter()
        .map(|(name, ty)| format!("{}: {}", rust_binding(name), type_str(ty)))
        .collect::<Vec<_>>();
    let inputs = signature_inputs(inputs, f.sig.is_c_variadic);
    let output = f
        .sig
        .output
        .as_ref()
        .map(|ty| format!(" -> {}", type_str(ty)))
        .unwrap_or_default();
    let mut rendered = format!(
        "{prefix}fn {name}{}({inputs}){output}{}",
        generics(&f.generics),
        where_clause(&f.generics)
    );
    if matches!(context, FunctionContext::TraitItem) {
        if f.has_body {
            rendered.push_str(" { ... }");
        } else {
            rendered.push(';');
        }
    }
    rendered
}

fn union_def(krate: &Crate, name: &str, u: &rustdoc_types::Union) -> String {
    let mut fields = u
        .fields
        .iter()
        .filter_map(|id| field_line(krate, *id, FieldContext::TypeDefinition))
        .collect::<Vec<_>>();
    if u.has_stripped_fields {
        fields.push("/* private/stripped fields */".to_string());
    }
    format!(
        "pub union {name}{}{} {{ {} }}",
        generics(&u.generics),
        where_clause(&u.generics),
        fields.join(", ")
    )
}

fn assoc_type_def(
    name: &str,
    generics: &rustdoc_types::Generics,
    bounds: &[GenericBound],
    type_: Option<&Type>,
) -> String {
    let bounds = if bounds.is_empty() {
        String::new()
    } else {
        format!(": {}", bounds_str(bounds))
    };
    let default = type_
        .map(|ty| format!(" = {}", type_str(ty)))
        .unwrap_or_default();
    format!(
        "type {name}{}{bounds}{}{};",
        self::generics(generics),
        where_clause(generics),
        default
    )
}

fn union_details(krate: &Crate, u: &rustdoc_types::Union) -> Vec<NestedDoc> {
    let mut details = u
        .fields
        .iter()
        .filter_map(|id| field_doc(krate, *id, FieldContext::TypeDefinition))
        .collect::<Vec<_>>();
    if u.has_stripped_fields {
        details.push(plain_nested_doc("fields: private/stripped"));
    }
    details
}

#[derive(Debug, Clone, Copy)]
enum FieldContext {
    TypeDefinition,
    EnumVariant,
}

fn field_line(krate: &Crate, id: Id, context: FieldContext) -> Option<String> {
    let field = krate.index.get(&id)?;
    let ItemEnum::StructField(ty) = &field.inner else {
        return None;
    };
    Some(format!(
        "{}{}: {}",
        field_visibility(&field.visibility, context),
        field
            .name
            .as_deref()
            .map(rust_identifier)
            .unwrap_or_else(|| "_".into()),
        type_str(ty)
    ))
}

fn field_doc(krate: &Crate, id: Id, context: FieldContext) -> Option<NestedDoc> {
    let field = krate.index.get(&id)?;
    Some(nested_doc(
        field,
        field_line(krate, id, context)?,
        Vec::new(),
    ))
}

fn variant_field_docs(krate: &Crate, variant: &rustdoc_types::Variant) -> Vec<NestedDoc> {
    match &variant.kind {
        VariantKind::Plain => Vec::new(),
        VariantKind::Tuple(fields) => fields
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                let id = (*id)?;
                let field = krate.index.get(&id)?;
                field_type(krate, id, FieldContext::EnumVariant)
                    .map(|ty| nested_doc(field, format!("#{index}: {ty}"), Vec::new()))
            })
            .collect(),
        VariantKind::Struct { fields, .. } => fields
            .iter()
            .filter_map(|id| field_doc(krate, *id, FieldContext::EnumVariant))
            .collect(),
    }
}

fn plain_nested_doc(definition: &str) -> NestedDoc {
    NestedDoc {
        definition: definition.to_string(),
        deprecation: None,
        attributes: Vec::new(),
        docs: Vec::new(),
        children: Vec::new(),
    }
}

fn field_type(krate: &Crate, id: Id, context: FieldContext) -> Option<String> {
    let field = krate.index.get(&id)?;
    let ItemEnum::StructField(ty) = &field.inner else {
        return None;
    };
    Some(format!(
        "{}{}",
        field_visibility(&field.visibility, context),
        type_str(ty)
    ))
}

fn field_visibility(visibility: &Visibility, context: FieldContext) -> String {
    if matches!(context, FieldContext::EnumVariant) {
        return String::new();
    }
    match visibility {
        Visibility::Public => "pub ".to_string(),
        Visibility::Default => String::new(),
        Visibility::Crate => "pub(crate) ".to_string(),
        Visibility::Restricted { path, .. } => format!("pub(in {}) ", rust_path(path)),
    }
}

fn assoc_const_def(name: &str, type_: &Type, value: Option<&str>) -> String {
    format!(
        "const {name}: {}{};",
        type_str(type_),
        value
            .map(|value| {
                format!(
                    " = {}",
                    constant_expression(value, None)
                        .unwrap_or("/* unsupported constant expression */")
                )
            })
            .unwrap_or_default()
    )
}

fn constant_initializer(constant: &rustdoc_types::Constant) -> Option<&str> {
    constant_expression(&constant.expr, constant.value.as_deref())
}

fn constant_expression<'a>(expr: &'a str, value: Option<&'a str>) -> Option<&'a str> {
    if matches!(expr.trim(), "_" | "{ _ }") {
        value
    } else {
        Some(expr)
    }
}

fn constant_def(name: &str, type_: &Type, constant: &rustdoc_types::Constant) -> String {
    match constant_initializer(constant) {
        Some(initializer) => format!("pub const {name}: {} = {initializer};", type_str(type_)),
        None => format!(
            "definition rendering unsupported for constant initializer; item: const {name}: {}",
            type_str(type_)
        ),
    }
}

fn static_def(name: &str, static_: &rustdoc_types::Static) -> String {
    let mutable = if static_.is_mutable { "mut " } else { "" };
    if static_.is_unsafe || static_.expr.is_empty() {
        let safety = if static_.is_unsafe {
            "unsafe "
        } else {
            "safe "
        };
        return format!(
            "definition rendering unsupported as standalone Rust for {safety}extern static; declaration inside extern block: static {mutable}{name}: {};",
            type_str(&static_.type_)
        );
    }
    if matches!(static_.expr.as_str(), "_" | "{ _ }") {
        return format!(
            "definition rendering unsupported for static initializer; item: static {mutable}{name}: {}",
            type_str(&static_.type_)
        );
    }
    format!(
        "pub static {mutable}{name}: {} = {};",
        type_str(&static_.type_),
        static_.expr
    )
}

fn generics(g: &rustdoc_types::Generics) -> String {
    let params = g
        .params
        .iter()
        .filter(|p| {
            !matches!(
                &p.kind,
                GenericParamDefKind::Type {
                    is_synthetic: true,
                    ..
                }
            )
        })
        .map(generic_param_decl)
        .collect::<Vec<_>>();
    if params.is_empty() {
        String::new()
    } else {
        format!("<{}>", params.join(", "))
    }
}

fn generic_param_decl(p: &rustdoc_types::GenericParamDef) -> String {
    match &p.kind {
        GenericParamDefKind::Lifetime { outlives } => {
            let name = lifetime_str(&p.name);
            if outlives.is_empty() {
                name
            } else {
                format!(
                    "{name}: {}",
                    outlives
                        .iter()
                        .map(|l| lifetime_str(l))
                        .collect::<Vec<_>>()
                        .join(" + ")
                )
            }
        }
        GenericParamDefKind::Type {
            bounds, default, ..
        } => {
            let mut param = rust_identifier(&p.name);
            if !bounds.is_empty() {
                param.push_str(&format!(": {}", bounds_str(bounds)));
            }
            if let Some(default) = default {
                param.push_str(&format!(" = {}", type_str(default)));
            }
            param
        }
        GenericParamDefKind::Const { type_, default } => {
            let mut param = format!("const {}: {}", rust_identifier(&p.name), type_str(type_));
            if let Some(default) = default {
                param.push_str(&format!(
                    " = {}",
                    constant_expression(default, None)
                        .unwrap_or("/* unsupported constant expression */")
                ));
            }
            param
        }
    }
}

fn where_clause(g: &rustdoc_types::Generics) -> String {
    let predicates = g
        .where_predicates
        .iter()
        .filter_map(where_predicate_str)
        .collect::<Vec<_>>();
    if predicates.is_empty() {
        String::new()
    } else {
        format!(" where {}", predicates.join(", "))
    }
}

fn where_predicate_str(predicate: &WherePredicate) -> Option<String> {
    match predicate {
        WherePredicate::BoundPredicate {
            type_,
            bounds,
            generic_params,
        } => {
            if bounds.is_empty() {
                return None;
            }
            let binder = if generic_params.is_empty() {
                String::new()
            } else {
                format!("for<{}> ", generic_param_decls(generic_params))
            };
            Some(format!(
                "{binder}{}: {}",
                type_str(type_),
                bounds_str(bounds)
            ))
        }
        WherePredicate::LifetimePredicate { lifetime, outlives } => {
            if outlives.is_empty() {
                return None;
            }
            Some(format!(
                "{}: {}",
                lifetime_str(lifetime),
                outlives
                    .iter()
                    .map(|l| lifetime_str(l))
                    .collect::<Vec<_>>()
                    .join(" + ")
            ))
        }
        WherePredicate::EqPredicate { lhs, rhs } => {
            Some(format!("{} = {}", type_str(lhs), term_str(rhs)))
        }
    }
}

fn pointee_str(ty: &Type) -> String {
    let needs_parens = match ty {
        Type::DynTrait(object) => object.traits.len() + usize::from(object.lifetime.is_some()) > 1,
        Type::ImplTrait(bounds) => bounds.len() > 1,
        _ => false,
    };
    let rendered = type_str(ty);
    if needs_parens {
        format!("({rendered})")
    } else {
        rendered
    }
}

fn type_str(ty: &Type) -> String {
    match ty {
        Type::ResolvedPath(path) => format_path(path),
        Type::DynTrait(dyn_trait) => {
            let traits = dyn_trait
                .traits
                .iter()
                .map(|t| {
                    let bound = format_path(&t.trait_);
                    if t.generic_params.is_empty() {
                        bound
                    } else {
                        format!("for<{}> {bound}", generic_param_names(&t.generic_params))
                    }
                })
                .chain(dyn_trait.lifetime.as_ref().map(|l| lifetime_str(l)))
                .collect::<Vec<_>>()
                .join(" + ");
            format!("dyn {traits}")
        }
        Type::Generic(name) => rust_identifier(name),
        Type::Primitive(name) => name.clone(),
        Type::FunctionPointer(fp) => fn_pointer_str(fp),
        Type::Tuple(items) => {
            let inner = items.iter().map(type_str).collect::<Vec<_>>().join(", ");
            if items.len() == 1 {
                format!("({inner},)")
            } else {
                format!("({inner})")
            }
        }
        Type::Slice(inner) => format!("[{}]", type_str(inner)),
        Type::Array { type_, len } => format!(
            "[{}; {}]",
            type_str(type_),
            constant_expression(len, None).unwrap_or("/* unsupported constant expression */")
        ),
        Type::Pat {
            type_,
            __pat_unstable_do_not_use: pattern,
        } => format!("{} is {pattern}", type_str(type_)),
        Type::ImplTrait(bounds) => format!("impl {}", bounds_str(bounds)),
        Type::Infer => "_".to_string(),
        Type::RawPointer { is_mutable, type_ } => format!(
            "*{} {}",
            if *is_mutable { "mut" } else { "const" },
            pointee_str(type_)
        ),
        Type::BorrowedRef {
            lifetime,
            is_mutable,
            type_,
        } => format!(
            "&{}{}{}",
            lifetime
                .as_ref()
                .map(|l| format!("{} ", lifetime_str(l)))
                .unwrap_or_default(),
            if *is_mutable { "mut " } else { "" },
            pointee_str(type_)
        ),
        Type::QualifiedPath {
            name,
            args,
            self_type,
            trait_,
        } => {
            let name = format!(
                "{}{}",
                rust_identifier(name),
                args.as_deref().map(args_str).unwrap_or_default()
            );
            if let Some(trait_) = trait_ {
                let trait_path = format_path(trait_);
                if trait_path.is_empty() {
                    format!("{}::{name}", type_str(self_type))
                } else {
                    format!("<{} as {trait_path}>::{name}", type_str(self_type))
                }
            } else {
                format!("{}::{name}", type_str(self_type))
            }
        }
    }
}

fn format_path(path: &rustdoc_types::Path) -> String {
    format!(
        "{}{}",
        rust_path(&path.path),
        path.args.as_deref().map(args_str).unwrap_or_default()
    )
}

fn args_str(args: &GenericArgs) -> String {
    match args {
        GenericArgs::AngleBracketed { args, constraints } => {
            let mut parts = args.iter().map(generic_arg_str).collect::<Vec<_>>();
            parts.extend(constraints.iter().map(|constraint| {
                let args = constraint.args.as_deref().map(args_str).unwrap_or_default();
                match &constraint.binding {
                    AssocItemConstraintKind::Equality(term) => {
                        format!(
                            "{}{} = {}",
                            rust_identifier(&constraint.name),
                            args,
                            term_str(term)
                        )
                    }
                    AssocItemConstraintKind::Constraint(bounds) => {
                        format!(
                            "{}{}: {}",
                            rust_identifier(&constraint.name),
                            args,
                            bounds_str(bounds)
                        )
                    }
                }
            }));
            if parts.is_empty() {
                String::new()
            } else {
                format!("<{}>", parts.join(", "))
            }
        }
        GenericArgs::Parenthesized { inputs, output } => {
            let inputs = inputs.iter().map(type_str).collect::<Vec<_>>().join(", ");
            let output = output
                .as_ref()
                .map(|ty| format!(" -> {}", type_str(ty)))
                .unwrap_or_default();
            format!("({inputs}){output}")
        }
        GenericArgs::ReturnTypeNotation => "(..)".to_string(),
    }
}

fn generic_arg_str(arg: &GenericArg) -> String {
    match arg {
        GenericArg::Lifetime(lifetime) => lifetime_str(lifetime),
        GenericArg::Type(ty) => type_str(ty),
        GenericArg::Const(c) => constant_expression(&c.expr, c.value.as_deref())
            .unwrap_or("/* unsupported constant expression */")
            .to_string(),
        GenericArg::Infer => "_".to_string(),
    }
}

fn term_str(term: &Term) -> String {
    match term {
        Term::Type(ty) => type_str(ty),
        Term::Constant(c) => constant_expression(&c.expr, c.value.as_deref())
            .unwrap_or("/* unsupported constant expression */")
            .to_string(),
    }
}

fn bounds_str(bounds: &[GenericBound]) -> String {
    if bounds.is_empty() {
        return "Trait".to_string();
    }
    bounds.iter().map(bound_str).collect::<Vec<_>>().join(" + ")
}

fn bound_str(bound: &GenericBound) -> String {
    match bound {
        GenericBound::TraitBound {
            trait_,
            generic_params,
            modifier,
        } => {
            let modifier = match modifier {
                TraitBoundModifier::None => "",
                TraitBoundModifier::Maybe => "?",
                TraitBoundModifier::MaybeConst => "~const ",
            };
            let bound = format!("{modifier}{}", format_path(trait_));
            if generic_params.is_empty() {
                bound
            } else {
                format!("for<{}> {bound}", generic_param_names(generic_params))
            }
        }
        GenericBound::Outlives(lifetime) => lifetime_str(lifetime),
        GenericBound::Use(args) => format!(
            "use<{}>",
            args.iter()
                .map(|arg| match arg {
                    rustdoc_types::PreciseCapturingArg::Lifetime(l) => lifetime_str(l),
                    rustdoc_types::PreciseCapturingArg::Param(p) => rust_identifier(p),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn generic_param_names(params: &[rustdoc_types::GenericParamDef]) -> String {
    params
        .iter()
        .map(|p| match &p.kind {
            GenericParamDefKind::Lifetime { .. } => lifetime_str(&p.name),
            GenericParamDefKind::Type { .. } | GenericParamDefKind::Const { .. } => {
                rust_identifier(&p.name)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn generic_param_decls(params: &[rustdoc_types::GenericParamDef]) -> String {
    params
        .iter()
        .map(generic_param_decl)
        .collect::<Vec<_>>()
        .join(", ")
}

fn lifetime_str(lifetime: &str) -> String {
    if lifetime.starts_with('\'') {
        lifetime.to_string()
    } else {
        format!("'{lifetime}")
    }
}

fn fn_pointer_str(fp: &rustdoc_types::FunctionPointer) -> String {
    let binder = if fp.generic_params.is_empty() {
        String::new()
    } else {
        format!("for<{}> ", generic_param_decls(&fp.generic_params))
    };
    let prefix = if fp.header.is_unsafe { "unsafe " } else { "" };
    let abi = abi_str(&fp.header.abi);
    let inputs = fp
        .sig
        .inputs
        .iter()
        .map(|(_, ty)| type_str(ty))
        .collect::<Vec<_>>();
    let inputs = signature_inputs(inputs, fp.sig.is_c_variadic);
    let output = fp
        .sig
        .output
        .as_ref()
        .map(|ty| format!(" -> {}", type_str(ty)))
        .unwrap_or_default();
    format!("{binder}{prefix}{abi}fn({inputs}){output}")
}

fn signature_inputs(mut inputs: Vec<String>, is_c_variadic: bool) -> String {
    if is_c_variadic {
        inputs.push("...".to_string());
    }
    inputs.join(", ")
}

fn abi_str(abi: &rustdoc_types::Abi) -> String {
    use rustdoc_types::Abi;

    let name = match abi {
        Abi::Rust => return String::new(),
        Abi::C { unwind: false } => "C",
        Abi::C { unwind: true } => "C-unwind",
        Abi::Cdecl { unwind: false } => "cdecl",
        Abi::Cdecl { unwind: true } => "cdecl-unwind",
        Abi::Stdcall { unwind: false } => "stdcall",
        Abi::Stdcall { unwind: true } => "stdcall-unwind",
        Abi::Fastcall { unwind: false } => "fastcall",
        Abi::Fastcall { unwind: true } => "fastcall-unwind",
        Abi::Aapcs { unwind: false } => "aapcs",
        Abi::Aapcs { unwind: true } => "aapcs-unwind",
        Abi::Win64 { unwind: false } => "win64",
        Abi::Win64 { unwind: true } => "win64-unwind",
        Abi::SysV64 { unwind: false } => "sysv64",
        Abi::SysV64 { unwind: true } => "sysv64-unwind",
        Abi::System { unwind: false } => "system",
        Abi::System { unwind: true } => "system-unwind",
        Abi::Other(name) => name,
    };
    format!("extern \"{name}\" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustdoc_types::{
        Abi, Attribute, Constant, Enum, ExternalCrate, Function, FunctionHeader, FunctionSignature,
        GenericParamDef, GenericParamDefKind, Generics, Impl, Item, ItemKind, ItemSummary, Module,
        Path, ProcMacro, Static, Struct, Trait, TraitAlias, TypeAlias, Union, Use, Variant,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn generics_empty() -> Generics {
        Generics {
            params: Vec::new(),
            where_predicates: Vec::new(),
        }
    }

    fn generics_all() -> Generics {
        Generics {
            params: vec![
                GenericParamDef {
                    name: "a".into(),
                    kind: GenericParamDefKind::Lifetime { outlives: vec![] },
                },
                GenericParamDef {
                    name: "T".into(),
                    kind: GenericParamDefKind::Type {
                        bounds: vec![],
                        default: None,
                        is_synthetic: false,
                    },
                },
                GenericParamDef {
                    name: "N".into(),
                    kind: GenericParamDefKind::Const {
                        type_: Type::Primitive("usize".into()),
                        default: None,
                    },
                },
            ],
            where_predicates: Vec::new(),
        }
    }

    fn constrained_generics() -> Generics {
        Generics {
            params: vec![
                GenericParamDef {
                    name: "a".into(),
                    kind: GenericParamDefKind::Lifetime {
                        outlives: vec!["b".into()],
                    },
                },
                GenericParamDef {
                    name: "T".into(),
                    kind: GenericParamDefKind::Type {
                        bounds: vec![
                            GenericBound::TraitBound {
                                trait_: Path {
                                    path: "Clone".into(),
                                    id: Id(1),
                                    args: None,
                                },
                                generic_params: vec![],
                                modifier: TraitBoundModifier::None,
                            },
                            GenericBound::TraitBound {
                                trait_: Path {
                                    path: "Send".into(),
                                    id: Id(2),
                                    args: None,
                                },
                                generic_params: vec![],
                                modifier: TraitBoundModifier::None,
                            },
                        ],
                        default: Some(Type::Primitive("String".into())),
                        is_synthetic: false,
                    },
                },
                GenericParamDef {
                    name: "N".into(),
                    kind: GenericParamDefKind::Const {
                        type_: Type::Primitive("usize".into()),
                        default: Some("32".into()),
                    },
                },
            ],
            where_predicates: vec![
                WherePredicate::BoundPredicate {
                    type_: Type::Generic("T".into()),
                    bounds: vec![
                        GenericBound::TraitBound {
                            trait_: Path {
                                path: "Sync".into(),
                                id: Id(3),
                                args: None,
                            },
                            generic_params: vec![],
                            modifier: TraitBoundModifier::None,
                        },
                        GenericBound::Outlives("a".into()),
                    ],
                    generic_params: vec![],
                },
                WherePredicate::LifetimePredicate {
                    lifetime: "a".into(),
                    outlives: vec!["b".into()],
                },
                WherePredicate::EqPredicate {
                    lhs: Type::QualifiedPath {
                        name: "Item".into(),
                        args: None,
                        self_type: Box::new(Type::Generic("T".into())),
                        trait_: None,
                    },
                    rhs: Term::Type(Type::Primitive("u8".into())),
                },
                WherePredicate::BoundPredicate {
                    type_: Type::Generic("T".into()),
                    bounds: vec![GenericBound::TraitBound {
                        trait_: Path {
                            path: "Borrow".into(),
                            id: Id(4),
                            args: Some(Box::new(GenericArgs::AngleBracketed {
                                args: vec![GenericArg::Lifetime("x".into())],
                                constraints: vec![],
                            })),
                        },
                        generic_params: vec![],
                        modifier: TraitBoundModifier::None,
                    }],
                    generic_params: vec![GenericParamDef {
                        name: "x".into(),
                        kind: GenericParamDefKind::Lifetime { outlives: vec![] },
                    }],
                },
            ],
        }
    }

    fn span(line: usize) -> rustdoc_types::Span {
        rustdoc_types::Span {
            filename: PathBuf::from(format!("src/{line}.rs")),
            begin: (line, 1),
            end: (line, 10),
        }
    }

    fn item(id: u32, name: Option<&str>, visibility: Visibility, inner: ItemEnum) -> Item {
        Item {
            id: Id(id),
            crate_id: 0,
            name: name.map(ToOwned::to_owned),
            span: Some(span(id as usize)),
            visibility,
            docs: name.map(|n| format!("docs for {n}\n\nmore")),
            links: HashMap::new(),
            attrs: vec![Attribute::Other("#[cfg(test)]".into())],
            deprecation: None,
            inner,
        }
    }

    fn krate(items: Vec<Item>, root: Id) -> Crate {
        Crate {
            root,
            crate_version: Some("1.0.0".into()),
            includes_private: false,
            index: items.into_iter().map(|i| (i.id, i)).collect(),
            paths: HashMap::new(),
            external_crates: HashMap::new(),
            target: rustdoc_types::Target {
                triple: "x86_64-unknown-linux-gnu".into(),
                target_features: Vec::new(),
            },
            format_version: rustdoc_types::FORMAT_VERSION,
        }
    }

    fn function() -> Function {
        Function {
            sig: FunctionSignature {
                inputs: vec![("x".into(), Type::Primitive("u8".into()))],
                output: Some(Type::Primitive("bool".into())),
                is_c_variadic: false,
            },
            generics: generics_all(),
            header: FunctionHeader {
                is_const: true,
                is_unsafe: true,
                is_async: true,
                abi: Abi::Rust,
            },
            has_body: true,
        }
    }

    #[test]
    fn graph_resolves_direct_modules_uses_globs_and_cycles() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2), Id(10), Id(8), Id(9)],
                is_stripped: false,
            }),
        );
        let api = item(
            2,
            Some("api"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: false,
                items: vec![Id(3), Id(4), Id(5), Id(6), Id(7)],
                is_stripped: false,
            }),
        );
        let hidden = item(
            3,
            Some("Hidden"),
            Visibility::Crate,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let field = item(
            4,
            Some("name"),
            Visibility::Public,
            ItemEnum::StructField(Type::Primitive("String".into())),
        );
        let config = item(
            5,
            Some("Config"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Plain {
                    fields: vec![Id(4)],
                    has_stripped_fields: false,
                },
                generics: generics_all(),
                impls: vec![],
            }),
        );
        let alias = item(
            6,
            Some("Alias"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "api::Config".into(),
                name: "Alias".into(),
                id: Some(Id(5)),
                is_glob: false,
            }),
        );
        let cycle = item(
            7,
            Some("Cycle"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "api::Cycle".into(),
                name: "Cycle".into(),
                id: Some(Id(7)),
                is_glob: false,
            }),
        );
        let glob_mod = item(
            8,
            Some("glob_mod"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: false,
                items: vec![Id(11)],
                is_stripped: false,
            }),
        );
        let glob_use = item(
            9,
            Some("glob"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "glob_mod::*".into(),
                name: "glob".into(),
                id: Some(Id(8)),
                is_glob: true,
            }),
        );
        let bad_glob = item(
            10,
            Some("bad"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "missing::*".into(),
                name: "bad".into(),
                id: None,
                is_glob: true,
            }),
        );
        let globbed = item(
            11,
            Some("Globbed"),
            Visibility::Public,
            ItemEnum::Enum(Enum {
                generics: generics_empty(),
                has_stripped_variants: false,
                variants: vec![],
                impls: vec![],
            }),
        );
        let krate = krate(
            vec![
                root, api, hidden, field, config, alias, cycle, glob_mod, glob_use, bad_glob,
                globbed,
            ],
            Id(1),
        );

        let direct = find_symbol(
            &krate,
            &ImportPath {
                crate_name: "x".into(),
                segments: vec!["api".into()],
                item: "Config".into(),
                namespace: None,
            },
        )
        .unwrap();
        assert_eq!(direct.kind, "struct");
        assert!(direct.definition.contains("Config<'a, T, const N: usize>"));
        assert!(direct.details[0].definition.contains("name: String"));
        assert!(direct.docs.iter().any(|line| line == "docs for Config"));

        let via_use = find_symbol(
            &krate,
            &ImportPath {
                crate_name: "x".into(),
                segments: vec!["api".into()],
                item: "Alias".into(),
                namespace: None,
            },
        )
        .unwrap();
        assert_eq!(via_use.name, "Config");

        let via_use_report = find_symbol_report(
            &krate,
            &ImportPath {
                crate_name: "x".into(),
                segments: vec!["api".into()],
                item: "Alias".into(),
                namespace: None,
            },
        )
        .unwrap();
        assert_eq!(via_use_report.imported.kind, "use");
        assert_eq!(via_use_report.imported.name, "Alias");
        assert!(
            via_use_report
                .imported
                .docs
                .iter()
                .any(|line| line == "docs for Alias")
        );
        let resolved = via_use_report.resolved.unwrap();
        assert_eq!(resolved.kind, "struct");
        assert_eq!(resolved.name, "Config");

        let via_glob = find_symbol(
            &krate,
            &ImportPath {
                crate_name: "x".into(),
                segments: vec![],
                item: "Globbed".into(),
                namespace: None,
            },
        )
        .unwrap();
        assert_eq!(via_glob.kind, "enum");

        assert!(
            find_symbol(
                &krate,
                &ImportPath {
                    crate_name: "x".into(),
                    segments: vec!["api".into()],
                    item: "Hidden".into(),
                    namespace: None,
                }
            )
            .unwrap_err()
            .contains("private")
        );
        assert!(
            find_symbol(
                &krate,
                &ImportPath {
                    crate_name: "x".into(),
                    segments: vec!["api".into()],
                    item: "Cycle".into(),
                    namespace: None,
                }
            )
            .unwrap_err()
            .contains("cycle")
        );
        assert!(
            find_child(&krate, Id(1), "Nope", None, &mut HashSet::new())
                .unwrap_err()
                .contains("glob import")
        );
        assert!(
            find_child(&krate, Id(5), "Nope", None, &mut HashSet::new())
                .unwrap_err()
                .contains("cannot contain imported names")
        );
        assert!(
            super::item(&krate, Id(999))
                .unwrap_err()
                .contains("missing")
        );
    }

    #[test]
    fn glob_resolution_keeps_searching_after_failed_branch() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(8), Id(3), Id(4), Id(5)],
                is_stripped: false,
            }),
        );
        let cyclic_mod = item(
            2,
            Some("cyclic"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: false,
                items: vec![Id(6)],
                is_stripped: false,
            }),
        );
        let cyclic_glob = item(
            6,
            Some("cycle"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "cyclic::*".into(),
                name: "cycle".into(),
                id: Some(Id(2)),
                is_glob: true,
            }),
        );
        let root_cyclic_glob = item(
            8,
            Some("root_cycle"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "cyclic::*".into(),
                name: "root_cycle".into(),
                id: Some(Id(2)),
                is_glob: true,
            }),
        );
        let bad_glob = item(
            3,
            Some("bad"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "missing::*".into(),
                name: "bad".into(),
                id: None,
                is_glob: true,
            }),
        );
        let good_mod = item(
            4,
            Some("good"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: false,
                items: vec![Id(7)],
                is_stripped: false,
            }),
        );
        let good_glob = item(
            5,
            Some("good_glob"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "good::*".into(),
                name: "good_glob".into(),
                id: Some(Id(4)),
                is_glob: true,
            }),
        );
        let hit = item(
            7,
            Some("Hit"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let krate = krate(
            vec![
                root,
                cyclic_mod,
                bad_glob,
                good_mod,
                good_glob,
                cyclic_glob,
                hit,
                root_cyclic_glob,
            ],
            Id(1),
        );

        let found = find_symbol(
            &krate,
            &ImportPath {
                crate_name: "x".into(),
                segments: vec![],
                item: "Hit".into(),
                namespace: None,
            },
        )
        .unwrap();
        assert_eq!(found.name, "Hit");

        let err = find_child(&krate, Id(1), "Miss", None, &mut HashSet::new()).unwrap_err();
        assert!(err.contains("glob branches failed"));
        assert!(err.contains("missing::*"));
        assert!(err.contains("cyclic::*"));
    }

    #[test]
    fn detects_external_reexport_path_from_rustdoc_summary() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2)],
                is_stripped: false,
            }),
        );
        let reexport = item(
            2,
            Some("Thing"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "dep_crate::module::Thing".into(),
                name: "Thing".into(),
                id: Some(Id(99)),
                is_glob: false,
            }),
        );
        let mut krate = krate(vec![root, reexport], Id(1));
        krate.external_crates.insert(
            7,
            ExternalCrate {
                name: "dep_crate".into(),
                html_root_url: None,
            },
        );
        krate.paths.insert(
            Id(99),
            ItemSummary {
                crate_id: 7,
                path: vec!["dep_crate".into(), "module".into(), "Thing".into()],
                kind: ItemKind::Struct,
            },
        );

        let external = external_reexport(
            &krate,
            &ImportPath {
                crate_name: "fixture".into(),
                segments: vec![],
                item: "Thing".into(),
                namespace: None,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            external,
            ExternalReexport {
                crate_name: "dep_crate".into(),
                path: vec!["module".into(), "Thing".into()],
                canonical_fallback: None,
                via_glob: false,
                namespace: None,
            }
        );
        assert_eq!(
            external.import_path().unwrap().full_path(),
            "dep_crate::module::Thing"
        );
    }

    #[test]
    fn missing_source_path_keeps_immediate_edge_and_canonical_fallback() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2)],
                is_stripped: false,
            }),
        );
        let reexport = item(
            2,
            Some("Ident"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "ident::Ident".into(),
                name: "Ident".into(),
                id: Some(Id(99)),
                is_glob: false,
            }),
        );
        let mut docs = krate(vec![root, reexport], Id(1));
        docs.external_crates.insert(
            7,
            ExternalCrate {
                name: "proc_macro2".into(),
                html_root_url: None,
            },
        );
        docs.paths.insert(
            Id(99),
            ItemSummary {
                crate_id: 7,
                path: vec!["proc_macro2".into(), "Ident".into()],
                kind: ItemKind::Struct,
            },
        );

        assert_eq!(
            external_reexports(
                &docs,
                &ImportPath {
                    crate_name: "fixture".into(),
                    segments: vec![],
                    item: "Ident".into(),
                    namespace: None,
                },
                false,
            )
            .unwrap(),
            vec![ExternalReexport {
                crate_name: "ident".into(),
                path: vec!["Ident".into()],
                canonical_fallback: Some(ExternalTarget {
                    crate_name: "proc_macro2".into(),
                    path: vec!["Ident".into()],
                }),
                via_glob: false,
                namespace: None,
            }]
        );
    }

    #[test]
    fn known_external_source_keeps_the_immediate_dependency_edge() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2)],
                is_stripped: false,
            }),
        );
        let reexport = item(
            2,
            Some("Thing"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "middle::Thing".into(),
                name: "Thing".into(),
                id: Some(Id(99)),
                is_glob: false,
            }),
        );
        let mut docs = krate(vec![root, reexport], Id(1));
        docs.external_crates.insert(
            7,
            ExternalCrate {
                name: "origin".into(),
                html_root_url: None,
            },
        );
        docs.external_crates.insert(
            8,
            ExternalCrate {
                name: "middle".into(),
                html_root_url: None,
            },
        );
        docs.paths.insert(
            Id(99),
            ItemSummary {
                crate_id: 7,
                path: vec!["origin".into(), "Thing".into()],
                kind: ItemKind::Struct,
            },
        );

        assert_eq!(
            external_reexports(
                &docs,
                &ImportPath {
                    crate_name: "fixture".into(),
                    segments: vec![],
                    item: "Thing".into(),
                    namespace: None,
                },
                false,
            )
            .unwrap(),
            vec![ExternalReexport {
                crate_name: "middle".into(),
                path: vec!["Thing".into()],
                canonical_fallback: None,
                via_glob: false,
                namespace: None,
            }]
        );
    }

    #[test]
    fn source_level_extern_crate_alias_uses_the_canonical_external_id() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2), Id(3)],
                is_stripped: false,
            }),
        );
        let reexport = item(
            2,
            Some("Ident"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "source_alias::Ident".into(),
                name: "Ident".into(),
                id: Some(Id(99)),
                is_glob: false,
            }),
        );
        let alias = item(
            3,
            Some("source_alias"),
            Visibility::Public,
            ItemEnum::ExternCrate {
                name: "proc_macro2".into(),
                rename: Some("source_alias".into()),
            },
        );
        let mut docs = krate(vec![root, reexport, alias], Id(1));
        docs.external_crates.insert(
            7,
            ExternalCrate {
                name: "proc_macro2".into(),
                html_root_url: None,
            },
        );
        docs.paths.insert(
            Id(99),
            ItemSummary {
                crate_id: 7,
                path: vec!["proc_macro2".into(), "Ident".into()],
                kind: ItemKind::Struct,
            },
        );

        assert_eq!(
            external_reexports(
                &docs,
                &ImportPath {
                    crate_name: "fixture".into(),
                    segments: vec![],
                    item: "Ident".into(),
                    namespace: None,
                },
                false,
            )
            .unwrap(),
            vec![ExternalReexport {
                crate_name: "proc_macro2".into(),
                path: vec!["Ident".into()],
                canonical_fallback: None,
                via_glob: false,
                namespace: None,
            }]
        );
    }

    #[test]
    fn detects_external_crate_root_and_appends_unresolved_tail() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2)],
                is_stripped: false,
            }),
        );
        let reexport = item(
            2,
            Some("dep"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "dep_crate".into(),
                name: "dep".into(),
                id: Some(Id(99)),
                is_glob: false,
            }),
        );
        let mut krate = krate(vec![root, reexport], Id(1));
        krate.external_crates.insert(
            7,
            ExternalCrate {
                name: "dep_crate".into(),
                html_root_url: None,
            },
        );
        krate.paths.insert(
            Id(99),
            ItemSummary {
                crate_id: 7,
                path: vec!["dep_crate".into()],
                kind: ItemKind::Module,
            },
        );

        let root_external = external_reexport(
            &krate,
            &ImportPath {
                crate_name: "fixture".into(),
                segments: vec![],
                item: "dep".into(),
                namespace: None,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            root_external,
            ExternalReexport {
                crate_name: "dep_crate".into(),
                path: Vec::new(),
                canonical_fallback: None,
                via_glob: false,
                namespace: None,
            }
        );
        assert!(root_external.import_path().is_none());
        let root_doc = format_crate_root(&krate).unwrap();
        assert_eq!(root_doc.kind, "crate");
        assert_eq!(
            root_doc.definition,
            "definition rendering unsupported for crate root fixture"
        );

        let tailed_external = external_reexport(
            &krate,
            &ImportPath {
                crate_name: "fixture".into(),
                segments: vec!["dep".into()],
                item: "Thing".into(),
                namespace: None,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            tailed_external,
            ExternalReexport {
                crate_name: "dep_crate".into(),
                path: vec!["Thing".into()],
                canonical_fallback: None,
                via_glob: false,
                namespace: None,
            }
        );
        assert_eq!(
            tailed_external.import_path().unwrap().full_path(),
            "dep_crate::Thing"
        );
    }

    #[test]
    fn external_glob_appends_the_concrete_requested_tail() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2)],
                is_stripped: false,
            }),
        );
        let glob = item(
            2,
            Some("glob"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "middle::api::*".into(),
                name: "glob".into(),
                id: Some(Id(99)),
                is_glob: true,
            }),
        );
        let mut docs = krate(vec![root, glob], Id(1));
        docs.external_crates.insert(
            7,
            ExternalCrate {
                name: "middle".into(),
                html_root_url: None,
            },
        );
        docs.paths.insert(
            Id(99),
            ItemSummary {
                crate_id: 7,
                path: vec!["middle".into(), "api".into()],
                kind: ItemKind::Module,
            },
        );

        assert_eq!(
            external_reexports(
                &docs,
                &ImportPath {
                    crate_name: "fixture".into(),
                    segments: vec![],
                    item: "Thing".into(),
                    namespace: None,
                },
                false,
            )
            .unwrap(),
            vec![ExternalReexport {
                crate_name: "middle".into(),
                path: vec!["api".into(), "Thing".into()],
                canonical_fallback: None,
                via_glob: true,
                namespace: None,
            }]
        );
    }

    #[test]
    fn resolves_enum_variants_and_raw_identifiers() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2), Id(4)],
                is_stripped: false,
            }),
        );
        let enum_item = item(
            2,
            Some("Number"),
            Visibility::Public,
            ItemEnum::Enum(Enum {
                generics: generics_empty(),
                has_stripped_variants: false,
                variants: vec![Id(3)],
                impls: vec![],
            }),
        );
        let variant = item(
            3,
            Some("One"),
            Visibility::Default,
            ItemEnum::Variant(Variant {
                kind: VariantKind::Plain,
                discriminant: None,
            }),
        );
        let raw_module = item(
            4,
            Some("match"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: false,
                items: vec![Id(5)],
                is_stripped: false,
            }),
        );
        let raw_type = item(
            5,
            Some("type"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let docs = krate(vec![root, enum_item, variant, raw_module, raw_type], Id(1));

        let found = find_symbol_report(
            &docs,
            &ImportPath {
                crate_name: "fixture".into(),
                segments: vec!["Number".into()],
                item: "One".into(),
                namespace: None,
            },
        )
        .unwrap();
        assert_eq!(found.imported.kind, "variant");
        assert_eq!(found.imported.definition, "One");

        let raw = find_symbol_report(
            &docs,
            &ImportPath {
                crate_name: "fixture".into(),
                segments: vec!["r#match".into()],
                item: "r#type".into(),
                namespace: None,
            },
        )
        .unwrap();
        assert_eq!(raw.imported.name, "type");
        assert_eq!(raw.imported.definition, "pub struct r#type;");
    }

    #[test]
    fn same_spelling_in_multiple_namespaces_is_an_explicit_ambiguity() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2), Id(3)],
                is_stripped: false,
            }),
        );
        let trait_use = item(
            2,
            Some("Serialize"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "serde_core::Serialize".into(),
                name: "Serialize".into(),
                id: Some(Id(20)),
                is_glob: false,
            }),
        );
        let derive_use = item(
            3,
            Some("Serialize"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "serde_derive::Serialize".into(),
                name: "Serialize".into(),
                id: Some(Id(30)),
                is_glob: false,
            }),
        );
        let mut docs = krate(vec![root, trait_use, derive_use], Id(1));
        docs.paths.insert(
            Id(20),
            ItemSummary {
                crate_id: 2,
                path: vec!["serde_core".into(), "Serialize".into()],
                kind: ItemKind::Trait,
            },
        );
        docs.paths.insert(
            Id(30),
            ItemSummary {
                crate_id: 3,
                path: vec!["serde_derive".into(), "Serialize".into()],
                kind: ItemKind::ProcDerive,
            },
        );

        let error = find_symbol_report(
            &docs,
            &ImportPath {
                crate_name: "fixture".into(),
                segments: vec![],
                item: "Serialize".into(),
                namespace: None,
            },
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "imported name 'Serialize' is ambiguous across Rust namespaces (Trait Serialize, ProcDerive Serialize); query a namespace-specific canonical path"
        );
    }

    #[test]
    fn explicit_items_shadow_same_namespace_globs_but_not_macro_namespaces() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2), Id(3), Id(4), Id(7)],
                is_stripped: false,
            }),
        );
        let direct = item(
            2,
            Some("Thing"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let glob_module = item(
            3,
            Some("globbed"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: false,
                items: vec![Id(5), Id(6)],
                is_stripped: false,
            }),
        );
        let glob = item(
            4,
            Some("glob"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "globbed::*".into(),
                name: "glob".into(),
                id: Some(Id(3)),
                is_glob: true,
            }),
        );
        let macro_item = item(
            5,
            Some("Thing"),
            Visibility::Public,
            ItemEnum::Macro("macro_rules! Thing { () => { ... }; }".into()),
        );
        let glob_same = item(
            6,
            Some("Same"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let direct_same = item(
            7,
            Some("Same"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let docs = krate(
            vec![
                root,
                direct,
                glob_module,
                glob,
                macro_item,
                glob_same,
                direct_same,
            ],
            Id(1),
        );

        let ambiguous = find_symbol_report(
            &docs,
            &ImportPath {
                crate_name: "fixture".into(),
                segments: vec![],
                item: "Thing".into(),
                namespace: None,
            },
        )
        .unwrap_err();
        assert!(ambiguous.contains("ambiguous across Rust namespaces"));
        assert!(ambiguous.contains("struct Thing"));
        assert!(ambiguous.contains("macro Thing"));

        let shadowed = find_symbol_report(
            &docs,
            &ImportPath {
                crate_name: "fixture".into(),
                segments: vec![],
                item: "Same".into(),
                namespace: None,
            },
        )
        .unwrap();
        assert_eq!(shadowed.imported.definition, "pub struct Same;");
    }

    #[test]
    fn private_bindings_shadow_globs_only_in_their_namespaces() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![
                    Id(2),
                    Id(3),
                    Id(6),
                    Id(7),
                    Id(8),
                    Id(10),
                    Id(11),
                    Id(14),
                    Id(17),
                ],
                is_stripped: false,
            }),
        );
        let donor = item(
            2,
            Some("donor"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: false,
                items: vec![Id(4), Id(5), Id(9), Id(12), Id(15), Id(16), Id(18)],
                is_stripped: false,
            }),
        );
        let glob = item(
            3,
            Some("glob"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "donor::*".into(),
                name: "glob".into(),
                id: Some(Id(2)),
                is_glob: true,
            }),
        );
        let public_type = item(
            4,
            Some("Thing"),
            Visibility::Public,
            ItemEnum::TypeAlias(TypeAlias {
                type_: Type::Primitive("u8".into()),
                generics: generics_empty(),
            }),
        );
        let public_value = item(
            5,
            Some("action"),
            Visibility::Public,
            ItemEnum::Function(function()),
        );
        let private_type = item(
            6,
            Some("Thing"),
            Visibility::Crate,
            ItemEnum::TypeAlias(TypeAlias {
                type_: Type::Primitive("u16".into()),
                generics: generics_empty(),
            }),
        );
        let private_value = item(
            7,
            Some("action"),
            Visibility::Crate,
            ItemEnum::Function(function()),
        );
        let private_other_namespace = item(
            8,
            Some("Marker"),
            Visibility::Crate,
            ItemEnum::Function(function()),
        );
        let public_macro = item(
            9,
            Some("Marker"),
            Visibility::Public,
            ItemEnum::Macro("macro_rules! Marker { () => {}; }".into()),
        );
        let direct_type = item(
            10,
            Some("Same"),
            Visibility::Public,
            ItemEnum::TypeAlias(TypeAlias {
                type_: Type::Primitive("u8".into()),
                generics: generics_empty(),
            }),
        );
        let private_same_value = item(
            11,
            Some("Same"),
            Visibility::Crate,
            ItemEnum::Function(function()),
        );
        let glob_same = item(
            12,
            Some("Same"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let private_split_value = item(
            14,
            Some("Split"),
            Visibility::Crate,
            ItemEnum::Function(function()),
        );
        let public_split_type = item(
            15,
            Some("Split"),
            Visibility::Public,
            ItemEnum::TypeAlias(TypeAlias {
                type_: Type::Primitive("u32".into()),
                generics: generics_empty(),
            }),
        );
        let public_split_value = item(
            16,
            Some("Split"),
            Visibility::Public,
            ItemEnum::Function(function()),
        );
        let mut disabled_private_value = item(
            17,
            Some("Available"),
            Visibility::Crate,
            ItemEnum::Function(function()),
        );
        disabled_private_value.attrs.push(Attribute::Other(
            crate::rustdoc_json::CFG_UNAVAILABLE_ATTRIBUTE.into(),
        ));
        let public_available_value = item(
            18,
            Some("Available"),
            Visibility::Public,
            ItemEnum::Function(function()),
        );
        let docs = krate(
            vec![
                root,
                donor,
                glob,
                public_type,
                public_value,
                private_type,
                private_value,
                private_other_namespace,
                public_macro,
                direct_type,
                private_same_value,
                glob_same,
                private_split_value,
                public_split_type,
                public_split_value,
                disabled_private_value,
                public_available_value,
            ],
            Id(1),
        );

        for name in ["Thing", "action"] {
            let error = find_child(&docs, Id(1), name, None, &mut HashSet::new()).unwrap_err();
            assert!(error.contains(&format!("'{name}' is private")), "{error}");
        }
        assert_eq!(
            find_child(&docs, Id(1), "Marker", None, &mut HashSet::new()).unwrap(),
            Id(9)
        );
        assert_eq!(
            find_child(&docs, Id(1), "Same", None, &mut HashSet::new()).unwrap(),
            Id(10)
        );
        assert_eq!(
            find_child(&docs, Id(1), "Split", None, &mut HashSet::new()).unwrap(),
            Id(15)
        );
        assert_eq!(
            find_child(&docs, Id(1), "Available", None, &mut HashSet::new()).unwrap(),
            Id(18)
        );
    }

    #[test]
    fn formatter_preserves_markdown_and_reports_supported_or_unsupported_syntax_exactly() {
        let mut documented = item(
            40,
            Some("Documented"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        documented.docs = Some("First paragraph.\n\n- parent\n  - child\n\n    code".into());
        let module = item(
            41,
            Some("api"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: false,
                items: vec![],
                is_stripped: false,
            }),
        );
        let alias = item(
            42,
            Some("Sendable"),
            Visibility::Public,
            ItemEnum::TraitAlias(TraitAlias {
                generics: generics_empty(),
                params: vec![GenericBound::TraitBound {
                    trait_: Path {
                        path: "Send".into(),
                        id: Id(99),
                        args: None,
                    },
                    generic_params: vec![],
                    modifier: TraitBoundModifier::None,
                }],
            }),
        );
        let auto_trait = item(
            43,
            Some("Marker"),
            Visibility::Public,
            ItemEnum::Trait(Trait {
                is_auto: true,
                is_unsafe: false,
                is_dyn_compatible: false,
                items: vec![],
                generics: generics_empty(),
                bounds: vec![],
                implementations: vec![],
            }),
        );
        let derive = item(
            44,
            Some("Model"),
            Visibility::Public,
            ItemEnum::ProcMacro(ProcMacro {
                kind: MacroKind::Derive,
                helpers: vec!["model".into(), "field".into()],
            }),
        );
        let unsupported = item(
            45,
            Some("Foreign"),
            Visibility::Public,
            ItemEnum::ExternType,
        );
        let docs = krate(
            vec![
                documented.clone(),
                module.clone(),
                alias.clone(),
                auto_trait.clone(),
                derive.clone(),
                unsupported.clone(),
            ],
            Id(41),
        );

        assert_eq!(
            format_item(&docs, &documented).docs,
            vec![
                "First paragraph.",
                "",
                "- parent",
                "  - child",
                "",
                "    code"
            ]
        );
        assert_eq!(format_item(&docs, &module).definition, "pub mod api;");
        assert_eq!(
            format_item(&docs, &alias).definition,
            "pub trait Sendable = Send;"
        );
        assert_eq!(
            format_item(&docs, &auto_trait).definition,
            "pub auto trait Marker { ... }"
        );
        assert_eq!(
            format_item(&docs, &derive).definition,
            "definition rendering unsupported for derive procedural macro; usage: #[derive(Model)]; helper attributes: model, field"
        );
        assert_eq!(
            format_item(&docs, &unsupported).definition,
            "definition rendering unsupported for extern type"
        );
    }

    #[test]
    fn formatter_preserves_field_visibility_values_and_extern_static_semantics() {
        let public_field = item(
            1,
            Some("value"),
            Visibility::Public,
            ItemEnum::StructField(Type::Primitive("u8".into())),
        );
        let private_field = item(
            2,
            Some("hidden"),
            Visibility::Default,
            ItemEnum::StructField(Type::Primitive("u16".into())),
        );
        let crate_field = item(
            3,
            Some("shared"),
            Visibility::Crate,
            ItemEnum::StructField(Type::Primitive("u32".into())),
        );
        let named = item(
            4,
            Some("Named"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Plain {
                    fields: vec![Id(1), Id(2), Id(3)],
                    has_stripped_fields: false,
                },
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let tuple = item(
            5,
            Some("Tuple"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Tuple(vec![Some(Id(1)), Some(Id(2)), Some(Id(3))]),
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let union = item(
            6,
            Some("Either"),
            Visibility::Public,
            ItemEnum::Union(Union {
                generics: generics_empty(),
                has_stripped_fields: false,
                fields: vec![Id(1), Id(3)],
                impls: vec![],
            }),
        );
        let variant = item(
            7,
            Some("Five"),
            Visibility::Default,
            ItemEnum::Variant(Variant {
                kind: VariantKind::Plain,
                discriminant: Some(rustdoc_types::Discriminant {
                    expr: "0x05".into(),
                    value: "5".into(),
                }),
            }),
        );
        let struct_variant = item(
            8,
            Some("Fields"),
            Visibility::Default,
            ItemEnum::Variant(Variant {
                kind: VariantKind::Struct {
                    fields: vec![Id(1)],
                    has_stripped_fields: false,
                },
                discriminant: None,
            }),
        );
        let number = item(
            9,
            Some("Number"),
            Visibility::Public,
            ItemEnum::Enum(Enum {
                generics: generics_empty(),
                has_stripped_variants: false,
                variants: vec![Id(7), Id(8)],
                impls: vec![],
            }),
        );
        let constant = item(
            10,
            Some("COUNT"),
            Visibility::Public,
            ItemEnum::Constant {
                type_: Type::Primitive("usize".into()),
                const_: Constant {
                    expr: "1 + 2".into(),
                    value: Some("3".into()),
                    is_literal: false,
                },
            },
        );
        let static_item = item(
            11,
            Some("READY"),
            Visibility::Public,
            ItemEnum::Static(Static {
                type_: Type::Primitive("bool".into()),
                is_mutable: false,
                expr: "true".into(),
                is_unsafe: false,
            }),
        );
        let foreign = item(
            12,
            Some("FOREIGN"),
            Visibility::Public,
            ItemEnum::Static(Static {
                type_: Type::Primitive("u8".into()),
                is_mutable: false,
                expr: String::new(),
                is_unsafe: true,
            }),
        );
        let assoc = item(
            13,
            Some("VALUE"),
            Visibility::Default,
            ItemEnum::AssocConst {
                type_: Type::Primitive("u8".into()),
                value: Some("7".into()),
            },
        );
        let trait_item = item(
            14,
            Some("Defaults"),
            Visibility::Public,
            ItemEnum::Trait(Trait {
                is_auto: false,
                is_unsafe: false,
                is_dyn_compatible: false,
                items: vec![Id(13)],
                generics: generics_empty(),
                bounds: vec![],
                implementations: vec![],
            }),
        );
        let docs = krate(
            vec![
                public_field,
                private_field,
                crate_field,
                named.clone(),
                tuple.clone(),
                union.clone(),
                variant,
                struct_variant,
                number.clone(),
                constant.clone(),
                static_item.clone(),
                foreign.clone(),
                assoc,
                trait_item.clone(),
            ],
            Id(4),
        );

        assert_eq!(
            format_item(&docs, &named).definition,
            "pub struct Named { pub value: u8, hidden: u16, pub(crate) shared: u32 }"
        );
        assert_eq!(
            format_item(&docs, &tuple).definition,
            "pub struct Tuple(pub u8, u16, pub(crate) u32);"
        );
        assert_eq!(
            format_item(&docs, &union).definition,
            "pub union Either { pub value: u8, pub(crate) shared: u32 }"
        );
        assert_eq!(
            format_item(&docs, &number).definition,
            "pub enum Number { Five = 0x05, Fields { value: u8 } }"
        );
        assert_eq!(
            format_item(&docs, &constant).definition,
            "pub const COUNT: usize = 1 + 2;"
        );
        assert_eq!(
            constant_initializer(&Constant {
                expr: "_".into(),
                value: Some("3".into()),
                is_literal: false,
            }),
            Some("3")
        );
        assert_eq!(
            format_item(&docs, &static_item).definition,
            "pub static READY: bool = true;"
        );
        assert_eq!(
            format_item(&docs, &foreign).definition,
            "definition rendering unsupported as standalone Rust for unsafe extern static; declaration inside extern block: static FOREIGN: u8;"
        );
        assert_eq!(
            format_item(&docs, &trait_item)
                .details
                .into_iter()
                .map(|detail| detail.definition)
                .collect::<Vec<_>>(),
            vec!["const VALUE: u8 = 7;"]
        );
    }

    #[test]
    fn formatter_covers_item_kinds() {
        let f1 = item(
            1,
            Some("a"),
            Visibility::Public,
            ItemEnum::StructField(Type::Primitive("u8".into())),
        );
        let f2 = item(
            2,
            None,
            Visibility::Default,
            ItemEnum::StructField(Type::BorrowedRef {
                lifetime: Some("a".into()),
                is_mutable: true,
                type_: Box::new(Type::Primitive("str".into())),
            }),
        );
        let v1 = item(
            3,
            Some("Plain"),
            Visibility::Public,
            ItemEnum::Variant(Variant {
                kind: VariantKind::Plain,
                discriminant: None,
            }),
        );
        let v2 = item(
            4,
            Some("Tuple"),
            Visibility::Public,
            ItemEnum::Variant(Variant {
                kind: VariantKind::Tuple(vec![Some(Id(1)), None]),
                discriminant: None,
            }),
        );
        let v3 = item(
            5,
            Some("Structy"),
            Visibility::Public,
            ItemEnum::Variant(Variant {
                kind: VariantKind::Struct {
                    fields: vec![Id(2)],
                    has_stripped_fields: false,
                },
                discriminant: None,
            }),
        );
        let mut items = vec![f1, f2, v1, v2, v3];
        let cases = vec![
            item(
                10,
                Some("Unit"),
                Visibility::Public,
                ItemEnum::Struct(Struct {
                    kind: StructKind::Unit,
                    generics: generics_empty(),
                    impls: vec![],
                }),
            ),
            item(
                11,
                Some("TupleStruct"),
                Visibility::Public,
                ItemEnum::Struct(Struct {
                    kind: StructKind::Tuple(vec![Some(Id(1)), None]),
                    generics: generics_empty(),
                    impls: vec![],
                }),
            ),
            item(
                12,
                Some("E"),
                Visibility::Public,
                ItemEnum::Enum(Enum {
                    generics: generics_empty(),
                    has_stripped_variants: false,
                    variants: vec![Id(3), Id(4), Id(5)],
                    impls: vec![],
                }),
            ),
            item(
                13,
                Some("run"),
                Visibility::Public,
                ItemEnum::Function(function()),
            ),
            item(
                14,
                Some("Alias"),
                Visibility::Public,
                ItemEnum::TypeAlias(TypeAlias {
                    type_: Type::Array {
                        type_: Box::new(Type::Primitive("u8".into())),
                        len: "4".into(),
                    },
                    generics: generics_empty(),
                }),
            ),
            item(
                15,
                Some("C"),
                Visibility::Public,
                ItemEnum::Constant {
                    type_: Type::Primitive("usize".into()),
                    const_: Constant {
                        expr: "1".into(),
                        value: Some("1".into()),
                        is_literal: true,
                    },
                },
            ),
            item(
                16,
                Some("S"),
                Visibility::Public,
                ItemEnum::Static(Static {
                    type_: Type::Primitive("bool".into()),
                    is_mutable: true,
                    expr: "false".into(),
                    is_unsafe: false,
                }),
            ),
            item(
                17,
                Some("U"),
                Visibility::Public,
                ItemEnum::Union(Union {
                    generics: generics_empty(),
                    has_stripped_fields: false,
                    fields: vec![Id(1)],
                    impls: vec![],
                }),
            ),
            item(
                18,
                Some("m"),
                Visibility::Public,
                ItemEnum::Macro("macro_rules! m { () => { ... }; }".into()),
            ),
            item(
                19,
                Some("bang"),
                Visibility::Public,
                ItemEnum::ProcMacro(ProcMacro {
                    kind: MacroKind::Bang,
                    helpers: vec![],
                }),
            ),
            item(
                20,
                Some("attr"),
                Visibility::Public,
                ItemEnum::ProcMacro(ProcMacro {
                    kind: MacroKind::Attr,
                    helpers: vec![],
                }),
            ),
            item(
                21,
                Some("Der"),
                Visibility::Public,
                ItemEnum::ProcMacro(ProcMacro {
                    kind: MacroKind::Derive,
                    helpers: vec!["helper".into()],
                }),
            ),
            item(
                22,
                Some("import"),
                Visibility::Public,
                ItemEnum::Use(Use {
                    source: "a::b".into(),
                    name: "import".into(),
                    id: None,
                    is_glob: false,
                }),
            ),
            item(
                23,
                Some("mod"),
                Visibility::Public,
                ItemEnum::Module(Module {
                    is_crate: false,
                    items: vec![],
                    is_stripped: false,
                }),
            ),
        ];
        items.extend(cases.clone());
        let docs = krate(items, Id(23));
        for it in &cases {
            let doc = format_item(&docs, it);
            assert!(!doc.definition.is_empty());
        }
        assert_eq!(
            format_item(&docs, docs.index.get(&Id(18)).unwrap()).definition,
            "macro_rules! m { () => { ... }; }"
        );

        let trait_item = item(
            30,
            Some("go"),
            Visibility::Default,
            ItemEnum::Function(function()),
        );
        let assoc_type = item(
            31,
            Some("Out"),
            Visibility::Default,
            ItemEnum::AssocType {
                generics: generics_empty(),
                bounds: vec![],
                type_: None,
            },
        );
        let assoc_const = item(
            32,
            Some("ID"),
            Visibility::Default,
            ItemEnum::AssocConst {
                type_: Type::Primitive("u8".into()),
                value: None,
            },
        );
        let tr = item(
            33,
            Some("Worker"),
            Visibility::Public,
            ItemEnum::Trait(Trait {
                is_auto: false,
                is_unsafe: true,
                is_dyn_compatible: true,
                items: vec![Id(30), Id(31), Id(32)],
                generics: generics_empty(),
                bounds: vec![],
                implementations: vec![],
            }),
        );
        let krate = krate(
            vec![trait_item, assoc_type, assoc_const, tr.clone()],
            Id(33),
        );
        let doc = format_item(&krate, &tr);
        assert_eq!(doc.kind, "trait");
        assert_eq!(doc.details.len(), 3);
    }

    #[test]
    fn formatter_reports_derived_traits_methods_and_impls() {
        let method = item(
            2,
            Some("new"),
            Visibility::Public,
            ItemEnum::Function(Function {
                sig: FunctionSignature {
                    inputs: vec![],
                    output: Some(Type::Generic("Self".into())),
                    is_c_variadic: false,
                },
                generics: generics_empty(),
                header: FunctionHeader {
                    is_const: false,
                    is_unsafe: false,
                    is_async: false,
                    abi: Abi::Rust,
                },
                has_body: true,
            }),
        );
        let inherent_impl = item(
            3,
            None,
            Visibility::Default,
            ItemEnum::Impl(Impl {
                is_unsafe: false,
                generics: generics_empty(),
                provided_trait_methods: vec![],
                trait_: None,
                for_: Type::ResolvedPath(Path {
                    path: "Widget".into(),
                    id: Id(1),
                    args: None,
                }),
                items: vec![Id(2)],
                is_negative: false,
                is_synthetic: false,
                blanket_impl: None,
            }),
        );
        let mut clone_impl = item(
            4,
            None,
            Visibility::Default,
            ItemEnum::Impl(Impl {
                is_unsafe: false,
                generics: generics_empty(),
                provided_trait_methods: vec![],
                trait_: Some(Path {
                    path: "Clone".into(),
                    id: Id(99),
                    args: None,
                }),
                for_: Type::ResolvedPath(Path {
                    path: "Widget".into(),
                    id: Id(1),
                    args: None,
                }),
                items: vec![],
                is_negative: false,
                is_synthetic: false,
                blanket_impl: None,
            }),
        );
        clone_impl.attrs = vec![Attribute::AutomaticallyDerived];
        let widget = item(
            1,
            Some("Widget"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![Id(3), Id(4)],
            }),
        );
        let krate = krate(
            vec![widget.clone(), method, inherent_impl, clone_impl],
            Id(1),
        );
        let doc = format_item(&krate, &widget);
        assert_eq!(doc.derives, vec!["Clone"]);
        assert!(
            doc.methods
                .iter()
                .any(|method| method.definition == "impl Widget { pub fn new() -> Self }")
        );
        assert!(doc.impls.iter().any(|imp| imp == "impl Clone for Widget"));
    }

    #[test]
    fn derive_attributes_accept_only_genuine_top_level_derives() {
        let attrs = vec![
            Attribute::Other("#[derive(Clone, marker::Qualified)]".into()),
            Attribute::Other("#[<derive>(Debug)]".into()),
            Attribute::Other(
                "#[<cfg_attr>(feature = \"builder\", derive(DisabledBuilder))]".into(),
            ),
            Attribute::Other("#[serde(note = \"derive(NotADerive)\")]".into()),
            Attribute::Other("prefix #[derive(AlsoNotADerive)]".into()),
        ];

        assert_eq!(
            derive_attrs(&attrs),
            ["Clone", "Debug", "marker::Qualified"]
        );
    }

    #[test]
    fn formatter_reports_deprecation_and_semantic_attributes() {
        let mut annotated = item(
            1,
            Some("Annotated"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        annotated.deprecation = Some(rustdoc_types::Deprecation {
            since: Some("1.2.3".into()),
            note: Some("use Replacement".into()),
        });
        annotated.attrs = vec![
            Attribute::Repr(AttributeRepr {
                kind: ReprKind::C,
                align: Some(8),
                packed: None,
                int: None,
            }),
            Attribute::Repr(AttributeRepr {
                kind: ReprKind::Rust,
                align: None,
                packed: None,
                int: Some("u8".into()),
            }),
            Attribute::NonExhaustive,
            Attribute::MustUse {
                reason: Some("inspect the value".into()),
            },
            Attribute::MustUse { reason: None },
        ];

        let doc = format_item(&krate(vec![annotated.clone()], Id(1)), &annotated);

        assert_eq!(
            doc.deprecation,
            Some(DeprecationDoc {
                since: Some("1.2.3".into()),
                note: Some("use Replacement".into()),
            })
        );
        assert_eq!(
            doc.attributes
                .iter()
                .map(ReportedAttribute::render)
                .collect::<Vec<_>>(),
            vec![
                "#[repr(C, align(8))]",
                "#[repr(u8)]",
                "#[non_exhaustive]",
                "#[must_use = \"inspect the value\"]",
                "#[must_use]",
            ]
        );
        assert!(doc.derives.is_empty());
    }

    #[test]
    fn type_formatting_handles_common_shapes() {
        assert_eq!(type_str(&Type::Primitive("usize".into())), "usize");
        assert_eq!(
            type_str(&Type::Tuple(vec![Type::Primitive("u8".into())])),
            "(u8,)"
        );
        assert_eq!(
            type_str(&Type::Slice(Box::new(Type::Primitive("u8".into())))),
            "[u8]"
        );
        assert_eq!(
            type_str(&Type::RawPointer {
                is_mutable: false,
                type_: Box::new(Type::Primitive("u8".into()))
            }),
            "*const u8"
        );
        assert_eq!(type_str(&Type::Infer), "_");
        assert_eq!(
            type_str(&Type::FunctionPointer(Box::new(
                rustdoc_types::FunctionPointer {
                    sig: FunctionSignature {
                        inputs: vec![("x".into(), Type::Primitive("u8".into()))],
                        output: Some(Type::Primitive("bool".into())),
                        is_c_variadic: false
                    },
                    generic_params: vec![],
                    header: FunctionHeader {
                        is_const: false,
                        is_unsafe: false,
                        is_async: false,
                        abi: Abi::Rust
                    }
                }
            ))),
            "fn(u8) -> bool"
        );
        assert_eq!(
            type_str(&Type::FunctionPointer(Box::new(
                rustdoc_types::FunctionPointer {
                    sig: FunctionSignature {
                        inputs: vec![(
                            "value".into(),
                            Type::BorrowedRef {
                                lifetime: Some("a".into()),
                                is_mutable: false,
                                type_: Box::new(Type::Primitive("str".into())),
                            },
                        )],
                        output: Some(Type::BorrowedRef {
                            lifetime: Some("a".into()),
                            is_mutable: false,
                            type_: Box::new(Type::Primitive("str".into())),
                        }),
                        is_c_variadic: false,
                    },
                    generic_params: vec![GenericParamDef {
                        name: "a".into(),
                        kind: GenericParamDefKind::Lifetime { outlives: vec![] },
                    }],
                    header: FunctionHeader {
                        is_const: false,
                        is_unsafe: false,
                        is_async: false,
                        abi: Abi::Rust,
                    },
                },
            ))),
            "for<'a> fn(&'a str) -> &'a str"
        );
        assert_eq!(
            type_str(&Type::ResolvedPath(Path {
                path: "std::vec::Vec".into(),
                id: Id(1),
                args: Some(Box::new(GenericArgs::AngleBracketed {
                    args: vec![GenericArg::Type(Type::Primitive("u8".into()))],
                    constraints: vec![]
                }))
            })),
            "std::vec::Vec<u8>"
        );
        assert_eq!(
            type_str(&Type::Pat {
                type_: Box::new(Type::Primitive("u8".into())),
                __pat_unstable_do_not_use: "1..".into()
            }),
            "u8 is 1.."
        );
        assert_eq!(
            type_str(&Type::QualifiedPath {
                name: "Item".into(),
                args: None,
                self_type: Box::new(Type::Generic("T".into())),
                trait_: None
            }),
            "T::Item"
        );
        assert_eq!(
            type_str(&Type::ImplTrait(vec![GenericBound::TraitBound {
                trait_: Path {
                    path: "Future".into(),
                    id: Id(1),
                    args: Some(Box::new(GenericArgs::AngleBracketed {
                        args: vec![],
                        constraints: vec![rustdoc_types::AssocItemConstraint {
                            name: "Output".into(),
                            args: None,
                            binding: AssocItemConstraintKind::Equality(Term::Type(
                                Type::Primitive("u8".into())
                            ))
                        }]
                    }))
                },
                generic_params: vec![],
                modifier: TraitBoundModifier::None,
            }])),
            "impl Future<Output = u8>"
        );
        assert_eq!(generics(&generics_all()), "<'a, T, const N: usize>");
    }

    #[test]
    fn generic_formatting_preserves_bounds_defaults_and_where_clauses() {
        let constrained = constrained_generics();

        assert_eq!(
            generics(&constrained),
            "<'a: 'b, T: Clone + Send = String, const N: usize = 32>"
        );
        assert_eq!(
            where_clause(&constrained),
            " where T: Sync + 'a, 'a: 'b, T::Item = u8, for<'x> T: Borrow<'x>"
        );

        let field = item(
            1,
            Some("value"),
            Visibility::Public,
            ItemEnum::StructField(Type::Generic("T".into())),
        );
        let cache = item(
            2,
            Some("Cache"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Plain {
                    fields: vec![Id(1)],
                    has_stripped_fields: false,
                },
                generics: constrained.clone(),
                impls: vec![],
            }),
        );
        let docs = krate(vec![field, cache.clone()], Id(2));
        assert_eq!(
            format_item(&docs, &cache).definition,
            "pub struct Cache<'a: 'b, T: Clone + Send = String, const N: usize = 32> where T: Sync + 'a, 'a: 'b, T::Item = u8, for<'x> T: Borrow<'x> { pub value: T }"
        );

        let alias = item(
            3,
            Some("Alias"),
            Visibility::Public,
            ItemEnum::TypeAlias(TypeAlias {
                type_: Type::Generic("T".into()),
                generics: constrained.clone(),
            }),
        );
        assert_eq!(
            format_item(&krate(vec![alias.clone()], Id(3)), &alias).definition,
            "pub type Alias<'a: 'b, T: Clone + Send = String, const N: usize = 32> where T: Sync + 'a, 'a: 'b, T::Item = u8, for<'x> T: Borrow<'x> = T;"
        );

        let function = Function {
            sig: FunctionSignature {
                inputs: vec![("x".into(), Type::Generic("T".into()))],
                output: Some(Type::Generic("T".into())),
                is_c_variadic: false,
            },
            generics: constrained.clone(),
            header: FunctionHeader {
                is_const: false,
                is_unsafe: false,
                is_async: false,
                abi: Abi::Rust,
            },
            has_body: true,
        };
        assert_eq!(
            fn_def("load", &function),
            "pub fn load<'a: 'b, T: Clone + Send = String, const N: usize = 32>(x: T) -> T where T: Sync + 'a, 'a: 'b, T::Item = u8, for<'x> T: Borrow<'x>"
        );

        let assoc = item(
            4,
            Some("Output"),
            Visibility::Default,
            ItemEnum::AssocType {
                generics: constrained.clone(),
                bounds: vec![GenericBound::TraitBound {
                    trait_: Path {
                        path: "Clone".into(),
                        id: Id(1),
                        args: None,
                    },
                    generic_params: vec![],
                    modifier: TraitBoundModifier::None,
                }],
                type_: Some(Type::Generic("T".into())),
            },
        );
        let required_method = item(
            8,
            Some("load"),
            Visibility::Default,
            ItemEnum::Function(Function {
                has_body: false,
                ..function.clone()
            }),
        );
        let provided_method = item(
            9,
            Some("load_default"),
            Visibility::Default,
            ItemEnum::Function(Function {
                has_body: true,
                ..function.clone()
            }),
        );
        let trait_item = item(
            5,
            Some("Loader"),
            Visibility::Public,
            ItemEnum::Trait(Trait {
                is_auto: false,
                is_unsafe: false,
                is_dyn_compatible: true,
                items: vec![Id(4), Id(8), Id(9)],
                generics: constrained.clone(),
                bounds: vec![GenericBound::TraitBound {
                    trait_: Path {
                        path: "Debug".into(),
                        id: Id(6),
                        args: None,
                    },
                    generic_params: vec![],
                    modifier: TraitBoundModifier::None,
                }],
                implementations: vec![],
            }),
        );
        let docs = krate(
            vec![assoc, required_method, provided_method, trait_item.clone()],
            Id(5),
        );
        let trait_doc = format_item(&docs, &trait_item);
        assert_eq!(
            trait_doc.definition,
            "pub trait Loader<'a: 'b, T: Clone + Send = String, const N: usize = 32>: Debug where T: Sync + 'a, 'a: 'b, T::Item = u8, for<'x> T: Borrow<'x> { ... }"
        );
        assert_eq!(
            trait_doc
                .details
                .into_iter()
                .map(|detail| detail.definition)
                .collect::<Vec<_>>(),
            vec![
                "type Output<'a: 'b, T: Clone + Send = String, const N: usize = 32>: Clone where T: Sync + 'a, 'a: 'b, T::Item = u8, for<'x> T: Borrow<'x> = T;",
                "fn load<'a: 'b, T: Clone + Send = String, const N: usize = 32>(x: T) -> T where T: Sync + 'a, 'a: 'b, T::Item = u8, for<'x> T: Borrow<'x>;",
                "fn load_default<'a: 'b, T: Clone + Send = String, const N: usize = 32>(x: T) -> T where T: Sync + 'a, 'a: 'b, T::Item = u8, for<'x> T: Borrow<'x> { ... }"
            ]
        );

        let impl_item = item(
            6,
            None,
            Visibility::Default,
            ItemEnum::Impl(Impl {
                is_unsafe: true,
                generics: constrained.clone(),
                provided_trait_methods: vec![],
                trait_: Some(Path {
                    path: "Loader".into(),
                    id: Id(5),
                    args: None,
                }),
                for_: Type::ResolvedPath(Path {
                    path: "Cache".into(),
                    id: Id(2),
                    args: Some(Box::new(GenericArgs::AngleBracketed {
                        args: vec![GenericArg::Type(Type::Generic("T".into()))],
                        constraints: vec![],
                    })),
                }),
                items: vec![],
                is_negative: false,
                is_synthetic: false,
                blanket_impl: None,
            }),
        );
        let cache = item(
            7,
            Some("Cache"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![Id(6)],
            }),
        );
        let docs = krate(vec![impl_item, cache.clone()], Id(7));
        assert_eq!(
            format_item(&docs, &cache).impls,
            vec![
                "unsafe impl<'a: 'b, T: Clone + Send = String, const N: usize = 32> Loader for Cache<T> where T: Sync + 'a, 'a: 'b, T::Item = u8, for<'x> T: Borrow<'x>"
            ]
        );
    }

    #[test]
    fn function_formatting_preserves_abi_and_variadics() {
        let function = Function {
            sig: FunctionSignature {
                inputs: vec![("fmt".into(), Type::Primitive("*const u8".into()))],
                output: Some(Type::Primitive("i32".into())),
                is_c_variadic: true,
            },
            generics: generics_empty(),
            header: FunctionHeader {
                is_const: false,
                is_unsafe: true,
                is_async: false,
                abi: Abi::C { unwind: false },
            },
            has_body: false,
        };
        assert_eq!(
            fn_def("printf_like", &function),
            "pub unsafe extern \"C\" fn printf_like(fmt: *const u8, ...) -> i32"
        );

        let pointer = Type::FunctionPointer(Box::new(rustdoc_types::FunctionPointer {
            sig: FunctionSignature {
                inputs: vec![("fmt".into(), Type::Primitive("*const u8".into()))],
                output: Some(Type::Primitive("i32".into())),
                is_c_variadic: true,
            },
            generic_params: vec![],
            header: FunctionHeader {
                is_const: false,
                is_unsafe: false,
                is_async: false,
                abi: Abi::C { unwind: true },
            },
        }));
        assert_eq!(
            type_str(&pointer),
            "extern \"C-unwind\" fn(*const u8, ...) -> i32"
        );
    }

    #[test]
    fn placeholder_constants_use_values_or_an_explicit_marker() {
        let discriminant = Variant {
            kind: VariantKind::Plain,
            discriminant: Some(rustdoc_types::Discriminant {
                expr: "{ _ }".into(),
                value: "42".into(),
            }),
        };
        assert_eq!(
            variant_def(&krate(Vec::new(), Id(0)), "Answer", &discriminant),
            "Answer = 42"
        );

        let evaluated = Constant {
            expr: "_".into(),
            value: Some("7".into()),
            is_literal: false,
        };
        assert_eq!(generic_arg_str(&GenericArg::Const(evaluated.clone())), "7");
        assert_eq!(term_str(&Term::Constant(evaluated)), "7");

        let unavailable = Constant {
            expr: "{ _ }".into(),
            value: None,
            is_literal: false,
        };
        assert_eq!(
            generic_arg_str(&GenericArg::Const(unavailable.clone())),
            "/* unsupported constant expression */"
        );
        assert_eq!(
            term_str(&Term::Constant(unavailable)),
            "/* unsupported constant expression */"
        );
        assert_eq!(
            assoc_const_def("VALUE", &Type::Primitive("usize".into()), Some("{ _ }")),
            "const VALUE: usize = /* unsupported constant expression */;"
        );
    }

    #[test]
    fn constructor_namespaces_follow_shape_and_visibility() {
        let public_field = item(
            1,
            Some("value"),
            Visibility::Public,
            ItemEnum::StructField(Type::Primitive("u8".into())),
        );
        let private_field = item(
            2,
            Some("hidden"),
            Visibility::Default,
            ItemEnum::StructField(Type::Primitive("u8".into())),
        );
        let plain = item(
            3,
            Some("Plain"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Plain {
                    fields: vec![Id(1)],
                    has_stripped_fields: false,
                },
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let tuple = item(
            4,
            Some("Tuple"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Tuple(vec![Some(Id(1))]),
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let private_tuple = item(
            5,
            Some("PrivateTuple"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Tuple(vec![Some(Id(2))]),
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        let unit_variant = item(
            6,
            Some("Unit"),
            Visibility::Default,
            ItemEnum::Variant(Variant {
                kind: VariantKind::Plain,
                discriminant: None,
            }),
        );
        let struct_variant = item(
            7,
            Some("Fields"),
            Visibility::Default,
            ItemEnum::Variant(Variant {
                kind: VariantKind::Struct {
                    fields: vec![Id(1)],
                    has_stripped_fields: false,
                },
                discriminant: None,
            }),
        );
        let mut non_exhaustive_unit = item(
            8,
            Some("FutureUnit"),
            Visibility::Public,
            ItemEnum::Struct(Struct {
                kind: StructKind::Unit,
                generics: generics_empty(),
                impls: vec![],
            }),
        );
        non_exhaustive_unit.attrs = vec![Attribute::NonExhaustive];
        let mut non_exhaustive_tuple_variant = item(
            9,
            Some("FutureTuple"),
            Visibility::Default,
            ItemEnum::Variant(Variant {
                kind: VariantKind::Tuple(vec![Some(Id(1))]),
                discriminant: None,
            }),
        );
        non_exhaustive_tuple_variant.attrs = vec![Attribute::NonExhaustive];
        let docs = krate(
            vec![
                public_field,
                private_field,
                plain,
                tuple,
                private_tuple,
                unit_variant,
                struct_variant,
                non_exhaustive_unit,
                non_exhaustive_tuple_variant,
            ],
            Id(3),
        );

        assert_eq!(item_namespaces(&docs, Id(3)).unwrap(), TYPE_NAMESPACE);
        assert_eq!(
            item_namespaces(&docs, Id(4)).unwrap(),
            TYPE_NAMESPACE | VALUE_NAMESPACE
        );
        assert_eq!(item_namespaces(&docs, Id(5)).unwrap(), TYPE_NAMESPACE);
        assert_eq!(
            item_namespaces(&docs, Id(6)).unwrap(),
            TYPE_NAMESPACE | VALUE_NAMESPACE
        );
        assert_eq!(item_namespaces(&docs, Id(7)).unwrap(), TYPE_NAMESPACE);
        assert_eq!(item_namespaces(&docs, Id(8)).unwrap(), TYPE_NAMESPACE);
        assert_eq!(item_namespaces(&docs, Id(9)).unwrap(), TYPE_NAMESPACE);
    }

    #[test]
    fn type_constrained_self_import_selects_the_type_namespace() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2), Id(3)],
                is_stripped: false,
            }),
        );
        let module = item(
            2,
            Some("foo"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: false,
                items: vec![],
                is_stripped: false,
            }),
        );
        let function = item(
            3,
            Some("foo"),
            Visibility::Public,
            ItemEnum::Function(function()),
        );
        let docs = krate(vec![root, module, function], Id(1));
        let found = find_symbol_report(
            &docs,
            &ImportPath {
                crate_name: "fixture".into(),
                segments: vec![],
                item: "foo".into(),
                namespace: Some(NamespaceConstraint::Type),
            },
        )
        .unwrap();

        assert_eq!(found.imported.kind, "module");
    }

    #[test]
    fn primitive_reexports_do_not_require_a_rustdoc_id() {
        let root = item(
            1,
            Some("fixture"),
            Visibility::Public,
            ItemEnum::Module(Module {
                is_crate: true,
                items: vec![Id(2)],
                is_stripped: false,
            }),
        );
        let primitive = item(
            2,
            Some("MyI32"),
            Visibility::Public,
            ItemEnum::Use(Use {
                source: "i32".into(),
                name: "MyI32".into(),
                id: None,
                is_glob: false,
            }),
        );
        let docs = krate(vec![root, primitive], Id(1));
        let report = find_symbol_report(
            &docs,
            &ImportPath {
                crate_name: "fixture".into(),
                segments: vec![],
                item: "MyI32".into(),
                namespace: None,
            },
        )
        .unwrap();

        assert_eq!(report.imported.definition, "pub use i32 as MyI32;");
        let resolved = report.resolved.unwrap();
        assert_eq!(resolved.kind, "primitive");
        assert_eq!(resolved.name, "i32");
    }
}
