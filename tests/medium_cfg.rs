use std::{fs, process::Command};
use tempfile::TempDir;

fn workspace(source: &str) -> TempDir {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    for (name, dependencies, source) in [
        (
            "app",
            "[features]\nextra = []\n[dependencies]\ndep = { path = \"../dep\" }\n",
            "",
        ),
        ("dep", "", source),
    ] {
        let path = workspace.path().join(name);
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(path.join("Cargo.toml"), format!("[package]\nname = {name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\n{dependencies}")).unwrap();
        fs::write(path.join("src/lib.rs"), source).unwrap();
    }
    let output = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(workspace.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    workspace
}

fn query(workspace: &TempDir, path: &str, options: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([format!("use dep::{path};"), "--root".into()])
        .arg(workspace.path())
        .args(["--package", "app"])
        .args(options)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn disabled_modules_remove_impls_on_types_declared_elsewhere() {
    let workspace = workspace(
        r#"
pub struct S;
pub trait GhostTrait {}
macro_rules! methods { () => { impl crate::S { pub fn macro_ghost(&self) {} } }; }
impl S { pub fn live(&self) {} }
#[cfg(doc)]
mod docs_only {
    methods!();
    impl crate::GhostTrait for crate::S {}
    impl crate::S { pub fn ghost(&self) {} }
    mod nested { impl crate::S { pub fn nested_ghost(&self) {} } }
    pub use crate::S;
}
#[cfg(doc)] mod r#async { impl crate::S { pub fn raw_ghost(&self) {} } }
#[cfg(doc)] mod café { impl crate::S { pub fn unicode_ghost(&self) {} } }
#[cfg(doc)] mod external;
"#,
    );
    fs::write(
        workspace.path().join("dep/src/external.rs"),
        "impl crate::S { pub fn external_ghost(&self) {} }\nmod nested_external;\n",
    )
    .unwrap();
    fs::create_dir_all(workspace.path().join("dep/src/external")).unwrap();
    fs::write(
        workspace.path().join("dep/src/external/nested_external.rs"),
        "impl crate::S { pub fn deep_ghost(&self) {} }\n",
    )
    .unwrap();
    let report = query(&workspace, "S", &[]);
    assert!(report.contains("pub struct S;"), "{report}");
    assert!(
        report.contains("impl S { pub fn live(self: &Self) }"),
        "{report}"
    );
    assert!(!report.contains("ghost"), "{report}");
    assert!(!report.contains("GhostTrait"), "{report}");
    let json_path = report
        .lines()
        .find_map(|line| line.strip_prefix("source: "))
        .unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(json_path).unwrap()).unwrap();
    let items = raw["index"].as_object().unwrap();
    let ghost = items.values().find(|item| item["name"] == "ghost").unwrap();
    assert_eq!(ghost["attrs"], serde_json::json!([]));
    let ghost_impl = items
        .values()
        .find(|item| {
            item["inner"]["impl"]["items"]
                .as_array()
                .is_some_and(|ids| ids.contains(&ghost["id"]))
        })
        .unwrap();
    assert_eq!(ghost_impl["attrs"], serde_json::json!([]));
    let module = items
        .values()
        .find(|item| item["name"] == "docs_only")
        .unwrap();
    assert!(
        !module["inner"]["module"]["items"]
            .as_array()
            .unwrap()
            .contains(&ghost_impl["id"])
    );
}

#[test]
fn normal_only_modules_report_missing_cross_module_methods() {
    let workspace = workspace("");
    fs::write(
        workspace.path().join("dep/src/external.rs"),
        "use crate::{S as Renamed};\nuse self::Renamed as Alias;\nimpl Alias { pub fn real(&self) -> u8 { 42 } }\n",
    )
    .unwrap();
    for source in [
        "#[cfg(not(doc))] mod implementation { impl crate::S { pub fn real(&self) -> u8 { 42 } } }",
        "#[cfg(not(doc))] mod external;",
        "#[cfg(not(doc))] mod implementation { use crate::S; mod nested { impl super::S { pub fn real(&self) -> u8 { 42 } } } }",
        "#[cfg(not(doc))] mod implementation { use crate as root; impl root::S { pub fn real(&self) -> u8 { 42 } } }",
        "#[cfg(not(doc))] mod implementation { type Alias = crate::S; impl Alias { pub fn real(&self) -> u8 { 42 } } }",
        "#[cfg(not(doc))] mod implementation { use crate::*; impl S { pub fn real(&self) -> u8 { 42 } } }",
        "#[cfg(not(doc))] mod implementation { use crate::S::{self}; impl S { pub fn real(&self) -> u8 { 42 } } }",
        "pub mod other { pub struct S; } #[cfg(not(doc))] mod implementation { use crate::other::*; use crate::S; impl S { pub fn real(&self) -> u8 { 42 } } }",
        "pub trait LocalTrait {} pub mod first { pub use crate::second::*; } pub mod second { pub use crate::first::*; pub use crate::{S, LocalTrait}; } #[cfg(not(doc))] mod implementation { use crate::first::*; impl LocalTrait for u8 {} impl S { pub fn real(&self) -> u8 { 42 } } }",
        "extern crate self as local; #[cfg(not(doc))] mod implementation { impl ::local::S { pub fn real(&self) -> u8 { 42 } } }",
        "extern crate self as local; #[cfg(not(doc))] mod implementation { impl local::S { pub fn real(&self) -> u8 { 42 } } }",
        "extern crate self as local; #[cfg(not(doc))] mod implementation { impl crate::local::S { pub fn real(&self) -> u8 { 42 } } }",
        "extern crate self as local; #[cfg(not(doc))] mod implementation { use ::local::{S as Renamed}; type Alias = Renamed; impl Alias { pub fn real(&self) -> u8 { 42 } } }",
        "extern crate self as local; #[cfg(not(doc))] mod implementation { use ::local as root; impl root::S { pub fn real(&self) -> u8 { 42 } } }",
        "#[cfg(not(doc))] mod implementation { extern crate self as local; impl local::S { pub fn real(&self) -> u8 { 42 } } }",
        "#[cfg(not(doc))] mod implementation { extern crate self as local; mod nested { impl super::local::S { pub fn real(&self) -> u8 { 42 } } } }",
        "extern crate self as local; #[cfg(not(doc))] mod implementation { mod local { pub struct S; } impl ::local::S { pub fn real(&self) -> u8 { 42 } } }",
        "pub mod donor { pub use crate::S; } #[cfg(not(doc))] mod implementation { use donor::S as Alias; impl Alias { pub fn real(&self) -> u8 { 42 } } }",
        "extern crate self as local; #[cfg(not(doc))] mod implementation { impl ::local::S { pub fn real(&self) -> u8 { 42 } } }",
    ] {
        // The last two cases use Rust 2015, where absolute paths begin at the root.
        if source.contains("use donor::") {
            let manifest = workspace.path().join("dep/Cargo.toml");
            let contents = fs::read_to_string(&manifest).unwrap();
            fs::write(manifest, contents.replace("2024", "2015")).unwrap();
        }
        fs::write(
            workspace.path().join("dep/src/lib.rs"),
            format!("pub struct S;\npub use S as PublicAlias;\npub struct Unrelated;\n{source}\n"),
        )
        .unwrap();
        fs::write(
            workspace.path().join("app/src/lib.rs"),
            "pub fn check() -> u8 { dep::S.real() + dep::PublicAlias.real() }\n",
        )
        .unwrap();
        let consumer = Command::new("cargo")
            .args(["check", "--offline", "--locked", "-p", "app"])
            .current_dir(workspace.path())
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .output()
            .unwrap();
        assert!(
            consumer.status.success(),
            "{source}: {}",
            String::from_utf8_lossy(&consumer.stderr)
        );
        for name in ["S", "PublicAlias", "Unrelated"] {
            let output = Command::new(env!("CARGO_BIN_EXE_excra"))
                .arg(format!("use dep::{name};"))
                .arg("--root")
                .arg(workspace.path())
                .args(["--package", "app"])
                .env("CARGO_NET_OFFLINE", "true")
                .env("CARGO_TARGET_DIR", workspace.path().join("target"))
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if name == "Unrelated" {
                assert!(output.status.success(), "{source}: {stderr}");
            } else {
                assert!(!output.status.success(), "{source}: {name}");
                assert!(
                    stderr.contains("non-doc API extraction is incomplete")
                        && stderr.contains("real"),
                    "{source}: {name}: {stderr}"
                );
            }
        }
    }
}

#[test]
fn normal_only_empty_trait_impls_report_incomplete_extraction() {
    let workspace = workspace("");
    for (trait_definition, impl_header, consumer) in [
        (
            "pub trait Marker {}",
            "impl crate::Marker for crate::S {}",
            "fn require<T: dep::Marker>() {} pub fn check() { require::<dep::S>(); }",
        ),
        (
            "pub trait Defaults { fn provided(&self) -> u8 { 7 } }",
            "impl crate::Defaults for crate::S {}",
            "pub fn check() -> u8 { <dep::S as dep::Defaults>::provided(&dep::S) }",
        ),
    ] {
        let source = format!(
            "pub struct S; pub use S as PublicAlias; pub struct Unrelated;\n{trait_definition}\n#[cfg(not(doc))] mod implementation {{ {impl_header} }}\n"
        );
        fs::write(workspace.path().join("dep/src/lib.rs"), &source).unwrap();
        fs::write(workspace.path().join("app/src/lib.rs"), consumer).unwrap();
        let compiler = Command::new("cargo")
            .args(["check", "--offline", "--locked", "-p", "app"])
            .current_dir(workspace.path())
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .output()
            .unwrap();
        assert!(
            compiler.status.success(),
            "{}",
            String::from_utf8_lossy(&compiler.stderr)
        );
        for name in ["S", "PublicAlias"] {
            let output = Command::new(env!("CARGO_BIN_EXE_excra"))
                .arg(format!("use dep::{name};"))
                .arg("--root")
                .arg(workspace.path())
                .args(["--package", "app"])
                .env("CARGO_NET_OFFLINE", "true")
                .env("CARGO_TARGET_DIR", workspace.path().join("target"))
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(2), "{name}: {stderr}");
            assert!(
                stderr.contains("non-doc API extraction is incomplete")
                    && stderr.contains(impl_header.trim_end_matches(" {}"))
                    && stderr.contains("absent under cfg(doc)"),
                "{name}: {stderr}"
            );
        }
        assert!(query(&workspace, "Unrelated", &[]).contains("pub struct Unrelated;"));
        fs::write(
            workspace.path().join("dep/src/lib.rs"),
            source.replace("#[cfg(not(doc))] ", ""),
        )
        .unwrap();
        let report = query(&workspace, "S", &[]);
        let trait_name = trait_definition.split_whitespace().nth(2).unwrap();
        assert!(
            report.contains(&format!("impl {trait_name} for crate::S")),
            "{report}"
        );
    }
}

#[test]
fn feature_metadata_preserves_workspace_config_directory() {
    let workspace = workspace("pub struct S;\n");
    fs::create_dir_all(workspace.path().join("app/.cargo")).unwrap();
    fs::write(
        workspace.path().join("app/.cargo/config.toml"),
        "[build]\nrustc = \"/definitely/missing/member-only-rustc\"\n",
    )
    .unwrap();
    for options in [
        &[][..],
        &["--no-default-features"][..],
        &["--all-features"][..],
        &["--features", "extra"][..],
    ] {
        assert!(query(&workspace, "S", options).contains("pub struct S;"));
    }
}

#[test]
fn unrelated_cfg_imports_preserve_queries_and_relevant_cfg_checks() {
    let workspace = workspace(
        r#"
pub struct S;
pub use S as PublicAlias;
#[cfg(not(doc))] use std::fmt::Debug;
#[cfg(not(doc))] use std::fmt::{self, Display as HiddenDisplay};
pub mod keep {
    pub struct S;
    #[cfg(not(doc))] use std::fmt::Debug;
}
pub use keep::S as NestedAlias;
pub mod sibling {
    #[cfg(not(doc))] use std::{fmt::{Debug, Display as HiddenDisplay}};
}
pub mod file_sibling;
pub fn helper() { #[cfg(not(doc))] use std::fmt::Debug; }
"#,
    );
    fs::write(
        workspace.path().join("dep/src/file_sibling.rs"),
        "#[cfg(not(doc))] use std::fmt::Debug;\npub struct Marker;\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/src/lib.rs"),
        "pub fn check() { let _ = dep::S; let _ = dep::PublicAlias; let _ = dep::keep::S; let _ = dep::NestedAlias; }\n",
    )
    .unwrap();
    let consumer = Command::new("cargo")
        .args(["check", "--offline", "--locked", "-p", "app"])
        .current_dir(workspace.path())
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    assert!(
        consumer.status.success(),
        "{}",
        String::from_utf8_lossy(&consumer.stderr)
    );
    for path in ["S", "PublicAlias", "keep::S", "NestedAlias"] {
        let report = query(&workspace, path, &[]);
        assert!(
            report.contains("definition: pub struct S;"),
            "{path}: {report}"
        );
    }

    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    for source in [
        "#![cfg(not(doc))]\npub struct S;\npub use S as PublicAlias;\n",
        "pub mod actual { pub struct S; }\n#[cfg(not(doc))] pub use actual::S;\n#[cfg(not(doc))] pub use actual::S as PublicAlias;\n",
    ] {
        fs::write(workspace.path().join("dep/src/lib.rs"), source).unwrap();
        for path in ["S", "PublicAlias"] {
            let output = Command::new(env!("CARGO_BIN_EXE_excra"))
                .args([format!("use dep::{path};"), "--root".into()])
                .arg(workspace.path())
                .args(["--package", "app"])
                .env("CARGO_NET_OFFLINE", "true")
                .env("CARGO_TARGET_DIR", workspace.path().join("target"))
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{source}: {path}");
            assert!(
                stderr.contains("non-doc API extraction is incomplete")
                    && stderr.contains("enables source"),
                "{source}: {path}: {stderr}"
            );
        }
    }
}

#[test]
fn explicit_external_imports_shadow_globs_with_unrelated_cfg_imports() {
    let workspace = workspace("");
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\", \"origin\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::create_dir_all(workspace.path().join("origin/src")).unwrap();
    fs::write(
        workspace.path().join("origin/Cargo.toml"),
        "[package]\nname = \"origin\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("origin/src/lib.rs"),
        "pub struct S { pub external: u8 }\n",
    )
    .unwrap();
    let manifest = workspace.path().join("dep/Cargo.toml");
    let contents = fs::read_to_string(&manifest).unwrap();
    fs::write(
        manifest,
        format!("{contents}[dependencies]\norigin = {{ path = \"../origin\" }}\n"),
    )
    .unwrap();
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(workspace.path())
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    fs::write(
        workspace.path().join("app/src/lib.rs"),
        "use dep::LocalTrait;\npub fn check() -> u8 { dep::S { local: 1 }.live() + dep::PublicAlias { local: 2 }.live() + dep::ExternalS { external: 3 }.external() }\n",
    )
    .unwrap();
    for binding in [
        "use origin::S;",
        "use origin::{S};",
        "use origin::S::{self};",
        "use origin::S as r#S;",
        "use crate::route::S;",
    ] {
        for imports in [
            format!("use crate::*; {binding}"),
            format!("{binding} use crate::*;"),
        ] {
            fs::write(
                workspace.path().join("dep/src/lib.rs"),
                format!(
                    r#"
pub struct S {{ pub local: u8 }}
impl S {{ pub fn live(&self) -> u8 {{ self.local }} }}
pub use S as PublicAlias;
pub use origin::S as ExternalS;
pub trait LocalTrait {{ fn external(&self) -> u8; }}
mod route {{ pub use origin::S; }}
#[cfg(not(doc))] use std::fmt::Debug;
mod sibling {{ #[cfg(not(doc))] use std::fmt::Display; }}
#[cfg(not(doc))] mod implementation {{
    {imports}
    impl LocalTrait for S {{ fn external(&self) -> u8 {{ self.external }} }}
}}
"#
                ),
            )
            .unwrap();
            let consumer = Command::new("cargo")
                .args(["check", "--offline", "--locked", "-p", "app"])
                .current_dir(workspace.path())
                .env("CARGO_TARGET_DIR", workspace.path().join("target"))
                .output()
                .unwrap();
            assert!(
                consumer.status.success(),
                "{imports}: {}",
                String::from_utf8_lossy(&consumer.stderr)
            );
            for path in ["S", "PublicAlias"] {
                let report = query(&workspace, path, &[]);
                assert!(
                    report.contains("definition: pub struct S { pub local: u8 }")
                        && report.contains("pub fn live(self: &Self) -> u8"),
                    "{imports}: {path}: {report}"
                );
                assert!(
                    !report.contains("LocalTrait"),
                    "{imports}: {path}: {report}"
                );
                assert!(!report.contains("external"), "{imports}: {path}: {report}");
            }
        }
    }
}

#[test]
fn cfg_checks_distinguish_same_named_items_in_sibling_modules() {
    let workspace = workspace(
        r#"
pub mod good { pub struct Same; }
pub mod other {
    pub struct Same;
    impl Same { #[cfg(not(doc))] pub fn only(&self) {} }
}
pub mod separate { #[cfg(not(doc))] pub struct Same; }
pub use good::Same as GoodSame;
pub use other::Same as OtherSame;
pub enum Choice { Base, #[cfg(not(doc))] Extra }
pub mod file_good;
pub mod file_other;
#[path = "custom.rs"] pub mod remapped;
pub use file_good::Same as FileGoodSame;
"#,
    );
    fs::write(
        workspace.path().join("dep/src/file_good.rs"),
        "pub struct Same;\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("dep/src/file_other.rs"),
        "pub struct Same;\nimpl Same { #[cfg(not(doc))] pub fn only(&self) {} }\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("dep/src/custom.rs"),
        "pub struct Same;\nimpl Same { #[cfg(not(doc))] pub fn only(&self) {} }\n",
    )
    .unwrap();

    for path in ["good::Same", "GoodSame", "file_good::Same", "FileGoodSame"] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                format!("use dep::{path};"),
                "--root".into(),
                workspace.path().display().to_string(),
                "--package".into(),
                "app".into(),
            ])
            .env("CARGO_NET_OFFLINE", "true")
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("definition: pub struct Same;"),
            "{path}: {stdout}"
        );
        assert!(!stdout.contains("only"), "{path}: {stdout}");
    }

    for path in [
        "other::Same",
        "OtherSame",
        "file_other::Same",
        "remapped::Same",
        "separate::Same",
        "Choice::Extra",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                format!("use dep::{path};"),
                "--root".into(),
                workspace.path().display().to_string(),
                "--package".into(),
                "app".into(),
            ])
            .env("CARGO_NET_OFFLINE", "true")
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "{path}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("non-doc API extraction is incomplete"),
            "{path}: {stderr}"
        );
        if matches!(path, "OtherSame" | "Choice::Extra" | "remapped::Same") {
            assert!(stderr.contains("enables source"), "{path}: {stderr}");
        }
    }
}
