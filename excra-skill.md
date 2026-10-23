# excra Skill

Use `excra` for Markdown documentation from the crate versions resolved by the
local Cargo project. Prefer these local docs over docs for another release.

## Query

Pass a complete external `use` declaration as one shell argument:

```bash
excra 'use crate_name::module::Item;'
excra 'use syn::{parse_file, File};' --root /path/to/project
```

Use dependency renames as imported by the consumer. Nested braces, renames,
module imports, raw identifiers, and public re-exports with rustdoc HTML pages
in the requested crate are supported. Variant queries return the containing enum's page.

## Match Cargo selection

```bash
excra 'use dependency::Item;' --package workspace-member
excra 'use dependency::Extra;' --features extra
excra 'use dependency::Extra;' --all-features
excra 'use dependency::Item;' --no-default-features
excra 'use dependency::Item;' --target x86_64-unknown-linux-gnu
excra 'use dev_dependency::Item;' --include-dev
excra 'use build_dependency::Item;' --include-build
```

`--features` accepts comma- or space-separated names and may be repeated.
Virtual workspace roots require `--package`. Dev/build dependencies are
excluded by default. Requested dev/build packages use Cargo-resolved features;
normal/dev documentation shares Cargo's docs, while build documentation uses
a separate host-target invocation. Build-only packages outside the workspace
are supported.

## Read the result

The report contains the resolved version, selected package, dependency context,
root feature selection, `source:` HTML path, `markdown:` path, and converted
Markdown. Read signatures, docs, fields, variants, methods, and implementations
from that page. Markdown preserves relative links to generated HTML and source
pages.

Excra generates complete HTML and Markdown API documentation for each requested
crate, then prints only the requested item pages. It uses an isolated Cargo
manifest with the resolved local source, selected features, and a copy of the
project's lockfile, running `cargo doc --offline --no-deps --lib`. Required crate
dependencies may compile; the consuming app and unrelated dependencies are not
built or documented.

Stable Rust 1.89 or newer suffices. Every subprocess uses `cargo` from `PATH`,
including metadata; `CARGO` does not select the executable. Cargo uses the queried
project's toolchain and configuration; documentation uses the isolated manifest's
default development profile; root manifest profile and patch overrides are not copied.
Generated HTML, Markdown, and build artifacts remain under Cargo's
`target/excra/docs-*`. Matching selections reuse build artifacts and unchanged
Markdown. Cargo progress appears on stderr.

## Scope and failures

- Query direct external dependencies. Glob imports, local paths, and
  `std`/`core`/`alloc` are unsupported.
- Public pages follow rustdoc semantics, including `cfg(doc)`; output is not
  checked against a separate compiler expansion.
- Hidden items without public HTML pages are unavailable. Enum variants share
  their parent enum's Markdown.
- External module re-exports without a page in the requested crate are unavailable.
- Ambiguous names are errors.
- Non-inlined glob re-exports are unavailable when source inspection cannot
  rule out shadowing, including unknown attributes, expansions, conditional
  selection, or missing source.
- The requested crate must successfully document. Inspect
  reported Cargo/rustdoc diagnostics when generation fails.
- A missing/stale `Cargo.lock` must be refreshed explicitly with `cargo check`
  or `cargo build`. Excra leaves the project's lockfile unchanged.
