use cargo_metadata::{Metadata, Package};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const MEMBERS: &str = "EXCRA_COMPILER_WORKSPACE_MEMBERS";
const GENERAL: &str = "EXCRA_COMPILER_GENERAL_WRAPPER";
const WORKSPACE: &str = "EXCRA_COMPILER_WORKSPACE_WRAPPER";

#[derive(Default, Hash)]
pub(crate) struct Context {
    pub(crate) suppress_target_flags: bool,
    executable: Option<PathBuf>,
    general: Option<OsString>,
    workspace: Option<OsString>,
    members: Vec<PathBuf>,
    encoded_members: String,
}

impl Context {
    pub(crate) fn without_target_flags() -> Self {
        Self {
            suppress_target_flags: true,
            ..Self::default()
        }
    }

    pub(crate) fn new(
        manifest: &Path,
        metadata: &Metadata,
        configuration: &[(PathBuf, String)],
    ) -> Result<Self, String> {
        let workspace = wrapper(
            manifest,
            configuration,
            "rustc-workspace-wrapper",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        );
        let Some(workspace) = workspace else {
            return Ok(Self::default());
        };
        let general = wrapper(
            manifest,
            configuration,
            "rustc-wrapper",
            "RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC_WRAPPER",
        );
        let mut members = metadata
            .packages
            .iter()
            .filter(|package| metadata.workspace_members.contains(&package.id))
            .map(|package| {
                package
                    .manifest_path
                    .parent()
                    .expect("manifest has a parent")
                    .as_std_path()
                    .canonicalize()
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        members.sort();
        Ok(Self {
            suppress_target_flags: false,
            executable: Some(std::env::current_exe().map_err(|error| error.to_string())?),
            general,
            workspace: Some(workspace),
            encoded_members: serde_json::to_string(&members).map_err(|error| error.to_string())?,
            members,
        })
    }

    pub(crate) fn apply(&self, command: &mut Command) {
        if self.suppress_target_flags {
            // An empty encoded value overrides every source of target flags.
            command.env("CARGO_ENCODED_RUSTFLAGS", "");
        }
        if let Some(executable) = &self.executable {
            command
                .env("RUSTC_WRAPPER", executable)
                .env("RUSTC_WORKSPACE_WRAPPER", "")
                .env(MEMBERS, &self.encoded_members)
                .env(GENERAL, self.general.as_deref().unwrap_or_default())
                .env(WORKSPACE, self.workspace.as_deref().unwrap_or_default());
        }
    }

    pub(crate) fn wraps(&self, package: &Package) -> bool {
        package
            .manifest_path
            .parent()
            .and_then(|path| path.as_std_path().canonicalize().ok())
            .is_some_and(|path| self.members.contains(&path))
    }

    pub(crate) fn workspace_enabled(&self) -> bool {
        self.workspace.is_some()
    }
}

fn wrapper(
    manifest: &Path,
    configuration: &[(PathBuf, String)],
    key: &str,
    environment: &str,
    build_environment: &str,
) -> Option<OsString> {
    let configured = if let Some(value) =
        std::env::var_os(environment).or_else(|| std::env::var_os(build_environment))
    {
        Some((
            value,
            manifest
                .parent()
                .expect("manifest has a parent")
                .to_path_buf(),
        ))
    } else {
        configuration.iter().rev().find_map(|(file, contents)| {
            let table = contents.parse::<toml::Table>().ok()?;
            let value = table.get("build")?.get(key)?.as_str()?;
            Some((
                OsString::from(value),
                file.parent()?.parent()?.to_path_buf(),
            ))
        })
    };
    configured.and_then(|(value, base)| {
        if value.is_empty() {
            return None;
        }
        // Cargo searches PATH for bare executable names; other config paths
        // are relative to the directory containing the .cargo directory.
        let path = Path::new(&value);
        Some(if path.components().count() > 1 && path.is_relative() {
            base.join(path).into_os_string()
        } else {
            value
        })
    })
}

pub(crate) fn is_wrapper() -> bool {
    std::env::var_os(MEMBERS).is_some()
}

pub(crate) fn run_wrapper() -> Result<ExitCode, String> {
    let members: Vec<PathBuf> =
        serde_json::from_str(&std::env::var(MEMBERS).map_err(|error| error.to_string())?)
            .map_err(|error| format!("invalid compiler wrapper routing: {error}"))?;
    let mut arguments = std::env::args_os().skip(1);
    let compiler = arguments.next().ok_or("missing wrapped compiler")?;
    let arguments = arguments.collect::<Vec<_>>();
    // Cargo's version/cfg probes have no package context and use its workspace
    // wrapper too. Real compilations are routed by the original membership.
    let probe = arguments.iter().any(|argument| {
        argument == "-vV"
            || argument == "--print"
            || argument
                .to_str()
                .is_some_and(|argument| argument.starts_with("--print="))
    });
    let member = probe
        || std::env::var_os("CARGO_MANIFEST_DIR").is_none_or(|directory| {
            Path::new(&directory)
                .canonicalize()
                .is_ok_and(|path| members.contains(&path))
        });
    let general = std::env::var_os(GENERAL).filter(|value| !value.is_empty());
    let workspace = std::env::var_os(WORKSPACE).filter(|value| !value.is_empty());
    let mut invocation = Vec::new();
    invocation.extend(general.iter().cloned());
    if member {
        invocation.extend(workspace.iter().cloned());
    }
    invocation.push(compiler);
    let mut command = Command::new(&invocation[0]);
    command
        .args(&invocation[1..])
        .args(arguments)
        .env_remove(MEMBERS)
        .env_remove(GENERAL)
        .env_remove(WORKSPACE)
        .env("RUSTC_WRAPPER", general.as_deref().unwrap_or_default())
        .env(
            "RUSTC_WORKSPACE_WRAPPER",
            workspace.as_deref().unwrap_or_default(),
        );
    let status = command
        .status()
        .map_err(|error| format!("cannot run compiler wrapper: {error}"))?;
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}
