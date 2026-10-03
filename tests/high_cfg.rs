use std::{fs, process::Command};
use tempfile::TempDir;

#[test]
fn non_doc_only_api_reports_incomplete_extraction() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    for (name, dependencies, source) in [
        ("app", "[dependencies]\ndep = { path = \"../dep\" }\n", ""),
        (
            "dep",
            "",
            "pub struct Packet { #[cfg(not(doc))] pub byte: u8 }\n#[cfg(doc)] pub struct Choice { pub docs: u8 }\n#[cfg(not(doc))] pub struct Choice { pub actual: u8 }\npub struct Unrelated;\n",
        ),
    ] {
        let path = workspace.path().join(name);
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(
            path.join("Cargo.toml"),
            format!("[package]\nname = {name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\n{dependencies}"),
        )
        .unwrap();
        fs::write(path.join("src/lib.rs"), source).unwrap();
    }
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

    for name in ["Packet", "Choice"] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                format!("use dep::{name};"),
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
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("non-doc API extraction is incomplete"),
            "{name}: {stderr}"
        );
    }
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Unrelated;",
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
    assert!(String::from_utf8_lossy(&output.stdout).contains("definition: pub struct Unrelated;"));

    fs::write(workspace.path().join("dep/src/lib.rs"), "pub mod nested;\n").unwrap();
    fs::write(
        workspace.path().join("dep/src/nested.rs"),
        "pub struct Packet { #[cfg(not(doc))] pub byte: u8 }\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::nested::Packet;",
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
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("non-doc API extraction is incomplete"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::write(
        workspace.path().join("dep/src/lib.rs"),
        "macro_rules! packet { () => { pub struct Packet { #[cfg(not(doc))] pub byte: u8 } } }\npacket!();\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Packet;",
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
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("non-doc API extraction is incomplete"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    fs::write(
        workspace.path().join("dep/src/lib.rs"),
        "pub struct Packet;\nimpl Packet { #[cfg(not(doc))] pub fn byte(&self) -> u8 { 0 } }\npub enum Choice { #[cfg(not(doc))] Actual, Docs }\n",
    )
    .unwrap();
    for name in ["Packet", "Choice"] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                format!("use dep::{name};"),
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
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("non-doc API extraction is incomplete"),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn procedural_macro_api_changes_report_incomplete_extraction() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\", \"shape\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    for (name, manifest, source) in [
        (
            "app",
            "[dependencies]\ndep = { path = \"../dep\" }\n",
            "pub fn check() { let packet = dep::Packet { byte: 1 }; let _ = packet.byte; dep::S.ghost(); dep::Included.generated(); let _ = dep::Choice::Extra(1); let _: dep::api::Missing; dep::ContractAlias::required(&Consumer); dep::contracts::IdenticalContract::required(&Consumer); }\npub struct Consumer;\nimpl dep::Contract for Consumer { fn required(&self) {} }\nimpl dep::IdenticalContract for Consumer { fn required(&self) {} }\nimpl dep::contracts::IdenticalContract for Consumer { fn required(&self) {} }\nimpl dep::ConstContract for Consumer { const VALUE: u8 = 1; }\nimpl dep::TypeContract for Consumer { type Item = u8; }\nimpl dep::ProvidedContract for Consumer {}\nimpl dep::DefaultConstContract for Consumer {}\nimpl dep::DefaultTypeContract for Consumer {}\nimpl dep::StableContract for Consumer { fn r#type(&self) {} const REQUIRED: u8 = 1; type Item = u8; }\n",
        ),
        (
            "dep",
            "[dependencies]\nshape = { path = \"../shape\" }\n",
            "#![feature(associated_type_defaults)]\nuse shape::{packet, method, missing, variants};\n#[packet] pub struct Packet;\n#[method] pub struct S;\n#[shape::contract] pub trait Contract {}\n#[shape::identical(required)] pub trait IdenticalContract {}\npub use IdenticalContract as ContractAlias;\npub mod contracts { #[shape::identical(required)] pub trait IdenticalContract {} }\n#[shape::identical(constant)] pub trait ConstContract {}\n#[shape::identical(assoc_type)] pub trait TypeContract {}\n#[shape::identical(provided)] pub trait ProvidedContract {}\n#[shape::identical(default_constant)] pub trait DefaultConstContract {}\n#[shape::identical(default_type)] pub trait DefaultTypeContract {}\n#[shape::stable] pub trait StableContract {}\npub use StableContract as StableAlias;\n#[variants] pub enum Choice {}\nmissing!();\npub mod nested { #[shape::packet] pub struct Packet; }\npub mod donor { #[shape::packet] pub struct Packet; shape::missing!(); }\npub mod api { pub use crate::donor::*; }\npub struct Included;\ninclude!(concat!(env!(\"OUT_DIR\"), \"/included.rs\"));\npub struct Plain;\n",
        ),
        (
            "shape",
            "[lib]\nproc-macro = true\n",
            "extern crate proc_macro;\nuse proc_macro::TokenStream;\n#[proc_macro_attribute]\npub fn packet(_: TokenStream, _: TokenStream) -> TokenStream {\n    \"pub struct Packet { #[cfg(not(doc))] pub byte: u8 }\".parse().unwrap()\n}\n#[proc_macro_attribute]\npub fn method(_: TokenStream, _: TokenStream) -> TokenStream {\n    \"pub struct S; impl S { #[cfg(not(doc))] pub fn ghost(&self) {} }\".parse().unwrap()\n}\n#[proc_macro_attribute]\npub fn contract(_: TokenStream, _: TokenStream) -> TokenStream {\n    \"pub trait Contract { #[cfg(not(doc))] fn required(&self); #[cfg(doc)] fn required(&self) {} }\".parse().unwrap()\n}\n#[proc_macro_attribute]\npub fn variants(_: TokenStream, _: TokenStream) -> TokenStream {\n    \"pub enum Choice { Base, #[cfg(not(doc))] Extra(u8) }\".parse().unwrap()\n}\n#[proc_macro]\npub fn missing(_: TokenStream) -> TokenStream {\n    \"#[cfg(not(doc))] pub struct Missing;\".parse().unwrap()\n}\n",
        ),
    ] {
        let path = workspace.path().join(name);
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(
            path.join("Cargo.toml"),
            format!(
                "[package]\nname = {name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\n{manifest}"
            ),
        )
        .unwrap();
        fs::write(path.join("src/lib.rs"), source).unwrap();
    }
    let macro_path = workspace.path().join("shape/src/lib.rs");
    let macros = fs::read_to_string(&macro_path).unwrap();
    fs::write(
        macro_path,
        macros + r###"
#[proc_macro_attribute]
pub fn identical(mode: TokenStream, _: TokenStream) -> TokenStream {
    let (name, member) = match mode.to_string().as_str() {
        "required" => ("IdenticalContract", "fn required(&self);"),
        "constant" => ("ConstContract", "const VALUE: u8;"),
        "assoc_type" => ("TypeContract", "type Item;"),
        "provided" => ("ProvidedContract", "fn optional(&self) {}"),
        "default_constant" => ("DefaultConstContract", "const VALUE: u8 = 7;"),
        "default_type" => ("DefaultTypeContract", "type Item = u8;"),
        _ => panic!("unknown mode"),
    };
    format!("pub trait {name} {{ #[cfg(not(doc))] {member} #[cfg(doc)] {member} }}").parse().unwrap()
}
#[proc_macro_attribute]
pub fn stable(_: TokenStream, _: TokenStream) -> TokenStream {
    "pub trait StableContract { fn r#type(&self); fn optional(&self) {} const REQUIRED: u8; const DEFAULT: u8 = 7; type Item; type Default = u8; } pub trait Ambiguous { fn required(&self); } #[macro_export] macro_rules! Ambiguous { () => {} }".parse().unwrap()
}
#[proc_macro]
pub fn members(_: TokenStream) -> TokenStream {
    r##"
pub struct IdenticalFields {
    #[cfg(not(doc))] pub r#type: u8,
    #[cfg(doc)] pub r#type: u8,
}
pub use IdenticalFields as FieldsAlias;
pub struct IdenticalTuple(
    #[cfg(not(doc))] pub u8,
    #[cfg(doc)] pub u8,
);
pub struct ShiftedTuple(
    #[cfg(not(doc))] u8,
    #[cfg(doc)] u8,
    pub u16,
);
pub union IdenticalUnion {
    #[cfg(not(doc))] pub byte: u8,
    #[cfg(doc)] pub byte: u8,
}
pub enum IdenticalVariants {
    Base,
    #[cfg(not(doc))] Extra(u8),
    #[cfg(doc)] Extra(u8),
}
pub enum IdenticalVariantFields {
    Named { #[cfg(not(doc))] byte: u8, #[cfg(doc)] byte: u8 },
    Tuple(#[cfg(not(doc))] u8, #[cfg(doc)] u8),
}
pub use IdenticalVariantFields::Named as NamedAlias;
pub struct IdenticalMethods;
impl IdenticalMethods {
    #[cfg(not(doc))] pub fn r#type(&self) {}
    #[cfg(doc)] pub fn r#type(&self) {}
}
pub struct IdenticalConstants;
impl IdenticalConstants {
    #[cfg(not(doc))] pub const VALUE: u8 = 7;
    #[cfg(doc)] pub const VALUE: u8 = 7;
}
pub struct IdenticalImpl;
#[cfg(not(doc))] impl IdenticalImpl { pub fn live(&self) {} }
#[cfg(doc)] impl IdenticalImpl { pub fn live(&self) {} }
pub struct Specialized<T>(T);
impl Specialized<u8> { pub fn live(&self) {} }
impl Specialized<u16> {
    #[cfg(not(doc))] pub fn live(&self) {}
    #[cfg(doc)] pub fn live(&self) {}
}
pub struct StableMembers {
    pub r#type: u8,
    #[cfg(not(doc))] private: u8,
    #[cfg(doc)] private: u8,
    pub(crate) restricted: u8,
}
pub struct StableTuple(u8, pub u8);
pub union StableUnion { pub byte: u8, private: u8 }
pub enum StableVariants { Base, Named { r#type: u8 }, Tuple(u8), r#type }
impl StableMembers {
    pub fn r#type(&self) {}
    #[cfg(not(doc))] fn private(&self) {}
    #[cfg(doc)] fn private(&self) {}
    pub(crate) fn restricted(&self) {}
    pub const VALUE: u8 = 7;
    const PRIVATE: u8 = 0;
    pub(crate) const RESTRICTED: u8 = 0;
}
pub struct StableSpecialized<T>(T);
impl StableSpecialized<u8> { pub fn live(&self) {} }
impl StableSpecialized<u16> { pub fn live(&self) {} }
"##.parse().unwrap()
}
"###,
    )
    .unwrap();
    let dep_path = workspace.path().join("dep/src/lib.rs");
    let source = fs::read_to_string(&dep_path).unwrap();
    fs::write(dep_path, source + "\nshape::members!();\n").unwrap();
    let app_path = workspace.path().join("app/src/lib.rs");
    let source = fs::read_to_string(&app_path).unwrap();
    fs::write(
        app_path,
        source
            + r#"
pub fn generated_check() {
    let packet = dep::IdenticalFields { r#type: 1 };
    let _: u8 = packet.r#type;
    let _: u8 = dep::IdenticalTuple(1).0;
    let _ = dep::IdenticalUnion { byte: 1 };
    let _ = dep::IdenticalVariants::Extra(1);
    let _ = dep::IdenticalVariantFields::Named { byte: 1 };
    let _ = dep::IdenticalVariantFields::Tuple(1);
    dep::IdenticalMethods.r#type();
    let _: u8 = dep::IdenticalConstants::VALUE;
    dep::IdenticalImpl.live();
}
pub fn specialized_check(a: dep::Specialized<u8>, b: dep::Specialized<u16>) {
    a.live();
    b.live();
}
pub fn tuple_check(value: dep::ShiftedTuple) -> u16 { value.1 }
"#,
    )
    .unwrap();
    fs::write(
        workspace.path().join("dep/build.rs"),
        "fn main() { let path = std::path::PathBuf::from(std::env::var_os(\"OUT_DIR\").unwrap()).join(\"included.rs\"); std::fs::write(path, \"impl Included { #[cfg(not(doc))] pub fn generated(&self) {} }\").unwrap(); }\n",
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

    for (name, missing, filtered) in [
        ("Packet", "field byte", false),
        ("S", "ghost", false),
        (
            "Contract",
            "required trait method fn required(&self)",
            false,
        ),
        ("Included", "generated", false),
        ("Choice", "variant Extra", false),
        ("Missing", "struct", false),
        ("nested::Packet", "field byte", false),
        ("api::Packet", "field byte", false),
        (
            "IdenticalContract",
            "required trait method fn required(&self)",
            true,
        ),
        (
            "ContractAlias",
            "required trait method fn required(&self)",
            true,
        ),
        (
            "contracts::IdenticalContract",
            "required trait method fn required(&self)",
            true,
        ),
        ("ConstContract", "required trait const VALUE", true),
        ("TypeContract", "required trait type Item", true),
        (
            "ProvidedContract",
            "provided trait method fn optional(&self)",
            true,
        ),
        ("DefaultConstContract", "provided trait const VALUE", true),
        ("DefaultTypeContract", "provided trait type Item", true),
        ("IdenticalFields", "field r#type", true),
        ("FieldsAlias", "field r#type", true),
        ("IdenticalTuple", "field 0", true),
        ("ShiftedTuple", "field 1", true),
        ("IdenticalUnion", "field byte", true),
        ("IdenticalVariants", "variant Extra", true),
        ("IdenticalVariantFields", "field", true),
        ("IdenticalVariantFields::Named", "field byte", true),
        ("IdenticalVariantFields::Tuple", "field 0", true),
        ("NamedAlias", "field byte", true),
        ("IdenticalMethods", "fn r#type", true),
        ("IdenticalConstants", "const VALUE", true),
        ("IdenticalImpl", "fn live", true),
        ("Specialized", "fn live", true),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                format!("use dep::{name};"),
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
            "{name}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("non-doc API extraction is incomplete:"),
            "{name}: {stderr}"
        );
        if name == "Included" {
            assert!(stderr.contains("included.rs"), "{name}: {stderr}");
        } else {
            assert!(stderr.contains("compiler expansion"), "{name}: {stderr}");
            assert!(stderr.contains(missing), "{name}: {stderr}");
        }
        if filtered {
            assert!(stderr.contains("filtered Rustdoc JSON"), "{name}: {stderr}");
        }
    }
    let missing_glob = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::api::Missing;",
            "--root",
            workspace.path().to_str().unwrap(),
            "--package",
            "app",
        ])
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    assert!(!missing_glob.status.success());
    assert!(
        String::from_utf8_lossy(&missing_glob.stderr)
            .contains("non-doc API extraction is incomplete: compiler accepted import 'dep::api::Missing' but Rustdoc JSON omitted it"),
        "{}",
        String::from_utf8_lossy(&missing_glob.stderr)
    );
    let unaffected = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args([
            "use dep::Plain;",
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
        unaffected.status.success(),
        "{}",
        String::from_utf8_lossy(&unaffected.stderr)
    );
    assert!(String::from_utf8_lossy(&unaffected.stdout).contains("pub struct Plain;"));

    for (name, members) in [
        (
            "StableMembers",
            vec![
                "pub r#type: u8",
                "pub fn r#type(",
                "pub const VALUE: u8 = 7;",
            ],
        ),
        ("StableTuple", vec!["pub u8"]),
        ("StableUnion", vec!["pub byte: u8"]),
        (
            "StableVariants",
            vec!["Base", "Named { r#type: u8 }", "Tuple(u8)"],
        ),
        ("StableVariants::Named", vec!["Named { r#type: u8 }"]),
        ("StableVariants::Tuple", vec!["Tuple(u8)"]),
        (
            "StableSpecialized",
            vec!["impl StableSpecialized<u8>", "impl StableSpecialized<u16>"],
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .arg(format!("use dep::{name};"))
            .arg("--root")
            .arg(workspace.path())
            .args(["--package", "app"])
            .env("CARGO_NET_OFFLINE", "true")
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = String::from_utf8(output.stdout).unwrap();
        for member in members {
            assert!(report.contains(member), "{name}: {member}: {report}");
        }
        assert!(!report.contains("fn private("), "{report}");
        assert!(!report.contains("const PRIVATE:"), "{report}");
        assert!(!report.contains("restricted"), "{report}");
        assert!(!report.contains("const RESTRICTED:"), "{report}");
    }

    for name in ["StableContract", "StableAlias"] {
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .arg(format!("use dep::{name};"))
            .arg("--root")
            .arg(workspace.path())
            .args(["--package", "app"])
            .env("CARGO_NET_OFFLINE", "true")
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = String::from_utf8(output.stdout).unwrap();
        for member in [
            "fn r#type(",
            "fn optional(",
            "const REQUIRED: u8;",
            "const DEFAULT: u8 = 7;",
            "type Item;",
            "type Default = u8;",
        ] {
            assert!(report.contains(member), "{name}: {member}: {report}");
        }
    }

    let ambiguous = Command::new(env!("CARGO_BIN_EXE_excra"))
        .args(["use dep::Ambiguous;", "--root"])
        .arg(workspace.path())
        .args(["--package", "app"])
        .env("CARGO_NET_OFFLINE", "true")
        .env("CARGO_TARGET_DIR", workspace.path().join("target"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&ambiguous.stderr);
    assert!(!ambiguous.status.success());
    assert!(
        stderr.contains("ambiguous across Rust namespaces"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("non-doc API extraction is incomplete"),
        "{stderr}"
    );

    for (name, missing) in [
        ("Contract", "required"),
        ("IdenticalContract", "required"),
        ("ContractAlias", "required"),
        ("contracts::IdenticalContract", "required"),
        ("ConstContract", "VALUE"),
        ("TypeContract", "Item"),
    ] {
        fs::write(
            workspace.path().join("app/src/lib.rs"),
            format!("pub struct Consumer;\nimpl dep::{name} for Consumer {{}}\n"),
        )
        .unwrap();
        let incomplete_impl = Command::new("cargo")
            .args(["check", "--offline", "--locked", "-p", "app"])
            .current_dir(workspace.path())
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .output()
            .unwrap();
        assert!(!incomplete_impl.status.success(), "{name}");
        let stderr = String::from_utf8_lossy(&incomplete_impl.stderr);
        assert!(
            stderr.contains("E0046") && stderr.contains(missing),
            "{name}: {stderr}"
        );
    }
}
