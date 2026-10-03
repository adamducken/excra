# excra Skill

Use `excra` for local Rust dependency docs from the exact crate version and feature set resolved by the target Cargo project. Prefer it over web search/docs.rs when working inside a Cargo project.

## What it does

`excra` resolves external `use` paths against the project at `--root`, generates/loads Rustdoc JSON for direct dependencies, and prints compact docs for requested items.

It reports:

- crate name + exact resolved version
- normalized root feature selection
- rustdoc JSON/source location
- item kind/name, including modules and external crate roots
- absolute file + line where declared when available
- definition/signature, including higher-ranked function-pointer binders
- deprecation details (`since` and `note`) when present
- semantic attributes: `repr`, `non_exhaustive`, and `must_use`
- struct/union fields with their Rust visibility; definitions contain an explicit
  `private/stripped` comment and details contain a matching note when fields are hidden
- enum variants, including explicit discriminants; Rustdoc placeholder expressions
  use their evaluated value when one is available
- trait associated items, including provided associated-constant defaults
- constant and static initializers when Rustdoc preserves them; extern statics are
  labeled with their access safety instead of being shown with an invented initializer
- derives active for the selected Cargo unit when Rustdoc exposes them via top-level
  attrs, matching `cfg_attr` payloads, or derived impls
- inherent public methods and associated constants for structs/enums/unions,
  each with its enclosing implementation type, generics, and where-clause
- nested field, variant, and associated-member documentation, semantic attributes,
  and deprecation details attached to the member they describe
- direct non-blanket trait impls
- Rust doc comments

## When to use

Use when:

- User asks what an imported Rust function takes or returns.
- User asks what fields a struct has.
- User asks what variants an enum has.
- User asks what methods exist on a type.
- User asks what traits/derives a type implements.
- User asks what a trait requires.
- You see external `use crate_name::module::Item;` and need local docs/signature.
- You need docs for exact `Cargo.lock` version/features, not latest docs.rs.

Do not use for:

- `std`, `core`, or `alloc` items. Use official Rust docs.
- Local crate items (`crate::`, `self::`, `super::`). Inspect source directly.

## Command

```bash
excra 'use crate_name::module::item;' --root /path/to/cargo/project
```

If already in project root:

```bash
excra 'use crate_name::module::item;'
```

Quote the full use line as one shell argument.

## Examples

```bash
excra 'use syn::parse_file;' --root .
excra 'use cargo_metadata::MetadataCommand;' --root /home/adam/Desktop/rust/arbre_v1
excra 'use serde::ser::Serialize;' --root .
excra 'use tokio::sync::{Mutex, RwLock, Semaphore};' --root .
```

Root-package feature selection uses Cargo-compatible flags:

```bash
excra 'use dependency::Extra;' --root . --features extra
excra 'use dependency::Extra;' --root . --all-features
excra 'use dependency::DefaultApi;' --root . --no-default-features
```

`--features` accepts comma- or space-separated feature names and may be repeated.
In a virtual workspace, combine feature flags with `--package`; unqualified feature
names apply to that selected package just as they do for `cargo check -p`.

## Supported input

- External crate paths: `use serde::Serialize;`
- Multi-item brace imports: `use tokio::sync::{Mutex, RwLock, Semaphore};`
- Single-item brace imports: `use syn::{File};`
- Nested brace imports: `use tower::{service_fn, util::{MapResponseLayer}};`
- Renamed imports resolve original item: `use syn::File as SynFile;`
- `self` imports inside non-root braces: `use tokio::sync::{self, mpsc};`.
  These bind only the parent's type namespace, matching Rust import semantics;
  displayed imports retain `::{self}` so they can be queried again.
- Modules are supported and labeled: `use tokio::sync::watch;` -> `item: module watch`
- External crate-root re-exports are labeled as crates and use an explicit unsupported
  definition marker rather than inventing a module declaration.
- Enum variants are supported: `use facade::Number::One;`
- Raw identifiers are supported in crate aliases and item paths; declarations restore
  the `r#` prefix wherever Rustdoc reports an unescaped keyword name.
- Concrete items behind public external globs and multi-package re-export chains
  are followed through the resolved Cargo dependency graph.
- Public `extern crate` aliases, intermediate module-use aliases, and finite
  repeated module-alias paths are traversed while preserving Cargo dependency renames.
- Equivalent local and external re-export routes to the same item in the same exact Cargo unit count
  as one definition; distinct definitions and namespace conflicts remain ambiguous.
  Intermediate components and `::{self}` queries compare only the type namespace.
- Private and restricted intermediate modules shadow glob re-exports just as they
  do in Rust. Rustdoc retains private items internally to detect these bindings,
  including macro-generated ones; reports still exclude private paths and fields.
- Private and restricted final bindings likewise shadow public glob re-exports in
  their exact Rust namespace. Unrelated type, value, or macro namespaces remain usable.
- Root-query candidates are checked with the selected compiler against the exact
  compiled dependency artifact before reporting success. This detects local and external
  private and restricted `use` aliases omitted by Rustdoc, including aliases
  generated by macros, while preserving the dependency's compiled cfgs/features.
  Compiler validation failures are reported rather than accepting an unverified path.
  Validation invokes the selected rustc directly against the compiled artifact;
  Cargo-scoped wrappers run during compilation, artifact discovery, and Rustdoc
  generation, where their configured environment is available. The standalone
  import probe does not replay those wrappers or persist Cargo environment values.
  Limitation: validation uses a standalone `no_std` import probe; it does not
  reproduce consumer crate attributes such as nightly feature gates. Imports
  requiring those attributes may fail validation and need a direct Cargo check.
- Canonical external targets remain usable when Rustdoc strips a private module
  from the syntactic path of a public re-export, including bare crate-root-relative
  paths and source-level `extern crate` aliases.
- Public primitive re-exports such as `pub use i32 as MyI32` are reported with
  both the public use declaration and the built-in primitive identity.
- Constructor namespaces follow external usability: non-exhaustive unit and
  tuple structs/variants are type-only outside their defining crate.

Unsupported:

- glob imports: `use syn::*;`
- absolute leading paths: `use ::syn::File;`
- local paths: `use crate::Thing;`

## Dependency and feature workflow

`excra` asks Cargo for the selected root package's exact unit graph, then
runs the root Cargo operation with an internal compiler wrapper that emits
Rustdoc JSON for the matching dependency unit. Conceptually, the Cargo side is:

```bash
cargo +nightly-2025-09-10 rustdoc --locked -p <selected-root-package> --manifest-path <root>/Cargo.toml
```

Consequences:

- It refuses to create or update `Cargo.lock`; a missing or stale lockfile must be
  refreshed explicitly with `cargo check` or `cargo build`.
- Unit graphs are reused within one query for matching Cargo invocations, including
  batch imports and external routes; distinct dev graphs remain separate.
- It obtains the dependency's context-specific feature unit from Cargo's unit graph
  and emits JSON from that exact compiler invocation, including non-workspace dependencies.
- `--features`, `--all-features`, and `--no-default-features` are forwarded to
  metadata, unit selection, Rustdoc generation, and cache identity as one normalized selection.
  Selecting a member manifest to scope features preserves the original Cargo working
  directory for configuration discovery.
- With no `--target`, it honors Cargo's effective `build.target` configuration (including
  `CARGO_BUILD_TARGET` and Cargo's special `host` value); an explicit `--target`
  remains the highest-precedence override. Explicit `--target host` is forwarded to
  Cargo in that form while metadata and unit matching use Cargo's normalized host triple.
- Compiler-wrapper matching and cache paths include Cargo's compile mode, host/target
  platform, feature set, and complete Cargo profile/codegen identity (including
  normalized omitted defaults). Profile matching uses Cargo-generated options before
  appended compiler flag overrides, so equal-feature units built with different
  profiles cannot overwrite one another's Rustdoc JSON. Rustdoc generation and item
  filtering use the final compiler arguments and cfgs, including those overrides.
- Dev-only queries use Cargo's test graph, build-only queries use the host build unit,
  and external re-export traversal preserves that originating context at every hop.
- Target-specific normal/dev edges are evaluated for the selected target, while
  target-specific build-dependency edges are evaluated separately for the host.
- If one package is selected in multiple contexts (for example, normal and dev with
  different features), each context in which the item exists is reported separately;
  output never labels one generated unit as though it represented all contexts.
- Cargo unit roots and dependency edges select the exact dev/build unit, including
  resolver-v2/v3 graphs that compile one package more than once with different features.
  Recursive re-exports retain the selected parent package and graph-unit identity;
  another version or host/target unit with the same name cannot contribute edges.
- One extern name may identify different packages in disjoint normal, dev, and build
  contexts. Package identities that overlap in one effective context remain an error.
- Cargo's configured workspace compiler wrapper remains in the compiler chain and is
  replayed around the matching Rustdoc invocation.
- Cargo's configured general compiler wrapper also remains in the chain for both
  ordinary compiler work and the matching generated Rustdoc invocation. Relative
  wrapper paths retain Cargo's defining-config origin semantics, bare names use
  `PATH`, and wrapper discovery uses `EXCRA_TOOLCHAIN` consistently.
- The `EXCRA_TOOLCHAIN` selection is resolved once per query and applied to host
  detection, metadata, Cargo configuration, unit-graph resolution, wrapper discovery,
  compilation, and Rustdoc generation; ambient Rustup overrides do not split those phases.
- Each invocation starts from one clean, ownership-marked Cargo target tree under
  `target/excra` and holds its inter-process lock until all reports are emitted.
  Selected Cargo units use distinct subdirectories, so every reported Rustdoc source
  remains available and unit-correct until the next safely locked invocation, while
  retained build artifacts stay bounded to one inactive generation tree. A fresh
  ownership-marked tree is staged and atomically installed, so interrupted staging
  initialization cannot strand the managed cache; unowned paths are still preserved.
- Relative declaration paths are resolved against the captured compiler working
  directory, including when `--root` points at a workspace member.
- Rustdoc's implicit `cfg(doc)` is not treated as part of the selected Cargo unit.
  Retained item cfg expressions are evaluated against the exact non-doc cfg set from
  the matched compiler-wrapper invocation, so documentation-only platform APIs and
  disabled conditional derives are excluded while APIs and derives valid for the
  selected target/profile/features remain available. Conditional `repr`,
  `non_exhaustive`, `must_use`, and deprecation details follow that same cfg set;
  automatically derived impls emitted only under `cfg(doc)` are omitted.
  Derived trait impl headers present only in the selected non-doc compiler
  expansion are included in the report, even when Rustdoc JSON omits them.
  Disabled lexical ancestors also invalidate nested members and impls on types
  declared elsewhere. Inline module
  bodies are recovered from local source because Rustdoc omits their impl children;
  unavailable source needed for this check produces an extraction error.
  If a source cfg affecting the requested item is enabled in the selected
  non-doc build but disabled by Rustdoc's `cfg(doc)`, extraction reports
  incomplete instead of presenting a Rustdoc graph that omits fields,
  variants, methods, or alternate definitions.
  This checks the target source and source files referenced by local Rustdoc
  items, including cfg attributes inside macro bodies. Source that cannot be
  read or parsed also produces an extraction error. The matched Cargo compiler
  unit is also expanded with and without `cfg(doc)`; any requested declaration,
  field, variant, or associated member present only in the normal expansion
  produces an incomplete-extraction error. This covers API emitted by external
  procedural macros and `include!` even when its cfg is absent from local source.
  Public fields, variants, inherent methods and constants, and trait members
  are also checked against the filtered Rustdoc graph. Missing members produce
  an incomplete-extraction error even when both compiler expansions agree,
  such as identical generated members behind opposite `cfg(doc)` conditions.
  If Rustdoc omits a queried name entirely, a compiler-accepted import also
  produces an incomplete-extraction error.
  Raw cfg identifiers are matched
  against rustc's normalized cfg keys, including Unicode identifiers, while their
  raw spelling remains intact in rendered Rust paths.
- It uses exact versions from the project lockfile/resolution.
- It respects dependency renames from `Cargo.toml`.
- It uses exactly the features enabled by the target project.
- It only works for direct dependencies of the selected package.

Normal dependencies are included by default. Direct dev- and build-dependencies
are excluded unless their Cargo contexts are requested explicitly:

```bash
excra 'use dev_dependency::Item;' --root . --include-dev
excra 'use build_dependency::Item;' --root . --include-build
```

Both flags may be supplied together. Each selected context is resolved and reported
separately.

If dependency is missing:

1. Add it to `Cargo.toml` with needed features.
2. Run `cargo check` (or `cargo build`) to update resolution/lockfile.
3. Run `excra` again.

If item is “not found” but you believe it exists:

1. Check whether item is behind a crate feature.
2. Enable feature in `Cargo.toml`.
3. Run `cargo check`.
4. Re-run `excra`.

Example:

```toml
tokio = { version = "1", features = ["rt", "sync"] }
```

Then:

```bash
cargo check
excra 'use tokio::sync::mpsc::Sender;' --root .
```

## Nightly requirement

Rustdoc JSON requires the nightly pinned in `rust-toolchain.toml`, kept in sync
with `rustdoc-types 0.56.x`. Install it with:

```bash
rustup toolchain install nightly-2025-09-10 --component rustfmt clippy llvm-tools-preview
```

Or set:

```bash
EXCRA_TOOLCHAIN=<toolchain> excra 'use dependency::Item;' --root .
```

`EXCRA_TOOLCHAIN` is an advanced whole-query override; the selected Cargo and
Rust compiler must support the target workspace and emit the `rustdoc-types 0.56.x`
schema.

## Agent workflow

1. Locate project root containing `Cargo.toml`.
2. Ensure target crate is a direct dependency. If not, add it with needed features and run `cargo check`.
3. Copy exact external `use` line for target item(s).
4. Prefer batch brace query for related items from same module.
5. Run `excra '<use line>' --root <project-root>`.
6. Use output definition/details/methods/associated constants/impls/derives/docs for implementation or answer.
7. If “not found”, check feature flags before assuming item does not exist.
8. Use web/docs.rs only after local docs fail, are missing, or target item is not a direct dependency.

## Error interpretation

- `not a direct dependency`: add dependency to `Cargo.toml` or query from a project
  where it is direct. If the dependency is already declared only for dev or build,
  the diagnostic instead suggests `--include-dev` or `--include-build` exactly.
- `Rust standard library`: use <https://doc.rust-lang.org/std/>.
- `glob imports are not supported`: query concrete item path.
- `ambiguous across Rust namespaces`: the import spelling denotes more than one
  public symbol (for example, a trait and derive macro). Query a namespace-specific
  canonical path; the tool will not silently choose one rustdoc item.
- `external branches failed`: the item crossed an external re-export or glob, but
  no unique reachable normal dependency branch contained the requested concrete item.
- `compiler rejected import`: Rustdoc suggested an external re-export path that
  the compiled dependency does not expose publicly. Inspect the compiler diagnostic.
- `could not validate import`: the selected compiler could not check the candidate
  against its retained Cargo artifact; inspect the diagnostic before retrying.
- `failed to generate rustdoc JSON`: install/use the pinned nightly, run `cargo check`, inspect Cargo/Rustdoc stderr.
- `query ... is incomplete`: at least one requested dependency context had a Cargo,
  unit-selection, Rustdoc, parse, or ambiguity failure; fix that context before
  treating output from another context as complete. A conclusively absent item may
  still be omitted when it exists in another selected context.
- `failed to read cargo metadata without changing Cargo.lock`: follow lockfile-refresh
  guidance only when Cargo specifically reports a missing or stale lockfile; otherwise
  fix the manifest, target, feature, or other Cargo error shown.
- `item not found`: likely wrong path, private item, disabled feature, target/profile cfg,
  or unsupported Rustdoc shape. Items enabled only by Rustdoc's `cfg(doc)` are intentionally absent.
- `definition rendering unsupported for ...`: Rustdoc identified the item, but its schema does not contain enough source information to print a trustworthy Rust declaration.

## Best practices

- Query concrete item when you need methods/type details: `use tokio::sync::mpsc::Sender;`.
- Module query is OK for module docs: `use tokio::sync::watch;`.
- Prefer batch braces for related items: `use tokio::sync::{Mutex, RwLock, Semaphore};`.
- Prefer canonical module path if root re-export fails.
- For renamed dependencies, use import name used in code, not package name.
- After editing `Cargo.toml`, run `cargo check` before querying.
- Trust local output over docs.rs when versions/features differ.
