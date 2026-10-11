use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

fn member(root: &Path, name: &str, manifest: &str, source: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.join("src")).unwrap();
    fs::write(path.join("Cargo.toml"), manifest).unwrap();
    fs::write(path.join("src/lib.rs"), source).unwrap();
}

fn project() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(
        root.join("Cargo.toml"),
        r#"
[workspace]
members = ["app", "build-api"]
exclude = ["api", "inner", "dev-api"]
resolver = "3"
"#,
    )
    .unwrap();
    member(
        root,
        "app",
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"
[dependencies]
alias = { package = "api", path = "../api", default-features = false }
[dev-dependencies]
dev_api = { package = "dev-api", path = "../dev-api", default-features = false, features = ["enabled"] }
[build-dependencies]
build_api = { package = "build-api", path = "../build-api", default-features = false, features = ["enabled"] }
[features]
default = ["alias/base"]
extra = ["alias/extra"]
"#,
        "",
    );
    member(
        root,
        "api",
        r#"
[package]
name = "api"
version = "1.2.3"
edition = "2024"
[dependencies]
inner = { path = "../inner" }
[features]
default = ["off"]
off = []
base = []
extra = []
"#,
        r#"
//! Example API documentation.
/// A **record** with a field and a method.
#[derive(Debug)]
pub struct Record {
    /// The stored value.
    pub value: u8,
}
impl Record {
    /// Read the stored value.
    pub fn read(&self) -> u8 { self.value }
    pub const LIMIT: u8 = 8;
}
pub mod nested { /// A nested item.
    pub struct Item;
}
pub mod r#type { pub struct r#match; }
mod hidden { pub struct Original; }
pub use hidden::Original as Renamed;
pub use inner as external;
pub use inner::Kind;
pub trait Worker { fn run(&self) -> u8; }
pub type Number = u8;
pub const LIMIT: u8 = 8;
pub static VALUE: u8 = 2;
pub union Bits { pub byte: u8, pub number: u16 }
#[macro_export] macro_rules! create { () => { 1 }; }
pub struct Mixed;
#[macro_export] macro_rules! Mixed { () => { 1 }; }
#[cfg(feature = "base")] pub fn baseline(value: u8) -> u8 { value }
#[cfg(feature = "extra")] pub struct Extra;
#[cfg(feature = "off")] pub struct Off;
#[cfg(doc)] pub struct DocOnly;
#[doc(hidden)] pub struct Hidden;
"#,
    );
    member(
        root,
        "inner",
        "[package]\nname = \"inner\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "/// A choice.\npub enum Kind { /// First choice.\nOne, Two }\n",
    );
    for name in ["dev-api", "build-api"] {
        member(
            root,
            name,
            &format!(
                r#"
[package]
name = "{name}"
version = "0.1.0"
edition = "2024"
[features]
default = ["off"]
off = []
enabled = []
"#
            ),
            "#[cfg(feature = \"enabled\")] pub struct Enabled;\n#[cfg(feature = \"off\")] pub struct Off;\n",
        );
    }
    fs::write(root.join("app/build.rs"), "fn main() {}\n").unwrap();
    let output = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    dir
}

fn query(root: &Path, import: &str, flags: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_excra"))
        .arg(import)
        .arg("--root")
        .arg(root)
        .args(["--package", "app"])
        .args(flags)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TERM_COLOR", "never")
        .env("RUSTC_BOOTSTRAP", "0")
        .output()
        .unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn failure(output: Output) -> String {
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    String::from_utf8(output.stderr).unwrap()
}

fn paths(report: &str, label: &str) -> Vec<PathBuf> {
    report
        .lines()
        .filter_map(|line| line.strip_prefix(label))
        .map(PathBuf::from)
        .collect()
}

#[test]
fn stable_rustdoc_pages_become_saved_markdown_with_api_details() {
    let dir = project();
    let lock = fs::read(dir.path().join("Cargo.lock")).unwrap();
    let report = success(query(
        dir.path(),
        "use alias::{Record, baseline, Worker, Number, LIMIT, VALUE, Bits, create};",
        &[],
    ));
    for text in [
        "crate: api",
        "version: 1.2.3",
        "dependency: normal",
        "pub struct Record",
        "pub value: u8",
        "The stored value.",
        "Read the stored value.",
        "fn read",
        "Trait Implementations",
        "Debug",
        "pub fn baseline",
        "fn run",
        "pub type Number",
        "pub const LIMIT",
        "pub static VALUE",
        "pub union Bits",
        "macro_rules! create",
    ] {
        assert!(report.contains(text), "missing {text}");
    }
    assert!(!report.contains("Copy item path"));
    assert!(!report.contains("Skip to main content"));
    assert!(report.contains("```"));
    let html = paths(&report, "source: ");
    let markdown = paths(&report, "markdown: ");
    assert_eq!(html.len(), 8);
    assert_eq!(markdown.len(), 8);
    for (html, markdown) in html.iter().zip(&markdown) {
        assert!(html.is_file());
        assert_eq!(html.extension().unwrap(), "html");
        assert_eq!(markdown, &html.with_extension("md"));
        let content = fs::read_to_string(markdown).unwrap();
        assert!(!content.trim().is_empty());
        assert!(report.contains(&content));
    }
    assert_eq!(fs::read(dir.path().join("Cargo.lock")).unwrap(), lock);
}

#[test]
fn modules_raw_identifiers_reexports_and_variants_follow_html() {
    let dir = project();
    let report = success(query(
        dir.path(),
        "use alias::{nested::{self, Item}, r#type::r#match, Renamed as Local, Kind::One, DocOnly};",
        &[],
    ));
    for text in [
        "import: use alias::nested::{self};",
        "pub struct Item",
        "import: use alias::r#type::r#match;",
        "# Struct match",
        "pub struct Renamed",
        "pub enum Kind",
        "First choice.",
        "pub struct DocOnly",
    ] {
        assert!(report.contains(text), "missing {text}");
    }
    assert!(failure(query(dir.path(), "use alias::Hidden;", &[])).contains("not found"));
    assert!(failure(query(dir.path(), "use alias::external::Kind;", &[])).contains("not found"));
    assert!(failure(query(dir.path(), "use alias::nested::Missing;", &[])).contains("not found"));
}

#[test]
fn cached_docs_respect_root_features_and_do_not_add_dependency_defaults() {
    let dir = project();
    let report = success(query(
        dir.path(),
        "use alias::Extra;",
        &["--features", "extra"],
    ));
    assert!(report.contains("root features: --features extra"));
    let saved = paths(&report, "markdown: ")[0].clone();
    assert!(failure(query(dir.path(), "use alias::Extra;", &[])).contains("not found"));
    assert!(saved.is_file());
    assert!(failure(query(dir.path(), "use alias::Off;", &[])).contains("not found"));
    assert!(
        failure(query(
            dir.path(),
            "use alias::baseline;",
            &["--no-default-features"]
        ))
        .contains("not found")
    );
    let report = success(query(dir.path(), "use alias::Extra;", &["--all-features"]));
    assert!(report.contains("root features: --all-features"));
}

#[test]
fn dev_and_build_docs_require_opt_in_and_use_resolved_features() {
    let dir = project();
    for (name, flag, context) in [
        ("dev_api", "--include-dev", "dev"),
        ("build_api", "--include-build", "build"),
    ] {
        assert!(failure(query(dir.path(), &format!("use {name}::Enabled;"), &[])).contains(flag));
        let report = success(query(dir.path(), &format!("use {name}::Enabled;"), &[flag]));
        assert!(
            report.contains(&format!("dependency: {context}")),
            "{report}"
        );
        assert!(report.contains("pub struct Enabled"));
        assert!(
            failure(query(dir.path(), &format!("use {name}::Off;"), &[flag])).contains("not found")
        );
    }
    let report = success(query(
        dir.path(),
        "use alias::Record;",
        &["--all-features", "--include-dev", "--include-build"],
    ));
    assert!(report.contains("pub struct Record"));
}

#[test]
fn external_build_only_packages_are_documented_in_isolation() {
    let dir = project();
    let manifest = dir.path().join("Cargo.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)
            .unwrap()
            .replace("members = [\"app\", \"build-api\"]", "members = [\"app\"]")
            .replace(
                "exclude = [\"api\", \"inner\", \"dev-api\"]",
                "exclude = [\"api\", \"inner\", \"dev-api\", \"build-api\"]",
            ),
    )
    .unwrap();
    let report = success(query(
        dir.path(),
        "use build_api::Enabled;",
        &["--include-build"],
    ));
    assert!(report.contains("pub struct Enabled"));
    assert!(report.contains("dependency: build"));
}

#[test]
fn only_the_requested_crate_is_documented_completely_and_builds_are_reused() {
    let dir = project();
    fs::write(
        dir.path().join("app/src/lib.rs"),
        "compile_error!(\"the consuming app must not be documented\");",
    )
    .unwrap();
    fs::write(
        dir.path().join("app/build.rs"),
        "fn main() { panic!(\"the consuming app must not be built\"); }",
    )
    .unwrap();
    fs::write(
        dir.path().join("api/build.rs"),
        r#"
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("build-count");
    let previous = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(path, format!("{previous}built\n")).unwrap();
}
"#,
    )
    .unwrap();
    let unrelated = TempDir::new().unwrap();
    member(
        unrelated.path(),
        "unrelated",
        "[package]\nname = \"unrelated\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "compile_error!(\"unrelated dependencies must not be built\");",
    );
    let manifest = dir.path().join("app/Cargo.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest).unwrap().replace(
            "[dependencies]",
            &format!(
                "[dependencies]\nunrelated = {{ path = {:?} }}",
                unrelated.path().join("unrelated")
            ),
        ),
    )
    .unwrap();
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(lock.status.success());

    let first = query(dir.path(), "use alias::Record;", &[]);
    let progress = String::from_utf8_lossy(&first.stderr);
    assert!(progress.contains("Documenting api"), "{progress}");
    assert!(!progress.contains("Documenting app"));
    let report = success(first);
    assert!(!report.contains("pub trait Worker"));
    let page = paths(&report, "source: ")[0].clone();
    let crate_docs = page.parent().unwrap();
    for file in [
        "index.html",
        "index.md",
        "all.md",
        "fn.baseline.md",
        "trait.Worker.md",
        "enum.Kind.md",
        "struct.Renamed.md",
        "nested/index.md",
        "nested/struct.Item.md",
    ] {
        assert!(crate_docs.join(file).is_file(), "missing {file}");
    }
    let docs = crate_docs.parent().unwrap();
    for crate_name in ["app", "inner", "unrelated", "dev_api", "build_api"] {
        assert!(!docs.join(crate_name).exists(), "documented {crate_name}");
    }
    let html_time = fs::metadata(&page).unwrap().modified().unwrap();
    let markdown_time = fs::metadata(page.with_extension("md"))
        .unwrap()
        .modified()
        .unwrap();
    let second = query(dir.path(), "use alias::nested::Item;", &[]);
    let progress = String::from_utf8_lossy(&second.stderr);
    assert!(!progress.contains("Documenting api"), "{progress}");
    let report = success(second);
    assert_eq!(
        paths(&report, "source: ")[0].parent().unwrap(),
        crate_docs.join("nested")
    );
    assert_eq!(fs::metadata(&page).unwrap().modified().unwrap(), html_time);
    assert_eq!(
        fs::metadata(page.with_extension("md"))
            .unwrap()
            .modified()
            .unwrap(),
        markdown_time
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("build-count")).unwrap(),
        "built\n"
    );
    let source = dir.path().join("api/src/lib.rs");
    fs::write(
        &source,
        fs::read_to_string(&source).unwrap().replace(
            "A **record** with a field and a method.",
            "Updated record documentation.",
        ),
    )
    .unwrap();
    let updated = success(query(dir.path(), "use alias::Record;", &[]));
    assert!(updated.contains("Updated record documentation."));
    assert_eq!(paths(&updated, "source: ")[0], page);
}

#[test]
fn an_external_package_used_normally_and_for_builds_has_both_reports() {
    let dir = project();
    let manifest = dir.path().join("app/Cargo.toml");
    fs::write(&manifest, fs::read_to_string(&manifest).unwrap().replace(
        "[build-dependencies]",
        "[build-dependencies]\nalias = { package = \"api\", path = \"../api\", default-features = false }"
    )).unwrap();
    let report = success(query(
        dir.path(),
        "use alias::Record;",
        &["--include-build"],
    ));
    assert!(report.contains("dependency: normal"));
    assert!(report.contains("dependency: build"));
    assert_eq!(paths(&report, "markdown: ").len(), 2);
}

#[test]
fn explicit_and_cargo_configured_targets_find_the_generated_docs() {
    let dir = project();
    let output = Command::new("rustc").arg("-vV").output().unwrap();
    let version = String::from_utf8(output.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    let report = success(query(dir.path(), "use alias::Record;", &["--target", host]));
    assert!(
        paths(&report, "source: ")[0]
            .to_string_lossy()
            .contains(host)
    );
    let report = success(query(
        dir.path(),
        "use build_api::Enabled;",
        &["--target", host, "--include-build"],
    ));
    assert!(
        paths(&report, "source: ")[0]
            .to_string_lossy()
            .contains(host)
    );
    let default = success(query(dir.path(), "use alias::Record;", &[]));
    let default_page = paths(&default, "source: ")[0].clone();
    fs::create_dir(dir.path().join(".cargo")).unwrap();
    fs::write(
        dir.path().join(".cargo/config.toml"),
        format!("[build]\ntarget = \"{host}\"\n"),
    )
    .unwrap();
    let report = success(query(dir.path(), "use alias::Record;", &[]));
    assert_ne!(paths(&report, "source: ")[0], default_page);
    assert!(
        paths(&report, "source: ")[0]
            .to_string_lossy()
            .contains(host)
    );
}

#[test]
fn documentation_honors_default_and_custom_cargo_home_configuration() {
    let dir = project();
    let mut command = Command::new(env!("CARGO_BIN_EXE_excra"));
    command
        .args(["use alias::Record;", "--root"])
        .arg(dir.path())
        .args(["--package", "app"])
        .env("CARGO_NET_OFFLINE", "true")
        .env("RUSTC_BOOTSTRAP", "0")
        .env_remove("CARGO_HOME");
    let default = success(command.output().unwrap());
    let default_page = paths(&default, "source: ")[0].clone();

    let output = Command::new("rustc").arg("-vV").output().unwrap();
    let version = String::from_utf8(output.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    let home = TempDir::new().unwrap();
    fs::write(
        home.path().join("config.toml"),
        format!("[build]\ntarget = \"{host}\"\n"),
    )
    .unwrap();
    command.env("CARGO_HOME", home.path());
    let custom = success(command.output().unwrap());
    let custom_page = paths(&custom, "source: ")[0].clone();
    assert_ne!(custom_page, default_page);
    assert!(custom_page.to_string_lossy().contains(host));
    assert!(custom_page.with_extension("md").is_file());
}

#[test]
fn missing_or_stale_lockfiles_are_not_changed() {
    let dir = project();
    let lock = fs::read(dir.path().join("Cargo.lock")).unwrap();
    let manifest = dir.path().join("api/Cargo.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)
            .unwrap()
            .replace("1.2.3", "1.2.4"),
    )
    .unwrap();
    assert!(
        failure(query(dir.path(), "use alias::Record;", &[]))
            .contains("Cargo.lock is missing or stale")
    );
    assert_eq!(fs::read(dir.path().join("Cargo.lock")).unwrap(), lock);
    fs::remove_file(dir.path().join("Cargo.lock")).unwrap();
    assert!(
        failure(query(dir.path(), "use alias::Record;", &[]))
            .contains("Cargo.lock is missing or stale")
    );
    assert!(!dir.path().join("Cargo.lock").exists());
}

#[test]
fn bad_imports_missing_packages_and_cargo_errors_are_explicit() {
    let dir = project();
    assert!(
        failure(query(dir.path(), "use inner::Kind;", &[])).contains("not a direct dependency")
    );
    assert!(failure(query(dir.path(), "use alias::*;", &[])).contains("glob imports"));
    assert!(failure(query(dir.path(), "use std::fmt::Debug;", &[])).contains("standard library"));
    assert!(failure(query(dir.path(), "use alias::Mixed;", &[])).contains("ambiguous"));
    assert!(
        failure(query(dir.path(), "use alias::Record;", &["--bad"])).contains("unknown argument")
    );
    assert!(
        failure(query(
            dir.path(),
            "use alias::Record;",
            &["--package", "missing"]
        ))
        .contains("not found in workspace")
    );
    assert!(
        failure(query(
            &dir.path().join("missing"),
            "use alias::Record;",
            &[]
        ))
        .contains("cannot read")
    );
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use alias::Record;", "--root"])
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(failure(output).contains("virtual workspace root"));
    fs::write(
        dir.path().join("api/src/lib.rs"),
        "compile_error!(\"broken dependency\");",
    )
    .unwrap();
    let error = failure(query(dir.path(), "use alias::Record;", &[]));
    assert!(error.contains("cargo doc failed"));
    assert!(error.contains("broken dependency"));
    assert!(!error.contains("Cargo.lock is missing or stale"));
}

#[test]
fn help_and_relative_member_roots_work() {
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(success(output).contains("usage: excra"));
    let dir = project();
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use alias::Record;", "--root", "app"])
        .current_dir(dir.path())
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .unwrap();
    assert!(success(output).contains("pub struct Record"));
}
