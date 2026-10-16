use icu_normalizer::ComposingNormalizer;
use std::borrow::Cow;
use syn::{ItemUse, UseTree};

#[derive(Debug, Clone)]
pub(crate) struct ImportPath {
    pub(crate) crate_name: String,
    pub(crate) segments: Vec<String>,
    pub(crate) item: String,
    pub(crate) self_parent: bool,
}

pub(crate) fn identifier_key(name: &str) -> Cow<'_, str> {
    ComposingNormalizer::new_nfc().normalize(name.strip_prefix("r#").unwrap_or(name))
}

impl ImportPath {
    pub(crate) fn full_path(&self) -> String {
        let mut parts = vec![self.crate_name.clone()];
        parts.extend(self.segments.clone());
        parts.push(self.item.clone());
        parts.join("::")
    }
}

#[cfg(test)]
pub(crate) fn parse_use_line(line: &str) -> Result<ImportPath, String> {
    let mut imports = parse_use_lines(line)?;
    if imports.len() != 1 {
        return Err("expected one import path".to_string());
    }
    Ok(imports.remove(0))
}

pub(crate) fn parse_use_lines(line: &str) -> Result<Vec<ImportPath>, String> {
    let item = parse_item_use(line)?;
    if item.leading_colon.is_some() {
        return Err("absolute use paths with leading `::` are not supported".to_string());
    }

    let mut imports = Vec::new();
    collect_use_tree(&item.tree, &mut Vec::new(), &mut imports)?;
    if imports.is_empty() {
        return Err(format!("expected external use path, got '{line}'"));
    }
    Ok(imports)
}

fn parse_item_use(line: &str) -> Result<ItemUse, String> {
    let text = line.trim();
    let raw_item = with_semicolon(text);
    match syn::parse_str::<ItemUse>(&raw_item) {
        Ok(item) => return Ok(item),
        Err(raw_err) if starts_like_use_item(text) => {
            return Err(format!("invalid use import syntax: {raw_err}"));
        }
        Err(_) => {}
    }

    let candidate = if text.ends_with(';') {
        format!("use {text}")
    } else {
        format!("use {text};")
    };
    syn::parse_str::<ItemUse>(&candidate).map_err(|err| format!("invalid use import syntax: {err}"))
}

fn starts_like_use_item(text: &str) -> bool {
    text.starts_with("use ") || text.starts_with("pub ") || text.starts_with("pub(")
}

fn with_semicolon(text: &str) -> String {
    if text.ends_with(';') {
        text.to_string()
    } else {
        format!("{text};")
    }
}

fn collect_use_tree(
    tree: &UseTree,
    prefix: &mut Vec<String>,
    imports: &mut Vec<ImportPath>,
) -> Result<(), String> {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            let result = collect_use_tree(&path.tree, prefix, imports);
            prefix.pop();
            result
        }
        UseTree::Name(name) => emit_path(prefix, name.ident.to_string(), imports),
        UseTree::Rename(rename) => emit_path(prefix, rename.ident.to_string(), imports),
        UseTree::Glob(_) => Err("glob imports are not supported".to_string()),
        UseTree::Group(group) => {
            for item in &group.items {
                collect_use_tree(item, prefix, imports)?;
            }
            Ok(())
        }
    }
}

fn emit_path(prefix: &[String], item: String, imports: &mut Vec<ImportPath>) -> Result<(), String> {
    let mut parts = prefix.to_vec();
    let self_parent = item == "self";
    if self_parent {
        if parts.len() < 2 {
            return Err("expected external use path, got crate root `self` import".to_string());
        }
    } else {
        parts.push(item);
    }

    if parts.len() < 2 {
        return Err("expected external use path".to_string());
    }
    if matches!(parts[0].as_str(), "crate" | "self" | "super") {
        return Err("only external crate use paths are supported".to_string());
    }

    imports.push(ImportPath {
        crate_name: parts[0].replace('-', "_"),
        segments: parts[1..parts.len() - 1].to_vec(),
        item: parts.last().expect("parts has len >= 2").clone(),
        self_parent,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_use() {
        let import = parse_use_line("use syn::ItemUse;").unwrap();
        assert_eq!(import.crate_name, "syn");
        assert_eq!(import.segments, Vec::<String>::new());
        assert_eq!(import.item, "ItemUse");
        assert!(!import.self_parent);

        let import = parse_use_line("use syn::ItemUse").unwrap();
        assert_eq!(import.full_path(), "syn::ItemUse");
    }

    #[test]
    fn preserves_raw_spelling_and_exposes_unraw_lookup_key() {
        let import = parse_use_line("use r#type::r#match::r#loop;").unwrap();
        assert_eq!(import.full_path(), "r#type::r#match::r#loop");
        assert_eq!(identifier_key(&import.crate_name), "type");
        assert_eq!(identifier_key(&import.segments[0]), "match");
        assert_eq!(identifier_key(&import.item), "loop");
    }

    #[test]
    fn parses_visible_use_items() {
        for line in [
            "pub(crate) use syn::ItemUse;",
            "pub(super) use syn::ItemUse;",
            "pub(in crate::module) use syn::ItemUse;",
            "pub(crate) use syn::ItemUse",
        ] {
            let import = parse_use_line(line).unwrap();
            assert_eq!(import.full_path(), "syn::ItemUse");
        }
    }

    #[test]
    fn parses_alias_without_use_or_semicolon() {
        let import = parse_use_line("syn::ItemUse as IU").unwrap();
        assert_eq!(import.crate_name, "syn");
        assert_eq!(import.item, "ItemUse");
    }

    #[test]
    fn parses_alias_inside_braces() {
        let imports = parse_use_lines("use syn::{ItemUse as IU, UseTree};").unwrap();
        assert_eq!(
            imports
                .iter()
                .map(ImportPath::full_path)
                .collect::<Vec<_>>(),
            vec!["syn::ItemUse", "syn::UseTree"]
        );
        assert!(!imports[0].self_parent);
    }

    #[test]
    fn parses_alias_to_underscore() {
        let import = parse_use_line("use syn::ItemUse as _;").unwrap();
        assert_eq!(import.full_path(), "syn::ItemUse");
    }

    #[test]
    fn parses_nested_single_brace_use() {
        let import = parse_use_line("use syn::{item::UseTree};").unwrap();
        assert_eq!(import.crate_name, "syn");
        assert_eq!(import.segments, vec!["item"]);
        assert_eq!(import.item, "UseTree");
    }

    #[test]
    fn parses_multi_item_brace_use() {
        let imports = parse_use_lines("use syn::{ItemUse, item::UseTree};").unwrap();
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].item, "ItemUse");
        assert_eq!(imports[1].segments, vec!["item"]);
        assert_eq!(imports[1].item, "UseTree");

        let err = parse_use_line("use syn::{ItemUse, UseTree};").unwrap_err();
        assert_eq!(err, "expected one import path");
    }

    #[test]
    fn parses_nested_groups_comments_trailing_commas_and_self() {
        let imports = parse_use_lines(
            "use tokio::{
                sync::{
                    self,
                    Mutex, // shared lock
                    mpsc::Sender,
                },
                task::JoinHandle,
            };",
        )
        .unwrap();
        assert_eq!(
            imports
                .iter()
                .map(ImportPath::full_path)
                .collect::<Vec<_>>(),
            vec![
                "tokio::sync",
                "tokio::sync::Mutex",
                "tokio::sync::mpsc::Sender",
                "tokio::task::JoinHandle"
            ]
        );
        assert!(imports[0].self_parent);
        assert!(imports[1..].iter().all(|import| !import.self_parent));
    }

    #[test]
    fn preserves_self_parent_constraints_for_trailing_and_renamed_self() {
        for line in [
            "use api::module::self;",
            "use api::module::self as Renamed;",
            "use api::module::{self as Renamed};",
        ] {
            let import = parse_use_line(line).unwrap();
            assert_eq!(import.full_path(), "api::module");
            assert!(import.self_parent);
        }
    }

    #[test]
    fn rejects_glob_use() {
        let err = parse_use_line("use syn::*;").unwrap_err();
        assert_eq!(err, "glob imports are not supported");
    }

    #[test]
    fn rejects_absolute_and_too_short_paths() {
        let err = parse_use_line("use ::syn::ItemUse;").unwrap_err();
        assert_eq!(
            err,
            "absolute use paths with leading `::` are not supported"
        );

        let err = parse_use_line("use syn;").unwrap_err();
        assert!(err.contains("expected external use path"));

        let err = parse_use_line("use self::Thing;").unwrap_err();
        assert_eq!(err, "only external crate use paths are supported");

        let err = parse_use_line("use syn::{self};").unwrap_err();
        assert!(err.contains("crate root `self` import"));
    }

    #[test]
    fn rejects_empty_path_segments_and_empty_brace_items() {
        let err = parse_use_line("use syn::::ItemUse;").unwrap_err();
        assert!(err.contains("invalid use import syntax"));

        let err = parse_use_lines("use syn::{ItemUse,,UseTree};").unwrap_err();
        assert!(err.contains("invalid use import syntax"));
    }

    #[test]
    fn full_path_joins_all_segments() {
        let import = ImportPath {
            crate_name: "tokio".into(),
            segments: vec!["sync".into(), "mpsc".into()],
            item: "Sender".into(),
            self_parent: false,
        };
        assert_eq!(import.full_path(), "tokio::sync::mpsc::Sender");
    }
}
