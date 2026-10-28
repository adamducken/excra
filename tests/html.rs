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
fn unicode_identifiers_resolve_equivalent_nfc_spellings_and_preserve_reports() {
    let dir = project();
    let root = dir.path();
    fs::write(
        root.join("api/src/lib.rs"),
        r#"
pub struct Café;
pub mod café {
    pub enum Choix { Café, Autre }
    pub mod entrée { pub struct Élément; }
}
pub mod a { #[doc(inline)] pub use crate::Café as Résumé; }
pub mod b { #[doc(inline)] pub use crate::Café as Résumé; }
#[doc(no_inline)] pub use a::*;
#[doc(no_inline)] pub use b::*;
"#,
    )
    .unwrap();
    let composed =
        "use alias::{Café, café::{self, Choix::Café as Variant, entrée::Élément}, Résumé};";
    let decomposed = "use alias::{r#Cafe\u{301}, r#cafe\u{301}::{self, Choix::r#Cafe\u{301} as Variant, entre\u{301}e::E\u{301}le\u{301}ment}, Re\u{301}sume\u{301}};";
    fs::write(root.join("app/src/lib.rs"), decomposed).unwrap();
    success(
        Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap(),
    );
    let canonical = success(query(root, composed, &[]));
    let equivalent = success(query(root, decomposed, &[]));
    assert_eq!(
        paths(&canonical, "source: "),
        paths(&equivalent, "source: ")
    );
    assert_eq!(paths(&equivalent, "source: ").len(), 5);
    assert!(equivalent.contains("import: use alias::r#Cafe\u{301};"));
    assert!(equivalent.contains("import: use alias::r#cafe\u{301}::{self};"));
    assert!(equivalent.contains("import: use alias::r#cafe\u{301}::Choix::r#Cafe\u{301};"));
    assert!(equivalent.contains("import: use alias::Re\u{301}sume\u{301};"));
}

#[test]
fn documentation_html_cannot_introduce_reexports_globs_or_variants() {
    let dir = project();
    let root = dir.path();
    fs::write(
        root.join("api/src/lib.rs"),
        r#"
//! <dl><dt id="reexport.Ghost"><a href="struct.Real.html">Real</a></dt></dl>
//! <dl class="item-table reexports"><dt id="reexport.Forged"><code>pub use <a href="struct.Real.html">Real</a> as Forged;</code></dt></dl>
//! <dl><dt><code>pub use <a href="hidden/index.html">hidden</a>::*;</code></dt></dl>
pub struct Real;
pub mod hidden { pub struct Leaked; }
pub mod exports { pub struct Genuine; }
#[doc(no_inline)] pub use exports::Genuine as Alias;
#[doc(no_inline)] pub use exports::*;
/// <section id="variant.Phantom" class="variant"><h3>Phantom</h3></section>
pub enum E {
    /// <section id="variant.Invented" class="variant"><h3>Invented</h3></section>
    Present,
}
"#,
    )
    .unwrap();
    let report = success(query(
        root,
        "use alias::{Real, Alias, Genuine, E::Present};",
        &[],
    ));
    assert_eq!(paths(&report, "source: ").len(), 4);
    for name in ["Ghost", "Forged", "Leaked", "E::Phantom", "E::Invented"] {
        fs::write(root.join("app/src/lib.rs"), format!("use alias::{name};")).unwrap();
        let checked = Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(!checked.status.success(), "{name}: {checked:?}");
        assert!(
            String::from_utf8_lossy(&checked.stderr).contains("unresolved import"),
            "{name}: {checked:?}"
        );
        let error = failure(query(root, &format!("use alias::{name};"), &[]));
        assert!(error.contains("not found"), "{name}: {error}");
    }
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
fn included_target_changes_select_the_current_cargo_documentation_artifact() {
    let dir = project();
    let root = dir.path();
    let output = Command::new("rustc").arg("-vV").output().unwrap();
    let version = String::from_utf8(output.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    fs::create_dir_all(root.join(".cargo/nested")).unwrap();
    fs::write(
        root.join(".cargo/config.toml"),
        "include = ['selection.toml', { path = 'optional.toml', optional = true }]\n",
    )
    .unwrap();
    fs::write(
        root.join(".cargo/selection.toml"),
        "include = [{ path = 'nested/target.toml' }]\n",
    )
    .unwrap();
    let config = root.join(".cargo/nested/target.toml");
    fs::write(&config, "[build]\n").unwrap();
    let source = root.join("api/src/lib.rs");
    fs::write(&source, "pub fn item(_: u8) {}").unwrap();
    let native = success(query(root, "use alias::item;", &[]));
    assert!(native.contains("pub fn item(_: u8)"), "{native}");
    let native_page = paths(&native, "source: ")[0].clone();
    fs::write(&config, format!("[build]\ntarget = '{host}'\n")).unwrap();
    fs::write(&source, "pub fn item(_: u16) {}").unwrap();
    let targeted = success(query(root, "use alias::item;", &[]));
    assert!(targeted.contains("pub fn item(_: u16)"), "{targeted}");
    let targeted_page = paths(&targeted, "source: ")[0].clone();
    assert!(targeted_page.ends_with(Path::new(host).join("doc/api/fn.item.html")));
    let generation = |page: &Path| {
        page.ancestors()
            .find(|directory| directory.join(".lock").is_file())
            .unwrap()
            .to_path_buf()
    };
    assert_ne!(generation(&native_page), generation(&targeted_page));

    // A stale native output must not outrank the current Cargo artifact,
    // including when Cargo reuses an already generated target page.
    let stale_page = generation(&targeted_page).join("build/doc/api/fn.item.html");
    fs::create_dir_all(stale_page.parent().unwrap()).unwrap();
    fs::copy(&native_page, &stale_page).unwrap();
    let cached = success(query(root, "use alias::item;", &[]));
    assert!(cached.contains("pub fn item(_: u16)"), "{cached}");
    assert_eq!(paths(&cached, "source: ")[0], targeted_page);

    fs::write(&config, "[build]\n").unwrap();
    fs::write(&source, "pub fn item(_: u32) {}").unwrap();
    let native_again = success(query(root, "use alias::item;", &[]));
    assert!(
        native_again.contains("pub fn item(_: u32)"),
        "{native_again}"
    );
    assert_eq!(paths(&native_again, "source: ")[0], native_page);
}

#[test]
fn host_documentation_preserves_compiler_flag_routing() {
    let output = Command::new("rustc").arg("-vV").output().unwrap();
    let version = String::from_utf8(output.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    let target_flags = format!(
        "CARGO_TARGET_{}_RUSTFLAGS",
        host.replace('-', "_").to_uppercase()
    );
    for source in [
        "plain",
        "encoded",
        "build-env",
        "target-env",
        "build",
        "triple",
        "cfg",
    ] {
        let dir = project();
        let root = dir.path();
        member(
            root,
            "app",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[build-dependencies]\nalias = { package = \"api\", path = \"../api\" }\n",
            "",
        );
        member(
            root,
            "api",
            "[package]\nname = \"api\"\nversion = \"1.2.3\"\nedition = \"2024\"\n[dependencies]\ninner = { path = \"../inner\" }\n[build-dependencies]\ninner = { path = \"../inner\" }\n[target.'cfg(host_flag)'.dependencies]\npositive = { path = '../positive' }\n[target.'cfg(not(host_flag))'.dependencies]\nnegative = { path = '../negative' }\n",
            "pub use inner::Kind;",
        );
        member(
            root,
            "inner",
            "[package]\nname = \"inner\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            "#![allow(unexpected_cfgs)]\npub enum Kind { Base, #[cfg(host_flag)] Flagged }",
        );
        for (name, item) in [("positive", "Positive"), ("negative", "Negative")] {
            member(
                root,
                name,
                &format!(
                    "[package]\nname = '{name}'\nversion = '0.1.0'\nedition = '2024'\n[workspace]\n"
                ),
                &format!("pub struct {item};"),
            );
        }
        success(
            Command::new("cargo")
                .args(["generate-lockfile", "--offline"])
                .current_dir(root)
                .output()
                .unwrap(),
        );
        fs::create_dir(root.join(".cargo")).unwrap();
        for selection in ["native", "cli", "config", "env"] {
            let predicate = if selection == "native" {
                "host_flag"
            } else {
                "not(host_flag)"
            };
            fs::write(root.join("app/Cargo.toml"), format!(
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[target.'cfg({predicate})'.build-dependencies]\nalias = {{ package = \"api\", path = \"../api\" }}\n"
            )).unwrap();
            let mut config = match source {
                "build" => "[build]\nrustflags = ['--cfg', 'host_flag']\n".to_string(),
                "triple" => format!("[target.{host}]\nrustflags = ['--cfg', 'host_flag']\n"),
                "cfg" => "[target.'cfg(all())']\nrustflags = ['--cfg', 'host_flag']\n".to_string(),
                _ => String::new(),
            };
            if selection == "config" {
                if source != "build" {
                    config.push_str("[build]\n");
                }
                config.push_str(&format!("target = '{host}'\n"));
            }
            fs::write(root.join(".cargo/config.toml"), config).unwrap();
            let build = if selection == "native" {
                "fn main() { let _ = inner::Kind::Flagged; }"
            } else {
                "fn main() { match inner::Kind::Base { inner::Kind::Base => () } }"
            };
            fs::write(root.join("api/build.rs"), build).unwrap();
            fs::write(
                root.join("app/build.rs"),
                build.replace("inner::", "alias::"),
            )
            .unwrap();
            let mut check = Command::new("cargo");
            check
                .args(["check", "--offline", "--locked", "--package", "app"])
                .current_dir(root);
            let mut query = query_command(root, "use alias::Kind;", &["--include-build"]);
            for command in [&mut check, &mut query] {
                for name in [
                    "RUSTFLAGS",
                    "CARGO_ENCODED_RUSTFLAGS",
                    "CARGO_BUILD_RUSTFLAGS",
                    "CARGO_BUILD_TARGET",
                    &target_flags,
                ] {
                    command.env_remove(name);
                }
                match source {
                    "plain" => {
                        command.env("RUSTFLAGS", "--cfg host_flag");
                    }
                    "encoded" => {
                        command.env("CARGO_ENCODED_RUSTFLAGS", "--cfg\x1fhost_flag");
                    }
                    "build-env" => {
                        command.env("CARGO_BUILD_RUSTFLAGS", "--cfg host_flag");
                    }
                    "target-env" => {
                        command.env(&target_flags, "--cfg host_flag");
                    }
                    _ => {}
                }
                if selection == "cli" {
                    command.args(["--target", host]);
                } else if selection == "env" {
                    command.env("CARGO_BUILD_TARGET", host);
                }
            }
            success(check.output().unwrap());
            let output = query.output().unwrap();
            assert!(output.status.success(), "{source}/{selection}: {output:?}");
            let report = success(output);
            assert!(
                report.contains(&format!("dependency: build (cfg({predicate}))\n")),
                "{report}"
            );
            assert!(
                report.contains("pub enum Kind"),
                "{source}/{selection}: {report}"
            );
            assert_eq!(
                report.contains("Flagged"),
                selection == "native",
                "{source}/{selection}: {report}"
            );
        }
    }
}

#[test]
fn proc_macro_host_graphs_preserve_conditional_transitive_dependencies() {
    let version = success(Command::new("rustc").arg("-vV").output().unwrap());
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    for flags in ["plain", "encoded", "configured"] {
        let dir = project();
        let root = dir.path();
        member(
            root,
            "app",
            "[package]\nname = 'app'\nversion = '0.1.0'\nedition = '2024'\n[lib]\nproc-macro = true\n[dependencies]\nalias = { package = 'api', path = '../api' }\n",
            "",
        );
        member(
            root,
            "api",
            "[package]\nname = 'api'\nversion = '1.2.3'\nedition = '2024'\n[dependencies]\ninner = { path = '../inner' }\n[target.'cfg(host_flag)'.dependencies]\npositive = { path = '../positive' }\n[target.'cfg(not(host_flag))'.dependencies]\nnegative = { path = '../negative' }\n",
            "pub use inner::Kind;",
        );
        fs::write(root.join("inner/src/lib.rs"), "#![allow(unexpected_cfgs)]\n#[cfg(host_flag)] pub struct Kind { pub target_flag: u8 }\n#[cfg(not(host_flag))] pub struct Kind { pub host_default: u8 }\n").unwrap();
        for (name, field) in [("positive", "target_flag"), ("negative", "host_default")] {
            member(
                root,
                name,
                &format!(
                    "[package]\nname = '{name}'\nversion = '0.1.0'\nedition = '2024'\n[workspace]\n"
                ),
                &format!("pub struct Kind {{ pub {field}: u8 }}"),
            );
        }
        success(
            Command::new("cargo")
                .args(["generate-lockfile", "--offline"])
                .current_dir(root)
                .output()
                .unwrap(),
        );
        fs::create_dir(root.join(".cargo")).unwrap();
        for selection in ["native", "cli", "config"] {
            let mut config = String::from("[build]\n");
            if flags == "configured" {
                config.push_str("rustflags = ['--cfg', 'host_flag']\n");
            }
            if selection == "config" {
                config.push_str(&format!("target = '{host}'\n"));
            }
            fs::write(root.join(".cargo/config.toml"), config).unwrap();
            let mut check = Command::new("cargo");
            check
                .args(["check", "--offline", "--locked", "--package", "app"])
                .current_dir(root);
            let mut query = query_command(root, "use alias::Kind;", &[]);
            for command in [&mut check, &mut query] {
                for name in [
                    "RUSTFLAGS",
                    "CARGO_ENCODED_RUSTFLAGS",
                    "CARGO_BUILD_RUSTFLAGS",
                    "CARGO_BUILD_TARGET",
                ] {
                    command.env_remove(name);
                }
                if flags == "plain" {
                    command.env("RUSTFLAGS", "--cfg host_flag");
                }
                if flags == "encoded" {
                    command.env("CARGO_ENCODED_RUSTFLAGS", "--cfg\x1fhost_flag");
                }
                if selection == "cli" {
                    command.args(["--target", host]);
                }
            }
            success(check.output().unwrap());
            let report = success(query.output().unwrap());
            assert_eq!(
                report.contains("pub target_flag: u8"),
                selection == "native",
                "{flags}/{selection}: {report}"
            );
            assert_eq!(
                report.contains("pub host_default: u8"),
                selection != "native",
                "{flags}/{selection}: {report}"
            );
        }
    }
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
#[cfg(unix)]
fn every_subprocess_uses_path_cargo_even_when_cargo_env_selects_another_wrapper() {
    use std::os::unix::fs::PermissionsExt;

    let dir = project();
    let root = dir.path();
    fs::write(
        root.join("api/src/lib.rs"),
        "#![allow(unexpected_cfgs)]\npub struct Record;\nimpl Record {\n#[cfg(path_cargo)] pub fn path_only() {}\n#[cfg(env_cargo)] pub fn env_only() {}\n}",
    )
    .unwrap();
    let bin = root.join("cargo-wrappers");
    fs::create_dir(&bin).unwrap();
    for (name, log, flag) in [
        ("cargo", "EXCRA_PATH_CARGO_LOG", "path_cargo"),
        ("env-cargo", "EXCRA_ENV_CARGO_LOG", "env_cargo"),
    ] {
        let script = bin.join(name);
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$1\" >> \"${log}\"\nexport RUSTFLAGS='--cfg {flag}'\nexport RUSTDOCFLAGS='--cfg {flag}'\nexec \"$EXCRA_REAL_CARGO\" \"$@\"\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = std::env::var_os("PATH").unwrap();
    let calls = root.join("path-cargo.log");
    let ignored = root.join("env-cargo.log");
    let mut command = query_command(root, "use alias::Record;", &[]);
    command
        .env(
            "PATH",
            std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(&path)))
                .unwrap(),
        )
        .env("CARGO", bin.join("env-cargo"))
        .env("EXCRA_REAL_CARGO", env!("CARGO"))
        .env("EXCRA_PATH_CARGO_LOG", &calls)
        .env("EXCRA_ENV_CARGO_LOG", &ignored);
    let report = success(command.output().unwrap());
    assert!(report.contains("pub fn path_only()"), "{report}");
    assert!(!report.contains("env_only"), "{report}");
    assert!(
        !ignored.exists(),
        "CARGO wrapper ran: {:?}",
        fs::read_to_string(ignored)
    );
    let calls = fs::read_to_string(calls).unwrap();
    for subcommand in ["metadata", "rustc", "tree", "doc"] {
        assert!(calls.lines().any(|line| line == subcommand), "{calls}");
    }
}

#[test]
#[cfg(unix)]
fn isolated_docs_preserve_workspace_wrapper_membership_and_composition() {
    use std::os::unix::fs::PermissionsExt;

    let dir = project();
    let root = dir.path();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = ['app', 'api', 'inner']\nexclude = ['outside']\nresolver = '3'\n",
    )
    .unwrap();
    fs::write(root.join("app/Cargo.toml"), "[package]\nname = 'app'\nversion = '0.1.0'\nedition = '2024'\n[dependencies]\nalias = { package = 'api', path = '../api' }\n").unwrap();
    fs::write(
        root.join("app/src/lib.rs"),
        "pub fn item() -> alias::Item { alias::Item { expected: 1 } }",
    )
    .unwrap();
    fs::write(root.join("api/Cargo.toml"), "[package]\nname = 'api'\nversion = '1.2.3'\nedition = '2024'\n[dependencies]\ninner = { path = '../inner' }\noutside = { path = '../outside' }\n").unwrap();
    fs::write(
        root.join("api/src/lib.rs"),
        "pub use inner::Item; pub use outside::External;",
    )
    .unwrap();
    fs::write(
        root.join("inner/src/lib.rs"),
        r#"
#![allow(unexpected_cfgs)]
#[cfg(workspace_flag)] pub struct Item { pub expected: u8 }
#[cfg(not(workspace_flag))] pub struct Item { pub incorrect: u16 }
"#,
    )
    .unwrap();
    member(
        root,
        "outside",
        "[package]\nname = 'outside'\nversion = '0.1.0'\nedition = '2024'\n",
        r#"
#![allow(unexpected_cfgs)]
#[cfg(workspace_flag)] pub struct External { pub incorrect_external: u16 }
#[cfg(not(workspace_flag))] pub struct External {
    pub expected_external: u8,
    #[cfg(general_flag)] pub general: bool,
}
"#,
    );
    fs::create_dir(root.join(".cargo")).unwrap();
    for (name, flag) in [
        ("workspace-wrapper", "workspace_flag"),
        ("general-wrapper", "general_flag"),
    ] {
        let wrapper = root.join(name);
        fs::write(&wrapper, format!("#!/bin/sh\nexec \"$@\" --cfg {flag}\n")).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    }
    success(
        Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(root)
            .output()
            .unwrap(),
    );
    for mode in [
        "environment",
        "composed",
        "build-environment",
        "configuration",
        "included-configuration",
        "print-link-args",
        "print-link-args-split",
        "print-native-static-libs",
        "external-request",
    ] {
        if mode == "external-request" {
            fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = ['app', 'inner']\nexclude = ['api', 'outside']\nresolver = '3'\n").unwrap();
        }
        let config = root.join(".cargo/config.toml");
        let mut contents = String::from(match mode {
            "configuration" => {
                "[build]\nrustc-workspace-wrapper = 'workspace-wrapper'\nrustc-wrapper = './general-wrapper'\n"
            }
            "included-configuration"
            | "print-link-args"
            | "print-link-args-split"
            | "print-native-static-libs" => "include = ['wrappers.toml']\n",
            _ => "",
        });
        contents.push_str(match mode {
            "print-link-args" => "[build]\nrustflags = ['--print=link-args']\n",
            "print-link-args-split" => "[build]\nrustflags = ['--print', 'link-args']\n",
            "print-native-static-libs" => "[build]\nrustflags = ['--print=native-static-libs']\n",
            _ => "",
        });
        fs::write(&config, contents).unwrap();
        fs::write(root.join(".cargo/wrappers.toml"), "[build]\nrustc-workspace-wrapper = './workspace-wrapper'\nrustc-wrapper = './general-wrapper'\n").unwrap();
        let configure = |command: &mut Command| {
            for name in [
                "RUSTC_WRAPPER",
                "RUSTC_WORKSPACE_WRAPPER",
                "CARGO_BUILD_RUSTC_WRAPPER",
                "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            ] {
                command.env_remove(name);
            }
            match mode {
                "environment" | "composed" | "external-request" => {
                    command.env("RUSTC_WORKSPACE_WRAPPER", "./workspace-wrapper");
                }
                "build-environment" => {
                    command.env("CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER", "./workspace-wrapper");
                }
                "configuration" => {
                    let mut path = std::env::split_paths(&std::env::var_os("PATH").unwrap())
                        .collect::<Vec<_>>();
                    path.insert(0, root.to_path_buf());
                    command.env("PATH", std::env::join_paths(path).unwrap());
                }
                _ => {}
            }
            if mode == "composed" || mode == "external-request" {
                command.env("RUSTC_WRAPPER", "./general-wrapper");
            }
            if mode == "build-environment" {
                command.env("CARGO_BUILD_RUSTC_WRAPPER", "./general-wrapper");
            }
            command.current_dir(root);
        };
        let mut ordinary = Command::new("cargo");
        ordinary.args([
            "doc",
            "--offline",
            "--locked",
            "--no-deps",
            "--package",
            "api",
        ]);
        configure(&mut ordinary);
        success(ordinary.output().unwrap());
        let expected = fs::read_to_string(root.join("target/doc/api/struct.Item.html")).unwrap();
        assert!(expected.contains("expected"), "{mode}");
        assert!(!expected.contains("incorrect"), "{mode}");
        let expected =
            fs::read_to_string(root.join("target/doc/api/struct.External.html")).unwrap();
        assert!(expected.contains("expected_external"), "{mode}");
        assert!(!expected.contains("incorrect_external"), "{mode}");
        let mut command = query_command(root, "use alias::{Item, External};", &[]);
        configure(&mut command);
        let report = success(command.output().unwrap());
        assert!(report.contains("pub expected: u8"), "{mode}: {report}");
        assert!(
            report.contains("pub expected_external: u8"),
            "{mode}: {report}"
        );
        assert!(!report.contains("incorrect"), "{mode}: {report}");
        assert_eq!(
            report.contains("pub general: bool"),
            mode != "environment",
            "{mode}: {report}"
        );
    }
}

#[test]
#[cfg(unix)]
fn compiler_wrapper_distinguishes_probes_with_inherited_package_context() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let member = root.join("member");
    let outside = root.join("outside");
    fs::create_dir(&member).unwrap();
    fs::create_dir(&outside).unwrap();
    let compiler = root.join("compiler");
    let wrapper = root.join("wrapper");
    fs::write(&compiler, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
    fs::write(&wrapper, "#!/bin/sh\nexec \"$@\" --cfg workspace_flag\n").unwrap();
    for script in [&compiler, &wrapper] {
        fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    for (arguments, probe) in [
        (vec!["-vV"], true),
        (vec!["--print=cfg"], true),
        (vec!["--print", "cfg", "--print", "target-libdir"], true),
        (vec!["--print=file-names", "--print=link-args"], true),
        (vec!["lib.rs", "--emit=metadata"], false),
        (vec!["lib.rs", "--print=link-args"], false),
        (vec!["lib.rs", "--print", "link-args"], false),
        (vec!["lib.rs", "--print=link-args=output.txt"], false),
        (vec!["lib.rs", "--print=native-static-libs"], false),
        (vec!["lib.rs", "--print", "native-static-libs"], false),
    ] {
        for directory in [Some(&member), Some(&outside), None] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_excra"));
            command
                .arg(&compiler)
                .args(&arguments)
                .env(
                    "EXCRA_COMPILER_WORKSPACE_MEMBERS",
                    serde_json::to_string(&[member.canonicalize().unwrap()]).unwrap(),
                )
                .env("EXCRA_COMPILER_GENERAL_WRAPPER", "")
                .env("EXCRA_COMPILER_WORKSPACE_WRAPPER", &wrapper);
            if let Some(directory) = directory {
                command.env("CARGO_MANIFEST_DIR", directory);
            } else {
                command.env_remove("CARGO_MANIFEST_DIR");
            }
            let output = success(command.output().unwrap());
            assert_eq!(
                output.contains("workspace_flag"),
                probe || directory != Some(&outside),
                "{arguments:?}, {directory:?}: {output}"
            );
        }
    }
}

#[test]
#[cfg(unix)]
fn repeated_configuration_includes_preserve_workspace_wrapper_precedence() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().unwrap();
    let parent = dir.path();
    let root = parent.join("child");
    fs::create_dir_all(root.join(".cargo")).unwrap();
    fs::create_dir(parent.join(".cargo")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = ['app', 'api', 'inner']\nresolver = '3'\n",
    )
    .unwrap();
    for (name, dependencies, source) in [
        (
            "app",
            "[dependencies]\nalias = { package = 'api', path = '../api' }\n",
            "",
        ),
        (
            "api",
            "[dependencies]\ninner = { path = '../inner' }\n",
            "pub use inner::Item;",
        ),
        (
            "inner",
            "",
            "#![allow(unexpected_cfgs)]\n#[cfg(one)] pub struct Item { pub expected: u8 }\n#[cfg(not(one))] pub struct Item { pub incorrect: u64 }\n",
        ),
    ] {
        member(
            &root,
            name,
            &format!(
                "[package]\nname = '{name}'\nversion = '0.1.0'\nedition = '2024'\n{dependencies}"
            ),
            source,
        );
    }
    for flag in ["one", "two"] {
        let wrapper = parent.join(format!("{flag}-wrapper"));
        fs::write(&wrapper, format!("#!/bin/sh\nexec \"$@\" --cfg {flag}\n")).unwrap();
        fs::set_permissions(wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        parent.join(".cargo/shared.toml"),
        "[build]\nrustc-workspace-wrapper = './one-wrapper'\n",
    )
    .unwrap();
    fs::write(
        parent.join(".cargo/config.toml"),
        "include = ['shared.toml']\n[build]\nrustc-workspace-wrapper = './two-wrapper'\n",
    )
    .unwrap();
    fs::write(
        root.join(".cargo/config.toml"),
        "include = ['../../.cargo/shared.toml']\n",
    )
    .unwrap();
    success(
        Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(&root)
            .output()
            .unwrap(),
    );
    success(
        Command::new("cargo")
            .args([
                "doc",
                "--offline",
                "--locked",
                "--no-deps",
                "--package",
                "api",
            ])
            .current_dir(&root)
            .output()
            .unwrap(),
    );
    let expected = fs::read_to_string(root.join("target/doc/api/struct.Item.html")).unwrap();
    assert!(expected.contains("expected"));
    assert!(!expected.contains("incorrect"));
    let report = success(query(&root, "use alias::Item;", &[]));
    assert!(report.contains("pub expected: u8"), "{report}");
    assert!(!report.contains("incorrect"), "{report}");
}

#[test]
#[cfg(unix)]
fn relative_cargo_home_preserves_workspace_wrappers_from_both_invocation_directories() {
    use std::os::unix::fs::PermissionsExt;

    let dir = project();
    let root = dir.path();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = ['app', 'api', 'inner']\nresolver = '3'\n",
    )
    .unwrap();
    fs::write(root.join("app/Cargo.toml"), "[package]\nname = 'app'\nversion = '0.1.0'\nedition = '2024'\n[dependencies]\nalias = { package = 'api', path = '../api' }\n").unwrap();
    fs::write(
        root.join("app/src/lib.rs"),
        "pub fn item() -> alias::Item { alias::Item { expected: 1 } }",
    )
    .unwrap();
    fs::write(root.join("api/src/lib.rs"), "pub use inner::Item;").unwrap();
    fs::write(
        root.join("inner/src/lib.rs"),
        "#![allow(unexpected_cfgs)]\n#[cfg(wrapper_flag)] pub struct Item { pub expected: u64 }\n#[cfg(not(wrapper_flag))] pub struct Item { pub incorrect: u8 }",
    )
    .unwrap();
    let wrapper = root.join("workspace-wrapper");
    fs::write(&wrapper, "#!/bin/sh\nexec \"$@\" --cfg wrapper_flag\n").unwrap();
    fs::set_permissions(wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let home = root.join("custom-cargo-home");
    fs::create_dir(&home).unwrap();
    let wrappers = "[build]\nrustc-workspace-wrapper = './workspace-wrapper'\n";
    fs::write(home.join("wrappers.toml"), wrappers).unwrap();
    for config in [wrappers, "include = ['wrappers.toml']\n"] {
        fs::write(home.join("config.toml"), config).unwrap();
        success(
            Command::new("cargo")
                .args(["check", "--offline", "--package", "app"])
                .env("CARGO_HOME", "custom-cargo-home")
                .current_dir(root)
                .output()
                .unwrap(),
        );
        let mut pages = Vec::new();
        for caller in [root, root.parent().unwrap()] {
            let report = success(
                query_command(root, "use alias::Item;", &[])
                    .env("CARGO_HOME", "custom-cargo-home")
                    .current_dir(caller)
                    .output()
                    .unwrap(),
            );
            assert!(report.contains("pub expected: u64"), "{report}");
            assert!(!report.contains("incorrect"), "{report}");
            pages.push(paths(&report, "source: "));
        }
        assert_eq!(pages[0], pages[1]);
    }
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
fn irrelevant_external_macro_reexports_do_not_block_local_modules() {
    let dir = project();
    fs::write(
        dir.path().join("api/src/lib.rs"),
        r#"
pub mod concat { pub struct Item; }
#[doc(no_inline)] pub use core::concat;
#[doc(no_inline)] pub use core::concat as OnlyExternal;
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("app/src/lib.rs"),
        "use alias::concat::{self, Item};",
    )
    .unwrap();
    let checked = Command::new("cargo")
        .args(["check", "--offline", "--locked", "--package", "app"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(checked.status.success(), "{:?}", checked);
    for import in ["use alias::concat::{self};", "use alias::concat::Item;"] {
        let report = success(query(dir.path(), import, &[]));
        assert!(report.contains("Item"), "{report}");
    }
    let error = failure(query(dir.path(), "use alias::OnlyExternal;", &[]));
    assert!(error.contains("rustdoc link 'https://doc.rust-lang.org/"));
    assert!(error.contains("is unavailable"));
}

#[test]
fn unavailable_globs_do_not_block_independent_local_bindings() {
    for (source, external) in [("std::fmt", "Formatter"), ("inner", "Kind")] {
        for nested in [false, true] {
            let dir = project();
            let root = dir.path();
            if source == "inner" {
                let inner = root.join("inner/src/lib.rs");
                fs::write(
                    &inner,
                    "#![doc(html_root_url = \"https://example.invalid/inner/\")]\n".to_string()
                        + &fs::read_to_string(&inner).unwrap(),
                )
                .unwrap();
            }
            let glob = format!("#[doc(no_inline)] pub use {source}::*;");
            let glob = if nested {
                format!("pub mod exports {{ {glob} }}\n#[doc(no_inline)] pub use exports::*;")
            } else {
                glob
            };
            let source = format!(
                "{glob}\npub struct Local {{ pub value: u8 }}\npub mod nested {{ pub struct Item; }}"
            );
            fs::write(root.join("api/src/lib.rs"), &source).unwrap();
            fs::write(
                root.join("app/src/lib.rs"),
                "use alias::{Local, nested::Item};",
            )
            .unwrap();
            success(
                Command::new("cargo")
                    .args(["check", "--offline", "--locked", "--package", "app"])
                    .current_dir(root)
                    .output()
                    .unwrap(),
            );
            let report = success(query(root, "use alias::{Local, nested::Item};", &[]));
            assert!(report.contains("pub struct Local"), "{report}");
            assert!(report.contains("pub struct Item"), "{report}");
            let error = failure(query(root, &format!("use alias::{external};"), &[]));
            assert!(
                error.contains("rustdoc link") && error.contains("is unavailable"),
                "{error}"
            );

            fs::write(
                root.join("api/src/lib.rs"),
                source + "\npub mod values { pub fn Local() {} }\n#[doc(no_inline)] pub use values::*;",
            )
            .unwrap();
            assert!(failure(query(root, "use alias::Local;", &[])).contains("ambiguous"));
        }
    }
}

#[test]
fn unavailable_globs_cannot_hide_competing_definitions() {
    for nested in [false, true] {
        let dir = project();
        let root = dir.path();
        fs::write(
            root.join("inner/src/lib.rs"),
            "#![doc(html_root_url = \"https://example.invalid/inner/\")]\npub struct Item { pub external: u64 }",
        )
        .unwrap();
        let external = "#[doc(no_inline)] pub use inner::*;";
        let external = if nested {
            format!("pub mod exports {{ {external} }}\n#[doc(no_inline)] pub use exports::*;")
        } else {
            external.to_string()
        };
        fs::write(
            root.join("api/src/lib.rs"),
            format!(
                "pub mod local {{ pub struct Item {{ pub local: u8 }} }}\n\
                 #[doc(no_inline)] pub use local::*;\n{external}"
            ),
        )
        .unwrap();
        fs::write(
            root.join("app/src/lib.rs"),
            "pub fn item(_: alias::Item) {}",
        )
        .unwrap();
        let checked = Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(!checked.status.success());
        assert!(
            String::from_utf8_lossy(&checked.stderr).contains("ambiguous"),
            "{checked:?}"
        );
        let error = failure(query(root, "use alias::Item;", &[]));
        assert!(
            error.contains("unavailable") || error.contains("ambiguous"),
            "{error}"
        );
        let report = success(query(root, "use alias::local::Item;", &[]));
        assert!(report.contains("pub local: u8"), "{report}");
    }
}

#[test]
fn local_non_inlined_globs_resolve_concrete_names_and_preserve_precedence() {
    let dir = project();
    fs::write(
        dir.path().join("api/src/lib.rs"),
        r#"
pub mod one {
    pub struct Item;
    pub struct Shadowed { pub from_glob: u8 }
    pub struct Clash;
    pub enum Choice { Variant }
    #[doc(no_inline)] pub use Choice::*;
    #[doc(no_inline)] pub use super::two::*;
}
pub mod two {
    pub struct Second;
    pub struct Clash;
    #[doc(no_inline)] pub use super::one::*;
}
#[doc(no_inline)] pub use one::*;
#[doc(no_inline)] pub use two::*;
pub struct Shadowed { pub explicit: u8 }
#[macro_export] macro_rules! exported { () => { 1 }; }
pub mod macros { #[doc(no_inline)] pub use crate::exported as Shared; }
#[doc(no_inline)] pub use macros::*;
pub mod Shared { pub struct Nested; }
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("app/src/lib.rs"),
        "use alias::{Item, Second, Shadowed, Variant, Shared::{self, Nested}};",
    )
    .unwrap();
    let checked = Command::new("cargo")
        .args(["check", "--offline", "--locked", "--package", "app"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(checked.status.success(), "{:?}", checked);
    let report = success(query(
        dir.path(),
        "use alias::{Item, Second, Shadowed, Variant, Shared::{self, Nested}};",
        &[],
    ));
    for text in [
        "pub struct Item",
        "pub struct Second",
        "pub explicit: u8",
        "pub enum Choice",
        "pub struct Nested",
    ] {
        assert!(report.contains(text), "missing {text}: {report}");
    }
    assert!(!report.contains("pub from_glob"), "{report}");
    for import in ["use alias::Clash;", "use alias::Shared;"] {
        let output = query(dir.path(), import, &[]);
        assert_eq!(output.status.code(), Some(2), "{import}: {output:?}");
        assert!(failure(output).contains("ambiguous"));
    }
    assert!(failure(query(dir.path(), "use alias::Missing;", &[])).contains("not found"));
    assert!(failure(query(dir.path(), "use alias::*;", &[])).contains("glob imports"));
}

#[test]
fn omitted_bindings_shadow_globs_without_substituting_another_definition() {
    let dir = project();
    let root = dir.path();
    for nested in [false, true] {
        for binding in [
            "#[doc(hidden)] pub struct Item { pub actual: u64 }",
            "#[derive(Debug)] #[doc(hidden)] pub struct Item { pub actual: u64 }",
            "struct Item;",
            "mod local { pub struct Item; } use local::Item;",
            "mod local { pub struct Item; } #[doc(hidden)] pub use local::Item;",
            "#[doc(hidden)] pub struct Actual { pub actual: u64 } pub use Actual as Item;",
            "#[doc(hidden)] pub struct Actual { pub actual: u64 } #[doc(no_inline)] pub use Actual as Item;",
            "#[doc(hidden)] pub mod local { pub struct Actual { pub actual: u64 } } #[doc(no_inline)] pub use local::Actual as Item;",
            "pub fn Item() {} #[doc(hidden)] pub struct Actual { pub actual: u64 } pub use Actual as Item;",
        ] {
            let source = format!(
                "pub mod origin {{ pub struct Item {{ pub wrong: u8 }} pub struct Control; }}\n\
                 #[doc(no_inline)] pub use origin::*;\n{binding}"
            );
            let source = if nested {
                format!("pub mod nested {{ {source} }}")
            } else {
                source
            };
            fs::write(root.join("api/src/lib.rs"), source).unwrap();
            let prefix = if nested { "alias::nested" } else { "alias" };
            let import = format!("use {prefix}::Item;");
            fs::write(root.join("app/src/lib.rs"), &import).unwrap();
            let checked = Command::new("cargo")
                .args(["check", "--offline", "--locked", "--package", "app"])
                .current_dir(root)
                .output()
                .unwrap();
            assert_eq!(
                checked.status.success(),
                binding.contains("doc(hidden)"),
                "{binding}: {checked:?}"
            );
            let output = query(root, &import, &[]);
            assert!(
                !output.status.success(),
                "{binding} (nested={nested}): {output:?}"
            );
            let error = failure(output);
            assert!(
                error.contains("no available rustdoc HTML page"),
                "{binding}: {error}"
            );
            // Derives may generate extra bindings. The explicit path remains
            // available even when the glob's complete binding set is unknown.
            let control = if binding.contains("derive") {
                "origin::Control"
            } else {
                "Control"
            };
            let report = success(query(root, &format!("use {prefix}::{control};"), &[]));
            assert!(
                paths(&report, "source: ")[0].ends_with("origin/struct.Control.html"),
                "{report}"
            );
        }
    }
}

#[test]
fn foreign_bindings_shadow_globs_and_keep_their_namespace_and_cfg() {
    let dir = project();
    let root = dir.path();
    for nested in [false, true] {
        for (binding, available) in [
            ("#[doc(hidden)] pub fn item(arg: u64);", false),
            ("#[doc(hidden)] pub static item: u64;", false),
            ("fn item(arg: u64);", false),
            ("#[cfg(any())] #[doc(hidden)] pub fn item(arg: u64);", true),
            ("#[cfg(all())] #[doc(hidden)] pub static item: u64;", false),
            (
                "#[cfg_attr(all(), doc(hidden))] pub fn item(arg: u64);",
                false,
            ),
        ] {
            let source = format!(
                "pub mod origin {{ pub fn item(_: u8) {{}} pub struct Control; }}\n\
                 #[doc(no_inline)] pub use origin::*;\n\
                 unsafe extern \"C\" {{ {binding} }}"
            );
            let source = format!(
                "{source}\npub mod independent {{ {source} pub mod item {{ pub struct Nested; }} }}"
            );
            fs::write(
                root.join("api/src/lib.rs"),
                if nested {
                    format!("pub mod scope {{ {source} }}")
                } else {
                    source
                },
            )
            .unwrap();
            let prefix = if nested { "alias::scope" } else { "alias" };
            success(
                Command::new("cargo")
                    .args(["check", "--offline", "--locked", "--package", "api"])
                    .current_dir(root)
                    .output()
                    .unwrap(),
            );
            let output = query(root, &format!("use {prefix}::item;"), &[]);
            if available {
                let report = success(output);
                assert!(report.contains("pub fn item(_: u8)"), "{report}");
            } else {
                let error = failure(output);
                assert!(
                    error.contains("no available rustdoc HTML page")
                        || error.contains("source bindings are incomplete"),
                    "{binding}: {error}"
                );
            }
            let report = success(query(
                root,
                &format!("use {prefix}::independent::item::Nested;"),
                &[],
            ));
            assert!(report.contains("pub struct Nested"), "{report}");
            let report = success(query(root, &format!("use {prefix}::origin::Control;"), &[]));
            assert!(report.contains("pub struct Control"), "{report}");
        }
    }
}

#[test]
fn exported_macros_shadow_globs_at_the_crate_root() {
    let dir = project();
    let root = dir.path();
    for (binding, available) in [
        (
            "#[doc(hidden)] #[macro_export] macro_rules! Item { () => { 2u64 }; }",
            false,
        ),
        (
            "pub mod nested { #[doc(hidden)] #[macro_export] macro_rules! Item { () => { 2u64 }; } }",
            false,
        ),
        ("pub mod nested;", false),
        (
            "pub fn define() { #[doc(hidden)] #[macro_export] macro_rules! Item { () => { 2u64 }; } }",
            false,
        ),
        (
            "#[cfg(any())] #[doc(hidden)] #[macro_export] macro_rules! Item { () => { 2u64 }; }",
            true,
        ),
        ("macro_rules! Item { () => { 2u64 }; }", true),
    ] {
        fs::write(
            root.join("api/src/lib.rs"),
            format!(
                "#[macro_export] macro_rules! Visible {{ () => {{ 1u8 }}; }}\n\
                 #[doc(no_inline)] pub use Visible as Direct;\n\
                 pub mod origin {{ #[doc(no_inline)] pub use crate::Visible as Item; pub struct Control; }}\n\
                 #[doc(no_inline)] pub use origin::*;\n{binding}\n{}",
                if binding == "pub mod nested;" {
                    "#[doc(no_inline)] pub use Extra as NestedAlias;"
                } else {
                    ""
                }
            ),
        )
        .unwrap();
        fs::write(
            root.join("api/src/nested.rs"),
            "#[doc(hidden)] #[macro_export] macro_rules! Item { () => { 2u64 }; }\n\
             #[macro_export] macro_rules! Extra { () => { 3u8 }; }\n\
             #[doc(no_inline)] pub use crate::origin::*;",
        )
        .unwrap();
        fs::write(
            root.join("app/src/lib.rs"),
            format!(
                "pub fn value() -> {} {{ alias::Item!() }}",
                if available { "u8" } else { "u64" }
            ),
        )
        .unwrap();
        success(
            Command::new("cargo")
                .args(["check", "--offline", "--locked", "--package", "app"])
                .current_dir(root)
                .output()
                .unwrap(),
        );
        let output = query(root, "use alias::Item;", &[]);
        if available {
            let report = success(output);
            assert!(
                paths(&report, "source: ")[0].ends_with("macro.Visible.html"),
                "{binding}: {report}"
            );
        } else {
            let error = failure(output);
            assert!(
                error.contains("no available rustdoc HTML page")
                    || error.contains("source bindings are incomplete"),
                "{binding}: {error}"
            );
        }
        let report = success(query(root, "use alias::Control;", &[]));
        assert!(report.contains("pub struct Control"), "{report}");
        let report = success(query(root, "use alias::Direct;", &[]));
        assert!(report.contains("macro_rules!"), "{report}");
        if binding == "pub mod nested;" {
            let report = success(query(root, "use alias::NestedAlias;", &[]));
            assert!(report.contains("macro_rules! Extra"), "{report}");
            let report = success(query(root, "use alias::nested::Item;", &[]));
            assert!(
                paths(&report, "source: ")[0].ends_with("macro.Visible.html"),
                "{report}"
            );
        }
    }
}

#[test]
fn uncertain_source_bindings_never_substitute_glob_definitions() {
    let dir = project();
    let root = dir.path();
    for nested in [false, true] {
        for binding in [
            "#[cfg_attr(all(), doc(hidden))] pub struct Item { pub actual: u64 }",
            "macro_rules! define { () => { #[doc(hidden)] pub struct Item { pub actual: u64 } }; } define!();",
        ] {
            let source = format!(
                "pub mod origin {{ pub struct Item {{ pub wrong: u8 }} pub struct Control; }}\n\
                 #[doc(no_inline)] pub use origin::*;\n\
                 #[doc(no_inline)] pub use origin::Control;\n{binding}"
            );
            let source = if nested {
                format!("pub mod nested {{ {source} }}")
            } else {
                source
            };
            fs::write(root.join("api/src/lib.rs"), source).unwrap();
            let prefix = if nested { "alias::nested" } else { "alias" };
            fs::write(
                root.join("app/src/lib.rs"),
                format!("pub fn item() -> {prefix}::Item {{ {prefix}::Item {{ actual: 1 }} }}"),
            )
            .unwrap();
            success(
                Command::new("cargo")
                    .args(["check", "--offline", "--locked", "--package", "app"])
                    .current_dir(root)
                    .output()
                    .unwrap(),
            );
            let error = failure(query(root, &format!("use {prefix}::Item;"), &[]));
            assert!(
                error.contains("cannot establish binding"),
                "{binding}: {error}"
            );
            let report = success(query(root, &format!("use {prefix}::Control;"), &[]));
            assert!(report.contains("pub struct Control"), "{report}");
        }
    }
}

#[test]
fn documentation_source_links_cannot_supply_glob_binding_evidence() {
    let dir = project();
    let root = dir.path();
    for genuine_source in [false, true] {
        fs::write(
            root.join("api/src/lib.rs"),
            format!(
                r#"{}
//! <a class="src" href="struct.Control.html">Source</a>
pub struct Control;
pub mod origin {{ pub struct Item {{ pub wrong: u8 }} }}
#[doc(no_inline)] pub use origin::*;
#[doc(hidden)] pub struct Item {{ pub actual: u64 }}
"#,
                if genuine_source {
                    ""
                } else {
                    "#![doc(html_no_source)]"
                }
            ),
        )
        .unwrap();
        fs::write(
            root.join("app/src/lib.rs"),
            "pub fn item() -> alias::Item { alias::Item { actual: 1 } }",
        )
        .unwrap();
        success(
            Command::new("cargo")
                .args(["check", "--offline", "--locked", "--package", "app"])
                .current_dir(root)
                .output()
                .unwrap(),
        );
        let error = failure(query(root, "use alias::Item;", &[]));
        assert!(
            error.contains(if genuine_source {
                "no available rustdoc HTML page"
            } else {
                "source bindings are incomplete"
            }),
            "{error}"
        );
    }
}

#[test]
fn inactive_conditional_bindings_allow_globs_but_enabled_hidden_bindings_block_them() {
    let dir = project();
    let root = dir.path();
    // This fixture checks the default compiler context. Coverage instruments
    // excra itself already; its inherited compiler wrapper must not change it.
    let (active_platform, inactive_platform) = if cfg!(windows) {
        ("windows", "unix")
    } else {
        ("unix", "windows")
    };
    for nested in [false, true] {
        for (predicate, flags, active) in [
            ("any()", &[][..], false),
            ("feature = \"extra\"", &[][..], false),
            ("feature = \"extra\"", &["--features", "extra"][..], true),
            (inactive_platform, &[][..], false),
            (active_platform, &[][..], true),
            ("not(doc)", &[][..], false),
            ("doc", &[][..], true),
        ] {
            let source = format!(
                "pub mod origin {{ pub struct Item {{ pub right: u8 }} }}\n\
                 #[doc(no_inline)] pub use origin::*;\n\
                 #[cfg({predicate})] #[doc(hidden)] pub struct Item {{ pub actual: u64 }}"
            );
            fs::write(
                root.join("api/src/lib.rs"),
                if nested {
                    format!("pub mod nested {{ {source} }}")
                } else {
                    source
                },
            )
            .unwrap();
            let prefix = if nested { "alias::nested" } else { "alias" };
            if !predicate.contains("doc") {
                fs::write(
                    root.join("app/src/lib.rs"),
                    format!(
                        "pub fn item() -> {prefix}::Item {{ {prefix}::Item {{ {}: 1 }} }}",
                        if active { "actual" } else { "right" }
                    ),
                )
                .unwrap();
                success(
                    Command::new("cargo")
                        .args(["check", "--offline", "--locked", "--package", "app"])
                        .args(flags)
                        .env_remove("RUSTC_WRAPPER")
                        .current_dir(root)
                        .output()
                        .unwrap(),
                );
            }
            let output = query_command(root, &format!("use {prefix}::Item;"), flags)
                .env_remove("RUSTC_WRAPPER")
                .output()
                .unwrap();
            if active {
                let error = failure(output);
                assert!(
                    error.contains("no available rustdoc HTML page"),
                    "{predicate}: {error}"
                );
            } else {
                let report = success(output);
                assert!(report.contains("pub right: u8"), "{predicate}: {report}");
                assert!(
                    paths(&report, "source: ")[0].ends_with("origin/struct.Item.html"),
                    "{report}"
                );
            }
        }
    }
    // Rustdoc flags can enable a feature outside Cargo's feature graph. Never
    // infer that such a declaration is inactive from metadata alone.
    fs::write(root.join("api/src/lib.rs"), "pub mod origin { pub struct Item { pub wrong: u8 } }\n#[doc(no_inline)] pub use origin::*;\n#[cfg(feature = \"injected\")] #[doc(hidden)] pub struct Item { pub actual: u64 }").unwrap();
    let error = failure(
        query_command(root, "use alias::Item;", &[])
            .env_remove("RUSTC_WRAPPER")
            .env("RUSTDOCFLAGS", "--cfg feature=\"injected\"")
            .output()
            .unwrap(),
    );
    assert!(error.contains("no available rustdoc HTML page"), "{error}");
}

#[test]
fn deeply_layered_glob_reexports_resolve_to_one_item() {
    let dir = project();
    let levels = 30;
    let mut source = "pub mod leaf { pub struct Item; }\n".to_string();
    for level in 0..levels {
        let previous = if level == 0 {
            vec!["leaf".to_string()]
        } else {
            vec![format!("a{}", level - 1), format!("b{}", level - 1)]
        };
        let exports = previous
            .iter()
            .map(|name| format!("#[doc(no_inline)] pub use crate::{name}::*;"))
            .collect::<String>();
        for side in ["a", "b"] {
            source.push_str(&format!("pub mod {side}{level} {{ {exports} }}\n"));
        }
    }
    source.push_str(&format!(
        "#[doc(no_inline)] pub use a{}::*;\n#[doc(no_inline)] pub use b{}::*;",
        levels - 1,
        levels - 1
    ));
    fs::write(dir.path().join("api/src/lib.rs"), source).unwrap();
    let report = success(query(dir.path(), "use alias::Item;", &[]));
    assert_eq!(paths(&report, "source: ").len(), 1);
    assert!(report.contains("pub struct Item"), "{report}");
}

#[test]
fn mixed_alias_glob_layers_resolve_to_the_shared_definition() {
    let dir = project();
    let root = dir.path();
    let levels = 12;
    let mut source = "pub mod leaf { pub mod M0 { pub struct Item; } }\n".to_string();
    for level in 1..=levels {
        let previous = level - 1;
        let imports = if level == 1 {
            "pub use crate::leaf::*;".to_string()
        } else {
            format!("pub use crate::a{previous}::*; pub use crate::b{previous}::*;")
        };
        for side in ["a", "b"] {
            source.push_str(&format!(
                "pub mod {side}{level} {{ {imports} #[doc(inline)] pub use M{previous} as M{level}; }}\n"
            ));
        }
    }
    source.push_str(&format!(
        "#[doc(no_inline)] pub use a{levels}::*;\n#[doc(no_inline)] pub use b{levels}::*;"
    ));
    fs::write(root.join("api/src/lib.rs"), source).unwrap();
    let import = format!("use alias::M{levels}::Item;");
    fs::write(root.join("app/src/lib.rs"), &import).unwrap();
    success(
        Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap(),
    );
    let report = success(query(root, &import, &[]));
    assert_eq!(paths(&report, "source: ").len(), 1);
    assert!(report.contains("pub struct Item"), "{report}");
}

#[test]
fn inlined_reexport_pages_share_item_identity_without_hiding_real_conflicts() {
    let dir = project();
    let root = dir.path();
    fs::write(
        root.join("api/src/lib.rs"),
        r#"
pub mod a {
    pub struct Item;
    pub enum Choice { First, Second }
    pub trait Trait {}
    pub type Alias = u8;
    pub fn function() {}
    pub const VALUE: u8 = 1;
    pub static STATIC: u8 = 1;
    pub mod nested { pub struct Nested; }
    #[doc(inline)] pub use Item as Renamed;
}
pub mod b { #[doc(inline)] pub use crate::a::*; }
#[doc(no_inline)] pub use a::*;
#[doc(no_inline)] pub use b::*;
pub mod one { pub struct Clash; pub fn conflict() {} pub const CONSTANT: u8 = 1; }
pub mod two { pub struct Clash; pub fn conflict() {} pub const CONSTANT: u8 = 1; }
pub mod same_line { pub mod one { pub struct Same; pub fn same() {} } pub mod two { pub struct Same; pub fn same() {} } #[doc(no_inline)] pub use one::*; #[doc(no_inline)] pub use two::*; }
#[doc(no_inline)] pub use one::*;
#[doc(no_inline)] pub use two::*;
pub mod first { #[doc(no_inline)] pub use crate::a::Choice::First as Variant; }
pub mod second { #[doc(no_inline)] pub use crate::a::Choice::Second as Variant; }
#[doc(no_inline)] pub use first::*;
#[doc(no_inline)] pub use second::*;
pub struct Namespace;
#[macro_export] macro_rules! Namespace { () => { 1 }; }
"#,
    )
    .unwrap();
    let import = "use alias::{Item, Renamed, Choice::{self, First}, Trait, Alias, function, VALUE, STATIC, nested::Nested};";
    fs::write(root.join("app/src/lib.rs"), import).unwrap();
    success(
        Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap(),
    );
    let report = success(query(root, import, &[]));
    assert_eq!(paths(&report, "source: ").len(), 10, "{report}");
    for name in [
        "Clash",
        "conflict",
        "CONSTANT",
        "same_line::Same",
        "same_line::same",
        "Variant",
        "Namespace",
    ] {
        let error = failure(query(root, &format!("use alias::{name};"), &[]));
        assert!(error.contains("ambiguous"), "{name}: {error}");
    }
}

#[test]
fn documentation_html_cannot_merge_distinct_structs_or_traits() {
    let dir = project();
    let root = dir.path();
    for injected in [false, true] {
        let struct_doc = if injected {
            r#"/// <div id="synthetic-implementations-list"><a class="struct" href="../shared/struct.Shared.html">Shared</a></div>"#
        } else {
            ""
        };
        let trait_doc = if injected {
            r#"/// <script src="../../trait.impl/api/shared/trait.SharedTrait.js"></script>"#
        } else {
            ""
        };
        fs::write(
            root.join("api/src/lib.rs"),
            format!(
                r#"
pub mod a {{
    {struct_doc}
    pub struct Clash {{ pub first: u8 }}
    {trait_doc}
    pub trait TraitClash {{ fn first(&self); }}
}}
pub mod b {{
    {struct_doc}
    pub struct Clash {{ pub second: u64 }}
    {trait_doc}
    pub trait TraitClash {{ fn second(&self); }}
}}
pub mod shared {{ pub struct Shared; pub trait SharedTrait {{}} }}
#[doc(no_inline)] pub use a::*;
#[doc(no_inline)] pub use b::*;
"#
            ),
        )
        .unwrap();
        for name in ["Clash", "TraitClash"] {
            let import = format!("use alias::{name};");
            fs::write(root.join("app/src/lib.rs"), &import).unwrap();
            let checked = Command::new("cargo")
                .args(["check", "--offline", "--locked", "--package", "app"])
                .current_dir(root)
                .output()
                .unwrap();
            assert!(!checked.status.success());
            assert!(String::from_utf8_lossy(&checked.stderr).contains("ambiguous"));
            let error = failure(query(root, &import, &[]));
            assert!(error.contains("ambiguous"), "{injected}: {name}: {error}");
        }
    }
}

#[test]
fn scoped_aliases_and_edition_2015_roots_share_canonical_reexport_identity() {
    let dir = project();
    let root = dir.path();
    let manifest = root.join("api/Cargo.toml");
    let original = fs::read_to_string(&manifest).unwrap();
    fs::write(root.join("inner/src/lib.rs"), "pub fn item() {}").unwrap();
    for out_of_line in [false, true] {
        for (edition, body) in [
            (
                "2024",
                "use crate::source as local; #[doc(inline)] pub use local::item;",
            ),
            (
                "2024",
                "use super::source::{self as first}; use self::first as local; #[doc(inline)] pub use local::item;",
            ),
            (
                "2024",
                "pub mod imports { pub use crate::source as local; } #[doc(inline)] pub use imports::local::item;",
            ),
            (
                "2024",
                "pub use crate::source::item as local; #[doc(inline)] pub use self::local as item;",
            ),
            (
                "2024",
                "use inner as local; #[doc(inline)] pub use local::item;",
            ),
            ("2015", "#[doc(inline)] pub use source::item;"),
            ("2015", "#[doc(inline)] pub use ::source::item;"),
            (
                "2015",
                "use source as local; #[doc(inline)] pub use self::local::item;",
            ),
            (
                "2015",
                "use upstream as local; #[doc(inline)] pub use self::local::item;",
            ),
        ] {
            fs::write(
                &manifest,
                original.replace("edition = \"2024\"", &format!("edition = \"{edition}\"")),
            )
            .unwrap();
            let mut source =
                "extern crate inner as upstream;\npub mod source { pub fn item() {} }\n"
                    .to_string();
            for name in ["a", "b"] {
                fs::write(root.join(format!("api/src/{name}.rs")), body).unwrap();
                source.push_str(&if out_of_line {
                    format!("pub mod {name};\n")
                } else {
                    format!("pub mod {name} {{ {body} }}\n")
                });
            }
            source.push_str("#[doc(no_inline)] pub use a::*;\n#[doc(no_inline)] pub use b::*;");
            fs::write(root.join("api/src/lib.rs"), source).unwrap();
            fs::write(root.join("app/src/lib.rs"), "use alias::item;").unwrap();
            success(
                Command::new("cargo")
                    .args(["check", "--offline", "--locked", "--package", "app"])
                    .current_dir(root)
                    .output()
                    .unwrap(),
            );
            let report = success(query(root, "use alias::item;", &[]));
            assert_eq!(
                paths(&report, "source: ").len(),
                1,
                "{edition}: {body}: {report}"
            );
            assert!(report.contains("pub fn item()"), "{report}");
        }
    }
    fs::write(&manifest, original).unwrap();
    fs::write(root.join("api/src/lib.rs"), "pub mod source { pub fn item() {} }\npub mod a { use crate::source as local; #[doc(inline)] pub use local::item; }\npub mod b { use inner as local; #[doc(inline)] pub use local::item; }\n#[doc(no_inline)] pub use a::*;\n#[doc(no_inline)] pub use b::*;").unwrap();
    let error = failure(query(root, "use alias::item;", &[]));
    assert!(error.contains("ambiguous"), "{error}");
}

#[test]
fn glob_imported_module_aliases_follow_their_canonical_origins() {
    let dir = project();
    let root = dir.path();
    for (imports, origin, field) in [
        ("pub use crate::original::*;", "original", "correct: u8"),
        (
            "pub use crate::first::*; pub use crate::second::*;",
            "original",
            "correct: u8",
        ),
        (
            "pub use crate::other::*; pub use crate::original::nested;",
            "original",
            "correct: u8",
        ),
        (
            "pub use crate::values::*; pub use crate::original::*;",
            "original",
            "correct: u8",
        ),
        (
            "pub use crate::original::nested; pub use crate::values::nested;",
            "original",
            "correct: u8",
        ),
        (
            "pub use crate::values::nested; pub use crate::original::nested;",
            "original",
            "correct: u8",
        ),
        (
            "pub use crate::original::nested as common; pub use crate::values::nested as common; pub use common as nested;",
            "original",
            "correct: u8",
        ),
        (
            "pub use crate::original::*; pub use crate::values::nested;",
            "original",
            "correct: u8",
        ),
        (
            "pub use crate::other::*; pub mod nested { pub struct Item { pub local: u16 } }",
            "a",
            "local: u16",
        ),
        ("pub use crate::cycle_one::*;", "original", "correct: u8"),
    ] {
        fs::write(
            root.join("api/src/lib.rs"),
            format!(
                r#"
pub mod original {{ pub mod nested {{ pub struct Item {{ pub correct: u8 }} }} }}
pub mod other {{ pub mod nested {{ pub struct Item {{ pub wrong: u64 }} }} }}
pub mod values {{ pub fn nested() {{}} }}
pub mod first {{ pub use crate::original::*; }}
pub mod second {{ pub use crate::first::*; pub use crate::original::*; }}
pub mod cycle_one {{ pub use crate::cycle_two::*; pub use crate::original::*; }}
pub mod cycle_two {{ pub use crate::cycle_one::*; }}
pub mod a {{ {imports} #[doc(inline)] pub use nested as Alias; }}
pub mod b {{ #[doc(inline)] pub use crate::{origin}::nested as Alias; }}
#[doc(no_inline)] pub use a::*;
#[doc(no_inline)] pub use b::*;
"#
            ),
        )
        .unwrap();
        let import = "use alias::Alias::Item;";
        fs::write(
            root.join("app/src/lib.rs"),
            "use alias::Alias::{self, Item};",
        )
        .unwrap();
        success(
            Command::new("cargo")
                .args(["check", "--offline", "--locked", "--package", "app"])
                .current_dir(root)
                .output()
                .unwrap(),
        );
        let report = success(query(root, import, &[]));
        assert_eq!(paths(&report, "source: ").len(), 1, "{imports}: {report}");
        assert!(
            report.contains(&format!("pub {field}")),
            "{imports}: {report}"
        );
        let report = success(query(root, "use alias::Alias::{self};", &[]));
        assert_eq!(paths(&report, "source: ").len(), 1, "{imports}: {report}");
        assert!(report.contains("# Module"), "{imports}: {report}");
        if imports.contains("values") {
            let error = failure(query(root, "use alias::Alias;", &[]));
            assert!(
                error.contains("ambiguous") || error.contains("no available rustdoc HTML page"),
                "{imports}: {error}"
            );
        }
    }
    fs::write(
        root.join("api/src/lib.rs"),
        r#"
pub mod original { pub mod nested { pub struct Item; } }
pub mod other { pub mod nested { pub struct Item; } }
pub mod a { pub use crate::original::*; #[doc(inline)] pub use nested as Alias; }
pub mod b { pub use crate::other::*; #[doc(inline)] pub use nested as Alias; }
#[doc(no_inline)] pub use a::*;
#[doc(no_inline)] pub use b::*;
"#,
    )
    .unwrap();
    let checked = Command::new("cargo")
        .args(["check", "--offline", "--locked", "--package", "app"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(!checked.status.success());
    assert!(String::from_utf8_lossy(&checked.stderr).contains("ambiguous"));
    let error = failure(query(root, "use alias::Alias::Item;", &[]));
    assert!(error.contains("ambiguous"), "{error}");
    let error = failure(query(root, "use alias::Alias::{self};", &[]));
    assert!(error.contains("ambiguous"), "{error}");
}

#[test]
fn harmless_attributes_do_not_disable_explicit_reexport_identity() {
    let dir = project();
    let root = dir.path();
    for (crate_attrs, unrelated) in [
        ("#![no_std]", ""),
        ("", "#[derive(Debug)] pub struct Unrelated;"),
        (
            "#![no_std]\n#![recursion_limit = \"128\"]",
            "#[derive(Debug, Clone)] pub struct Unrelated;",
        ),
    ] {
        fs::write(
            root.join("api/src/lib.rs"),
            format!(
                r#"
{crate_attrs}
pub fn source() {{}}
pub mod a {{ {unrelated} #[doc(inline)] pub use crate::source as Alias; }}
pub mod b {{ {unrelated} #[doc(inline)] pub use crate::source as Alias; }}
#[doc(no_inline)] pub use a::*;
#[doc(no_inline)] pub use b::*;
"#
            ),
        )
        .unwrap();
        fs::write(root.join("app/src/lib.rs"), "use alias::Alias;").unwrap();
        success(
            Command::new("cargo")
                .args(["check", "--offline", "--locked", "--package", "app"])
                .current_dir(root)
                .output()
                .unwrap(),
        );
        let report = success(query(root, "use alias::Alias;", &[]));
        assert_eq!(paths(&report, "source: ").len(), 1, "{report}");
        assert!(report.contains("pub fn Alias"), "{report}");
    }
}

#[test]
fn external_facade_and_macro_exports_share_their_canonical_definitions() {
    let dir = project();
    let root = dir.path();
    let manifest = root.join("inner/Cargo.toml");
    let original = fs::read_to_string(&manifest).unwrap();
    fs::write(root.join("inner/src/internal.rs"), "pub fn original() {}").unwrap();
    for (proc_macro, source, left, right, name, usage, kind) in [
        (
            false,
            "mod internal { pub fn item() {} } pub use internal::item;",
            "item",
            "item",
            "item",
            "pub fn call() { alias::item(); }",
            "fn",
        ),
        (
            false,
            "mod internal; pub use internal::original as first; pub use first as second;",
            "first",
            "second",
            "item",
            "pub fn call() { alias::item(); }",
            "fn",
        ),
        (
            false,
            "mod internal { pub fn original() {} } pub mod facade { pub use crate::internal::original as item; } pub use facade::*;",
            "item",
            "facade::item",
            "item",
            "pub fn call() { alias::item(); }",
            "fn",
        ),
        (
            false,
            "mod nested { #[macro_export] macro_rules! Foo { () => { 1u8 }; } }",
            "Foo",
            "Foo",
            "Foo",
            "pub fn value() -> u8 { alias::Foo!() }",
            "macro",
        ),
        (
            false,
            "mod nested { #[macro_export] macro_rules! Foo { () => { 1u8 }; } } pub use crate::Foo as Other;",
            "Foo",
            "Other",
            "Foo",
            "pub fn value() -> u8 { alias::Foo!() }",
            "macro",
        ),
        (
            true,
            "#[proc_macro_derive(Foo, attributes(helper))] pub fn derive_foo(_: proc_macro::TokenStream) -> proc_macro::TokenStream { proc_macro::TokenStream::new() }",
            "Foo",
            "Foo",
            "Foo",
            "#[derive(alias::Foo)] #[helper] pub struct Value;",
            "derive",
        ),
        (
            true,
            "#[proc_macro_attribute] pub fn Foo(_: proc_macro::TokenStream, item: proc_macro::TokenStream) -> proc_macro::TokenStream { item }",
            "Foo",
            "Foo",
            "Foo",
            "#[alias::Foo] pub struct Value;",
            "attr",
        ),
        (
            true,
            "#[proc_macro] pub fn Foo(_: proc_macro::TokenStream) -> proc_macro::TokenStream { \"1u8\".parse().unwrap() }",
            "Foo",
            "Foo",
            "Foo",
            "pub fn value() -> u8 { alias::Foo!() }",
            "macro",
        ),
    ] {
        fs::write(
            &manifest,
            if proc_macro {
                format!("{original}\n[lib]\nproc-macro = true\n")
            } else {
                original.clone()
            },
        )
        .unwrap();
        fs::write(root.join("inner/src/lib.rs"), source).unwrap();
        fs::write(
            root.join("api/src/lib.rs"),
            format!(
                "pub mod a {{ #[doc(inline)] pub use inner::{left} as {name}; }}\n\
                 pub mod b {{ #[doc(inline)] pub use inner::{right} as {name}; }}\n\
                 #[doc(no_inline)] pub use a::*;\n#[doc(no_inline)] pub use b::*;"
            ),
        )
        .unwrap();
        fs::write(root.join("app/src/lib.rs"), usage).unwrap();
        success(
            Command::new("cargo")
                .args(["check", "--offline", "--locked", "--package", "app"])
                .current_dir(root)
                .output()
                .unwrap(),
        );
        let report = success(query(root, &format!("use alias::{name};"), &[]));
        let pages = paths(&report, "source: ");
        assert_eq!(pages.len(), 1, "{source}: {report}");
        assert!(
            pages[0].ends_with(format!("{kind}.{name}.html")),
            "{report}"
        );
    }
}

#[test]
fn distinct_external_facade_exports_remain_ambiguous() {
    let dir = project();
    let root = dir.path();
    for (source, first, second, kind) in [
        (
            "mod one { pub fn item() {} } mod two { pub fn item() {} } pub use one::item as First; pub use two::item as Second;",
            "First",
            "Second",
            "fn",
        ),
        (
            "mod one { #[macro_export] macro_rules! First { () => { 1u8 }; } } mod two { #[macro_export] macro_rules! Second { () => { 2u8 }; } }",
            "First",
            "Second",
            "macro",
        ),
    ] {
        fs::write(root.join("inner/src/lib.rs"), source).unwrap();
        fs::write(
            root.join("api/src/lib.rs"),
            format!(
                "pub mod a {{ #[doc(inline)] pub use inner::{first} as Item; }}\n\
                 pub mod b {{ #[doc(inline)] pub use inner::{second} as Item; }}\n\
                 #[doc(no_inline)] pub use a::*;\n#[doc(no_inline)] pub use b::*;"
            ),
        )
        .unwrap();
        fs::write(root.join("app/src/lib.rs"), "use alias::Item;").unwrap();
        let checked = Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(!checked.status.success(), "{kind}: {checked:?}");
        assert!(String::from_utf8_lossy(&checked.stderr).contains("ambiguous"));
        let error = failure(query(root, "use alias::Item;", &[]));
        assert!(error.contains("ambiguous"), "{kind}: {error}");
    }
}

#[test]
fn external_inlined_aliases_share_identity_without_merging_distinct_definitions() {
    let dir = project();
    let root = dir.path();
    fs::write(
        root.join("inner/src/lib.rs"),
        r#"
pub struct Record;
pub enum Choice { One, Two }
pub trait Trait {}
pub type Alias = u8;
pub fn function() {}
pub const VALUE: u8 = 1;
pub static STATIC: u8 = 1;
pub mod nested { pub mod deeper { pub struct Nested; } }
pub fn first() {}
pub fn second() {}
"#,
    )
    .unwrap();
    fs::write(
        root.join("api/src/lib.rs"),
        r#"
pub mod a {
    #[doc(inline)] pub use inner::*;
    #[doc(inline)] pub use inner::function as Renamed;
}
pub mod b {
    #[doc(inline)] pub use inner::*;
    #[doc(inline)] pub use inner::function as Renamed;
}
#[doc(no_inline)] pub use a::*;
#[doc(no_inline)] pub use b::*;
pub mod first { #[doc(inline)] pub use inner::first as Conflict; }
pub mod second { #[doc(inline)] pub use inner::second as Conflict; }
#[doc(no_inline)] pub use first::*;
#[doc(no_inline)] pub use second::*;
pub mod local_one {
    mod inner { pub fn function() {} }
    #[doc(inline)] pub use inner::function as LocalConflict;
}
pub mod local_two {
    mod inner { pub fn function() {} }
    #[doc(inline)] pub use inner::function as LocalConflict;
}
#[doc(no_inline)] pub use local_one::*;
#[doc(no_inline)] pub use local_two::*;
pub mod imported_one {
    mod hidden { pub mod scope { pub fn function() {} } }
    use hidden::scope as inner;
    #[doc(inline)] pub use inner::function as ImportedConflict;
}
pub mod imported_two {
    mod hidden { pub mod scope { pub fn function() {} } }
    use hidden::scope as inner;
    #[doc(inline)] pub use inner::function as ImportedConflict;
}
#[doc(no_inline)] pub use imported_one::*;
#[doc(no_inline)] pub use imported_two::*;
pub mod generated_one {
    macro_rules! local { () => { mod inner { pub fn function() {} } }; }
    local!();
    #[doc(inline)] pub use self::inner::function as GeneratedConflict;
    #[doc(inline)] pub use ::inner::function as GeneratedAbsolute;
}
pub mod generated_two {
    macro_rules! local { () => { mod inner { pub fn function() {} } }; }
    local!();
    #[doc(inline)] pub use self::inner::function as GeneratedConflict;
    #[doc(inline)] pub use ::inner::function as GeneratedAbsolute;
}
#[doc(no_inline)] pub use generated_one::*;
#[doc(no_inline)] pub use generated_two::*;
pub mod absolute_one {
    pub mod inner { pub fn function() {} }
    #[doc(inline)] pub use ::inner::function as Absolute;
    #[doc(inline)] pub use ::inner::function as AbsoluteConflict;
}
pub mod absolute_two {
    pub mod inner { pub fn function() {} }
    #[doc(inline)] pub use ::inner::function as Absolute;
    #[doc(inline)] pub use crate::absolute_one::inner::function as AbsoluteConflict;
}
#[doc(no_inline)] pub use absolute_one::*;
#[doc(no_inline)] pub use absolute_two::*;
"#,
    )
    .unwrap();
    let import = "use alias::{Record, Choice::{self, One}, Trait, Alias, function, Renamed, Absolute, GeneratedAbsolute, VALUE, STATIC, nested::{self, deeper::Nested}};";
    fs::write(root.join("app/src/lib.rs"), import).unwrap();
    success(
        Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap(),
    );
    let report = success(query(root, import, &[]));
    assert_eq!(paths(&report, "source: ").len(), 13, "{report}");
    for name in [
        "Conflict",
        "LocalConflict",
        "ImportedConflict",
        "GeneratedConflict",
        "AbsoluteConflict",
    ] {
        fs::write(root.join("app/src/lib.rs"), format!("use alias::{name};")).unwrap();
        let checked = Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(!checked.status.success(), "{name}: {checked:?}");
        assert!(
            String::from_utf8_lossy(&checked.stderr).contains("ambiguous"),
            "{name}: {checked:?}"
        );
        let error = failure(query(root, &format!("use alias::{name};"), &[]));
        assert!(error.contains("ambiguous"), "{name}: {error}");
    }
}

#[test]
fn inactive_and_macro_reexports_do_not_merge_distinct_modules() {
    let dir = project();
    let root = dir.path();
    fs::write(
        root.join("inner/src/lib.rs"),
        r#"
pub mod one { pub mod M { pub struct Item { pub first: u8 } } }
pub mod two { pub mod M { pub struct Item { pub second: u16 } } }
#[macro_export] macro_rules! make { () => {} }
pub fn function() {}
pub mod separate;
"#,
    )
    .unwrap();
    fs::write(root.join("inner/src/separate.rs"), "pub fn function() {}").unwrap();
    for attribute in ["#[cfg(any())]", "#[doc(inline)]", "#[doc(no_inline)]"] {
        fs::write(
            root.join("api/src/lib.rs"),
            format!(
                r#"
pub mod a {{
    #[doc(inline)] pub use inner::one::*;
    {attribute} pub use inner::make as M;
    #[doc(inline)] pub use inner::function as Alias;
    #[doc(inline)] pub use inner::separate::function as Separate;
}}
pub mod b {{
    #[doc(inline)] pub use inner::two::*;
    {attribute} pub use inner::make as M;
    #[doc(inline)] pub use inner::function as Alias;
    #[doc(inline)] pub use inner::separate::function as Separate;
}}
#[doc(no_inline)] pub use a::*;
#[doc(no_inline)] pub use b::*;
"#
            ),
        )
        .unwrap();
        fs::write(root.join("app/src/lib.rs"), "use alias::M::Item;").unwrap();
        let checked = Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(!checked.status.success(), "{attribute}: {checked:?}");
        assert!(
            String::from_utf8_lossy(&checked.stderr).contains("ambiguous"),
            "{checked:?}"
        );
        let error = failure(query(root, "use alias::M::Item;", &[]));
        assert!(error.contains("ambiguous"), "{attribute}: {error}");
        let report = success(query(root, "use alias::{Alias, Separate};", &[]));
        assert!(report.contains("pub fn Alias"), "{report}");
        assert!(report.contains("pub fn Separate"), "{report}");
    }
}

#[test]
fn conditional_module_declarations_do_not_unify_distinct_active_definitions() {
    let dir = project();
    let root = dir.path();
    for (inactive_file, active_file) in [(false, false), (false, true), (true, false), (true, true)]
    {
        let mut source = "pub mod common { pub fn item(_: u8) {} }\n".to_string();
        for (name, argument) in [("a", "u16"), ("b", "u32")] {
            let inactive = "pub use crate::common::item;";
            let active = format!("pub fn item(_: {argument}) {{}}");
            fs::write(root.join(format!("api/src/inactive_{name}.rs")), inactive).unwrap();
            fs::write(root.join(format!("api/src/{name}.rs")), &active).unwrap();
            if inactive_file {
                source.push_str(&format!(
                    "#[cfg(any())] #[path = \"inactive_{name}.rs\"] pub mod {name};\n"
                ));
            } else {
                source.push_str(&format!("#[cfg(any())] pub mod {name} {{ {inactive} }}\n"));
            }
            if active_file {
                source.push_str(&format!("#[cfg(all())] pub mod {name};\n"));
            } else {
                source.push_str(&format!("#[cfg(all())] pub mod {name} {{ {active} }}\n"));
            }
        }
        source.push_str("#[doc(no_inline)] pub use a::*;\n#[doc(no_inline)] pub use b::*;");
        fs::write(root.join("api/src/lib.rs"), source).unwrap();
        fs::write(root.join("app/src/lib.rs"), "use alias::item;").unwrap();
        let checked = Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(!checked.status.success(), "{checked:?}");
        assert!(
            String::from_utf8_lossy(&checked.stderr).contains("ambiguous"),
            "{checked:?}"
        );
        let error = failure(query(root, "use alias::item;", &[]));
        assert!(
            error.contains("ambiguous"),
            "{inactive_file}/{active_file}: {error}"
        );
    }
    fs::write(
        root.join("api/src/lib.rs"),
        r#"
#![allow(unexpected_cfgs)]
pub mod common { pub fn item(_: u8) {} }
#[cfg(excra_disabled)] pub mod a { pub use crate::common::item; }
#[cfg(excra_disabled)] pub mod b { pub use crate::common::item; }
macro_rules! define {
    () => { pub mod a { pub fn item(_: u16) {} } pub mod b { pub fn item(_: u32) {} } };
}
define!();
#[doc(no_inline)] pub use a::*;
#[doc(no_inline)] pub use b::*;
"#,
    )
    .unwrap();
    let error = failure(query(root, "use alias::item;", &[]));
    assert!(error.contains("ambiguous"), "{error}");
}

#[test]
fn inlined_module_aliases_follow_out_of_line_and_nested_sources() {
    let dir = project();
    let root = dir.path();
    fs::write(
        root.join("api/src/lib.rs"),
        "pub mod a;\npub mod b;\n#[doc(no_inline)] pub use a::*;\n#[doc(no_inline)] pub use b::*;",
    )
    .unwrap();
    fs::write(
        root.join("api/src/a.rs"),
        "pub mod separate;\npub mod nested { pub mod deeper { pub struct Item; } }",
    )
    .unwrap();
    fs::create_dir(root.join("api/src/a")).unwrap();
    fs::write(
        root.join("api/src/a/separate.rs"),
        "pub struct Item;\npub fn function() {}",
    )
    .unwrap();
    fs::write(
        root.join("api/src/b.rs"),
        "#[doc(inline)] pub use crate::a::*;",
    )
    .unwrap();
    let import = "use alias::{separate::{self, Item, function}, nested::{self, deeper::{self, Item as Nested}}};";
    fs::write(root.join("app/src/lib.rs"), import).unwrap();
    success(
        Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap(),
    );
    let report = success(query(root, import, &[]));
    assert_eq!(paths(&report, "source: ").len(), 6, "{report}");
}

#[test]
fn inlined_module_aliases_keep_glob_source_evidence_and_shadowing() {
    for out_of_line in [false, true] {
        let dir = project();
        let root = dir.path();
        let original = r#"
pub mod origin { pub struct Item { pub correct: u64 } pub struct Hidden; pub fn foreign() {} }
#[doc(no_inline)] pub use origin::*;
#[doc(hidden)] pub struct Actual;
pub use Actual as Hidden;
unsafe extern "C" { #[doc(hidden)] pub fn foreign(value: u64); }
pub use super::sibling::Unit as Explicit;
pub mod deep {
    pub mod origin { pub struct Deep; }
    #[doc(no_inline)] pub use origin::*;
}
"#;
        let declaration = if out_of_line {
            fs::create_dir_all(root.join("api/src/outer")).unwrap();
            fs::write(root.join("api/src/outer/original.rs"), original).unwrap();
            "pub mod original;".to_string()
        } else {
            format!("pub mod original {{ {original} }}")
        };
        fs::write(
            root.join("api/src/lib.rs"),
            format!(
                "pub mod outer {{ pub mod sibling {{ pub struct Unit; }} {declaration} }}\n\
                 #[cfg(any())] pub use nonexistent::Module as renamed;\n\
                 #[cfg(all())] #[doc(inline)] pub use outer::original as renamed;\n\
                 #[doc(inline)] pub use renamed as chained;"
            ),
        )
        .unwrap();
        fs::write(
            root.join("app/src/lib.rs"),
            "use alias::{outer::original::Item, renamed::Item as Alias};\n\
             pub fn item(_: Item) -> Alias { Alias { correct: 1 } }\n\
             pub fn explicit() -> alias::renamed::Explicit { alias::renamed::Explicit }",
        )
        .unwrap();
        success(
            Command::new("cargo")
                .args(["check", "--offline", "--locked", "--package", "app"])
                .current_dir(root)
                .output()
                .unwrap(),
        );
        let mut pages = Vec::new();
        for prefix in ["alias::outer::original", "alias::renamed", "alias::chained"] {
            let report = success(query(root, &format!("use {prefix}::Item;"), &[]));
            assert!(report.contains("pub correct: u64"), "{report}");
            pages.push(paths(&report, "source: ")[0].clone());
            let report = success(query(root, &format!("use {prefix}::Explicit;"), &[]));
            assert!(report.contains("pub struct"), "{report}");
            let report = success(query(root, &format!("use {prefix}::deep::Deep;"), &[]));
            assert!(report.contains("pub struct Deep"), "{report}");
            for name in ["Hidden", "foreign"] {
                let error = failure(query(root, &format!("use {prefix}::{name};"), &[]));
                assert!(error.contains("no available rustdoc HTML page"), "{error}");
            }
        }
        assert!(pages.iter().all(|page| page == &pages[0]), "{pages:?}");
    }
}

#[test]
fn macro_definitions_with_shared_source_spans_remain_ambiguous() {
    let dir = project();
    let root = dir.path();
    member(
        root,
        "macros",
        "[package]\nname = \"macros\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\nproc-macro = true\n",
        r#"
use proc_macro::{Delimiter, Group, TokenStream, TokenTree};
#[proc_macro_attribute]
pub fn duplicate(_: TokenStream, item: TokenStream) -> TokenStream {
    let mut out = TokenStream::new();
    for name in ["a", "b"] {
        out.extend(format!("pub mod {name}").parse::<TokenStream>().unwrap());
        out.extend([TokenTree::Group(Group::new(Delimiter::Brace, item.clone()))]);
    }
    out
}
"#,
    );
    let manifest = root.join("api/Cargo.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest).unwrap().replace(
            "[dependencies]",
            "[dependencies]\nmacros = { path = \"../macros\" }",
        ),
    )
    .unwrap();
    for source in [
        "#[macros::duplicate]\npub fn conflict() {}\n#[doc(no_inline)] pub use a::*;\n#[doc(no_inline)] pub use b::*;",
        "pub mod a { include!(\"shared.rs\"); }\npub mod b { include!(\"shared.rs\"); }\n#[doc(no_inline)] pub use a::*;\n#[doc(no_inline)] pub use b::*;",
    ] {
        fs::write(root.join("api/src/lib.rs"), source).unwrap();
        fs::write(root.join("api/src/shared.rs"), "pub fn conflict() {}").unwrap();
        fs::write(root.join("app/src/lib.rs"), "use alias::conflict;").unwrap();
        let checked = Command::new("cargo")
            .args(["check", "--offline", "--package", "app"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(!checked.status.success(), "{checked:?}");
        assert!(
            String::from_utf8_lossy(&checked.stderr).contains("ambiguous"),
            "{checked:?}"
        );
        let error = failure(query(root, "use alias::conflict;", &[]));
        assert!(error.contains("ambiguous"), "{error}");
    }
}

#[test]
fn glob_shadowing_tracks_type_and_value_constructor_namespaces() {
    let dir = project();
    fs::write(
        dir.path().join("api/src/lib.rs"),
        r#"
pub mod values {
    pub const Unit: u8 = 1;
    pub const Tuple: u8 = 1;
    pub const NamedUnit: u8 = 1;
    pub fn Record() {}
    pub struct Combined(pub u8);
    pub fn UnitVariant() {}
    pub fn TupleVariant() {}
    pub fn RecordVariant() {}
    pub fn EmptyRecordVariant() {}
    pub fn AttributedRecord() {}
    pub fn DeprecatedRecord() {}
    pub fn r#type() {}
}
#[doc(no_inline)] pub use values::*;
pub struct Unit;
pub struct Tuple(pub u8);
pub mod original { pub struct Unit; }
#[doc(no_inline)] pub use original::Unit as NamedUnit;
pub struct Record { pub value: u8 }
pub struct Combined { pub value: u8 }
pub enum E {
    UnitVariant, TupleVariant(u8), RecordVariant { value: u8 }, EmptyRecordVariant {},
    #[non_exhaustive] AttributedRecord { value: u8 },
    #[deprecated] DeprecatedRecord { value: u8 },
    #[non_exhaustive] r#type { value: u8 },
}
#[doc(no_inline)] pub use E::{UnitVariant, TupleVariant, RecordVariant, EmptyRecordVariant, AttributedRecord, DeprecatedRecord, r#type};
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("app/src/lib.rs"),
        "use alias::{Unit, Tuple, NamedUnit, Record, Combined, UnitVariant, TupleVariant, RecordVariant, EmptyRecordVariant, AttributedRecord, DeprecatedRecord, r#type};\nfn probe() { let _: Unit = Unit; let _: Tuple = Tuple(0); let _: NamedUnit = NamedUnit; Record(); Combined(0); RecordVariant(); EmptyRecordVariant(); AttributedRecord(); DeprecatedRecord(); r#type(); }",
    )
    .unwrap();
    let checked = Command::new("cargo")
        .args(["check", "--offline", "--locked", "--package", "app"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(checked.status.success(), "{checked:?}");
    let report = success(query(
        dir.path(),
        "use alias::{Unit, Tuple, NamedUnit, UnitVariant, TupleVariant};",
        &[],
    ));
    assert!(report.contains("pub struct Unit"), "{report}");
    assert!(report.contains("pub struct Tuple"), "{report}");
    assert!(report.contains("pub enum E"), "{report}");
    for name in [
        "Record",
        "Combined",
        "RecordVariant",
        "EmptyRecordVariant",
        "AttributedRecord",
        "DeprecatedRecord",
        "r#type",
    ] {
        let error = failure(query(dir.path(), &format!("use alias::{name};"), &[]));
        assert!(error.contains("ambiguous"), "{name}: {error}");
    }
}

#[test]
fn variant_reexports_keep_their_identity_during_path_traversal() {
    let dir = project();
    fs::write(
        dir.path().join("api/src/lib.rs"),
        r#"
pub enum E { First, Other }
#[doc(no_inline)] pub use E::First as Alias;
#[doc(no_inline)] pub use E::First;
#[doc(no_inline)] pub use E as EnumAlias;
pub mod variants { #[doc(no_inline)] pub use super::E::*; }
#[doc(no_inline)] pub use variants::*;
pub mod one { #[doc(no_inline)] pub use super::E::First as Conflict; }
pub mod two { #[doc(no_inline)] pub use super::E::Other as Conflict; }
#[doc(no_inline)] pub use one::*;
#[doc(no_inline)] pub use two::*;
"#,
    )
    .unwrap();
    let valid = "use alias::{Alias, First, E::Other, EnumAlias::Other as Renamed};";
    fs::write(dir.path().join("app/src/lib.rs"), valid).unwrap();
    let check = || {
        Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(dir.path())
            .output()
            .unwrap()
    };
    let checked = check();
    assert!(checked.status.success(), "{checked:?}");
    let report = success(query(dir.path(), valid, &[]));
    assert!(report.contains("pub enum E"), "{report}");
    for name in ["Alias", "First", "variants::First"] {
        let import = format!("use alias::{name}::Other;");
        fs::write(dir.path().join("app/src/lib.rs"), &import).unwrap();
        let checked = check();
        assert!(!checked.status.success(), "{import}: {checked:?}");
        assert!(String::from_utf8_lossy(&checked.stderr).contains("E0432"));
        assert!(failure(query(dir.path(), &import, &[])).contains("has no rustdoc HTML page"));
    }
    assert!(failure(query(dir.path(), "use alias::Conflict;", &[])).contains("ambiguous"));
}

#[test]
fn non_inlined_reexports_participate_in_namespace_and_ambiguity_checks() {
    let dir = project();
    fs::write(
        dir.path().join("api/src/lib.rs"),
        r#"
pub mod original { pub enum Mixed { Variant } }
#[doc(no_inline)] pub use original::Mixed;
#[macro_export] macro_rules! Mixed { () => { 1 }; }
#[doc(no_inline)] pub use Mixed as OnlyMacroAndType;
#[macro_export] macro_rules! make { () => { 1 }; }
#[doc(no_inline)] pub use make as OnlyMacro;
"#,
    )
    .unwrap();
    fs::write(
        dir.path().join("app/src/lib.rs"),
        "use alias::Mixed::{self, Variant};\nuse alias::OnlyMacroAndType::{self as Renamed};",
    )
    .unwrap();
    let checked = Command::new("cargo")
        .args(["check", "--offline", "--locked", "--package", "app"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(checked.status.success(), "{checked:?}");
    for name in ["Mixed", "OnlyMacroAndType"] {
        assert!(
            failure(query(dir.path(), &format!("use alias::{name};"), &[])).contains("ambiguous")
        );
        let report = success(query(
            dir.path(),
            &format!("use alias::{name}::{{self}};"),
            &[],
        ));
        assert!(report.contains("pub enum Mixed"));
    }
    assert!(success(query(dir.path(), "use alias::OnlyMacro;", &[])).contains("macro_rules! make"));
    assert!(failure(query(dir.path(), "use alias::OnlyMacro::{self};", &[])).contains("not found"));
}

#[test]
fn self_imports_require_module_enum_or_trait_parents() {
    let dir = project();
    fs::write(
        dir.path().join("api/src/lib.rs"),
        r#"
pub mod module {}
pub enum Enum { Variant }
pub trait Trait {}
pub struct Item;
pub union Bits { pub value: u8 }
pub type Alias = Item;
pub mod globbed { pub mod Item {} }
#[doc(no_inline)] pub use globbed::*;
#[doc(no_inline)] pub use module as ModuleAlias;
#[doc(no_inline)] pub use Enum as EnumAlias;
#[doc(no_inline)] pub use Trait as TraitAlias;
#[doc(no_inline)] pub use Item as ItemAlias;
#[doc(no_inline)] pub use Bits as BitsAlias;
#[doc(no_inline)] pub use Alias as TypeAlias;
#[doc(no_inline)] pub use Enum::Variant as VariantAlias;
#[doc(no_inline)] pub use Enum::*;
"#,
    )
    .unwrap();
    let valid = "use alias::{module::{self}, Enum::self as Renamed, Trait::{self}, ModuleAlias::{self}, EnumAlias::{self}, TraitAlias::{self}};";
    fs::write(dir.path().join("app/src/lib.rs"), valid).unwrap();
    let check = || {
        Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "app"])
            .current_dir(dir.path())
            .output()
            .unwrap()
    };
    let checked = check();
    assert!(checked.status.success(), "{checked:?}");
    let report = success(query(dir.path(), valid, &[]));
    for text in ["# Module module", "pub enum Enum", "pub trait Trait"] {
        assert!(report.contains(text), "missing {text}: {report}");
    }
    for name in [
        "Item",
        "Bits",
        "Alias",
        "ItemAlias",
        "BitsAlias",
        "TypeAlias",
        "Enum::Variant",
        "Variant",
        "VariantAlias",
    ] {
        let import = format!("use alias::{name}::{{self}};");
        fs::write(dir.path().join("app/src/lib.rs"), &import).unwrap();
        let checked = check();
        assert!(!checked.status.success(), "{import}: {checked:?}");
        assert!(String::from_utf8_lossy(&checked.stderr).contains("E0432"));
        assert!(failure(query(dir.path(), &import, &[])).contains("self import parent"));
    }
    assert!(
        success(query(
            dir.path(),
            "use alias::{Item, Bits, Alias, Enum::Variant};",
            &[],
        ))
        .contains("pub struct Item")
    );
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
    let (active, inactive) = if cfg!(windows) {
        ("windows", "unix")
    } else {
        ("unix", "windows")
    };
    member(
        root,
        "app",
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nalias = { package = \"api\", path = \"../api\", features = [\"target\"] }\nmacros = { path = \"../macros\" }\n",
        "pub use alias::Record;",
    );
    member(
        root,
        "macros",
        &format!(
            "[package]\nname = \"macros\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\nproc-macro = true\n[dependencies]\nalias = {{ package = \"api\", path = \"../api\", features = [\"host\"] }}\n[target.'cfg({active})'.dependencies]\nhost_api = {{ package = \"build-api\", path = \"../build-api\", features = [\"enabled\"] }}\n[target.'cfg({inactive})'.dependencies]\nopposite_api = {{ package = \"dev-api\", path = \"../dev-api\", features = [\"enabled\"] }}\n"
        ),
        "use host_api::Enabled;",
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
        let checked = Command::new("cargo")
            .args(["check", "--offline", "--locked", "--package", "macros"])
            .args(&flags)
            .current_dir(root)
            .output()
            .unwrap();
        assert!(checked.status.success(), "{:?}", checked);
        let tree = Command::new("cargo")
            .args(["tree", "--offline", "--locked", "--package", "macros"])
            .args(&flags)
            .current_dir(root)
            .output()
            .unwrap();
        let tree = success(tree);
        assert!(tree.contains("build-api"), "{tree}");
        assert!(!tree.contains("dev-api"), "{tree}");
        let query_macros = |import| {
            Command::new(env!("CARGO_BIN_EXE_excra"))
                .args([import, "--root"])
                .arg(root)
                .args(["--package", "macros"])
                .args(&flags)
                .env("CARGO_NET_OFFLINE", "true")
                .output()
                .unwrap()
        };
        let report = success(query_macros("use alias::Record;"));
        assert!(report.contains("dependency: normal\n"), "{report}");
        assert!(report.contains("fn host_only("), "{report}");
        assert!(!report.contains("fn target_only("), "{report}");
        let report = success(query_macros("use host_api::Enabled;"));
        assert!(
            report.contains(&format!("dependency: normal (cfg({active}))\n")),
            "{report}"
        );
        assert!(report.contains("pub struct Enabled"), "{report}");
        assert!(
            failure(query_macros("use opposite_api::Enabled;")).contains("not a direct dependency")
        );
    }
}

#[test]
fn proc_macro_dev_dependencies_keep_target_units_separate_from_normal_units() {
    let dir = project();
    let root = dir.path();
    let version = Command::new("rustc").arg("-vV").output().unwrap();
    let version = String::from_utf8(version.stdout).unwrap();
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    member(
        root,
        "api",
        "[package]\nname = \"api\"\nversion = \"1.2.3\"\nedition = \"2024\"\n[dependencies]\ninner = { path = \"../inner\" }\n[features]\nhost = [\"inner/host\"]\ndev = [\"inner/dev\"]\n",
        "#[doc(inline)] pub use inner::Item;",
    );
    member(
        root,
        "inner",
        "[package]\nname = \"inner\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[features]\nhost = []\ndev = []\n",
        "#![allow(unexpected_cfgs)]\npub struct Item;\nimpl Item {\n#[cfg(target_unit)] pub fn target_only() {}\n#[cfg(feature = \"host\")] pub fn host_feature() {}\n#[cfg(feature = \"dev\")] pub fn dev_feature() {}\n}",
    );
    fs::create_dir_all(root.join("app/tests")).unwrap();
    fs::write(
        root.join("app/tests/target.rs"),
        "#[test] fn target() { alias::Item::target_only(); }",
    )
    .unwrap();
    for normal in [false, true] {
        let normal_dependency = if normal {
            "[dependencies]\nalias = { package = \"api\", path = \"../api\", features = [\"host\"] }\n"
        } else {
            ""
        };
        member(
            root,
            "app",
            &format!(
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\nproc-macro = true\n{normal_dependency}[target.'{host}'.dev-dependencies]\nalias = {{ package = \"api\", path = \"../api\", features = [\"dev\"] }}\n"
            ),
            "",
        );
        success(
            Command::new("cargo")
                .args([
                    "check",
                    "--offline",
                    "--package",
                    "app",
                    "--tests",
                    "--target",
                    host,
                ])
                .env("RUSTFLAGS", "--cfg target_unit")
                .current_dir(root)
                .output()
                .unwrap(),
        );
        let report = success(
            query_command(
                root,
                "use alias::Item;",
                &["--include-dev", "--target", host],
            )
            .env("RUSTFLAGS", "--cfg target_unit")
            .output()
            .unwrap(),
        );
        let reports = report.split("crate: api\n").skip(1).collect::<Vec<_>>();
        assert_eq!(reports.len(), if normal { 2 } else { 1 }, "{report}");
        let dev = reports
            .iter()
            .find(|text| text.contains("dependency: dev ("))
            .unwrap();
        assert!(dev.contains("fn target_only("), "{report}");
        assert!(dev.contains("fn dev_feature("), "{report}");
        assert_eq!(dev.contains("fn host_feature("), normal, "{report}");
        if normal {
            let normal = reports
                .iter()
                .find(|text| text.contains("dependency: normal\n"))
                .unwrap();
            assert!(normal.contains("fn host_feature("), "{report}");
            assert!(!normal.contains("fn target_only("), "{report}");
            assert!(normal.contains("fn dev_feature("), "{report}");
        }
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
