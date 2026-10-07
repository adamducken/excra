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
#![feature(auto_traits, associated_type_defaults)]
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
#[cfg_attr(doc, maker::unsafe_trait)] pub trait NormallySafe {}
#[cfg_attr(not(doc), maker::unsafe_trait)] pub trait NormallyUnsafe {}
#[cfg_attr(doc, maker::auto_trait)] pub trait NormallyOrdinary {}
#[cfg_attr(doc, maker::discriminant)] pub enum Levels { Low, High }
pub use Levels::Low as LowAlias;
#[cfg_attr(not(doc), maker::semantics(non_exhaustive))] pub struct MacroClosed { pub byte: u8 }
pub use MacroClosed as MacroClosedAlias;
#[cfg_attr(doc, maker::semantics(non_exhaustive))] pub enum MacroOpen { A }
#[cfg_attr(doc, maker::semantics(repr(align(64))))] pub struct MacroAligned(pub u8);
#[cfg_attr(not(doc), maker::semantics(must_use = "inspect"))] pub fn macro_result() -> u8 { 1 }
#[cfg_attr(not(doc), maker::semantics(deprecated(note = "old")))] pub struct MacroDeprecated;
#[cfg_attr(doc, maker::nested)] pub enum Nested { A { byte: u8 } }
pub use Nested::A as NestedAlias;
#[cfg_attr(doc, maker::field_metadata)] pub struct FieldMetadata { pub byte: u8 }
#[cfg_attr(doc, maker::tuple_metadata)] pub struct TupleMetadata(pub u8);
pub trait TraitMetadata { #[cfg_attr(not(doc), maker::semantics(must_use))] fn value(&self) -> u8; }
pub struct ConstantMetadata;
impl ConstantMetadata { #[cfg_attr(doc, maker::semantics(deprecated(note = "old constant")))] pub const VALUE: u8 = 1; }
#[cfg_attr(doc, maker::semantics(derive(Clone)))] pub struct DocDerived;
#[cfg_attr(doc, maker::block_derive)] pub struct DocBlockDerived;
pub struct NormalBlockDerived;
#[cfg(not(doc))] const _: () = {
    #[automatically_derived] impl Clone for crate::NormalBlockDerived { fn clone(&self) -> Self { Self } }
};
pub struct StableBlockDerived;
const _: () = {
    extern crate core as _core;
    #[automatically_derived] impl _core::clone::Clone for crate::StableBlockDerived { fn clone(&self) -> Self { Self } }
};
maker::derived_alternatives!();
pub struct AliasCandidates<T>(pub T);
maker::alias_candidates!();
pub struct StableAliasCandidates<T>(pub T);
const _: () = {
    type Selected = StableAliasCandidates<u8>;
    impl Selected { pub fn byte(&self) -> u8 { 1 } }
};
const _: () = {
    type Selected = StableAliasCandidates<u16>;
    impl Selected { pub fn byte(&self) -> u8 { 1 } }
};
pub struct AliasOwners<T>(pub T);
#[cfg(not(doc))] type SelectedOwner = AliasOwners<u8>;
#[cfg(doc)] type SelectedOwner = AliasOwners<u16>;
impl SelectedOwner { pub fn byte(&self) -> u8 { 1 } }
#[cfg(not(doc))] type Scalar = u8;
#[cfg(doc)] type Scalar = u16;
pub struct AliasFields { pub byte: Scalar }
pub struct AliasTuple(pub Scalar);
pub union AliasUnion { pub byte: Scalar }
pub enum AliasVariants { A(Scalar) }
pub use AliasVariants::A as AliasVariant;
pub fn alias_value(value: Scalar) -> Scalar { value }
pub trait AliasContract { fn value(&self) -> Scalar; }
pub trait AliasTypeContract { type Item = Scalar; }
pub struct NestedAliasOwners<T>(pub T);
impl NestedAliasOwners<Vec<Scalar>> { pub fn byte(&self) -> u8 { 1 } }
pub struct CrossModuleOwner<T>(pub T);
mod alias_origin {
    #[cfg(not(doc))] type Argument = u8;
    #[cfg(doc)] type Argument = u16;
    pub type Selected = crate::CrossModuleOwner<Vec<Argument>>;
}
impl alias_origin::Selected { pub fn byte(&self) -> u8 { 1 } }
pub struct BlockAliasOwner<T>(pub T);
const _: () = {
    type Input = u8;
    type Outer = crate::BlockAliasOwner<Input>;
    const _: () = {
        #[cfg(not(doc))] type Input = u16;
        #[cfg(doc)] type Input = u32;
        impl Outer { pub fn byte(&self) -> u8 { 1 } }
    };
};
pub struct TraitAliasOwners;
impl Link<Scalar> for TraitAliasOwners {}
pub mod first { pub trait Marker {} }
pub mod second { pub trait Marker {} }
#[cfg(not(doc))] use first::Marker as SelectedMarker;
#[cfg(doc)] use second::Marker as SelectedMarker;
pub struct TraitIdentity;
impl SelectedMarker for TraitIdentity {}
type StableScalar = u8;
type StableOwner = StableAliasOwners<Vec<StableScalar>>;
pub struct StableAliasOwners<T>(pub T);
impl StableOwner { pub fn byte(&self) -> u8 { 1 } }
#[cfg(not(doc))] type EquivalentScalar = u8;
#[cfg(doc)] type EquivalentScalar = u8;
pub struct EquivalentFields { pub byte: EquivalentScalar }
#[cfg(not(doc))] type T = u8;
#[cfg(doc)] type T = u16;
pub struct GenericShadow<T>(pub T);
impl<T> GenericShadow<T> { pub fn value(&self) -> &T { &self.0 } }
pub trait ShadowContract { fn value<T>(&self, value: T) -> T; }
pub struct AttributeOwners<T>(T);
impl AttributeOwners<u8> { #[cfg_attr(not(doc), maker::semantics(must_use))] pub fn value(&self) -> u8 { 1 } }
impl AttributeOwners<u16> { #[cfg_attr(doc, maker::semantics(must_use))] pub fn value(&self) -> u8 { 1 } }
#[maker::semantics(repr(C, align(16)))] #[derive(Clone)] pub struct StableLayout(pub u8);
#[maker::semantics(non_exhaustive)] pub enum StableVariants { #[non_exhaustive] A { byte: u8 } }
#[maker::semantics(must_use = "inspect")] pub fn stable_result() -> u8 { 1 }
pub unsafe trait StableUnsafe {}
pub auto trait StableAuto {}
#[deprecated(note = "old module")] pub mod old {
    pub struct Legacy { #[deprecated(note = "old field")] pub byte: u8 }
    pub trait Contract { fn method(&self); }
}
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
#[proc_macro_attribute]
pub fn semantics(attribute: TokenStream, input: TokenStream) -> TokenStream {
    let mut output = format!("#[{attribute}]").parse::<TokenStream>().unwrap();
    output.extend(input);
    output
}
#[proc_macro_attribute]
pub fn unsafe_trait(_: TokenStream, input: TokenStream) -> TokenStream {
    input.to_string().replace("pub trait", "pub unsafe trait").parse().unwrap()
}
#[proc_macro_attribute]
pub fn auto_trait(_: TokenStream, input: TokenStream) -> TokenStream {
    input.to_string().replace("pub trait", "pub auto trait").parse().unwrap()
}
#[proc_macro_attribute]
pub fn discriminant(_: TokenStream, _: TokenStream) -> TokenStream {
    "pub enum Levels { Low = 7, High }".parse().unwrap()
}
#[proc_macro_attribute]
pub fn nested(_: TokenStream, _: TokenStream) -> TokenStream {
    "pub enum Nested { #[non_exhaustive] A { #[deprecated(note = \"old field\")] byte: u8 } }".parse().unwrap()
}
#[proc_macro_attribute]
pub fn field_metadata(_: TokenStream, _: TokenStream) -> TokenStream {
    "pub struct FieldMetadata { #[deprecated(note = \"old field\")] pub byte: u8 }".parse().unwrap()
}
#[proc_macro_attribute]
pub fn tuple_metadata(_: TokenStream, _: TokenStream) -> TokenStream {
    "pub struct TupleMetadata(#[deprecated(note = \"old field\")] pub u8);".parse().unwrap()
}
#[proc_macro]
pub fn derived_alternatives(_: TokenStream) -> TokenStream {
    r#"
pub struct LostDerived;
#[cfg(not(doc))] #[automatically_derived] impl Clone for LostDerived { fn clone(&self) -> Self { Self } }
#[cfg(doc)] #[automatically_derived] impl Clone for LostDerived { fn clone(&self) -> Self { Self } }
"#.parse().unwrap()
}
#[proc_macro]
pub fn alias_candidates(_: TokenStream) -> TokenStream {
    r#"
#[cfg(doc)] const _: () = {
    type Selected = AliasCandidates<u16>;
    impl Selected { pub fn byte(&self) -> u8 { 1 } }
};
const _: () = {
    type Selected = AliasCandidates<u8>;
    #[cfg(not(doc))] impl Selected { pub fn byte(&self) -> u8 { 1 } }
    #[cfg(doc)] impl Selected { pub fn byte(&self) -> u8 { 1 } }
};
"#.parse().unwrap()
}
#[proc_macro_attribute]
pub fn block_derive(_: TokenStream, input: TokenStream) -> TokenStream {
    append(input, "const _: () = { #[automatically_derived] impl Clone for DocBlockDerived { fn clone(&self) -> Self { Self } } };")
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
        "StableLayout",
        "NormalBlockDerived",
        "StableBlockDerived",
        "StableAliasOwners",
        "StableAliasCandidates",
        "EquivalentFields",
        "GenericShadow",
        "ShadowContract",
        "BlockAliasOwner",
        "StableVariants",
        "StableVariants::A",
        "stable_result",
        "StableUnsafe",
        "StableAuto",
        "old::Legacy",
        "old::Contract",
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
        "#[repr(C, align(16))]",
        "#[must_use = \"inspect\"]",
        "pub unsafe trait StableUnsafe",
        "pub auto trait StableAuto",
        "note: old module",
        "note: old field",
        "impl Clone for crate::NormalBlockDerived",
        "impl Clone for crate::StableBlockDerived",
        "impl StableAliasOwners<Vec<u8>>",
        "impl StableAliasCandidates<u8>",
        "impl StableAliasCandidates<u16>",
        "pub struct EquivalentFields { pub byte: u8 }",
        "pub fn value(self: &Self) -> &T",
        "fn value<T>(self: &Self, value: T) -> T;",
        "impl crate::BlockAliasOwner<u8>",
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
        (
            "struct S; impl excra_dependency::NormallySafe for S {}",
            true,
        ),
        (
            "struct S; impl excra_dependency::NormallyUnsafe for S {}",
            false,
        ),
        (
            "struct S; impl excra_dependency::NormallyOrdinary for S {}",
            true,
        ),
        (
            "const _: [(); 0] = [(); excra_dependency::Levels::Low as usize];",
            true,
        ),
        (
            "const _: [(); 7] = [(); excra_dependency::Levels::Low as usize];",
            false,
        ),
        (
            "pub fn f() { let _ = excra_dependency::MacroClosed { byte: 1 }; }",
            false,
        ),
        (
            "pub fn f(value: excra_dependency::MacroOpen) { match value { excra_dependency::MacroOpen::A => {} } }",
            true,
        ),
        (
            "const _: [(); 1] = [(); std::mem::align_of::<excra_dependency::MacroAligned>()];",
            true,
        ),
        (
            "#![deny(unused_must_use)] pub fn f() { excra_dependency::macro_result(); }",
            false,
        ),
        (
            "#![deny(deprecated)] pub fn f(_: excra_dependency::MacroDeprecated) {}",
            false,
        ),
        (
            "pub fn f() { let _ = excra_dependency::Nested::A { byte: 1 }; }",
            true,
        ),
        (
            "#![deny(deprecated)] pub fn f() { let _ = excra_dependency::FieldMetadata { byte: 1 }; let _ = excra_dependency::TupleMetadata(1); let _ = excra_dependency::ConstantMetadata::VALUE; }",
            true,
        ),
        (
            "fn needs<T: Clone>() {} pub fn f() { needs::<excra_dependency::DocDerived>(); }",
            false,
        ),
        (
            "pub fn f() { let _ = excra_dependency::LostDerived.clone(); }",
            true,
        ),
        (
            "fn needs<T: Clone>() {} pub fn f() { needs::<excra_dependency::DocBlockDerived>(); }",
            false,
        ),
        (
            "pub fn f() { let _ = excra_dependency::NormalBlockDerived.clone(); let _ = excra_dependency::StableBlockDerived.clone(); }",
            true,
        ),
        (
            "pub fn f(value: excra_dependency::AliasOwners<u8>) -> u8 { value.byte() }",
            true,
        ),
        (
            "pub fn f(value: excra_dependency::AliasCandidates<u8>) -> u8 { value.byte() }",
            true,
        ),
        (
            "pub fn f(value: excra_dependency::AliasCandidates<u16>) -> u8 { value.byte() }",
            false,
        ),
        (
            "pub fn f(a: excra_dependency::StableAliasCandidates<u8>, b: excra_dependency::StableAliasCandidates<u16>) { a.byte(); b.byte(); }",
            true,
        ),
        (
            "pub fn f(value: excra_dependency::AliasOwners<u16>) -> u8 { value.byte() }",
            false,
        ),
        (
            "pub fn f(value: excra_dependency::AliasFields) -> u8 { value.byte }",
            true,
        ),
        (
            "pub fn f(value: excra_dependency::AliasFields) -> u16 { value.byte }",
            false,
        ),
        (
            "pub fn f(value: u8) -> u8 { excra_dependency::alias_value(value) }",
            true,
        ),
        (
            "pub fn f(value: u16) -> u16 { excra_dependency::alias_value(value) }",
            false,
        ),
        (
            "pub fn f(value: excra_dependency::CrossModuleOwner<Vec<u8>>) -> u8 { value.byte() }",
            true,
        ),
        (
            "pub fn f(value: excra_dependency::CrossModuleOwner<Vec<u16>>) -> u8 { value.byte() }",
            false,
        ),
        (
            "pub fn f(value: excra_dependency::BlockAliasOwner<u8>) -> u8 { value.byte() }",
            true,
        ),
        (
            "pub fn f() { let _ = excra_dependency::AliasVariants::A(1u8); }",
            true,
        ),
        (
            "pub fn f() { let _ = excra_dependency::AliasVariants::A(1u16); }",
            false,
        ),
        (
            "struct S; impl excra_dependency::AliasContract for S { fn value(&self) -> u8 { 1 } }",
            true,
        ),
        (
            "struct S; impl excra_dependency::AliasContract for S { fn value(&self) -> u16 { 1 } }",
            false,
        ),
        (
            "fn needs<T: excra_dependency::Link<u8>>() {} pub fn f() { needs::<excra_dependency::TraitAliasOwners>(); }",
            true,
        ),
        (
            "fn needs<T: excra_dependency::Link<u16>>() {} pub fn f() { needs::<excra_dependency::TraitAliasOwners>(); }",
            false,
        ),
        (
            "fn needs<T: excra_dependency::first::Marker>() {} pub fn f() { needs::<excra_dependency::TraitIdentity>(); }",
            true,
        ),
        (
            "fn needs<T: excra_dependency::second::Marker>() {} pub fn f() { needs::<excra_dependency::TraitIdentity>(); }",
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
        ("MacroClosed", "semantic attributes"),
        ("MacroClosedAlias", "semantic attributes"),
        ("MacroOpen", "semantic attributes"),
        ("MacroAligned", "semantic attributes"),
        ("macro_result", "semantic attributes"),
        ("MacroDeprecated", "semantic attributes"),
        ("Nested", "semantic attributes"),
        ("Nested::A", "semantic attributes"),
        ("NestedAlias", "semantic attributes"),
        ("FieldMetadata", "semantic attributes"),
        ("TupleMetadata", "semantic attributes"),
        ("TraitMetadata", "semantic attributes"),
        ("ConstantMetadata", "semantic attributes"),
        ("DocDerived", "derived trait impl Clone"),
        ("DocBlockDerived", "derived trait impl Clone"),
        ("LostDerived", "impl Clone"),
        ("AttributeOwners", "semantic attributes"),
        ("AliasCandidates", "type aliases or resolved paths"),
        ("AliasOwners", "type aliases or resolved paths"),
        ("AliasFields", "type aliases or resolved paths"),
        ("AliasTuple", "type aliases or resolved paths"),
        ("AliasUnion", "type aliases or resolved paths"),
        ("AliasVariants", "type aliases or resolved paths"),
        ("AliasVariants::A", "type aliases or resolved paths"),
        ("AliasVariant", "type aliases or resolved paths"),
        ("alias_value", "type aliases or resolved paths"),
        ("AliasContract", "type aliases or resolved paths"),
        ("AliasTypeContract", "type aliases or resolved paths"),
        ("NestedAliasOwners", "type aliases or resolved paths"),
        ("CrossModuleOwner", "type aliases or resolved paths"),
        ("TraitAliasOwners", "type aliases or resolved paths"),
        ("TraitIdentity", "type aliases or resolved paths"),
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
    for (name, mismatch) in [
        ("NormallySafe", "trait"),
        ("NormallyUnsafe", "unsafe trait"),
        ("NormallyOrdinary", "trait"),
        ("Levels", "variant Low discriminant"),
        ("Levels::Low", "variant Low discriminant"),
        ("LowAlias", "variant Low discriminant"),
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{name}: {stderr}");
        assert!(
            stderr.contains("non-doc API extraction is incomplete") && stderr.contains(mismatch),
            "{name}: {stderr}"
        );
    }
}
