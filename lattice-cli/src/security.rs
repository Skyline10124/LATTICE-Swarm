use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use lattice_agent::{
    audit::AuditLog,
    hook::{HookChain, TirithHook},
    DefaultToolExecutor, SandboxConfig,
};

use crate::config::SecurityConfig;

const PROJECT_COMMAND_ALLOWLIST: &[&str] = &[
    "cargo test",
    "cargo clippy",
    "cargo build",
    "cargo fmt",
    "git status",
    "git diff",
    "git show",
    "git log",
    "git grep",
    "git ls-files",
    "git rev-parse",
    "git branch",
    "rg",
    "grep",
    "find",
    "ls",
    "ps",
    "pwd",
];

#[derive(Clone)]
pub(crate) struct RuntimeSecurity {
    pub(crate) sandbox: SandboxConfig,
    pub(crate) audit: Option<Arc<AuditLog>>,
    pub(crate) mode_label: String,
}

pub(crate) fn build_runtime_security(
    config: &SecurityConfig,
    workdir: &Path,
) -> Result<RuntimeSecurity> {
    let mode = config.sandbox_mode.trim().to_ascii_lowercase();
    let workdir = canonical_or_original(workdir);
    let mut sandbox = match mode.as_str() {
        "project" | "" => {
            let mut sandbox = SandboxConfig::project_dirs(
                effective_allowlist(&config.write_allowlist, &[workdir.clone()], &workdir),
                effective_commands(config, PROJECT_COMMAND_ALLOWLIST),
            );
            sandbox.read_allowlist =
                effective_allowlist(&config.read_allowlist, &[workdir.clone()], &workdir);
            sandbox.sandbox_label = "project".into();
            sandbox
        }
        "strict" | "default" => {
            let mut sandbox = SandboxConfig::default();
            apply_optional_allowlists(&mut sandbox, config, &workdir, PROJECT_COMMAND_ALLOWLIST);
            sandbox.sandbox_label = "strict".into();
            sandbox
        }
        "permissive" | "yolo" => {
            let mut sandbox = SandboxConfig::permissive();
            apply_optional_allowlists(&mut sandbox, config, &workdir, &[]);
            sandbox.sandbox_label = "permissive".into();
            sandbox
        }
        "off" => {
            let mut sandbox = SandboxConfig::permissive();
            apply_optional_allowlists(&mut sandbox, config, &workdir, &[]);
            sandbox.hook_chain = None;
            sandbox.sandbox_label = "off".into();
            sandbox
        }
        other => {
            return Err(anyhow!(
                "unknown security.sandbox_mode '{}'; expected project, strict, permissive, or off",
                other
            ));
        }
    };

    apply_limits(&mut sandbox, config);
    if config.hook_chain && mode != "off" {
        sandbox.hook_chain = Some(Arc::new(HookChain::new(vec![Box::new(TirithHook::new())])));
    }
    if config.landlock {
        sandbox.landlock = Some(Default::default());
    }

    let audit = if config.audit {
        Some(Arc::new(AuditLog::new(audit_dir(config))))
    } else {
        None
    };
    if let Some(ref audit) = audit {
        sandbox.audit_log = Some(Arc::clone(audit));
    }

    Ok(RuntimeSecurity {
        sandbox,
        audit,
        mode_label: mode_label(&mode),
    })
}

pub(crate) fn default_runtime_security(workdir: &Path) -> Result<RuntimeSecurity> {
    build_runtime_security(&SecurityConfig::default(), workdir)
}

pub(crate) fn build_tool_executor(
    workdir: &Path,
    security: &RuntimeSecurity,
) -> Result<DefaultToolExecutor> {
    Ok(DefaultToolExecutor::new(workdir.to_string_lossy().as_ref())
        .map_err(anyhow::Error::msg)?
        .with_sandbox(security.sandbox.clone()))
}

pub(crate) async fn reap_audit(security: &RuntimeSecurity) {
    if let Some(ref audit) = security.audit {
        audit.reap_background_tasks().await;
        audit.rotate().await;
    }
}

fn apply_optional_allowlists(
    sandbox: &mut SandboxConfig,
    config: &SecurityConfig,
    workdir: &Path,
    default_commands: &[&str],
) {
    if !config.read_allowlist.is_empty() {
        sandbox.read_allowlist = resolve_allowlist(&config.read_allowlist, workdir);
    }
    if !config.write_allowlist.is_empty() {
        sandbox.write_allowlist = resolve_allowlist(&config.write_allowlist, workdir);
    }
    if !config.command_allowlist.is_empty() || !default_commands.is_empty() {
        sandbox.command_allowlist = effective_commands(config, default_commands);
    }
}

fn apply_limits(sandbox: &mut SandboxConfig, config: &SecurityConfig) {
    if let Some(value) = config.max_command_timeout {
        sandbox.max_command_timeout = value;
    }
    if let Some(value) = config.max_read_size {
        sandbox.max_read_size = value;
    }
    if let Some(value) = config.max_write_size {
        sandbox.max_write_size = value;
    }
    if let Some(value) = config.max_http_response_size {
        sandbox.max_http_response_size = value;
    }
}

fn effective_allowlist(configured: &[String], defaults: &[PathBuf], workdir: &Path) -> Vec<String> {
    if configured.is_empty() {
        defaults
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect()
    } else {
        resolve_allowlist(configured, workdir)
    }
}

fn resolve_allowlist(paths: &[String], workdir: &Path) -> Vec<String> {
    paths
        .iter()
        .map(|path| {
            let path = PathBuf::from(path);
            let absolute = if path.is_absolute() {
                path
            } else {
                workdir.join(path)
            };
            canonical_or_original(&absolute)
                .to_string_lossy()
                .to_string()
        })
        .collect()
}

fn effective_commands(config: &SecurityConfig, defaults: &[&str]) -> Vec<String> {
    if config.command_allowlist.is_empty() {
        defaults.iter().map(|entry| (*entry).to_string()).collect()
    } else {
        config.command_allowlist.clone()
    }
}

fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn audit_dir(config: &SecurityConfig) -> PathBuf {
    config
        .audit_dir
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::config_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("lattice")
                .join("audit")
        })
}

fn mode_label(mode: &str) -> String {
    match mode {
        "" => "project".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_mode_restricts_paths_to_workdir() {
        let workdir = std::env::current_dir().unwrap();
        let security = build_runtime_security(&SecurityConfig::default(), &workdir).unwrap();

        assert_eq!(security.mode_label, "project");
        assert_eq!(
            security.sandbox.read_allowlist,
            vec![std::fs::canonicalize(&workdir)
                .unwrap()
                .to_string_lossy()
                .to_string()]
        );
        assert!(security
            .sandbox
            .command_allowlist
            .contains(&"cargo test".to_string()));
        assert!(security.sandbox.hook_chain.is_some());
    }

    #[test]
    fn custom_command_allowlist_overrides_project_defaults() {
        let workdir = std::env::current_dir().unwrap();
        let config = SecurityConfig {
            command_allowlist: vec!["just test".into()],
            ..SecurityConfig::default()
        };
        let security = build_runtime_security(&config, &workdir).unwrap();

        assert_eq!(security.sandbox.command_allowlist, vec!["just test"]);
    }

    #[test]
    fn permissive_mode_clears_command_allowlist() {
        let workdir = std::env::current_dir().unwrap();
        let config = SecurityConfig {
            sandbox_mode: "permissive".into(),
            ..SecurityConfig::default()
        };
        let security = build_runtime_security(&config, &workdir).unwrap();

        assert!(security.sandbox.command_allowlist.is_empty());
        assert_eq!(security.mode_label, "permissive");
    }

    #[test]
    fn invalid_mode_errors() {
        let workdir = std::env::current_dir().unwrap();
        let config = SecurityConfig {
            sandbox_mode: "unknown".into(),
            ..SecurityConfig::default()
        };

        let err = match build_runtime_security(&config, &workdir) {
            Ok(_) => panic!("invalid mode should fail"),
            Err(err) => err,
        };
        assert!(err.to_string().contains("unknown security.sandbox_mode"));
    }
}
