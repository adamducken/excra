use std::{fs, process::Command};
use tempfile::TempDir;

#[test]
fn doc_added_api_and_semantic_attributes_match_the_normal_artifact() {
    let workspace = TempDir::new().unwrap();
    fs::write(
        workspace.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\", \"dep\", \"maker\"]\nresolver = \"3\"\n",
    )
    .unwrap();
    for (name, extra, source) in [
        ("app", "[dependencies]\ndep = { path = \"../dep\" }\n", ""),
        (
            "dep",
            "[dependencies]\nmaker = { path = \"../maker\" }\n",
            r#"
pub trait Marker {}
pub trait Link<T> {}
#[cfg_attr(doc, maker::field)] pub struct Phantom {}
pub use Phantom as PhantomAlias;
#[cfg_attr(doc, maker::variant)] pub enum Mirage { Base }
pub use Mirage as MirageAlias;
#[cfg_attr(doc, maker::method)] pub struct Empty;
pub use Empty as EmptyAlias;
#[cfg_attr(doc, maker::constant)] pub struct Constants;
#[cfg_attr(doc, maker::contract)] pub trait Contract {}
#[cfg_attr(doc, maker::trait_impl)] pub struct DocImpl;
pub use DocImpl as DocImplAlias;
#[cfg_attr(doc, maker::owner)] pub struct Owners<T>(T);
pub use Owners as OwnersAlias;
#[cfg_attr(doc, maker::trait_owner)] pub struct TraitOwners<T>(T);
maker::original!();
pub struct PrivateStable { secret: u8 }
pub struct TupleStable(u8);
pub struct Filtered { pub live: u8, #[cfg(doc)] pub ghost: u8 }
pub struct FilteredTuple(pub u16, #[cfg(doc)] pub u8);
pub enum FilteredVariants { Base, #[cfg(doc)] Ghost }
pub struct FilteredMethods;
impl FilteredMethods {
    pub fn live(&self) {}
    #[cfg(doc)] pub fn ghost(&self) {}
}
#[cfg(doc)] impl Marker for FilteredMethods {}
impl Link<u8> for &FilteredMethods {}
impl Link<FilteredMethods> for u8 {}
impl Link<(FilteredMethods, u8)> for u16 {}
pub struct Wrapper<T>(T);
impl Link<u16> for Wrapper<FilteredMethods> {}
impl Wrapper<FilteredMethods> { pub fn wrapper_only(&self) {} }
pub struct StableImpl;
use Marker as LocalMarker;
impl LocalMarker for StableImpl {}
use std::fmt::Debug as ExternalDebug;
impl ExternalDebug for StableImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StableImpl")
    }
}
extern crate core as renamed_core;
use renamed_core::fmt::Display as ImportedDisplay;
impl ImportedDisplay for StableImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StableImpl")
    }
}
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub mod simd {
    #[cfg_attr(not(doc), maker::simd)] pub fn fast() {}
    pub use fast as FastAlias;
    #[cfg_attr(doc, maker::simd)] pub fn plain() {}
    #[maker::simd] pub fn stable_fast() {}
    pub struct FastEngine;
    pub use FastEngine as FastEngineAlias;
    impl FastEngine { #[cfg_attr(not(doc), maker::simd)] pub fn fast(&self) {} }
    pub struct PlainEngine;
    impl PlainEngine { #[cfg_attr(doc, maker::simd)] pub fn plain(&self) {} }
    pub struct FeatureOwners<T>(T);
    impl FeatureOwners<u8> { #[cfg_attr(not(doc), maker::simd)] pub fn fast(&self) {} }
    impl FeatureOwners<u16> { #[cfg_attr(doc, maker::simd)] pub fn fast(&self) {} }
    pub struct StableFeatures<T>(T);
    impl StableFeatures<u8> { #[maker::simd] pub fn fast(&self) {} }
    impl StableFeatures<u16> { pub fn fast(&self) {} }
}
"#,
        ),
        (
            "maker",
            "[lib]\nproc-macro = true\n",
            r##"
extern crate proc_macro;
use proc_macro::TokenStream;
fn append(input: TokenStream, extra: &str) -> TokenStream {
    let mut output = input;
    output.extend(extra.parse::<TokenStream>().unwrap());
    output
}
#[proc_macro_attribute]
pub fn field(_: TokenStream, _: TokenStream) -> TokenStream {
    "pub struct Phantom { pub ghost: u8 }".parse().unwrap()
}
#[proc_macro_attribute]
pub fn variant(_: TokenStream, _: TokenStream) -> TokenStream {
    "pub enum Mirage { Base, Ghost }".parse().unwrap()
}
#[proc_macro_attribute]
pub fn method(_: TokenStream, input: TokenStream) -> TokenStream {
    append(input, "impl Empty { pub fn ghost(&self) {} }")
}
#[proc_macro_attribute]
pub fn constant(_: TokenStream, input: TokenStream) -> TokenStream {
    append(input, "impl Constants { pub const GHOST: u8 = 7; }")
}
#[proc_macro_attribute]
pub fn contract(_: TokenStream, _: TokenStream) -> TokenStream {
    "pub trait Contract { fn ghost(&self); }".parse().unwrap()
}
#[proc_macro_attribute]
pub fn trait_impl(_: TokenStream, input: TokenStream) -> TokenStream {
    append(input, "impl Marker for DocImpl {}")
}
#[proc_macro_attribute]
pub fn owner(_: TokenStream, input: TokenStream) -> TokenStream {
    append(input, "impl Owners<u16> { pub fn live(&self) -> u16 { 7 } }")
}
#[proc_macro_attribute]
pub fn trait_owner(_: TokenStream, input: TokenStream) -> TokenStream {
    append(input, "impl Link<(u16, u16)> for TraitOwners<u16> {}")
}
#[proc_macro]
pub fn original(_: TokenStream) -> TokenStream {
    r#"
#[cfg(not(doc))] impl Owners<u8> { pub fn live(&self) -> u8 { 7 } }
#[cfg(doc)] impl Owners<u8> { pub fn live(&self) -> u8 { 7 } }
#[cfg(not(doc))] impl Link<(u8, u8)> for TraitOwners<u8> {}
#[cfg(doc)] impl Link<(u8, u8)> for TraitOwners<u8> {}
"#.parse().unwrap()
}
#[proc_macro_attribute]
pub fn simd(_: TokenStream, input: TokenStream) -> TokenStream {
    let mut output = "#[target_feature(enable = \"avx2\")]".parse::<TokenStream>().unwrap();
    output.extend(input);
    output
}
"##,
        ),
    ] {
        let path = workspace.path().join(name);
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(
            path.join("Cargo.toml"),
            format!("[package]\nname = {name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\n{extra}"),
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

    let mut controls = vec![
        "PrivateStable",
        "TupleStable",
        "Filtered",
        "FilteredTuple",
        "FilteredVariants",
        "FilteredMethods",
        "StableImpl",
    ];
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    controls.extend(["simd::stable_fast", "simd::StableFeatures"]);
    let output = Command::new(env!("CARGO_BIN_EXE_excra"))
        .arg(format!("use dep::{{{}}};", controls.join(", ")))
        .arg("--root")
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
    for expected in [
        "pub struct PrivateStable { /* private/stripped fields */ }",
        "pub struct TupleStable(/* private/stripped field */);",
        "pub struct Filtered { pub live: u8 }",
        "pub struct FilteredTuple(pub u16);",
        "pub enum FilteredVariants { Base }",
        "pub fn live(self: &Self)",
    ] {
        assert!(stdout.contains(expected), "{expected}: {stdout}");
    }
    assert!(!stdout.contains("ghost"), "{stdout}");
    assert!(!stdout.contains("secret"), "{stdout}");
    let filtered_methods = stdout
        .split("item: struct FilteredMethods\n")
        .nth(1)
        .unwrap()
        .split("crate: dep ")
        .next()
        .unwrap();
    assert!(!filtered_methods.contains("impl Marker"), "{stdout}");
    assert!(!stdout.contains("wrapper_only"), "{stdout}");
    assert!(
        stdout.contains("impl Link<FilteredMethods> for u8"),
        "{stdout}"
    );
    assert!(
        stdout.contains("impl Link<u8> for &FilteredMethods"),
        "{stdout}"
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
    let mut probes = vec![
        (
            "pub fn f() { let _ = excra_dependency::PrivateStable {}; }",
            false,
        ),
        (
            "pub fn f() { let _ = excra_dependency::TupleStable(); }",
            false,
        ),
        (
            "pub fn f() { let _ = excra_dependency::Phantom { ghost: 1 }; }",
            false,
        ),
        ("pub fn f() { let _ = excra_dependency::Phantom {}; }", true),
        (
            "pub fn f() { let _ = excra_dependency::Mirage::Ghost; }",
            false,
        ),
        (
            "pub fn f(value: excra_dependency::Mirage) { match value { excra_dependency::Mirage::Base => {} } }",
            true,
        ),
        ("pub fn f() { excra_dependency::Empty.ghost(); }", false),
        (
            "pub fn f() { let _ = excra_dependency::Constants::GHOST; }",
            false,
        ),
        ("struct S; impl excra_dependency::Contract for S {}", true),
        (
            "fn needs<T: excra_dependency::Marker>() {} pub fn f() { needs::<excra_dependency::DocImpl>(); }",
            false,
        ),
        (
            "pub fn f(value: excra_dependency::Owners<u8>) -> u8 { value.live() }",
            true,
        ),
        (
            "pub fn f(value: excra_dependency::Owners<u16>) -> u16 { value.live() }",
            false,
        ),
        (
            "fn needs<T: excra_dependency::Link<(u8, u8)>>() {} pub fn f() { needs::<excra_dependency::TraitOwners<u8>>(); }",
            true,
        ),
        (
            "fn needs<T: excra_dependency::Link<(u16, u16)>>() {} pub fn f() { needs::<excra_dependency::TraitOwners<u16>>(); }",
            false,
        ),
    ];
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    probes.extend([
        ("pub fn f() { excra_dependency::simd::fast(); }", false),
        ("pub fn f() { excra_dependency::simd::plain(); }", true),
        (
            "pub fn f() { excra_dependency::simd::FastEngine.fast(); }",
            false,
        ),
        (
            "pub fn f() { excra_dependency::simd::PlainEngine.plain(); }",
            true,
        ),
        (
            "pub fn f(value: excra_dependency::simd::FeatureOwners<u8>) { value.fast(); }",
            false,
        ),
        (
            "pub fn f(value: excra_dependency::simd::FeatureOwners<u16>) { value.fast(); }",
            true,
        ),
        (
            "pub fn f(value: excra_dependency::simd::StableFeatures<u8>) { value.fast(); }",
            false,
        ),
        (
            "pub fn f(value: excra_dependency::simd::StableFeatures<u16>) { value.fast(); }",
            true,
        ),
    ]);
    let source = workspace.path().join("caller.rs");
    for (body, succeeds) in probes {
        fs::write(&source, body).unwrap();
        let mut compiler = Command::new(&probe.compiler[0]);
        compiler
            .args(&probe.compiler[1..])
            .current_dir(&probe.directory)
            .args(&probe.arguments)
            .args(["--edition=2024", "--crate-type=lib", "--emit=metadata"])
            .arg(&source)
            .arg("-o")
            .arg(workspace.path().join("caller.rmeta"));
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        compiler.arg("-Ctarget-feature=-avx2");
        let output = compiler.output().unwrap();
        assert_eq!(
            output.status.success(),
            succeeds,
            "{body}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let mut cases = vec![
        ("Phantom", "struct field ghost"),
        ("PhantomAlias", "struct field ghost"),
        ("Mirage", "variant Ghost"),
        ("MirageAlias", "variant Ghost"),
        ("Empty", "inherent method ghost"),
        ("EmptyAlias", "inherent method ghost"),
        ("Constants", "inherent const GHOST"),
        ("Contract", "required trait method ghost"),
        ("DocImpl", "trait impl Marker"),
        ("DocImplAlias", "trait impl Marker"),
        ("Owners", "signature and owner"),
        ("OwnersAlias", "signature and owner"),
        ("TraitOwners", "signature and owner"),
    ];
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    cases.extend([
        ("simd::fast", "target_feature"),
        ("simd::FastAlias", "target_feature"),
        ("simd::plain", "target_feature"),
        ("simd::FastEngine", "target_feature"),
        ("simd::FastEngineAlias", "target_feature"),
        ("simd::PlainEngine", "target_feature"),
        ("simd::FeatureOwners", "target_feature"),
    ]);
    for (name, mismatch) in cases {
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
                && stderr.contains("filtered Rustdoc JSON")
                && stderr.contains(mismatch),
            "{name}: {stderr}"
        );
    }
}
