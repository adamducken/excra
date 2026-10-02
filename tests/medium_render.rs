use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn write_member(workspace: &TempDir, name: &str, manifest: &str, source: &str) {
    fs::create_dir_all(workspace.path().join(name).join("src")).unwrap();
    fs::write(workspace.path().join(name).join("Cargo.toml"), manifest).unwrap();
    fs::write(workspace.path().join(name).join("src/lib.rs"), source).unwrap();
}

#[test]
fn type_reports_retain_inherent_constants_constraints_and_nested_metadata() {
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
        r#"pub struct Limits<T>(pub T);

impl<T> Limits<T>
where
    T: Copy,
{
    /// Largest supported value.
    pub const MAX: usize = 8;

    #[doc(hidden)]
    pub const HIDDEN: usize = 9;

    #[doc(hidden)]
    const PRIVATE_HIDDEN: usize = 10;

    /// Inspect the old limit.
    #[deprecated(since = "0.2.0", note = "use MAX")]
    #[must_use = "inspect the limit"]
    pub fn old(&self) -> usize { Self::MAX }

    #[doc(hidden)]
    pub fn hidden_callable(&self) -> usize { Self::HIDDEN }

    #[doc(hidden)]
    fn private_hidden(&self) -> usize { Self::PRIVATE_HIDDEN }

    /// Reads without checks.
    ///
    /// # Safety
    /// The caller must uphold the limit invariant.
    pub unsafe fn unchecked(&self) -> usize { Self::MAX }
}

pub enum Choice {
    /// Cannot be exhaustively constructed.
    #[non_exhaustive]
    Record {
        /// Stored byte.
        #[deprecated(note = "use replacement")]
        value: u8,
    },
    Tuple(
        /// Tuple field documentation.
        #[deprecated(note = "use a new tuple field")]
        u16,
    ),
}

pub use Choice::Record as ReexportedRecord;

pub trait Contract {
    /// Check the contract.
    #[must_use]
    fn check(&self) -> bool;

    #[doc(hidden)]
    fn hidden_required(&self);

    /// Stable identifier.
    #[deprecated(note = "use NEW_ID")]
    const ID: u8 = 4;
}
"#,
    );
    let lock = Command::new("cargo")
        .args([
            "generate-lockfile",
            "--offline",
            "--manifest-path",
            workspace.path().join("Cargo.toml").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );

    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::{Limits, Choice, Choice::Record, Choice::Tuple, ReexportedRecord, Contract};",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(stdout.contains(
        "  impl<T> Limits<T> where T: Copy { pub fn old(self: &Self) -> usize }\n    deprecation:\n      since: 0.2.0\n      note: use MAX\n    attributes:\n      #[must_use = \"inspect the limit\"]\n    docs:\n      Inspect the old limit.\n"
    ), "{stdout}");
    assert!(stdout.contains(
        "  impl<T> Limits<T> where T: Copy { pub unsafe fn unchecked(self: &Self) -> usize }\n    docs:\n      Reads without checks.\n      \n      # Safety\n      The caller must uphold the limit invariant.\n"
    ), "{stdout}");
    assert!(stdout.contains(
        "  impl<T> Limits<T> where T: Copy { pub const MAX: usize = 8; }\n    docs:\n      Largest supported value.\n"
    ), "{stdout}");
    assert!(
        stdout.contains("pub fn hidden_callable(self: &Self) -> usize"),
        "{stdout}"
    );
    assert!(stdout.contains("pub const HIDDEN: usize = 9;"), "{stdout}");
    assert!(!stdout.contains("private_hidden"), "{stdout}");
    assert!(!stdout.contains("PRIVATE_HIDDEN"), "{stdout}");

    assert!(stdout.contains(
        "details:\n  Record { value: u8 }\n    attributes:\n      #[non_exhaustive]\n    docs:\n      Cannot be exhaustively constructed.\n    members:\n      value: u8\n        deprecation:\n          note: use replacement\n        docs:\n          Stored byte.\n"
    ), "{stdout}");
    assert!(stdout.contains("item: variant Record\n"), "{stdout}");
    assert!(stdout.contains(
        "definition: Record { value: u8 }\nattributes:\n  #[non_exhaustive]\ndetails:\n  value: u8\n    deprecation:\n      note: use replacement\n    docs:\n      Stored byte.\n"
    ), "{stdout}");
    assert!(stdout.contains(
        "definition: Tuple(u16)\ndetails:\n  #0: u16\n    deprecation:\n      note: use a new tuple field\n    docs:\n      Tuple field documentation.\n"
    ), "{stdout}");
    let reexport = stdout
        .split("import: use dep::ReexportedRecord;\n")
        .nth(1)
        .expect(&stdout);
    assert!(
        reexport.contains("details:\n  value: u8\n    deprecation:\n      note: use replacement\n    docs:\n      Stored byte.\n"),
        "{reexport}"
    );
    assert!(stdout.contains(
        "details:\n  fn check(self: &Self) -> bool;\n    attributes:\n      #[must_use]\n    docs:\n      Check the contract.\n  fn hidden_required(self: &Self);\n  const ID: u8 = 4;\n    deprecation:\n      note: use NEW_ID\n    docs:\n      Stable identifier.\n"
    ), "{stdout}");
    assert!(
        stdout.contains("  fn hidden_required(self: &Self);\n"),
        "{stdout}"
    );
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[test]
fn target_feature_requirements_match_the_selected_artifact() {
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
        r#"
#[target_feature(enable = "avx2")]
pub fn fast() {}
#[cfg_attr(not(doc), target_feature(enable = "avx2"))]
pub fn normal() {}
#[cfg_attr(doc, target_feature(enable = "avx2"))]
pub fn doc_only() {}
#[target_feature(enable = "sse2")]
#[target_feature(enable = "ssse3")]
#[cfg_attr(doc, target_feature(enable = "avx2,sse4.1"))]
#[cfg_attr(not(doc), cfg_attr(any(target_arch = "x86", target_arch = "x86_64"), target_feature(enable = "sse4.2")))]
pub fn mixed() {}
#[target_feature(enable = "avx2")]
#[cfg_attr(doc, target_feature(enable = "avx2"))]
pub fn duplicate() {}
pub struct Engine;
impl Engine {
    #[target_feature(enable = "avx2")]
    pub fn fast(&self) {}
    #[cfg_attr(not(doc), target_feature(enable = "avx2"))]
    pub fn normal(&self) {}
    #[cfg_attr(doc, target_feature(enable = "avx2"))]
    pub fn doc_only(&self) {}
}
"#,
    );
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
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::{fast, normal, doc_only, mixed, duplicate, Engine};",
            "--root",
        ])
        .arg(workspace.path())
        .args(["--package", "app"])
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    for name in ["fast", "normal", "duplicate"] {
        let report = stdout
            .split(&format!("item: fn {name}\n"))
            .nth(1)
            .unwrap()
            .split("crate: dep ")
            .next()
            .unwrap();
        assert!(
            report.contains("#[target_feature(enable = \"avx2\")]"),
            "{name}: {report}"
        );
    }
    let doc_only = stdout
        .split("item: fn doc_only\n")
        .nth(1)
        .unwrap()
        .split("crate: dep ")
        .next()
        .unwrap();
    assert!(!doc_only.contains("#[target_feature"), "{doc_only}");
    let mixed = stdout
        .split("item: fn mixed\n")
        .nth(1)
        .unwrap()
        .split("crate: dep ")
        .next()
        .unwrap();
    assert!(
        mixed.contains("#[target_feature(enable = \"sse2,ssse3,sse4.2\")]"),
        "{mixed}"
    );
    let methods = stdout.split("item: struct Engine\n").nth(1).unwrap();
    for name in ["fast", "normal"] {
        assert!(methods.contains(&format!("pub fn {name}(self: &Self) }}\n    attributes:\n      #[target_feature(enable = \"avx2\")]")), "{methods}");
    }
    assert!(
        !methods
            .split("pub fn doc_only")
            .nth(1)
            .unwrap()
            .split("\n  impl ")
            .next()
            .unwrap()
            .contains("#[target_feature"),
        "{methods}"
    );

    #[derive(serde::Deserialize)]
    struct Probe {
        compiler: Vec<std::ffi::OsString>,
        directory: std::path::PathBuf,
        arguments: Vec<std::ffi::OsString>,
    }
    let json_path = std::path::Path::new(
        stdout
            .lines()
            .find_map(|line| line.strip_prefix("source: "))
            .unwrap(),
    );
    let probe: Probe =
        serde_json::from_slice(&fs::read(json_path.with_extension("probe")).unwrap()).unwrap();
    let source = workspace.path().join("caller.rs");
    for (body, succeeds) in [
        ("pub fn caller() { excra_dependency::fast(); }", false),
        ("pub fn caller() { excra_dependency::normal(); }", false),
        (
            "pub fn caller() { excra_dependency::Engine.fast(); }",
            false,
        ),
        (
            "pub fn caller() { excra_dependency::Engine.normal(); }",
            false,
        ),
        ("pub fn caller() { excra_dependency::duplicate(); }", false),
        (
            "pub fn caller() { excra_dependency::doc_only(); excra_dependency::Engine.doc_only(); }",
            true,
        ),
        (
            "#[target_feature(enable = \"avx2\")] pub fn caller() { excra_dependency::fast(); excra_dependency::normal(); excra_dependency::Engine.fast(); excra_dependency::Engine.normal(); excra_dependency::duplicate(); }",
            true,
        ),
        (
            "#[target_feature(enable = \"sse4.2,ssse3\")] pub fn caller() { excra_dependency::mixed(); }",
            true,
        ),
        (
            "pub fn caller() { unsafe { excra_dependency::fast(); } }",
            true,
        ),
    ] {
        fs::write(&source, body).unwrap();
        let output = Command::new(&probe.compiler[0])
            .args(&probe.compiler[1..])
            .current_dir(&probe.directory)
            .args(&probe.arguments)
            .args([
                "--edition=2024",
                "--crate-type=lib",
                "--emit=metadata",
                "-Ctarget-feature=-avx2",
            ])
            .arg(&source)
            .arg("-o")
            .arg(workspace.path().join("caller.rmeta"))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.success(), succeeds, "{body}: {stderr}");
        if !succeeds {
            assert!(stderr.contains("E0133"), "{body}: {stderr}");
        }
    }
}
