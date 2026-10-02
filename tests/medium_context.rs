mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use tempfile::TempDir;

    fn write_package(workspace: &TempDir, name: &str, manifest: &str, source: &str) {
        let package = workspace.path().join(name);
        fs::create_dir_all(package.join("src")).unwrap();
        fs::write(package.join("Cargo.toml"), manifest).unwrap();
        fs::write(package.join("src/lib.rs"), source).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn external_probe_skips_cargo_scoped_wrapper_and_uses_compiler_span_directory() {
        let workspace = TempDir::new().unwrap();
        fs::create_dir_all(workspace.path().join(".cargo")).unwrap();
        fs::write(
            workspace.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"facade\", \"origin\"]\nresolver = \"3\"\n",
        )
        .unwrap();
        write_package(
            &workspace,
            "app",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nfacade = { path = \"../facade\" }\n",
            "",
        );
        write_package(
            &workspace,
            "facade",
            "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\norigin = { path = \"../origin\" }\n",
            "pub use origin::Thing;\n",
        );
        write_package(
            &workspace,
            "origin",
            "[package]\nname = \"origin\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            "pub struct Thing;\n",
        );

        let wrapper = workspace.path().join("required-env-wrapper.sh");
        fs::write(
            &wrapper,
            "#!/bin/sh\n[ \"$EXCRA_WRAPPER_CONTEXT\" = cargo-only ] || exit 41\ncase \"$*\" in *'--crate-name origin'*) [ \"$CARGO_PKG_NAME\" = origin ] || exit 42;; *'--crate-name facade'*) [ \"$CARGO_PKG_NAME\" = facade ] || exit 43;; esac\nprintf '%s\\n' \"$*\" >> \"$EXCRA_WRAPPER_LOG\"\nexec \"$@\"\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&wrapper, permissions).unwrap();
        let wrapper_log = workspace.path().join("wrapper.log");
        fs::write(
            workspace.path().join(".cargo/config.toml"),
            format!(
                "[build]\nrustc-wrapper = {:?}\n\n[env]\nEXCRA_WRAPPER_CONTEXT = {{ value = \"cargo-only\", force = true }}\nEXCRA_WRAPPER_LOG = {{ value = {:?}, force = true }}\n",
                wrapper.to_str().unwrap(),
                wrapper_log.to_str().unwrap(),
            ),
        )
        .unwrap();
        let lock = Command::new("cargo")
            .args(["generate-lockfile", "--offline", "--manifest-path"])
            .arg(workspace.path().join("Cargo.toml"))
            .output()
            .unwrap();
        assert!(
            lock.status.success(),
            "{}",
            String::from_utf8_lossy(&lock.stderr)
        );

        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                "use facade::Thing;",
                "--root",
                workspace.path().join("app").to_str().unwrap(),
            ])
            .env_remove("EXCRA_WRAPPER_CONTEXT")
            .env_remove("EXCRA_WRAPPER_LOG")
            .env_remove("RUSTC_WRAPPER")
            .env_remove("CARGO_BUILD_RUSTC_WRAPPER")
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("definition: pub struct Thing;"), "{stdout}");
        assert!(
            stdout.contains(&format!(
                "location: {}:1",
                workspace.path().join("origin/src/lib.rs").display()
            )),
            "{stdout}"
        );
        let json_path = std::path::Path::new(
            stdout
                .lines()
                .find_map(|line| line.strip_prefix("source: "))
                .unwrap(),
        );
        let raw: serde_json::Value = serde_json::from_slice(&fs::read(json_path).unwrap()).unwrap();
        let thing = raw["index"]
            .as_object()
            .unwrap()
            .values()
            .find(|item| item["name"] == "Thing" && item["inner"].get("struct").is_some())
            .unwrap();
        let filename = thing["span"]["filename"].as_str().unwrap();
        assert!(
            std::path::Path::new(filename).is_relative(),
            "expected the member-root relative-span reproduction: {filename}"
        );
        let probe: serde_json::Value =
            serde_json::from_slice(&fs::read(json_path.with_extension("probe")).unwrap()).unwrap();
        assert_eq!(probe["directory"], workspace.path().to_str().unwrap());
        assert_eq!(
            workspace.path().join(filename),
            workspace.path().join("origin/src/lib.rs")
        );
        assert!(
            !fs::read_to_string(wrapper_log)
                .unwrap()
                .contains("excra_import_probe")
        );
    }

    #[test]
    fn transitive_query_follows_the_selected_parent_cargo_unit() {
        transitive_query(false);
        transitive_query(true);
    }

    #[test]
    fn dependency_queries_do_not_require_consumer_documentation() {
        let workspace = TempDir::new().unwrap();
        fs::write(
            workspace.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"facade\", \"origin\"]\nresolver = \"3\"\n",
        )
        .unwrap();
        let app_manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\nautolib = false\nautobins = false\n[features]\nextra = [\"facade/extra\"]\n[dependencies]\nfacade = { path = \"../facade\" }\n";
        write_package(&workspace, "app", &format!("{app_manifest}[lib]\n"), "");
        write_package(
            &workspace,
            "facade",
            "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[features]\nextra = [\"origin/extra\"]\n[dependencies]\norigin = { path = \"../origin\" }\n",
            "pub struct Thing; pub use origin::Versioned;\n",
        );
        write_package(
            &workspace,
            "origin",
            "[package]\nname = \"origin\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[features]\nextra = []\n",
            "#[cfg(feature = \"extra\")] pub struct Versioned { pub extra: u8 }\n#[cfg(not(feature = \"extra\"))] pub struct Versioned { pub basic: u16 }\n",
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
        fs::write(workspace.path().join("app/src/other.rs"), "fn main() {}\n").unwrap();

        for (targets, filename, source) in [
            ("[lib]\ndoc = false\n", "lib.rs", ""),
            ("[lib]\n", "lib.rs", "use facade::DoesNotExist;\n"),
            (
                "[[bin]]\nname = \"app\"\npath = \"src/main.rs\"\ndoc = false\n",
                "main.rs",
                "fn main() {}\n",
            ),
            (
                "[[bin]]\nname = \"app\"\npath = \"src/main.rs\"\n",
                "main.rs",
                "use facade::DoesNotExist; fn main() {}\n",
            ),
            (
                "[[bin]]\nname = \"app\"\npath = \"src/main.rs\"\ndoc = false\n[[bin]]\nname = \"other\"\npath = \"src/other.rs\"\ndoc = false\n",
                "main.rs",
                "use facade::DoesNotExist; fn main() {}\n",
            ),
        ] {
            fs::write(
                workspace.path().join("app/Cargo.toml"),
                format!("{app_manifest}{targets}"),
            )
            .unwrap();
            fs::write(workspace.path().join("app/src").join(filename), source).unwrap();
            for extra in [false, true] {
                let mut command = Command::new(env!("CARGO_BIN_EXE_excra"));
                command
                    .arg("use facade::{Thing, Versioned};")
                    .arg("--root")
                    .arg(workspace.path())
                    .args(["--package", "app"])
                    .env("CARGO_TARGET_DIR", workspace.path().join("target"))
                    .env("CARGO_NET_OFFLINE", "true")
                    .env("RUSTDOCFLAGS", "--this-flag-is-invalid")
                    .env("CARGO_ENCODED_RUSTDOCFLAGS", "--another-invalid-flag");
                if extra {
                    command.args(["--features", "extra"]);
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "{targets}: {source}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(stdout.contains("definition: pub struct Thing;"), "{stdout}");
                let field = if extra { "extra: u8" } else { "basic: u16" };
                assert!(
                    stdout.contains(&format!(
                        "definition: pub struct Versioned {{ pub {field} }}"
                    )),
                    "{stdout}"
                );
            }
        }
        let manifest = workspace.path().join("app/Cargo.toml");
        let contents = fs::read_to_string(&manifest).unwrap();
        fs::write(
            manifest,
            format!("{contents}[build-dependencies]\nfacade = {{ path = \"../facade\", features = [\"extra\"] }}\n"),
        )
        .unwrap();
        fs::write(
            workspace.path().join("app/build.rs"),
            "fn main() { let _ = facade::Versioned { extra: 1 }; }\n",
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .arg("use facade::Versioned;")
            .arg("--root")
            .arg(workspace.path())
            .args(["--package", "app", "--include-build"])
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("basic: u16") && stdout.contains("extra: u8"),
            "{stdout}"
        );

        fs::write(
            workspace.path().join("app/build.rs"),
            "fn main() { panic!(\"consumer build script intentionally fails\"); }\n",
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .arg("use facade::Thing;")
            .arg("--root")
            .arg(workspace.path())
            .args(["--package", "app"])
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("consumer build script intentionally fails")
        );

        fs::write(
            workspace.path().join("origin/src/lib.rs"),
            "compile_error!(\"selected dependency intentionally fails\"); pub struct Versioned;\n",
        )
        .unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .arg("use facade::Versioned;")
            .arg("--root")
            .arg(workspace.path())
            .args(["--package", "app"])
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("selected dependency intentionally fails")
        );
    }

    fn transitive_query(same_package: bool) {
        let workspace = TempDir::new().unwrap();
        fs::write(
            workspace.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"facade\", \"macro_dep\", \"shared\"]\nexclude = [\"facade_host\"]\nresolver = \"3\"\n",
        )
        .unwrap();
        write_package(
            &workspace,
            "app",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dev-dependencies]\nfacade = { path = \"../facade\", features = [\"direct\"] }\nmacro_dep = { path = \"../macro_dep\" }\n",
            "",
        );
        write_package(
            &workspace,
            "facade",
            "[package]\nname = \"facade\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\ndirect = [\"shared/direct\"]\nmacro = [\"shared/macro\"]\n\n[dependencies]\nshared = { path = \"../shared\" }\n",
            "pub use shared::Thing;\n",
        );
        write_package(
            &workspace,
            "macro_dep",
            "[package]\nname = \"macro_dep\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[lib]\nproc-macro = true\n\n[dependencies]\nfacade = { path = \"../facade_host\", features = [\"macro\"] }\n",
            "extern crate proc_macro;\nuse proc_macro::TokenStream;\n#[proc_macro]\npub fn passthrough(input: TokenStream) -> TokenStream { input }\n",
        );
        write_package(
            &workspace,
            "facade_host",
            "[package]\nname = \"facade\"\nversion = \"0.2.0\"\nedition = \"2024\"\n\n[features]\nmacro = [\"shared/macro\"]\n\n[dependencies]\nshared = { path = \"../shared\" }\n",
            "pub use shared::Thing;\n",
        );
        write_package(
            &workspace,
            "shared",
            "[package]\nname = \"shared\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[features]\ndirect = []\nmacro = []\n",
            "#[cfg(feature = \"direct\")]\npub struct Thing { pub direct: u8 }\n#[cfg(all(feature = \"macro\", not(feature = \"direct\")))]\npub struct Thing { pub from_macro: u8 }\n",
        );
        if same_package {
            let manifest = workspace.path().join("macro_dep/Cargo.toml");
            fs::write(
                &manifest,
                fs::read_to_string(&manifest)
                    .unwrap()
                    .replace("../facade_host", "../facade"),
            )
            .unwrap();
        }
        let lock = Command::new("cargo")
            .args(["generate-lockfile", "--offline", "--manifest-path"])
            .arg(workspace.path().join("Cargo.toml"))
            .output()
            .unwrap();
        assert!(
            lock.status.success(),
            "{}",
            String::from_utf8_lossy(&lock.stderr)
        );

        let graph_output = Command::new("cargo")
            .arg("+nightly-2025-09-10")
            .args([
                "test",
                "--no-run",
                "--locked",
                "-p",
                "app",
                "--unit-graph",
                "-Z",
                "unstable-options",
            ])
            .current_dir(workspace.path())
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();
        assert!(
            graph_output.status.success(),
            "{}",
            String::from_utf8_lossy(&graph_output.stderr)
        );
        let graph: serde_json::Value = serde_json::from_slice(&graph_output.stdout).unwrap();
        let units = graph["units"].as_array().unwrap();
        let facade_units = units
            .iter()
            .filter(|unit| unit["target"]["name"] == "facade" && unit["mode"] == "build")
            .collect::<Vec<_>>();
        assert_eq!(facade_units.len(), 2, "{graph}");
        let parent = |feature: &str| {
            *facade_units
                .iter()
                .find(|unit| {
                    unit["features"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|value| value == feature)
                })
                .unwrap()
        };
        let direct = parent("direct");
        let via_macro = parent("macro");
        assert!(
            direct["platform"].is_null() && via_macro["platform"].is_null(),
            "{graph}"
        );
        assert_eq!(
            direct["pkg_id"] == via_macro["pkg_id"],
            same_package,
            "{graph}"
        );
        let shared_edge = |parent: &serde_json::Value| {
            parent["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .map(|edge| edge["index"].as_u64().unwrap() as usize)
                .find(|index| units[*index]["target"]["name"] == "shared")
                .unwrap()
        };
        let direct_shared = shared_edge(direct);
        let macro_shared = shared_edge(via_macro);
        assert_ne!(direct_shared, macro_shared, "{graph}");
        assert_eq!(
            units[direct_shared]["features"],
            serde_json::json!(["direct"])
        );
        assert_eq!(
            units[macro_shared]["features"],
            serde_json::json!(["macro"])
        );
        let shared_units = units
            .iter()
            .filter(|unit| unit["target"]["name"] == "shared" && unit["mode"] == "build")
            .collect::<Vec<_>>();
        assert_eq!(shared_units.len(), 2, "{graph}");

        let output = Command::new(env!("CARGO_BIN_EXE_excra"))
            .args([
                "use facade::Thing;",
                "--root",
                workspace.path().to_str().unwrap(),
                "--package",
                "app",
                "--include-dev",
            ])
            .env("CARGO_TARGET_DIR", workspace.path().join("target"))
            .env("CARGO_NET_OFFLINE", "true")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("definition: pub struct Thing"), "{stdout}");
        assert!(stdout.contains("direct: u8"), "{stdout}");
        assert!(!stdout.contains("from_macro: u8"), "{stdout}");
    }
}
