# Excra

**Cargo-resolved Rust documentation for coding agents.**

Excra takes a Rust `use` declaration, resolves its direct dependency in your
Cargo project, generates ordinary rustdoc HTML with stable Cargo, and converts
every API page of the requested crate to Markdown with
[html-to-markdown-rs](https://crates.io/crates/html-to-markdown-rs).

```bash
excra 'use syn::{parse_file, File};' --root /path/to/project
```

Each report includes the resolved crate version, selected package, dependency
context, root feature selection, HTML and Markdown paths, and only the requested
items' converted pages. Signatures, documentation, fields, variants, methods,
and implementations come directly from rustdoc's HTML.

## Install

Rust 1.89 or newer is required. Use your ordinary stable Rust installation:

```bash
cargo install excra --locked
```

The queried project must have a current `Cargo.lock`. Excra resolves it with
`cargo metadata --locked`; it never creates or updates the project's lockfile.
Refresh a missing or stale lockfile with `cargo check` or `cargo build`, then retry.

## Query a project

Pass a complete external `use` declaration as one shell argument. The current
directory is the default root.

```bash
excra 'use syn::parse_file;'
excra 'use tokio::sync::{Mutex, RwLock, Semaphore};' --root /path/to/project
excra 'use dependency::Extra;' --features extra
excra 'use dependency::Extra;' --all-features
excra 'use dependency::DefaultApi;' --no-default-features
excra 'use dependency::Item;' --package workspace-member
excra 'use dependency::Item;' --target x86_64-unknown-linux-gnu
```

`--features` accepts comma- or space-separated names and may be repeated.
Virtual workspace roots require `--package`. Dependency renames use the name
imported by the consumer.

Normal direct dependencies are queried by default. Additional contexts are
opt-in:

```bash
excra 'use dev_dependency::Item;' --include-dev
excra 'use build_dependency::Item;' --include-build
```

Requested crates use features from Cargo's selected package, dependency context,
and effective target. Dev features join normal documentation with `--include-dev`;
build features stay separate under resolvers 2 and 3. Build documentation uses
the compiler host. Build-only dependencies outside the workspace are supported.

## How it works

The normal pipeline is:

```text
cargo metadata --locked
cargo tree --offline --locked --package <consumer>
cargo doc --offline --locked --no-deps --lib --package <requested-crate>
all requested-crate HTML API pages → html-to-markdown-rs → .md files
requested item pages → stdout
```

Excra creates a small isolated Cargo manifest preserving the resolved crate's
source identity, workspace resolver, applicable features throughout its dependency
closure, and a copy of the project's lockfile. It verifies the isolated package,
host/target, and feature contexts before documentation, including locked Git revisions.
Only that crate is documented. Its required dependencies may be compiled;
the consuming application and unrelated dependencies are not built or documented.
Workspace compiler wrappers retain their original package membership and compose
inside a general compiler wrapper, as in the queried workspace.
Every subprocess uses `cargo` from `PATH`, including metadata; the `CARGO`
environment variable does not select the executable. Cargo runs from the queried
project's directory, using its installed toolchain and Cargo configuration.
With no `--target`, Cargo handles its configured target.
The isolated manifest uses its default development profile; root manifest
profile and patch overrides are not copied.

HTML, Markdown, and build artifacts remain under `target/excra/docs-*`, relative
to Cargo's target directory. Matching crate, feature, target, lockfile, Cargo
configuration, compiler, and rustdoc flag selections reuse build artifacts and
unchanged Markdown. Other selections use separate directories. Each crate's cache
stays locked until its requested reports have been copied into memory.
Cargo progress is displayed on stderr. Remove
`target/excra` when you no longer need the generated files.

Markdown files sit beside their HTML pages, keeping relative source and API
links usable. Navigation and interface controls are omitted from Markdown.

## Supported scope

Concrete paths, nested brace imports, renames, modules, enum variants, raw
identifiers, and public re-exports with pages inside the requested crate are
supported. Enum variant queries return the containing enum page. Ambiguous item
names are reported as errors.

Glob imports, local paths (`crate`, `self`, `super`), standard-library crates
(`std`, `core`, `alloc`), and dependencies not exposed through a direct
dependency are outside the query scope. Hidden items without a public rustdoc
page cannot be reported. External module re-exports without a page in the
requested crate are unavailable. Cargo failures are reported as errors.

Output follows ordinary rustdoc semantics, including `cfg(doc)`. Excra does
not reconstruct or validate a separate compiler-expanded API. The requested
crate must successfully document with `cargo doc`.

For agent instructions, see [excra-skill.md](excra-skill.md).

## License

Licensed under either Apache License 2.0 or MIT, at your option.
