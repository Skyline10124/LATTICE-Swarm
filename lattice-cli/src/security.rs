use std::path::{Path, PathBuf};

use anyhow::Result;
use lattice::runtime::RuntimeSecurityConfig;

use crate::config::SecurityConfig;

pub(crate) type RuntimeSecurity = lattice::runtime::RuntimeSecurity;

pub(crate) fn build_runtime_security(
    config: &SecurityConfig,
    workdir: &Path,
) -> Result<RuntimeSecurity> {
    Ok(lattice::runtime::build_runtime_security(
        &runtime_security_config(config),
        workdir,
    )?)
}

pub(crate) fn default_runtime_security(workdir: &Path) -> Result<RuntimeSecurity> {
    build_runtime_security(&SecurityConfig::default(), workdir)
}

pub(crate) async fn reap_audit(security: &RuntimeSecurity) {
    security.reap_audit().await;
}

pub(crate) fn runtime_security_config(config: &SecurityConfig) -> RuntimeSecurityConfig {
    RuntimeSecurityConfig {
        sandbox_mode: config.sandbox_mode.clone(),
        read_allowlist: config.read_allowlist.clone(),
        write_allowlist: config.write_allowlist.clone(),
        command_allowlist: config.command_allowlist.clone(),
        hook_chain: config.hook_chain,
        landlock: config.landlock,
        audit: config.audit,
        audit_dir: config.audit_dir.as_deref().map(PathBuf::from),
        max_command_timeout: config.max_command_timeout,
        max_read_size: config.max_read_size,
        max_write_size: config.max_write_size,
        max_http_response_size: config.max_http_response_size,
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
