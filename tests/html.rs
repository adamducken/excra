use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
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
    query_command(root, import, flags).output().unwrap()
}

fn query_command(root: &Path, import: &str, flags: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_excra"));
    command
        .arg(import)
        .arg("--root")
        .arg(root)
        .args(["--package", "app"])
        .args(flags)
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TERM_COLOR", "never")
        .env("RUSTC_BOOTSTRAP", "0");
    command
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

#[test]
fn documentation_selects_the_source_when_name_and_version_collide() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\"]\nexclude = [\"api\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    member(
        root,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nalias = { package = \"review_api\", path = \"../api\" }\n",
        "pub use alias::Item;",
    );
    member(
        root,
        "api",
        "[package]\nname = \"review_api\"\nversion = \"1.0.0\"\nedition = \"2024\"\n[dependencies]\ninner = { package = \"review_api\", version = \"1.0.0\" }\n",
        "pub struct Item { pub inner: inner::Item }",
    );
    member(
        root,
        "vendor/review_api-1.0.0",
        "[package]\nname = \"review_api\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
        "pub struct Item;",
    );
    fs::write(
        root.join("vendor/review_api-1.0.0/.cargo-checksum.json"),
        r#"{"package":null,"files":{}}"#,
    )
    .unwrap();
    fs::create_dir(root.join(".cargo")).unwrap();
    fs::write(
        root.join(".cargo/config.toml"),
        format!(
            "[source.crates-io]\nreplace-with = \"vendored\"\n[source.vendored]\ndirectory = {:?}\n",
            root.join("vendor")
        ),
    )
    .unwrap();
    let locked = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(locked.status.success());
    let report = success(query(root, "use alias::Item;", &[]));
    assert!(report.contains("pub inner: Item"), "{report}");
}

#[test]
fn signature_links_preserve_pointer_token_boundaries() {
    let dir = project();
    fs::write(
        dir.path().join("api/src/lib.rs"),
        r#"
pub struct Item;
impl Item {
    pub fn pointer(&self, value: *const u8) -> *const u8 { value }
    pub fn mut_pointer(&self, value: *mut Item) -> *mut Item { value }
    pub fn generic<T>(&self, value: T) -> T { value }
}
/// See [Item].
pub fn pointer(value: *const Item) -> *const Item { value }
"#,
    )
    .unwrap();
    let report = success(query(dir.path(), "use alias::{Item, pointer};", &[]));
    for signature in [
        "pub fn pointer(&self, value: *const u8) -> *const u8",
        "pub fn mut_pointer(&self, value: *mut Item) -> *mut Item",
        "pub fn pointer(value: *const Item) -> *const Item",
    ] {
        assert!(report.contains(signature), "missing {signature}: {report}");
    }
    assert!(report.contains("[Item](struct.Item.html"), "{report}");
    assert!(
        report.contains("#### `pub fn generic<T>(&self, value: T) -> T`"),
        "{report}"
    );
}

#[test]
fn non_inlined_reexports_participate_in_namespace_and_ambiguity_checks() {
    let dir = project();
    fs::write(
        dir.path().join("api/src/lib.rs"),
        r#"
pub mod original { pub struct Mixed; }
#[doc(no_inline)] pub use original::Mixed;
#[macro_export] macro_rules! Mixed { () => { 1 }; }
#[doc(no_inline)] pub use Mixed as OnlyMacroAndType;
#[macro_export] macro_rules! make { () => { 1 }; }
#[doc(no_inline)] pub use make as OnlyMacro;
"#,
    )
    .unwrap();
    for name in ["Mixed", "OnlyMacroAndType"] {
        assert!(
            failure(query(dir.path(), &format!("use alias::{name};"), &[])).contains("ambiguous")
        );
        let report = success(query(
            dir.path(),
            &format!("use alias::{name}::{{self}};"),
            &[],
        ));
        assert!(report.contains("pub struct Mixed"));
    }
    assert!(success(query(dir.path(), "use alias::OnlyMacro;", &[])).contains("macro_rules! make"));
    assert!(failure(query(dir.path(), "use alias::OnlyMacro::{self};", &[])).contains("not found"));
}

#[test]
fn empty_cached_markdown_is_repaired_and_concurrent_queries_are_complete() {
    let dir = project();
    let mut first = query_command(dir.path(), "use alias::Record;", &[]);
    let mut second = query_command(dir.path(), "use alias::Record;", &[]);
    let first = first
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = second
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let first = success(first.wait_with_output().unwrap());
    let second = success(second.wait_with_output().unwrap());
    assert!(first.contains("pub struct Record"));
    assert_eq!(first, second);
    let markdown = &paths(&first, "markdown: ")[0];
    fs::write(markdown, "").unwrap();
    let repaired = success(query(dir.path(), "use alias::Record;", &[]));
    assert!(repaired.contains("pub struct Record"));
    assert!(!fs::read_to_string(markdown).unwrap().is_empty());
}

#[test]
fn inactive_platform_dependencies_and_coalesced_contexts_are_filtered() {
    let dir = project();
    let root = dir.path();
    member(
        root,
        "windows-api",
        "[package]\nname = \"windows-api\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        "pub struct WindowsOnly;",
    );
    let manifest = root.join("app/Cargo.toml");
    let additions = r#"
[target.'cfg(windows)'.dependencies]
windows_api = { package = "windows-api", path = "../windows-api" }
alias = { package = "api", path = "../api", default-features = false }
[target.'cfg(windows)'.dev-dependencies]
alias = { package = "api", path = "../api", default-features = false }
[target.'cfg(windows)'.build-dependencies]
windows_api = { package = "windows-api", path = "../windows-api" }
build_api = { package = "build-api", path = "../build-api", default-features = false, features = ["enabled"] }
"#;
    fs::write(
        &manifest,
        fs::read_to_string(&manifest).unwrap() + additions,
    )
    .unwrap();
    let locked = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(locked.status.success());
    let version = Command::new("rustc").arg("-vV").output().unwrap();
    let version = String::from_utf8(version.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    // The fixture's inactive predicate must be opposite the compiler host.
    if cfg!(windows) {
        fs::write(
            &manifest,
            fs::read_to_string(&manifest)
                .unwrap()
                .replace("cfg(windows)", "cfg(unix)"),
        )
        .unwrap();
    }
    for flags in [vec![], vec!["--target", host], vec!["--include-dev"]] {
        assert!(
            failure(query(root, "use windows_api::WindowsOnly;", &flags))
                .contains("not a direct dependency")
        );
        let report = success(query(root, "use alias::Record;", &flags));
        assert!(report.contains("dependency: normal\n"), "{report}");
    }
    // A build dependency's predicate and documentation use the compiler host,
    // even when the selected consumer target has the opposite OS.
    let other_target = if cfg!(windows) {
        "x86_64-unknown-linux-gnu"
    } else {
        "x86_64-pc-windows-gnu"
    };
    let flags = ["--target", other_target, "--include-build"];
    let report = success(query(root, "use build_api::Enabled;", &flags));
    assert!(report.contains("dependency: build\n"), "{report}");
    assert!(report.contains("pub struct Enabled"));
    assert!(
        failure(query(
            root,
            "use windows_api::WindowsOnly;",
            &["--include-build"]
        ))
        .contains("not a direct dependency")
    );
    fs::create_dir(root.join(".cargo")).unwrap();
    fs::write(
        root.join(".cargo/config.toml"),
        format!("[build]\ntarget = \"{host}\"\n"),
    )
    .unwrap();
    assert!(
        failure(query(root, "use windows_api::WindowsOnly;", &[]))
            .contains("not a direct dependency")
    );
    let mut command = query_command(root, "use windows_api::WindowsOnly;", &[]);
    command.env("CARGO_BUILD_TARGET", host);
    assert!(failure(command.output().unwrap()).contains("not a direct dependency"));
}

#[test]
fn features_are_selected_per_package_platform_and_normal_build_dev_context() {
    let dir = project();
    let root = dir.path();
    member(
        root,
        "app",
        r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"
[dependencies]
alias = { package = "api", path = "../api", default-features = false, features = ["normal"] }
[build-dependencies]
alias = { package = "api", path = "../api", default-features = false, features = ["build"] }
[dev-dependencies]
alias = { package = "api", path = "../api", default-features = false, features = ["dev"] }
[target.'cfg(target_os = "none")'.dependencies]
alias = { package = "api", path = "../api", default-features = false, features = ["inactive"] }
"#,
        "pub use alias::Normal;",
    );
    fs::write(
        root.join("app/build.rs"),
        "fn main() { let _ = alias::Build; }",
    )
    .unwrap();
    member(
        root,
        "api",
        r#"
[package]
name = "api"
version = "1.2.3"
edition = "2024"
[features]
normal = []
build = []
dev = []
unselected = []
inactive = []
"#,
        r#"
pub struct Record;
#[cfg(feature = "normal")] pub struct Normal;
#[cfg(feature = "build")] pub struct Build;
impl Record {
    #[cfg(feature = "normal")] pub fn normal() {}
    #[cfg(feature = "build")] pub fn build() {}
    #[cfg(feature = "dev")] pub fn dev() {}
    #[cfg(feature = "unselected")] pub fn unselected() {}
    #[cfg(feature = "inactive")] pub fn inactive() {}
}

"#,
    );
    let unselected = root.join("build-api/Cargo.toml");
    fs::write(
        &unselected,
        fs::read_to_string(&unselected).unwrap()
            + "\n[dependencies]\napi = { path = \"../api\", features = [\"unselected\"] }\n",
    )
    .unwrap();
    let checked = Command::new("cargo")
        .args(["check", "--offline", "--package", "app"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        checked.status.success(),
        "{}",
        String::from_utf8_lossy(&checked.stderr)
    );
    let version = Command::new("rustc").arg("-vV").output().unwrap();
    let version = String::from_utf8(version.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    for target_flags in [vec![], vec!["--target", host]] {
        for include_dev in [false, true] {
            let mut flags = target_flags.clone();
            flags.push("--include-build");
            if include_dev {
                flags.push("--include-dev");
            }
            let report = success(query(root, "use alias::Record;", &flags));
            let pages = paths(&report, "markdown: ");
            assert_eq!(pages.len(), 2);
            for (page, build) in pages.iter().zip([false, true]) {
                let content = fs::read_to_string(page).unwrap();
                for (feature, enabled) in [
                    ("normal", !build),
                    ("build", build),
                    ("dev", !build && include_dev),
                    ("unselected", false),
                    ("inactive", false),
                ] {
                    assert_eq!(
                        content.contains(&format!("fn {feature}(")),
                        enabled,
                        "{feature}: {content}"
                    );
                }
            }
        }
    }
}

#[test]
fn isolated_docs_preserve_unified_transitive_features_and_resolver_policy() {
    for resolver in ["1", "2", "3"] {
        let dir = project();
        let root = dir.path();
        fs::write(root.join("Cargo.toml"), format!(
            "[workspace]\nmembers = [\"app\"]\nexclude = [\"api\", \"inner\"]\nresolver = \"{resolver}\"\n"
        )).unwrap();
        member(
            root,
            "app",
            r#"
[package]
name = "app"
version = "0.1.0"
edition = "2024"
[dependencies]
alias = { package = "api", path = "../api" }
inner = { path = "../inner", features = ["extra"] }
[build-dependencies]
inner = { path = "../inner", features = ["host"] }
"#,
            "pub use alias::{Kind::Extra as Variant, Extra};",
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
[build-dependencies]
inner = { path = "../inner" }
"#,
            "pub use inner::{Kind, Extra};",
        );
        fs::write(
            root.join("api/build.rs"),
            "fn main() { let _ = inner::Host; }",
        )
        .unwrap();
        member(
            root,
            "inner",
            r#"
[package]
name = "inner"
version = "0.1.0"
edition = "2024"
[features]
extra = []
host = []
"#,
            r#"
pub enum Kind {
    Base,
    #[cfg(feature = "extra")] Extra,
    #[cfg(feature = "host")] Host,
}
#[cfg(feature = "extra")] pub struct Extra;
#[cfg(feature = "host")] pub struct Host;
"#,
        );
        let checked = Command::new("cargo")
            .args(["check", "--offline", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(
            checked.status.success(),
            "{}",
            String::from_utf8_lossy(&checked.stderr)
        );
        let report = success(query(root, "use alias::{Kind, Kind::Extra, Extra};", &[]));
        assert!(report.contains("pub struct Extra"), "{report}");
        let pages = paths(&report, "markdown: ");
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0], pages[1]);
        let content = fs::read_to_string(&pages[0]).unwrap();
        assert!(content.contains("Extra"), "{content}");
        assert_eq!(content.contains("Host"), resolver == "1", "{content}");
        let generation = pages[0]
            .ancestors()
            .find(|path| path.join("Cargo.toml").is_file())
            .unwrap();
        let manifest = fs::read_to_string(generation.join("Cargo.toml"))
            .unwrap()
            .parse::<toml::Table>()
            .unwrap();
        assert_eq!(manifest["workspace"]["resolver"].as_str(), Some(resolver));
    }
}

#[test]
fn git_siblings_keep_resolved_sources_revisions_and_unified_features() {
    let dir = TempDir::new().unwrap();
    let repository = dir.path().join("repository");
    fs::create_dir(&repository).unwrap();
    fs::write(
        repository.join("Cargo.toml"),
        "[workspace]\nmembers = [\"api\", \"inner\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    member(
        &repository,
        "api",
        "[package]\nname = \"api\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\ninner = { path = \"../inner\" }\n",
        "pub use inner::{Kind, Extra};",
    );
    member(
        &repository,
        "inner",
        "[package]\nname = \"inner\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[features]\nextra = []\n",
        "pub enum Kind { Base, #[cfg(feature = \"extra\")] Extra }\n#[cfg(feature = \"extra\")] pub struct Extra;",
    );
    for args in [
        vec!["init", "--initial-branch", "fixture"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Excra Test",
            "-c",
            "user.email=excra@example.invalid",
            "commit",
            "-m",
            "fixture",
        ],
        vec!["tag", "fixture"],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(&repository)
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
    }
    let revision = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repository)
        .output()
        .unwrap();
    let revision = String::from_utf8(revision.stdout)
        .unwrap()
        .trim()
        .to_string();
    let url = url::Url::from_directory_path(&repository).unwrap();
    let cargo_home = dir.path().join("cargo-home");
    for (index, reference) in [
        String::new(),
        ", branch = \"fixture\"".into(),
        ", tag = \"fixture\"".into(),
        format!(", rev = \"{revision}\""),
    ]
    .into_iter()
    .enumerate()
    {
        let root = dir.path().join(format!("consumer-{index}"));
        fs::create_dir(&root).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n",
        )
        .unwrap();
        member(
            &root,
            "app",
            &format!(
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\napi = {{ git = \"{url}\"{reference} }}\ninner = {{ git = \"{url}\"{reference}, features = [\"extra\"] }}\n"
            ),
            "pub use api::{Kind::Extra as Variant, Extra};",
        );
        let checked = Command::new("cargo")
            .args(["check", "--package", "app"])
            .current_dir(&root)
            .env("CARGO_HOME", &cargo_home)
            .env("CARGO_NET_OFFLINE", "false")
            .output()
            .unwrap();
        assert!(checked.status.success(), "{:?}", checked);
        let lock = fs::read(root.join("Cargo.lock")).unwrap();
        let mut command = query_command(&root, "use api::{Kind, Kind::Extra, Extra};", &[]);
        command.env("CARGO_HOME", &cargo_home);
        let report = success(command.output().unwrap());
        assert!(report.contains("pub struct Extra"), "{report}");
        assert!(report.contains("Extra"), "{report}");
        let pages = paths(&report, "markdown: ");
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0], pages[1]);
        let generation = pages[0]
            .ancestors()
            .find(|path| path.join("Cargo.toml").is_file())
            .unwrap();
        let metadata = Command::new("cargo")
            .args([
                "metadata",
                "--offline",
                "--locked",
                "--format-version",
                "1",
                "--manifest-path",
            ])
            .arg(generation.join("Cargo.toml"))
            .current_dir(&root)
            .env("CARGO_HOME", &cargo_home)
            .output()
            .unwrap();
        assert!(metadata.status.success(), "{:?}", metadata);
        let metadata: serde_json::Value = serde_json::from_slice(&metadata.stdout).unwrap();
        for name in ["api", "inner"] {
            let packages = metadata["packages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|package| package["name"] == name)
                .collect::<Vec<_>>();
            assert_eq!(packages.len(), 1, "{metadata}");
            let source = packages[0]["source"].as_str().unwrap();
            assert!(source.starts_with("git+"), "{source}");
            assert!(source.ends_with(&format!("#{revision}")), "{source}");
        }
        assert_eq!(fs::read(root.join("Cargo.lock")).unwrap(), lock);
    }
}

#[test]
fn proc_macro_consumers_query_ordinary_dependencies_in_the_host_context() {
    let dir = project();
    let root = dir.path();
    member(
        root,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nalias = { package = \"api\", path = \"../api\", features = [\"target\"] }\nmacros = { path = \"../macros\" }\n",
        "pub use alias::Record;",
    );
    member(
        root,
        "macros",
        "[package]\nname = \"macros\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\nproc-macro = true\n[dependencies]\nalias = { package = \"api\", path = \"../api\", features = [\"host\"] }\n",
        "",
    );
    member(
        root,
        "api",
        "[package]\nname = \"api\"\nversion = \"1.2.3\"\nedition = \"2024\"\n[features]\nhost = []\ntarget = []\n",
        "pub struct Record;\nimpl Record {\n#[cfg(feature = \"host\")] pub fn host_only() {}\n#[cfg(feature = \"target\")] pub fn target_only() {}\n}",
    );
    let checked = Command::new("cargo")
        .args(["check", "--offline", "--package", "app"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(checked.status.success(), "{:?}", checked);
    let report = success(query(root, "use alias::Record;", &[]));
    assert!(report.contains("fn target_only("), "{report}");
    assert!(!report.contains("fn host_only("), "{report}");
    let version = Command::new("rustc").arg("-vV").output().unwrap();
    let version = String::from_utf8(version.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    let other_target = if cfg!(windows) {
        "x86_64-unknown-linux-gnu"
    } else {
        "x86_64-pc-windows-gnu"
    };
    for flags in [
        vec![],
        vec!["--target", host],
        vec!["--target", other_target],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args(["use alias::Record;", "--root"])
            .arg(root)
            .args(["--package", "macros"])
            .args(flags)
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        let report = success(output);
        assert!(report.contains("dependency: normal\n"), "{report}");
        assert!(report.contains("fn host_only("), "{report}");
        assert!(!report.contains("fn target_only("), "{report}");
    }
}

#[test]
fn concurrent_rustdoc_flag_selections_have_separate_consistent_caches() {
    let dir = project();
    fs::write(
        dir.path().join("api/src/lib.rs"),
        "#[cfg(first)] pub fn item(_: u8) {}\n#[cfg(not(first))] pub fn item(_: u16) {}",
    )
    .unwrap();
    let import = "use alias::{item as First, item as Second};";
    let mut first = query_command(dir.path(), import, &[]);
    first
        .env("RUSTDOCFLAGS", "--cfg first")
        .env_remove("CARGO_ENCODED_RUSTDOCFLAGS");
    let mut second = query_command(dir.path(), import, &[]);
    second
        .env_remove("RUSTDOCFLAGS")
        .env_remove("CARGO_ENCODED_RUSTDOCFLAGS");
    let first = first
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = second
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let first = success(first.wait_with_output().unwrap());
    let second = success(second.wait_with_output().unwrap());
    for (report, signature) in [
        (&first, "pub fn item(_: u8)"),
        (&second, "pub fn item(_: u16)"),
    ] {
        assert_eq!(report.matches(signature).count(), 2, "{report}");
    }
    let first_pages = paths(&first, "markdown: ");
    let second_pages = paths(&second, "markdown: ");
    assert_eq!(first_pages[0], first_pages[1]);
    assert_eq!(second_pages[0], second_pages[1]);
    assert_ne!(first_pages[0], second_pages[0]);
}
