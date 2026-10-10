use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::{PermissionsExt, symlink};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

fn lock_workspace(workspace: &TempDir) {
    let output = Command::new("cargo")
        .args([
            "generate-lockfile",
            "--offline",
            "--manifest-path",
            workspace.path().join("Cargo.toml").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "failed to generate fixture lockfile: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn host_target_triple() -> String {
    let output = Command::new("rustc").arg("-vV").output().unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap()
        .to_string()
}

fn report_source(report: &str) -> PathBuf {
    PathBuf::from(
        report
            .lines()
            .find_map(|line| line.strip_prefix("source: "))
            .expect("report contains a source path"),
    )
}

fn write_member(workspace: &TempDir, name: &str, manifest: &str, source: &str) {
    fs::create_dir_all(workspace.path().join(name).join("src")).unwrap();
    fs::write(workspace.path().join(name).join("Cargo.toml"), manifest).unwrap();
    fs::write(workspace.path().join(name).join("src/lib.rs"), source).unwrap();
}

#[test]
fn binary_reports_dependency_item() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use cargo_metadata::MetadataCommand;", "--root", "."])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: cargo_metadata"));
    assert!(stdout.contains("item: struct MetadataCommand"));
}

#[test]
fn binary_keeps_inherent_method_impl_constraints() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct Boxed<T>(pub T);\nimpl<T> Boxed<T> where T: Clone { pub fn duplicate(&self) -> T { self.0.clone() } }\nimpl Boxed<u8> { pub fn bytes(&self) -> u8 { self.0 } }\n",
    );
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Boxed;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("methods:\n  impl Boxed<u8> { pub fn bytes(self: &Self) -> u8 }\n  impl<T> Boxed<T> where T: Clone { pub fn duplicate(self: &Self) -> T }\n"),
        "{stdout}"
    );
}

#[test]
fn binary_uses_excra_toolchain_for_the_entire_query() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct Thing;\n",
    );
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env(
            "RUSTUP_TOOLCHAIN",
            "definitely-missing-excra-ambient-toolchain",
        )
        .env("EXCRA_TOOLCHAIN", "nightly-2025-09-10")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("definition: pub struct Thing;"));
}

#[test]
fn binary_reports_batch_brace_imports() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use cargo_metadata::{Metadata, Package};", "--root", "."])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("import: use cargo_metadata::Metadata;"));
    assert!(stdout.contains("item: struct Metadata"));
    assert!(stdout.contains("import: use cargo_metadata::Package;"));
    assert!(stdout.contains("item: struct Package"));
}

#[test]
fn binary_reports_transitive_item_through_external_module_reexport() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use cargo_metadata::camino::Utf8PathBuf;", "--root", "."])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: camino"));
    assert!(stdout.contains("item: struct Utf8PathBuf"));
}

#[test]
fn binary_reports_transitive_external_crate_root_reexport() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use cargo_metadata::camino;", "--root", "."])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: camino"));
    assert!(stdout.contains("item: crate camino"));
    assert!(
        stdout.contains("definition: definition rendering unsupported for crate root camino\n")
    );
    assert!(!stdout.contains("definition: pub mod camino;"));
}

#[test]
fn binary_rejects_same_spelling_across_rust_namespaces_as_ambiguous() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use serde::Serialize;", "--root", "."])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ambiguous import 'serde::Serialize'"),
        "{stderr}"
    );
    assert!(
        stderr.contains("ambiguous across Rust namespaces"),
        "{stderr}"
    );
    assert!(stderr.contains("Trait Serialize"), "{stderr}");
    assert!(stderr.contains("ProcDerive Serialize"), "{stderr}");
}

#[test]
fn binary_reports_visible_use_item() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["pub(crate) use syn::ItemUse;", "--root", "."])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: syn"));
    assert!(stdout.contains("item: struct ItemUse"));
}

#[test]
fn binary_formats_trait_methods_without_pub() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use syn::parse::Parse;", "--root", "."])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("fn parse(input: ParseStream<'_>) -> Result<Self>;"));
    assert!(!stdout.contains("pub fn parse(input: ParseStream<'_>) -> Result<Self>"));
}

#[test]
fn binary_supports_virtual_workspace_package_selection() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join("app/src")).unwrap();
    fs::create_dir_all(workspace.path().join("dep_crate/src")).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["app", "dep_crate"]
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
dep_crate = { path = "../dep_crate" }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("dep_crate/Cargo.toml"),
        r#"
[package]
name = "dep_crate"
version = "0.1.0"
edition = "2024"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("dep_crate/src/lib.rs"),
        "pub struct Thing;\n",
    )
    .unwrap();

    lock_workspace(&workspace);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep_crate::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: dep_crate"));
    assert!(stdout.contains("item: struct Thing"));
}

#[test]
fn binary_reports_deprecation_and_semantic_attributes() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join("app/src")).unwrap();
    fs::create_dir_all(workspace.path().join("metadata_dep/src")).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["app", "metadata_dep"]
resolver = "3"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
metadata_dep = { path = "../metadata_dep" }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("metadata_dep/Cargo.toml"),
        r#"
[package]
name = "metadata_dep"
version = "0.1.0"
edition = "2024"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("metadata_dep/src/lib.rs"),
        r#"
#[deprecated(since = "1.2.3", note = "use Replacement")]
#[must_use = "inspect the value"]
#[non_exhaustive]
#[repr(C, align(8))]
pub struct Annotated {
    pub value: u8,
}
"#,
    )
    .unwrap();

    lock_workspace(&workspace);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use metadata_dep::Annotated;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("deprecation:\n  since: 1.2.3\n  note: use Replacement\n"));
    assert!(stdout.contains("  #[must_use = \"inspect the value\"]\n"));
    assert!(stdout.contains("  #[non_exhaustive]\n"));
    assert!(stdout.contains("  #[repr(C, align(8))]\n"));
}

#[test]
fn binary_resolves_enum_variants_raw_identifiers_markdown_and_absolute_locations() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join("app/src")).unwrap();
    fs::create_dir_all(workspace.path().join("raw_dep/src")).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["app", "raw_dep"]
resolver = "3"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies.type]
package = "raw_dep"
path = "../raw_dep"
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("raw_dep/Cargo.toml"),
        r#"
[package]
name = "raw_dep"
version = "0.1.0"
edition = "2024"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("raw_dep/src/lib.rs"),
        r#"
pub enum Number {
    One,
}

pub mod r#match {
    pub struct r#type;
}

#[doc = "First paragraph.\n\n- parent\n  - child\n\n    code"]
pub struct Documented;
"#,
    )
    .unwrap();

    lock_workspace(&workspace);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use r#type::{Number::One, r#match::r#type, Documented};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("import: use r#type::Number::One;\nitem: variant One\n"));
    assert!(stdout.contains("definition: One\n"));
    assert!(stdout.contains("import: use r#type::r#match::r#type;\nitem: struct type\n"));
    assert!(
        stdout.contains("definition: pub struct r#type;\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("docs:\n  First paragraph.\n  \n  - parent\n    - child\n  \n      code\n")
    );
    assert!(stdout.contains(&format!(
        "location: {}/raw_dep/src/lib.rs:",
        workspace.path().display()
    )));
}

#[test]
fn binary_does_not_misclassify_a_missing_item_beside_an_external_glob() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "facade", "middle"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["app", "facade", "middle"]
resolver = "3"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
facade = { path = "../facade" }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("facade/Cargo.toml"),
        r#"
[package]
name = "facade"
version = "0.1.0"
edition = "2024"

[dependencies]
middle = { path = "../middle" }
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("facade/src/lib.rs"),
        "pub use middle::*;\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("middle/Cargo.toml"),
        r#"
[package]
name = "middle"
version = "0.1.0"
edition = "2024"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("middle/src/lib.rs"),
        "pub struct Present;\n",
    )
    .unwrap();

    lock_workspace(&workspace);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use facade::Missing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("item 'Missing' not found"), "{stderr}");
    assert!(!stderr.contains("is a public re-export"), "{stderr}");
}

#[test]
fn binary_renders_advanced_item_kinds_without_inventing_definitions() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "advanced"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["app", "advanced"]
resolver = "3"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
advanced = { path = "../advanced" }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("advanced/Cargo.toml"),
        r#"
[package]
name = "advanced"
version = "0.1.0"
edition = "2024"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("advanced/src/lib.rs"),
        r#"
#![feature(auto_traits, trait_alias)]

pub trait Alias = Send + Sync;
pub auto trait Marker {}
pub mod api {}
"#,
    )
    .unwrap();
    lock_workspace(&workspace);
    let advanced = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use advanced::{Alias, Marker, api};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        advanced.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&advanced.stderr)
    );
    let stdout = String::from_utf8_lossy(&advanced.stdout);
    assert!(stdout.contains("definition: pub trait Alias = Send + Sync;\n"));
    assert!(stdout.contains("definition: pub auto trait Marker { ... }\n"));
    assert!(stdout.contains("definition: pub mod api;\n"));
}

#[test]
fn binary_accepts_explicit_library_crate_types() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "facade"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["app", "facade"]
resolver = "3"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
facade = { path = "../facade" }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("facade/Cargo.toml"),
        r#"
[package]
name = "facade"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["rlib", "cdylib"]
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("facade/src/lib.rs"),
        "pub struct Thing;\n",
    )
    .unwrap();

    lock_workspace(&workspace);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use facade::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("item: struct Thing"));
}

#[test]
fn binary_walks_multi_hop_external_reexports_and_external_globs() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "facade", "middle", "origin"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["app", "facade", "middle", "origin"]
resolver = "3"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
facade = { path = "../facade" }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("facade/Cargo.toml"),
        r#"
[package]
name = "facade"
version = "0.1.0"
edition = "2024"

[dependencies]
middle = { path = "../middle" }
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("facade/src/lib.rs"),
        r#"
pub use middle::Thing;
pub mod globbed {
    pub use middle::*;
}
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("middle/Cargo.toml"),
        r#"
[package]
name = "middle"
version = "0.1.0"
edition = "2024"

[dependencies]
origin = { path = "../origin" }
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("middle/src/lib.rs"),
        "pub use origin::Thing;\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("origin/Cargo.toml"),
        r#"
[package]
name = "origin"
version = "0.1.0"
edition = "2024"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("origin/src/lib.rs"),
        "pub struct Thing;\n",
    )
    .unwrap();

    lock_workspace(&workspace);
    for import in ["use facade::Thing;", "use facade::globbed::Thing;"] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                import,
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "import: {import}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("crate: origin 0.1.0"), "{stdout}");
        assert!(
            stdout.contains("item: struct Thing") || stdout.contains("resolved item: struct Thing"),
            "{stdout}"
        );
    }
}

#[test]
fn binary_follows_named_and_glob_reexports_through_a_renamed_dependency() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"facade\", \"origin\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nfacade = { path = \"../facade\" }\n",
        "",
    );
    write_member(
        &workspace,
        "facade",
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies.alias]\npackage = \"origin\"\npath = \"../origin\"\n",
        "pub use alias::Named;\npub use alias::*;\n",
    );
    write_member(
        &workspace,
        "origin",
        "[package]\nname = \"origin\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct Named;\npub struct Globbed;\n",
    );
    lock_workspace(&workspace);

    for item in ["Named", "Globbed"] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                &format!("use facade::{item};"),
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
            ])
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "{item}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("crate: origin 0.1.0"), "{stdout}");
        assert!(stdout.contains(&format!("item: struct {item}")), "{stdout}");
    }
}

#[test]
fn binary_preserves_high_risk_definition_semantics() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "definitions"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["app", "definitions"]
resolver = "3"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
definitions = { path = "../definitions" }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("definitions/Cargo.toml"),
        r#"
[package]
name = "definitions"
version = "0.1.0"
edition = "2024"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("definitions/src/lib.rs"),
        r#"
pub type Callback = for<'a> fn(&'a str) -> &'a str;

pub struct Named {
    pub value: u8,
    hidden: u16,
}

pub struct Tuple(pub u8, u16);

pub union Choice {
    pub byte: u8,
    pub word: u16,
    hidden: u32,
}

#[repr(u8)]
pub enum Number {
    One = 1,
    Five = 5,
    Fields {
        visible: u8,
        #[doc(hidden)]
        hidden: u16,
    },
    #[doc(hidden)]
    Hidden,
}

#[macro_export]
macro_rules! make {
    () => { 7 };
}

pub const COUNT: usize = 1 + 2;
pub static READY: bool = true;

pub trait Defaults {
    const VALUE: u8 = 7;
}

unsafe extern "C" {
    pub static FOREIGN: u8;
    pub safe static SAFE_FOREIGN: u8;
}
"#,
    )
    .unwrap();

    lock_workspace(&workspace);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use definitions::{Callback, Named, Tuple, Choice, Number, make, COUNT, READY, Defaults, FOREIGN, SAFE_FOREIGN};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "definition: pub type Callback = for<'a> fn(&'a str) -> &'a str;",
        "definition: pub struct Named { pub value: u8, /* private/stripped fields */ }",
        "definition: pub struct Tuple(pub u8, /* private/stripped field */);",
        "definition: pub union Choice { pub byte: u8, pub word: u16, /* private/stripped fields */ }",
        "definition: pub enum Number { One = 1, Five = 5, Fields { visible: u8, hidden: u16 }, Hidden }",
        "definition: pub const COUNT: usize = 3usize;",
        "definition: pub static READY: bool = true;",
        "  const VALUE: u8 = 7;",
        "definition: definition rendering unsupported as standalone Rust for unsafe extern static; declaration inside extern block: static FOREIGN: u8;",
        "definition: definition rendering unsupported as standalone Rust for safe extern static; declaration inside extern block: static SAFE_FOREIGN: u8;",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("definition: macro_rules! make {\n    () => { ... };\n}\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("macro_rules! make macro_rules! make"));
    assert!(stdout.contains("fields: private/stripped"), "{stdout}");
}

#[test]
fn binary_uses_the_selected_dependency_feature_unit_without_defaults() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "feature_dep"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"feature_dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
feature_dep = { path = "../feature_dep", default-features = false, features = ["selected"] }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("feature_dep/Cargo.toml"),
        r#"
[package]
name = "feature_dep"
version = "0.1.0"
edition = "2024"

[features]
default = ["default_api"]
default_api = []
selected = []
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("feature_dep/src/lib.rs"),
        r#"
#[cfg(feature = "default_api")]
pub struct DefaultOnly;

#[cfg(feature = "selected")]
pub struct SelectedOnly;
"#,
    )
    .unwrap();
    lock_workspace(&workspace);

    let selected = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use feature_dep::SelectedOnly;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        selected.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&selected.stderr)
    );
    assert!(
        String::from_utf8_lossy(&selected.stdout).contains("definition: pub struct SelectedOnly;")
    );

    let default_only = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use feature_dep::DefaultOnly;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(!default_only.status.success());
    assert!(String::from_utf8_lossy(&default_only.stderr).contains("item 'DefaultOnly' not found"));
}

#[test]
fn binary_refuses_to_change_a_stale_lockfile() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "locked_dep"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"locked_dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
locked_dep = { path = "../locked_dep" }
"#,
    )
    .unwrap();
    let dependency_manifest = workspace.path().join("locked_dep/Cargo.toml");
    fs::write(
        &dependency_manifest,
        "[package]\nname = \"locked_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("locked_dep/src/lib.rs"),
        "pub struct Thing;\n",
    )
    .unwrap();
    lock_workspace(&workspace);
    let lock_path = workspace.path().join("Cargo.lock");
    let original_lock = fs::read(&lock_path).unwrap();
    fs::write(
        dependency_manifest,
        "[package]\nname = \"locked_dep\"\nversion = \"0.2.0\"\nedition = \"2024\"\n",
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use locked_dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("without changing Cargo.lock"), "{stderr}");
    assert!(
        stderr.contains("run `cargo check` or `cargo build`"),
        "{stderr}"
    );
    assert_eq!(fs::read(lock_path).unwrap(), original_lock);
}

#[test]
fn binary_follows_the_immediate_reexport_edge_when_versions_share_an_item() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "facade", "middle", "other", "origin_v1", "origin_v2"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"
[workspace]
members = ["app", "facade", "middle", "other"]
exclude = ["origin_v1", "origin_v2"]
resolver = "3"
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nfacade = { path = \"../facade\" }\n",
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("facade/Cargo.toml"),
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nmiddle = { path = \"../middle\" }\nother = { path = \"../other\" }\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("facade/src/lib.rs"),
        "pub use middle::*;\npub mod nested { pub use middle::Thing as NestedThing; }\npub use crate::nested::NestedThing;\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("middle/Cargo.toml"),
        "[package]\nname = \"middle\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\norigin = { path = \"../origin_v1\" }\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("middle/src/lib.rs"),
        "pub use origin::Thing;\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("other/Cargo.toml"),
        "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\norigin = { path = \"../origin_v2\" }\n",
    )
    .unwrap();
    fs::write(workspace.path().join("other/src/lib.rs"), "").unwrap();
    for (directory, version, source) in [
        ("origin_v1", "1.0.0", "pub struct Thing;\n"),
        ("origin_v2", "2.0.0", "pub struct Thing;\n"),
    ] {
        fs::write(
            workspace.path().join(directory).join("Cargo.toml"),
            format!("[package]\nname = \"origin\"\nversion = \"{version}\"\nedition = \"2024\"\n"),
        )
        .unwrap();
        fs::write(workspace.path().join(directory).join("src/lib.rs"), source).unwrap();
    }
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use facade::{Thing, NestedThing};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: origin 1.0.0"), "{stdout}");
    assert!(stdout.contains("item: struct Thing"), "{stdout}");
    assert!(
        stdout.contains("import: use facade::NestedThing;"),
        "{stdout}"
    );
}

#[test]
fn binary_detects_explicit_item_plus_external_glob_namespace_ambiguity() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "facade", "macros"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"facade\", \"macros\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nfacade = { path = \"../facade\" }\n",
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("facade/Cargo.toml"),
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nmacros = { path = \"../macros\" }\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("facade/src/lib.rs"),
        "pub struct Thing;\npub use macros::*;\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("macros/Cargo.toml"),
        "[package]\nname = \"macros\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\nproc-macro = true\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("macros/src/lib.rs"),
        "#![allow(non_snake_case)]\nuse proc_macro::TokenStream;\n#[proc_macro]\npub fn Thing(_input: TokenStream) -> TokenStream { TokenStream::new() }\n",
    )
    .unwrap();
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use facade::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ambiguous across Rust namespaces"),
        "{stderr}"
    );
    assert!(stderr.contains("struct Thing"), "{stderr}");
    assert!(stderr.contains("macro Thing"), "{stderr}");
}

#[test]
fn binary_does_not_call_a_missing_lowercase_item_a_module() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use syn::definitely_missing_function;", "--root", "."])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("item 'definitely_missing_function' not found"));
    assert!(!stderr.contains("appears to be a module"));
    assert!(stderr.contains("check the path, visibility, and selected feature set"));
}

fn context_reexport_workspace(dependency_section: &str, build_script: bool) -> TempDir {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "facade", "origin"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"facade\", \"origin\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n{dependency_section}\nfacade = {{ path = \"../facade\" }}\n"
        ),
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    if build_script {
        fs::write(workspace.path().join("app/build.rs"), "fn main() {}\n").unwrap();
    }
    fs::write(
        workspace.path().join("facade/Cargo.toml"),
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\norigin = { path = \"../origin\" }\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("facade/src/lib.rs"),
        "pub use origin::ContextThing;\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("origin/Cargo.toml"),
        "[package]\nname = \"origin\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("origin/src/lib.rs"),
        "pub struct ContextThing;\n",
    )
    .unwrap();
    lock_workspace(&workspace);
    workspace
}

#[test]
fn binary_preserves_dev_context_across_external_reexports() {
    let workspace = context_reexport_workspace("[dev-dependencies]", false);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use facade::ContextThing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--include-dev",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: origin 0.1.0"), "{stdout}");
    assert!(
        stdout.contains("dependency: transitive via facade (dev)"),
        "{stdout}"
    );
    assert!(stdout.contains("definition: pub struct ContextThing;"));
}

#[test]
fn binary_preserves_build_context_across_external_reexports() {
    let workspace = context_reexport_workspace("[build-dependencies]", true);
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        "[build]\ntarget = \"wasm32-unknown-unknown\"\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use facade::ContextThing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--include-build",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: origin 0.1.0"), "{stdout}");
    assert!(
        stdout.contains("dependency: transitive via facade (build)"),
        "{stdout}"
    );
    assert!(stdout.contains("definition: pub struct ContextThing;"));
}

#[test]
fn binary_selects_the_dev_unit_for_a_normal_plus_dev_feature_split() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "facade", "origin"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"facade\", \"origin\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
facade = { path = "../facade", default-features = false, features = ["normal-api"] }

[dev-dependencies]
facade = { path = "../facade", default-features = false, features = ["dev-api"] }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("facade/Cargo.toml"),
        r#"
[package]
name = "facade"
version = "0.1.0"
edition = "2024"

[features]
normal-api = ["origin/normal-api"]
dev-api = ["origin/dev-api"]

[dependencies]
origin = { path = "../origin", default-features = false }
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("facade/src/lib.rs"),
        "pub use origin::*;\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("origin/Cargo.toml"),
        r#"
[package]
name = "origin"
version = "0.1.0"
edition = "2024"

[features]
normal-api = []
dev-api = []
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("origin/src/lib.rs"),
        r#"
#[cfg(feature = "normal-api")]
pub struct NormalOnly;

#[cfg(feature = "dev-api")]
pub struct DevOnly;
"#,
    )
    .unwrap();
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use facade::DevOnly;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--include-dev",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: origin 0.1.0"), "{stdout}");
    assert!(
        stdout.contains("dependency: transitive via facade (dev)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("definition: pub struct DevOnly;"),
        "{stdout}"
    );
    assert!(!stdout.contains("dependency: normal, dev"), "{stdout}");
}

#[cfg(unix)]
#[test]
fn binary_composes_with_a_recording_workspace_rustc_wrapper() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "wrapped_dep"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"wrapped_dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nwrapped_dep = { path = \"../wrapped_dep\" }\n",
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("wrapped_dep/Cargo.toml"),
        "[package]\nname = \"wrapped_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("wrapped_dep/src/lib.rs"),
        "pub struct WrappedThing;\n",
    )
    .unwrap();
    let wrapper = workspace.path().join("recording-wrapper.sh");
    let wrapper_log = workspace.path().join("wrapper.log");
    fs::write(
        &wrapper,
        "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"$EXCRA_TEST_WRAPPER_LOG\"\nexec \"$@\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).unwrap();
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        format!(
            "[build]\nrustc-workspace-wrapper = {:?}\n",
            wrapper.to_str().unwrap()
        ),
    )
    .unwrap();
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use wrapped_dep::WrappedThing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env("EXCRA_TEST_WRAPPER_LOG", &wrapper_log)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("definition: pub struct WrappedThing;")
    );
    let log = fs::read_to_string(wrapper_log).unwrap();
    assert!(
        log.lines().any(|line| line.ends_with("rustdoc")),
        "workspace wrapper did not receive the rustdoc invocation:\n{log}"
    );
}

#[test]
fn binary_honors_configured_target_and_distinguishes_host_units() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "shared", "macro_dep"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"shared\", \"macro_dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        "[build]\ntarget = \"wasm32-unknown-unknown\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dependencies]
shared = { path = "../shared" }
macro_dep = { path = "../macro_dep" }

[build-dependencies]
shared = { path = "../shared" }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(workspace.path().join("app/build.rs"), "fn main() {}\n").unwrap();
    fs::write(
        workspace.path().join("shared/Cargo.toml"),
        "[package]\nname = \"shared\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("shared/src/lib.rs"),
        r#"
#[cfg(target_arch = "wasm32")]
pub struct TargetOnly;

#[cfg(not(target_arch = "wasm32"))]
pub struct HostOnly;
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("macro_dep/Cargo.toml"),
        "[package]\nname = \"macro_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\nproc-macro = true\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("macro_dep/src/lib.rs"),
        "use proc_macro::TokenStream;\n#[proc_macro]\npub fn make_thing(_input: TokenStream) -> TokenStream { TokenStream::new() }\n",
    )
    .unwrap();
    lock_workspace(&workspace);

    let context_output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use shared::{TargetOnly, HostOnly};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--include-build",
        ])
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    assert!(
        context_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&context_output.stderr)
    );
    let context_stdout = String::from_utf8_lossy(&context_output.stdout);
    let target_report = context_stdout
        .split("\n\n")
        .find(|report| report.contains("definition: pub struct TargetOnly;"))
        .expect("normal target report");
    assert!(
        target_report.contains("target: wasm32-unknown-unknown"),
        "{target_report}"
    );
    assert!(
        target_report.contains("dependency: normal"),
        "{target_report}"
    );
    let host_report = context_stdout
        .split("\n\n")
        .find(|report| report.contains("definition: pub struct HostOnly;"))
        .expect("build host report");
    assert!(
        host_report.contains(&format!("target: {}", host_target_triple())),
        "{host_report}"
    );
    assert!(host_report.contains("dependency: build"), "{host_report}");

    let target_source = report_source(target_report);
    let host_source = report_source(host_report);
    assert_ne!(target_source, host_source);
    let target_json = fs::read_to_string(&target_source)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", target_source.display()));
    let host_json = fs::read_to_string(&host_source)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", host_source.display()));
    assert!(target_json.contains("\"TargetOnly\""), "{target_source:?}");
    assert!(!target_json.contains("\"HostOnly\""), "{target_source:?}");
    assert!(host_json.contains("\"HostOnly\""), "{host_source:?}");
    assert!(!host_json.contains("\"TargetOnly\""), "{host_source:?}");

    let macro_output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use macro_dep::make_thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    assert!(
        macro_output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&macro_output.stderr)
    );
    let macro_stdout = String::from_utf8_lossy(&macro_output.stdout);
    assert!(
        macro_stdout.contains(&format!("target: {}", host_target_triple())),
        "{macro_stdout}"
    );
    assert!(
        macro_stdout.contains("item: macro make_thing"),
        "{macro_stdout}"
    );
}

#[cfg(unix)]
#[test]
fn binary_rejects_rustdoc_only_cfg_items_and_keeps_valid_platform_items() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"platform_dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nplatform_dep = { path = \"../platform_dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "platform_dep",
        "[package]\nname = \"platform_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        r#"#[cfg(any(doc, all(unix, windows)))]
pub struct DocOnly;

#[cfg(any(doc, unix))]
pub struct UnixOnly;

#[cfg_attr(not(doc), cfg(windows))]
pub struct CfgAttrOnly;
"#,
    );
    lock_workspace(&workspace);

    let valid = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use platform_dep::UnixOnly;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    assert!(String::from_utf8_lossy(&valid.stdout).contains("pub struct UnixOnly;"));

    let doc_only = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use platform_dep::DocOnly;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(!doc_only.status.success());
    let stderr = String::from_utf8_lossy(&doc_only.stderr);
    assert!(stderr.contains("item 'DocOnly' not found"), "{stderr}");

    let cfg_attr_only = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use platform_dep::CfgAttrOnly;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(!cfg_attr_only.status.success());
    let stderr = String::from_utf8_lossy(&cfg_attr_only.stderr);
    assert!(stderr.contains("item 'CfgAttrOnly' not found"), "{stderr}");
}

#[test]
fn binary_supports_relative_child_and_sibling_roots() {
    let parent = TempDir::new().unwrap();
    let sibling = parent.path().join("sibling");
    for member in ["app", "dep"] {
        fs::create_dir_all(sibling.join(member).join("src")).unwrap();
    }
    fs::write(
        sibling.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        sibling.join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
    )
    .unwrap();
    fs::write(sibling.join("app/src/lib.rs"), "").unwrap();
    fs::write(
        sibling.join("dep/Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(sibling.join("dep/src/lib.rs"), "pub struct Thing;\n").unwrap();
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline", "--manifest-path"])
        .arg(sibling.join("Cargo.toml"))
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );

    let child = Command::new(env!("CARGO_BIN_EXE_excra"))
        .current_dir(parent.path())
        .args(["use dep::Thing;", "--root", "sibling", "--package", "app"])
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );

    fs::create_dir_all(parent.path().join("runner")).unwrap();
    let relative_sibling = Command::new(env!("CARGO_BIN_EXE_excra"))
        .current_dir(parent.path().join("runner"))
        .args([
            "use dep::Thing;",
            "--root",
            "../sibling",
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        relative_sibling.status.success(),
        "{}",
        String::from_utf8_lossy(&relative_sibling.stderr)
    );
}

#[test]
fn binary_forwards_root_feature_selection_in_a_workspace() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "feature_dep"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"feature_dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"

[features]
default = ["feature_dep/default-api"]
extra = ["feature_dep/extra"]

[dependencies]
feature_dep = { path = "../feature_dep", default-features = false }
"#,
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("feature_dep/Cargo.toml"),
        r#"
[package]
name = "feature_dep"
version = "0.1.0"
edition = "2024"

[features]
default-api = []
extra = []
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("feature_dep/src/lib.rs"),
        "#[cfg(feature = \"default-api\")]\npub struct DefaultOnly;\n#[cfg(feature = \"extra\")]\npub struct Extra;\n",
    )
    .unwrap();
    lock_workspace(&workspace);

    for feature_args in [vec!["--features", "extra"], vec!["--all-features"]] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_excra"));
        command.args([
            "use feature_dep::Extra;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ]);
        command.args(feature_args);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("pub struct Extra;"));
        assert!(String::from_utf8_lossy(&output.stdout).contains("root features: --"));
    }

    let no_defaults = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use feature_dep::DefaultOnly;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--no-default-features",
        ])
        .output()
        .unwrap();
    assert!(!no_defaults.status.success());
    assert!(String::from_utf8_lossy(&no_defaults.stderr).contains("DefaultOnly' not found"));
}

#[test]
fn binary_reports_cfg_attr_derives_only_when_the_feature_is_enabled() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\", \"marker_derive\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2024"

[features]
marker = ["dep/marker"]

[dependencies]
dep = { path = "../dep", default-features = false }
"#,
        "",
    );
    write_member(
        &workspace,
        "dep",
        r#"[package]
name = "dep"
version = "0.1.0"
edition = "2024"

[features]
marker = ["dep:marker_derive"]

[dependencies]
marker_derive = { path = "../marker_derive", optional = true }
"#,
        r#"#[cfg(feature = "marker")]
use marker_derive::Marker;

#[cfg_attr(feature = "marker", derive(Marker))]
pub struct Thing;
"#,
    );
    write_member(
        &workspace,
        "marker_derive",
        "[package]\nname = \"marker_derive\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\nproc-macro = true\n",
        "use proc_macro::TokenStream;\n#[proc_macro_derive(Marker)]\npub fn marker(_: TokenStream) -> TokenStream { TokenStream::new() }\n",
    );
    lock_workspace(&workspace);

    let query = |feature_args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_excra"));
        command.args([
            "use dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ]);
        command.args(feature_args);
        command
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .output()
            .unwrap()
    };

    let disabled = query(&[]);
    assert!(
        disabled.status.success(),
        "{}",
        String::from_utf8_lossy(&disabled.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&disabled.stdout).contains("derives:"),
        "{}",
        String::from_utf8_lossy(&disabled.stdout)
    );

    let enabled = query(&["--features", "marker"]);
    assert!(
        enabled.status.success(),
        "{}",
        String::from_utf8_lossy(&enabled.stderr)
    );
    assert!(
        String::from_utf8_lossy(&enabled.stdout).contains("derives: Marker\n"),
        "{}",
        String::from_utf8_lossy(&enabled.stdout)
    );
}

#[test]
fn binary_reconciles_doc_only_and_normal_cfg_attr_semantics() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        r#"#[cfg_attr(doc, derive(Clone))]
pub struct Token;
#[cfg_attr(doc, derive(PartialEq))]
pub struct Comparable;
#[cfg_attr(not(doc), non_exhaustive)]
pub struct Closed;
#[cfg_attr(doc, non_exhaustive)]
pub struct Open;
#[cfg_attr(doc, repr(C))]
pub struct DocRepr(pub u8);
#[cfg_attr(not(doc), repr(transparent))]
pub struct NormalRepr(pub u8);
#[repr(C)]
#[cfg_attr(doc, repr(align(8)))]
pub struct MixedRepr(pub u8);
#[cfg_attr(not(doc), must_use = "normal")]
pub struct NormalMustUse;
#[cfg_attr(doc, must_use = "docs")]
pub struct DocMustUse;
#[cfg_attr(doc, deprecated(note = "docs only"))]
pub struct DocDeprecated;
#[cfg_attr(not(doc), deprecated(note = "normal only"))]
pub struct NormalDeprecated;
"#,
    );
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::{Token, Comparable, Closed, Open, DocRepr, NormalRepr, MixedRepr, NormalMustUse, DocMustUse, DocDeprecated, NormalDeprecated};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let report = |name: &str| {
        let start = format!("item: struct {name}\n");
        stdout
            .split(&start)
            .nth(1)
            .unwrap_or_else(|| panic!("missing {name}: {stdout}"))
            .split("crate: dep ")
            .next()
            .unwrap()
            .to_string()
    };
    assert!(!report("Token").contains("derives:"), "{stdout}");
    assert!(
        !report("Token").contains("impl Clone for Token"),
        "{stdout}"
    );
    assert!(!report("Comparable").contains("derives:"), "{stdout}");
    assert!(!report("Comparable").contains("impl PartialEq"), "{stdout}");
    assert!(
        !report("Comparable").contains("StructuralPartialEq"),
        "{stdout}"
    );
    assert!(
        report("Closed").contains("  #[non_exhaustive]\n"),
        "{stdout}"
    );
    assert!(!report("Open").contains("#[non_exhaustive]"), "{stdout}");
    assert!(!report("DocRepr").contains("#[repr("), "{stdout}");
    assert!(
        report("NormalRepr").contains("  #[repr(transparent)]\n"),
        "{stdout}"
    );
    assert!(report("MixedRepr").contains("  #[repr(C)]\n"), "{stdout}");
    assert!(!report("MixedRepr").contains("align(8)"), "{stdout}");
    assert!(
        report("NormalMustUse").contains("  #[must_use = \"normal\"]\n"),
        "{stdout}"
    );
    assert!(!report("DocMustUse").contains("#[must_use"), "{stdout}");
    assert!(
        !report("DocDeprecated").contains("deprecation:"),
        "{stdout}"
    );
    assert!(
        report("NormalDeprecated").contains("  note: normal only\n"),
        "{stdout}"
    );

    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(report_source(&stdout)).unwrap()).unwrap();
    let index = json["index"].as_object().unwrap();
    let token = index.values().find(|item| item["name"] == "Token").unwrap();
    assert!(token["attrs"].to_string().contains("cfg_attr"));
    assert!(
        token["inner"]["struct"]["impls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| {
                let impl_item = &index[&id.to_string()];
                impl_item["attrs"]
                    .to_string()
                    .contains("automatically_derived")
                    && impl_item["inner"]["impl"]["trait"]["path"] == "Clone"
            })
    );
    let mixed = index
        .values()
        .find(|item| item["name"] == "MixedRepr")
        .unwrap();
    assert_eq!(mixed["attrs"][1]["repr"]["align"], 8);

    fs::write(
        workspace.path().join("app/src/lib.rs"),
        "pub fn verify() { fn needs_clone<T: Clone>() {} needs_clone::<dep::Token>(); let _ = dep::Closed; }",
    )
    .unwrap();
    let normal = Command::new("cargo")
        .args(["check", "--offline", "--locked", "-p", "app"])
        .current_dir(workspace.path())
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    assert!(!normal.status.success());
    let diagnostics = String::from_utf8_lossy(&normal.stderr);
    assert!(diagnostics.contains("Token: Clone"), "{diagnostics}");
    assert!(
        diagnostics.contains("cannot be constructed because it is `#[non_exhaustive]`"),
        "{diagnostics}"
    );
}

#[test]
fn binary_restores_derive_impls_missing_from_rustdoc() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\", \"origin\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "pub fn verify() { fn needs_clone<T: Clone>() {} needs_clone::<dep::Token>(); needs_clone::<dep::Plain>(); needs_clone::<dep::Generic<String>>(); }",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\norigin = { path = \"../origin\" }\n",
        "#[cfg_attr(not(doc), derive(Clone))] pub struct Token;\n#[derive(Clone)] pub struct Plain;\n#[cfg_attr(not(doc), derive(Clone))] pub struct Generic<T>(pub T);\npub use origin::*;\n",
    );
    write_member(
        &workspace,
        "origin",
        "[package]\nname = \"origin\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub mod unrelated {}\n",
    );
    lock_workspace(&workspace);
    let normal = Command::new("cargo")
        .args(["check", "--offline", "--locked", "-p", "app"])
        .current_dir(workspace.path())
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    assert!(
        normal.status.success(),
        "{}",
        String::from_utf8_lossy(&normal.stderr)
    );

    let query = |item| {
        Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                format!("use dep::{item};"),
                "--root".into(),
                workspace.path().to_str().unwrap().into(),
                "--package".into(),
                "app".into(),
            ])
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .output()
            .unwrap()
    };
    let plain = query("Plain");
    assert!(
        plain.status.success(),
        "{}",
        String::from_utf8_lossy(&plain.stderr)
    );
    let stdout = String::from_utf8_lossy(&plain.stdout);
    assert!(stdout.contains("derives: Clone\n"), "{stdout}");
    assert!(stdout.contains("impl Clone for Plain"), "{stdout}");

    let token = query("Token");
    assert!(
        token.status.success(),
        "{}",
        String::from_utf8_lossy(&token.stderr)
    );
    let stdout = String::from_utf8_lossy(&token.stdout);
    assert!(stdout.contains("derives: Clone\n"), "{stdout}");
    assert!(
        stdout.contains("impls:\n  impl Clone for Token\n"),
        "{stdout}"
    );
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(report_source(&stdout)).unwrap()).unwrap();
    let index = json["index"].as_object().unwrap();
    let token = index.values().find(|item| item["name"] == "Token").unwrap();
    assert!(
        !token["inner"]["struct"]["impls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| index[&id.to_string()]["inner"]["impl"]["trait"]["path"] == "Clone")
    );

    let generic = query("Generic");
    assert!(
        generic.status.success(),
        "{}",
        String::from_utf8_lossy(&generic.stderr)
    );
    let stdout = String::from_utf8_lossy(&generic.stdout);
    assert!(stdout.contains("derives: Clone\n"), "{stdout}");
    assert!(
        stdout.contains("impl<T: ::core::clone::Clone> Clone for Generic<T>"),
        "{stdout}"
    );
}

#[cfg(unix)]
#[test]
fn binary_composes_with_a_general_cfg_injecting_rustc_wrapper() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "wrapped_dep"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"wrapped_dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nwrapped_dep = { path = \"../wrapped_dep\" }\n",
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("wrapped_dep/Cargo.toml"),
        "[package]\nname = \"wrapped_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("wrapped_dep/src/lib.rs"),
        "#[cfg(wrapped)]\npub struct WrappedThing;\n",
    )
    .unwrap();
    let wrapper = workspace.path().join("cfg-wrapper.sh");
    fs::write(
        &wrapper,
        "#!/bin/sh\ncompiler=\"$1\"\nshift\nexec \"$compiler\" --cfg wrapped \"$@\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).unwrap();
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        format!("[build]\nrustc-wrapper = {:?}\n", wrapper.to_str().unwrap()),
    )
    .unwrap();
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use wrapped_dep::WrappedThing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("pub struct WrappedThing;"));
}

#[cfg(unix)]
#[test]
fn binary_matches_raw_cfg_flags_and_cfg_attr_derives_from_a_rustc_wrapper() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"raw_dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nraw_dep = { path = \"../raw_dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "raw_dep",
        "[package]\nname = \"raw_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "#[cfg(any(doc, r#async))]\n#[cfg_attr(r#async, derive(Debug))]\npub struct RawCfg;\n#[cfg(café)] pub struct UnicodeCfg;\n#[cfg(café = \"oui\")] pub struct UnicodeValue;\n",
    );
    let wrapper = workspace.path().join("raw-cfg-wrapper.sh");
    fs::write(
        &wrapper,
        "#!/bin/sh\ncompiler=\"$1\"\nshift\ncase \" $* \" in\n  *' --crate-name raw_dep '*) set -- --cfg café --cfg 'café=\"oui\"' \"$@\" ;;\nesac\nexec \"$compiler\" --cfg r#async \"$@\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).unwrap();
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        format!("[build]\nrustc-wrapper = {:?}\n", wrapper.to_str().unwrap()),
    )
    .unwrap();
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use raw_dep::{RawCfg, UnicodeCfg, UnicodeValue};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("definition: pub struct RawCfg;"),
        "{stdout}"
    );
    assert!(stdout.contains("derives: Debug\n"), "{stdout}");
    assert!(
        stdout.contains("definition: pub struct UnicodeCfg;"),
        "{stdout}"
    );
    assert!(
        stdout.contains("definition: pub struct UnicodeValue;"),
        "{stdout}"
    );
}

#[test]
fn binary_does_not_add_a_toolchain_hint_to_an_unrelated_compiler_error() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct Thing;\npub fn broken() { let _ = toolchain; }\n",
    );
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot find value `toolchain`"), "{stderr}");
    assert!(
        !stderr.contains("compatible nightly toolchain not found"),
        "{stderr}"
    );
    assert!(!stderr.contains("rustup toolchain install"), "{stderr}");
}

#[test]
fn binary_fails_when_one_selected_context_cannot_be_verified() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "dep"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n[dev-dependencies]\ndep = { path = \"../dep\", features = [\"broken\"] }\n",
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("dep/Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[features]\nbroken = []\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("dep/src/lib.rs"),
        "#[cfg(feature = \"broken\")] compile_error!(\"dev graph intentionally fails\");\npub struct Thing;\n",
    )
    .unwrap();
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--include-dev",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("query 'dep::Thing' is incomplete"),
        "{stderr}"
    );
    assert!(stderr.contains("dev graph intentionally fails"), "{stderr}");
}

#[test]
fn binary_preserves_bound_pointee_parentheses() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    let signature = "pub fn inspect<'a>(a: &(dyn Send + Sync), b: &'a mut (dyn Send + 'a), c: *const (dyn Send + Sync), d: *mut (dyn Send + Sync), e: &(impl Send + Sync))";
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        &format!("{signature} {{}}\n"),
    );
    lock_workspace(&workspace);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::inspect;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let definition = stdout
        .lines()
        .find_map(|line| line.strip_prefix("definition: "))
        .unwrap();
    assert_eq!(definition, signature);
    fs::write(
        workspace.path().join("app/src/lib.rs"),
        format!("{definition} {{}}\n"),
    )
    .unwrap();
    let compiled = Command::new("cargo")
        .args([
            "check",
            "--offline",
            "--locked",
            "--manifest-path",
            workspace.path().join("Cargo.toml").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
}

#[test]
fn binary_respects_shadowed_intermediate_glob_paths() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"facade\", \"origin\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nfacade = { path = \"../facade\" }\n",
        "",
    );
    write_member(
        &workspace,
        "facade",
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\norigin = { path = \"../origin\" }\n",
        "pub mod nested {}\npub use origin::nested as named;\npub use origin::*;\n",
    );
    write_member(
        &workspace,
        "origin",
        "[package]\nname = \"origin\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub mod nested { pub struct Thing; }\npub mod sibling { pub struct Thing; }\n",
    );
    lock_workspace(&workspace);
    for (binding, shadows) in [
        ("pub mod nested {}", true),
        ("mod nested { pub struct Thing; }", true),
        ("pub(crate) mod nested { pub struct Thing; }", true),
        (
            "macro_rules! shadow { () => { mod nested {} } } shadow!();",
            true,
        ),
        ("#[cfg(any())] mod nested {}", false),
        ("use origin::sibling as nested;", true),
        ("pub(crate) use origin::sibling as nested;", true),
        (
            "macro_rules! shadow { () => { use origin::sibling as nested; } } shadow!();",
            true,
        ),
        ("#[cfg(any())] use origin::sibling as nested;", false),
    ] {
        fs::write(
            workspace.path().join("facade/src/lib.rs"),
            format!("{binding}\npub use origin::nested as named;\npub use origin::*;\n"),
        )
        .unwrap();
        for (path, succeeds) in [("nested", !shadows), ("sibling", true), ("named", true)] {
            let query = format!("use facade::{path}::Thing;");
            let output = Command::new(env!("CARGO_BIN_EXE_excra"))
                .env("CARGO_TARGET_DIR", workspace.path().join("target"))
                .args([
                    &query,
                    "--root",
                    workspace.path().to_str().unwrap(),
                    "--package",
                    "app",
                ])
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                succeeds,
                "{binding}: {query}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if succeeds {
                assert!(
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .any(|line| line == "definition: pub struct Thing;")
                );
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                assert!(
                    stderr.contains("item 'Thing' not found in facade 0.1.0"),
                    "{stderr}"
                );
            }
            fs::write(workspace.path().join("app/src/lib.rs"), query).unwrap();
            let compiled = Command::new("cargo")
                .env("CARGO_TARGET_DIR", workspace.path().join("target"))
                .args([
                    "check",
                    "--offline",
                    "--locked",
                    "--manifest-path",
                    workspace.path().join("Cargo.toml").to_str().unwrap(),
                ])
                .output()
                .unwrap();
            assert_eq!(
                compiled.status.success(),
                succeeds,
                "{binding}: {path}: {}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
        }
    }
}

#[test]
fn binary_deduplicates_equivalent_external_reexport_routes() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"facade\", \"left\", \"right\", \"origin\"]\nresolver = \"3\"\n",
    ).unwrap();
    for (name, dependencies, source) in [
        ("app", "facade = { path = \"../facade\" }", ""),
        (
            "facade",
            "left = { path = \"../left\" }\nright = { path = \"../right\" }",
            "pub use left::*; pub use right::*;",
        ),
        (
            "left",
            "origin = { path = \"../origin\" }",
            "pub use origin::Thing;",
        ),
        (
            "right",
            "origin = { path = \"../origin\" }",
            "pub use origin::Thing;",
        ),
        ("origin", "", "pub struct Thing;"),
    ] {
        write_member(
            &workspace,
            name,
            &format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\n{dependencies}\n"
            ),
            source,
        );
    }
    lock_workspace(&workspace);
    for (right, succeeds) in [
        ("pub use origin::Thing;", true),
        ("pub struct Thing;", false),
    ] {
        fs::write(workspace.path().join("right/src/lib.rs"), right).unwrap();
        let query = "use facade::Thing;";
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .args([
                query,
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            succeeds,
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if succeeds {
            let stdout = String::from_utf8(output.stdout).unwrap();
            assert_eq!(
                stdout
                    .lines()
                    .filter(|line| line.starts_with("definition:"))
                    .collect::<Vec<_>>(),
                [
                    "definition: pub use origin::Thing as Thing;",
                    "definition: pub struct Thing;"
                ]
            );
        } else {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("ambiguous external re-export")
            );
        }
        fs::write(workspace.path().join("app/src/lib.rs"), query).unwrap();
        let compiled = Command::new("cargo")
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .args([
                "check",
                "--offline",
                "--locked",
                "--manifest-path",
                workspace.path().join("Cargo.toml").to_str().unwrap(),
                "-p",
                "app",
            ])
            .output()
            .unwrap();
        assert_eq!(
            compiled.status.success(),
            succeeds,
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    }
}

#[test]
fn binary_applies_named_shadowing_per_exact_namespace() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "facade", "direct_origin", "glob_origin"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"facade\", \"direct_origin\", \"glob_origin\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nfacade = { path = \"../facade\" }\n",
    )
    .unwrap();
    fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    fs::write(
        workspace.path().join("facade/Cargo.toml"),
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndirect_origin = { path = \"../direct_origin\" }\nglob_origin = { path = \"../glob_origin\" }\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("facade/src/lib.rs"),
        "pub use direct_origin::{Same, Partial, Variant, VariantUnit};\npub use glob_origin::*;\n",
    )
    .unwrap();
    for member in ["direct_origin", "glob_origin"] {
        fs::write(
            workspace.path().join(member).join("Cargo.toml"),
            format!("[package]\nname = \"{member}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
        )
        .unwrap();
    }
    fs::write(
        workspace.path().join("direct_origin/src/lib.rs"),
        "pub struct Same;\npub struct Partial { pub value: u8 }\npub enum Direct { Variant { value: u8 }, VariantUnit }\npub use Direct::{Variant, VariantUnit};\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("glob_origin/src/lib.rs"),
        "pub struct Same;\npub struct Partial(pub u8);\npub enum Other { Variant(u8), VariantUnit }\npub use Other::*;\n",
    )
    .unwrap();
    lock_workspace(&workspace);

    for import in ["use facade::Same;", "use facade::VariantUnit;"] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                import,
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{import}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("crate: direct_origin"));
    }
    for import in ["use facade::Partial;", "use facade::Variant;"] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                import,
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{import}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("ambiguous external re-export"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn binary_rejects_local_globs_shadowed_by_private_type_and_value_bindings() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "#![allow(hidden_glob_reexports, dead_code, non_snake_case)]\npub mod donor {\n    pub struct Thing;\n    pub fn action() {}\n    pub struct Alias;\n    pub struct Same;\n    pub type Split = u8;\n    pub fn Split() {}\n    pub fn Conditional() {}\n}\npub use donor::*;\npub(crate) struct Thing;\npub(crate) fn action() {}\npub(crate) use donor::Alias;\npub type Same = u8;\npub(crate) fn Same() {}\npub(crate) fn Split() {}\n#[cfg(any())]\npub(crate) fn Conditional() {}\n",
    );
    lock_workspace(&workspace);

    for name in ["Thing", "action", "Alias"] {
        let shadowed = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                &format!("use dep::{name};"),
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
            ])
            .output()
            .unwrap();
        assert!(
            !shadowed.status.success(),
            "{name}: stdout: {}",
            String::from_utf8_lossy(&shadowed.stdout)
        );
        assert!(
            String::from_utf8_lossy(&shadowed.stderr).contains("is private"),
            "{name}: {}",
            String::from_utf8_lossy(&shadowed.stderr)
        );

        fs::write(
            workspace.path().join("app/src/lib.rs"),
            format!("use dep::{name};\n"),
        )
        .unwrap();
        let compiler = Command::new("cargo")
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .args([
                "check",
                "--offline",
                "--locked",
                "--manifest-path",
                workspace.path().join("Cargo.toml").to_str().unwrap(),
                "-p",
                "app",
            ])
            .output()
            .unwrap();
        assert!(
            !compiler.status.success(),
            "compiler unexpectedly accepted private binding {name}"
        );
        assert!(
            String::from_utf8_lossy(&compiler.stderr).contains("is private"),
            "{name}: {}",
            String::from_utf8_lossy(&compiler.stderr)
        );
        fs::write(workspace.path().join("app/src/lib.rs"), "").unwrap();
    }

    for (import, expected) in [
        ("use dep::Same;", "item: type Same"),
        ("use dep::Split;", "item: type Split"),
        ("use dep::Conditional;", "item: fn Conditional"),
        ("use dep::donor::Thing;", "item: struct Thing"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                import,
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{import}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "{import}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn binary_honors_self_import_namespaces_and_primitive_reexports() {
    let workspace = TempDir::new().unwrap();
    for member in ["app", "shape_dep", "origin"] {
        fs::create_dir_all(workspace.path().join(member).join("src")).unwrap();
    }
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"shape_dep\", \"origin\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nshape_dep = { path = \"../shape_dep\" }\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("app/src/lib.rs"),
        "pub use shape_dep::{foo::{Nested, External}, Choice::One, remote::Nested as Remote};",
    )
    .unwrap();
    fs::write(
        workspace.path().join("shape_dep/Cargo.toml"),
        "[package]\nname = \"shape_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\norigin = { path = \"../origin\" }\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("shape_dep/src/lib.rs"),
        "pub mod foo { pub struct Nested; pub use origin::External; }\npub fn foo() {}\npub enum Choice { One }\n#[macro_export]\nmacro_rules! Choice { () => {}; }\npub use origin::remote;\npub use origin::remote_fn as remote;\npub struct Bar;\n#[macro_export]\nmacro_rules! Bar { () => {}; }\npub use i32 as MyI32;\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "origin",
        "[package]\nname = \"origin\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct External; pub mod remote { pub struct Nested; } pub fn remote_fn() {}",
    );
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use shape_dep::{foo::{self, Nested, External}, Choice::One, remote::Nested, Bar::{self}, MyI32};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("import: use shape_dep::foo::{self};\nitem: module foo"),
        "{stdout}"
    );
    assert!(
        stdout.contains("import: use shape_dep::Bar::{self};\nitem: struct Bar"),
        "{stdout}"
    );
    assert!(stdout.contains("item: use MyI32"), "{stdout}");
    assert!(stdout.contains("resolved item: primitive i32"), "{stdout}");

    for expected in [
        "import: use shape_dep::foo::Nested;\nitem: struct Nested",
        "import: use shape_dep::foo::External;\nitem: use External",
        "import: use shape_dep::Choice::One;\nitem: variant One",
        "import: use shape_dep::remote::Nested;\nitem: struct Nested",
    ] {
        assert!(stdout.contains(expected), "{stdout}");
    }
    for import in ["use shape_dep::foo;", "use shape_dep::Choice;"] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                import,
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{import}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("ambiguous across Rust namespaces"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn binary_does_not_prescribe_lockfile_refresh_for_manifest_errors() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[package\nname = \"broken\"\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("failed to read cargo metadata"), "{stderr}");
    assert!(
        !stderr.contains("run `cargo check` or `cargo build`"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn binary_reports_non_unicode_roots_without_panicking() {
    let root = OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .arg("use dep::Thing;")
        .arg("--root")
        .arg(root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no Cargo.toml found"), "{stderr}");
    assert!(!stderr.contains("panicked at"), "{stderr}");
}

#[test]
fn binary_keeps_non_exhaustive_constructors_out_of_the_external_value_namespace() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"facade\", \"named\", \"globbed\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nfacade = { path = \"../facade\" }\n",
        "",
    );
    write_member(
        &workspace,
        "facade",
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nnamed = { path = \"../named\" }\nglobbed = { path = \"../globbed\" }\n",
        "pub use named::Token;\npub use globbed::*;\n",
    );
    write_member(
        &workspace,
        "named",
        "[package]\nname = \"named\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "#[non_exhaustive]\npub struct Token;\n",
    );
    write_member(
        &workspace,
        "globbed",
        "[package]\nname = \"globbed\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "#[allow(non_snake_case)]\npub fn Token() {}\n",
    );
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use facade::Token;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ambiguous external re-export"), "{stderr}");
}

#[test]
fn binary_follows_external_reexports_through_stripped_private_modules() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"facade\", \"origin\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nfacade = { path = \"../facade\" }\n",
        "",
    );
    write_member(
        &workspace,
        "facade",
        "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\norigin = { path = \"../origin\" }\n",
        "mod hidden { pub use origin::Thing; }\npub use hidden::Thing;\npub extern crate origin as source_alias;\npub use source_alias::AliasThing;\n",
    );
    write_member(
        &workspace,
        "origin",
        "[package]\nname = \"origin\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct Thing;\npub struct AliasThing;\n",
    );
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use facade::{Thing, AliasThing};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: origin 0.1.0"), "{stdout}");
    assert!(stdout.contains("resolved item: struct Thing"), "{stdout}");
    assert!(
        stdout.contains("resolved item: struct AliasThing"),
        "{stdout}"
    );

    let syn = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use syn::Ident;", "--root", "."])
        .output()
        .unwrap();
    assert!(
        !syn.status.success(),
        "{}",
        String::from_utf8_lossy(&syn.stdout)
    );
    assert!(
        String::from_utf8_lossy(&syn.stderr).contains("ambiguous import 'syn::Ident'"),
        "{}",
        String::from_utf8_lossy(&syn.stderr)
    );
}

fn disjoint_alias_workspace(context_section: &str, build_script: bool) -> TempDir {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies.shared]\npackage = \"serde\"\nversion = \"=1.0.228\"\n\n[{context_section}.shared]\npackage = \"serde_json\"\nversion = \"=1.0.149\"\n"
        ),
        "",
    );
    if build_script {
        fs::write(workspace.path().join("app/build.rs"), "fn main() {}\n").unwrap();
    }
    lock_workspace(&workspace);
    workspace
}

#[test]
fn binary_resolves_one_extern_name_to_disjoint_normal_and_build_packages() {
    let workspace = disjoint_alias_workspace("build-dependencies", true);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use shared::{de::IgnoredAny, Value};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--include-build",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: serde_core 1.0.228"), "{stdout}");
    assert!(
        stdout.contains("dependency: transitive via serde (normal)"),
        "{stdout}"
    );
    assert!(stdout.contains("crate: serde_json 1.0.149"), "{stdout}");
    assert!(stdout.contains("dependency: build"), "{stdout}");
}

#[test]
fn binary_resolves_one_extern_name_to_disjoint_normal_and_dev_packages() {
    let workspace = disjoint_alias_workspace("dev-dependencies", false);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use shared::{de::IgnoredAny, Value};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--include-dev",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("crate: serde_core 1.0.228"), "{stdout}");
    assert!(
        stdout.contains("dependency: transitive via serde (normal)"),
        "{stdout}"
    );
    assert!(stdout.contains("crate: serde_json 1.0.149"), "{stdout}");
    assert!(stdout.contains("dependency: dev"), "{stdout}");
}

#[test]
fn binary_selects_same_package_dev_and_build_units_from_cargo_edges() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"shared\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dev-dependencies]
shared = { path = "../shared", default-features = false, features = ["dev-api"] }

[build-dependencies]
shared = { path = "../shared", default-features = false, features = ["build-api"] }
"#,
        "",
    );
    fs::write(workspace.path().join("app/build.rs"), "fn main() {}\n").unwrap();
    write_member(
        &workspace,
        "shared",
        r#"[package]
name = "shared"
version = "0.1.0"
edition = "2024"

[features]
dev-api = []
build-api = []
"#,
        r#"#[cfg(feature = "dev-api")]
pub struct DevOnly;

#[cfg(feature = "build-api")]
pub struct BuildOnly;
"#,
    );
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use shared::{DevOnly, BuildOnly};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--include-dev",
            "--include-build",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("dependency: dev\n") && stdout.contains("definition: pub struct DevOnly;"),
        "{stdout}"
    );
    assert!(
        stdout.contains("dependency: build\n")
            && stdout.contains("definition: pub struct BuildOnly;"),
        "{stdout}"
    );
}

#[test]
fn binary_matches_profile_before_compiler_flag_overrides() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct Thing;\n#[cfg(any(doc, not(debug_assertions)))]\npub struct NoDebug;\n",
    );
    lock_workspace(&workspace);

    for (flags, query, definition) in [
        (
            "-C opt-level=1",
            "use dep::Thing;",
            "definition: pub struct Thing;",
        ),
        (
            "-C opt-level=1 -C debug-assertions=no -C overflow-checks=no -C debuginfo=0 -C codegen-units=1 -C panic=abort",
            "use dep::NoDebug;",
            "definition: pub struct NoDebug;",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                query,
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
            ])
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env("RUSTFLAGS", flags)
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{flags}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == definition)
        );
    }
}

#[test]
fn binary_matches_same_feature_dev_and_build_units_by_profile_cfg() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["app", "shared"]
resolver = "3"

[profile.dev]
debug-assertions = true
overflow-checks = true

[profile.dev.build-override]
debug-assertions = false
overflow-checks = false

[profile.test]
debug-assertions = true
overflow-checks = true

[profile.test.build-override]
debug-assertions = false
overflow-checks = false
"#,
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dev-dependencies]
shared = { path = "../shared" }

[build-dependencies]
shared = { path = "../shared" }
"#,
        "",
    );
    fs::write(workspace.path().join("app/build.rs"), "fn main() {}\n").unwrap();
    write_member(
        &workspace,
        "shared",
        "[package]\nname = \"shared\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        r#"#[cfg(any(debug_assertions, doc))]
pub struct DevProfile;

#[cfg(any(not(debug_assertions), doc))]
pub struct BuildProfile;
"#,
    );
    lock_workspace(&workspace);

    for (item, flag, dependency) in [
        ("DevProfile", "--include-dev", "dev"),
        ("BuildProfile", "--include-build", "build"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                &format!("use shared::{item};"),
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
                flag,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{item}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(&format!("dependency: {dependency}\n")),
            "{stdout}"
        );
        assert!(
            stdout.contains(&format!("definition: pub struct {item};")),
            "{stdout}"
        );
    }
}

#[cfg(unix)]
#[test]
fn binary_matches_same_feature_dev_and_build_units_by_opt_level() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        r#"[workspace]
members = ["app", "shared"]
resolver = "3"

[profile.test]
opt-level = 1
debug = 0
debug-assertions = true
overflow-checks = true
incremental = false
codegen-units = 1

[profile.test.build-override]
opt-level = 0
debug = 0
debug-assertions = true
overflow-checks = true
incremental = false
codegen-units = 1
"#,
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        r#"[package]
name = "app"
version = "0.1.0"
edition = "2024"

[dev-dependencies]
shared = { path = "../shared" }

[build-dependencies]
shared = { path = "../shared" }
"#,
        "",
    );
    fs::write(workspace.path().join("app/build.rs"), "fn main() {}\n").unwrap();
    write_member(
        &workspace,
        "shared",
        "[package]\nname = \"shared\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        r#"#[cfg(any(doc, dev_profile))]
pub struct DevProfile;

#[cfg(any(doc, build_profile))]
pub struct BuildProfile;
"#,
    );
    fs::write(
        workspace.path().join("shared/build.rs"),
        r#"fn main() {
    println!("cargo::rustc-check-cfg=cfg(dev_profile)");
    println!("cargo::rustc-check-cfg=cfg(build_profile)");
    match std::env::var("OPT_LEVEL").as_deref() {
        Ok("1") => println!("cargo::rustc-cfg=dev_profile"),
        Ok("0") => println!("cargo::rustc-cfg=build_profile"),
        other => panic!("unexpected OPT_LEVEL: {other:?}"),
    }
}
"#,
    )
    .unwrap();
    let wrapper = workspace.path().join("record-rustdoc.sh");
    let rustdoc_log = workspace.path().join("rustdoc.log");
    fs::write(
        &wrapper,
        "#!/bin/sh\ncase \"$1\" in\n  */rustdoc|rustdoc) printf '%s\\n' rustdoc >> \"$EXCRA_TEST_RUSTDOC_LOG\" ;;\nesac\nexec \"$@\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).unwrap();
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        format!(
            "[build]\nrustc-workspace-wrapper = {:?}\n",
            wrapper.to_str().unwrap()
        ),
    )
    .unwrap();
    lock_workspace(&workspace);

    for flags in ["", "-C opt-level=2"] {
        fs::write(&rustdoc_log, "").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                "use shared::DevProfile;",
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
                "--include-dev",
            ])
            .env("EXCRA_TEST_RUSTDOC_LOG", &rustdoc_log)
            .env_remove("CARGO_ENCODED_RUSTFLAGS")
            .env("RUSTFLAGS", flags)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let source = report_source(&stdout);
        assert_eq!(
            stdout,
            format!(
                "crate: shared 0.1.0\ndependency: dev\ntarget: {}\nroot features: default\nsource: {}\nimport: use shared::DevProfile;\nitem: struct DevProfile\nlocation: {}/shared/src/lib.rs:2\ndefinition: pub struct DevProfile;\ndocs: (none)\n",
                host_target_triple(),
                source.display(),
                workspace.path().display()
            )
        );
        assert_eq!(fs::read_to_string(&rustdoc_log).unwrap(), "rustdoc\n");
    }
}

#[test]
fn binary_suggests_flags_for_dependencies_excluded_by_kind() {
    let dev = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use tempfile::TempDir;", "--root", "."])
        .output()
        .unwrap();
    assert!(!dev.status.success());
    assert_eq!(
        String::from_utf8_lossy(&dev.stderr),
        "excra: crate 'tempfile' is declared only in dependency contexts excluded by default; retry with `--include-dev`\n"
    );

    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"only_build\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[build-dependencies]\nonly_build = { path = \"../only_build\" }\n",
        "",
    );
    fs::write(workspace.path().join("app/build.rs"), "fn main() {}\n").unwrap();
    write_member(
        &workspace,
        "only_build",
        "[package]\nname = \"only_build\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct BuildThing;\n",
    );
    lock_workspace(&workspace);

    let build = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use only_build::BuildThing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(!build.status.success());
    assert_eq!(
        String::from_utf8_lossy(&build.stderr),
        "excra: crate 'only_build' is declared only in dependency contexts excluded by default; retry with `--include-build`\n"
    );
}

#[cfg(unix)]
#[test]
fn binary_suggests_include_build_for_a_host_only_cross_target_dependency() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"host_build\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[target.'cfg(unix)'.build-dependencies]\nhost_build = { path = \"../host_build\" }\n",
        "",
    );
    fs::write(workspace.path().join("app/build.rs"), "fn main() {}\n").unwrap();
    write_member(
        &workspace,
        "host_build",
        "[package]\nname = \"host_build\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct HostThing;\n",
    );
    lock_workspace(&workspace);

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use host_build::HostThing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--target",
            "wasm32-unknown-unknown",
        ])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "excra: crate 'host_build' is declared only in dependency contexts excluded by default; retry with `--include-build`\n"
    );
}

#[test]
fn binary_normalizes_cargo_host_target_from_environment_and_config() {
    let host = host_target_triple();
    let environment = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use cargo_metadata::Metadata;", "--root", "."])
        .env("CARGO_BUILD_TARGET", "host")
        .output()
        .unwrap();
    assert!(
        environment.status.success(),
        "{}",
        String::from_utf8_lossy(&environment.stderr)
    );
    assert!(String::from_utf8_lossy(&environment.stdout).contains(&format!("target: {host}")));

    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct Thing;\n",
    );
    lock_workspace(&workspace);

    let explicit = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
            "--target",
            "host",
        ])
        .output()
        .unwrap();
    assert!(
        explicit.status.success(),
        "{}",
        String::from_utf8_lossy(&explicit.stderr)
    );
    let explicit_stdout = String::from_utf8(explicit.stdout).unwrap();
    let source = report_source(&explicit_stdout);
    assert_eq!(
        explicit_stdout,
        format!(
            "crate: dep 0.1.0\ndependency: normal\ntarget: {host}\nroot features: default\nsource: {}\nimport: use dep::Thing;\nitem: struct Thing\nlocation: {}/dep/src/lib.rs:1\ndefinition: pub struct Thing;\ndocs: (none)\n",
            source.display(),
            workspace.path().display()
        )
    );

    fs::write(
        workspace.path().join(".cargo/config.toml"),
        "[build]\ntarget = \"host\"\n",
    )
    .unwrap();
    let configured = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    assert!(String::from_utf8_lossy(&configured.stdout).contains(&format!("target: {host}")));

    fs::write(
        workspace.path().join(".cargo/config.toml"),
        "[build]\ntarget = [\"host\"]\n",
    )
    .unwrap();
    let singleton = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .output()
        .unwrap();
    assert!(
        singleton.status.success(),
        "{}",
        String::from_utf8_lossy(&singleton.stderr)
    );
    assert!(String::from_utf8_lossy(&singleton.stdout).contains(&format!("target: {host}")));
}

#[cfg(unix)]
#[test]
fn binary_regenerates_after_a_config_relative_wrapper_changes_semantics() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    fs::create_dir_all(workspace.path().join("tools")).unwrap();
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = \"./tools/cfg-wrapper.sh\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"wrapped_dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nwrapped_dep = { path = \"../wrapped_dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "wrapped_dep",
        "[package]\nname = \"wrapped_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "#[cfg(first)]\npub struct FirstOnly;\n#[cfg(second)]\npub struct SecondOnly;\n",
    );
    let wrapper = workspace.path().join("tools/cfg-wrapper.sh");
    fs::write(
        &wrapper,
        "#!/bin/sh\ncompiler=\"$1\"\nshift\nexec \"$compiler\" --cfg first \"$@\"\n",
    )
    .unwrap();
    let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions.clone()).unwrap();
    lock_workspace(&workspace);

    let first = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use wrapped_dep::FirstOnly;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    fs::write(
        &wrapper,
        "#!/bin/sh\ncompiler=\"$1\"\nshift\nexec \"$compiler\" --cfg second \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&wrapper, permissions).unwrap();
    let second = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use wrapped_dep::SecondOnly;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(String::from_utf8_lossy(&second.stdout).contains("pub struct SecondOnly;"));
}

#[test]
fn binary_bounds_managed_generation_retention_for_sequential_and_concurrent_queries() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct Thing;\n",
    );
    lock_workspace(&workspace);
    let root = workspace.path().to_path_buf();

    let query = |root: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                "use dep::Thing;",
                "--root",
                root.to_str().unwrap(),
                "--package",
                "app",
            ])
            .env("CARGO_TARGET_DIR", root.join("target"))
            .output()
            .unwrap()
    };
    for _ in 0..2 {
        let output = query(&root);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let first_root = root.clone();
    let second_root = root.clone();
    let first = std::thread::spawn(move || query(&first_root));
    let second = std::thread::spawn(move || query(&second_root));
    for output in [first.join().unwrap(), second.join().unwrap()] {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let managed_root = workspace.path().join("target/excra");
    let mut directories = fs::read_dir(&managed_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name())
        .collect::<Vec<_>>();
    directories.sort();
    assert_eq!(directories, [OsString::from("generation")]);
    assert_eq!(
        fs::read_to_string(managed_root.join("generation/.excra-generation")).unwrap(),
        "excra managed generation\n"
    );
}

#[cfg(unix)]
#[test]
fn binary_rejects_a_symlink_lock_without_modifying_its_target() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    let target_dir = workspace.path().join("cache");
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        format!("[build]\ntarget-dir = {:?}\n", target_dir.to_str().unwrap()),
    )
    .unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct Thing;\n",
    );
    lock_workspace(&workspace);

    let manifest = workspace.path().join("Cargo.toml");
    let original_manifest = fs::read_to_string(&manifest).unwrap();
    let lock_dir = target_dir.join("excra");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_path = lock_dir.join("generation.lock");
    symlink(Path::new("../../Cargo.toml"), &lock_path).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(&manifest).unwrap(), original_manifest);
    assert!(fs::symlink_metadata(&lock_path).unwrap().is_symlink());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refusing to use non-regular or symlink rustdoc JSON lock path"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn binary_rejects_a_symlink_managed_root_without_modifying_its_destination() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    let target_dir = workspace.path().join("cache");
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        format!("[build]\ntarget-dir = {:?}\n", target_dir.to_str().unwrap()),
    )
    .unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ndep = { path = \"../dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "dep",
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct Thing;\n",
    );
    lock_workspace(&workspace);

    let victim_generation = workspace.path().join("victim/generation");
    fs::create_dir_all(&victim_generation).unwrap();
    fs::write(
        victim_generation.join(".excra-generation"),
        "excra managed generation\n",
    )
    .unwrap();
    fs::write(victim_generation.join("keep.txt"), "keep me\n").unwrap();
    fs::create_dir(&target_dir).unwrap();
    let managed_root = target_dir.join("excra");
    symlink(Path::new("../victim"), &managed_root).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Thing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(victim_generation.join("keep.txt")).unwrap(),
        "keep me\n"
    );
    assert_eq!(
        fs::read_to_string(victim_generation.join(".excra-generation")).unwrap(),
        "excra managed generation\n"
    );
    assert!(fs::symlink_metadata(&managed_root).unwrap().is_symlink());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("refusing to use non-directory or symlink excra managed root"),
        "{stderr}"
    );
}

#[cfg(unix)]
#[test]
fn binary_preserves_path_lookup_for_bare_general_wrapper_names() {
    let workspace = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
    let tools = workspace.path().join("tools");
    fs::create_dir_all(&tools).unwrap();
    fs::write(
        workspace.path().join(".cargo/config.toml"),
        "[build]\nrustc-wrapper = \"bare-excra-wrapper\"\n",
    )
    .unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"wrapped_dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    write_member(
        &workspace,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nwrapped_dep = { path = \"../wrapped_dep\" }\n",
        "",
    );
    write_member(
        &workspace,
        "wrapped_dep",
        "[package]\nname = \"wrapped_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct WrappedThing;\n",
    );
    let wrapper = tools.join("bare-excra-wrapper");
    fs::write(&wrapper, "#!/bin/sh\nexec \"$@\"\n").unwrap();
    let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&wrapper, permissions).unwrap();
    lock_workspace(&workspace);
    let mut paths = vec![tools];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(paths).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use wrapped_dep::WrappedThing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env("PATH", path)
        .env_remove("RUSTC_WRAPPER")
        .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("pub struct WrappedThing;"));
}

#[test]
fn binary_rejects_bad_import() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .arg("use crate::local::Thing;")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("only external crate"));
}

#[test]
fn binary_rejects_rust_standard_library_items() {
    for import in [
        "use std::fs::File;",
        "use core::fmt::Debug;",
        "use alloc::vec::Vec;",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .arg(import)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("Rust standard library"));
        assert!(stderr.contains("https://doc.rust-lang.org/std/"));
    }
}

#[test]
fn binary_reports_unknown_argument() {
    for args in [["--bad", ""], ["use syn::ItemUse;", "--bad"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args(args.into_iter().filter(|arg| !arg.is_empty()))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "excra: unknown argument: --bad\nusage: excra '<use crate_name::module::item;>' [--root PATH] [--package NAME_OR_ID] [--target TRIPLE] [--features FEATURES] [--all-features] [--no-default-features] [--include-dev] [--include-build]\n  --include-dev    include direct dev-dependencies (excluded by default)\n  --include-build  include direct build-dependencies (excluded by default)\n"
        );
    }
}

#[test]
fn binary_rejects_malformed_import_syntax() {
    for import in [
        "use cargo_metadata::::Metadata;",
        "use cargo_metadata::{Metadata,,Package};",
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([import, "--root", "."])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn binary_reports_missing_root_value() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .arg("--root")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--root requires PATH"));
}

#[test]
fn binary_prints_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("usage: excra"));
}

#[cfg(unix)]
#[test]
fn binary_reuses_unit_graphs_for_batch_and_external_queries() {
    let workspace = context_reexport_workspace("[dependencies]", false);
    let shim_dir = workspace.path().join("bin");
    fs::create_dir(&shim_dir).unwrap();
    let cargo = Command::new("sh")
        .args(["-c", "command -v cargo"])
        .output()
        .unwrap();
    assert!(cargo.status.success());
    let real_cargo = String::from_utf8(cargo.stdout).unwrap();
    let log = workspace.path().join("cargo.log");
    let shim = shim_dir.join("cargo");
    fs::write(
        &shim,
        "#!/bin/sh\nfor arg in \"$@\"; do\n  if [ \"$arg\" = --unit-graph ]; then echo graph >> \"$GRAPH_LOG\"; fi\ndone\nexec \"$REAL_CARGO\" \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
    let mut paths = vec![shim_dir];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use facade::{ContextThing, ContextThing, ContextThing};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("REAL_CARGO", real_cargo.trim())
        .env("GRAPH_LOG", &log)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .matches("definition: pub struct ContextThing;")
            .count(),
        3
    );
    assert_eq!(fs::read_to_string(log).unwrap(), "graph\n");
}
