use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const DEFAULT_INSTALL_CONTEXT_PATH: &str = "/etc/aether/install.yaml";

fn default_release_channel() -> String {
    "stable".to_owned()
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct InstallContext {
    pub(crate) mode: String,
    #[serde(rename = "config_dir")]
    pub(crate) config_directory: PathBuf,
    #[serde(rename = "data_dir")]
    pub(crate) data_directory: PathBuf,
    #[serde(rename = "runtime_dir")]
    pub(crate) runtime_directory: PathBuf,
    #[serde(default = "default_release_channel")]
    pub(crate) channel: String,
    #[serde(default)]
    pub(crate) packs: Vec<String>,
}

impl InstallContext {
    fn load_optional(context_path: &Path) -> Result<Option<Self>> {
        let serialized_context = match std::fs::read_to_string(context_path) {
            Ok(serialized_context) => serialized_context,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to read install context {}", context_path.display())
                });
            },
        };

        let install_context =
            serde_yml::from_str::<Self>(&serialized_context).with_context(|| {
                format!("failed to parse install context {}", context_path.display())
            })?;
        install_context.validate(context_path)?;
        Ok(Some(install_context))
    }

    fn validate(&self, context_path: &Path) -> Result<()> {
        if self.mode.trim().is_empty() {
            bail!(
                "install context {} has an empty mode",
                context_path.display()
            );
        }
        if self.channel.trim().is_empty() {
            bail!(
                "install context {} has an empty release channel",
                context_path.display()
            );
        }
        if let Some(empty_pack_index) = self.packs.iter().position(|pack| pack.trim().is_empty()) {
            bail!(
                "install context {} has an empty pack at index {}",
                context_path.display(),
                empty_pack_index
            );
        }

        validate_absolute_directory(context_path, "config_dir", self.config_directory.as_path())?;
        validate_absolute_directory(context_path, "data_dir", self.data_directory.as_path())?;
        validate_absolute_directory(
            context_path,
            "runtime_dir",
            self.runtime_directory.as_path(),
        )
    }
}

fn validate_absolute_directory(
    context_path: &Path,
    field_name: &str,
    directory: &Path,
) -> Result<()> {
    if directory.as_os_str().is_empty() {
        bail!(
            "install context {} has an empty {}",
            context_path.display(),
            field_name
        );
    }
    if !directory.is_absolute() {
        bail!(
            "install context {} requires an absolute {}, got {}",
            context_path.display(),
            field_name,
            directory.display()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedInstallPaths {
    pub(crate) config_directory: PathBuf,
    pub(crate) data_directory: PathBuf,
    pub(crate) install_mode: Option<String>,
}

pub(crate) struct InstallPathSources<'source> {
    pub(crate) command_line_config_directory: Option<PathBuf>,
    pub(crate) command_line_data_directory: Option<PathBuf>,
    pub(crate) environment_config_directory: Option<PathBuf>,
    pub(crate) environment_data_directory: Option<PathBuf>,
    pub(crate) install_context: Option<&'source InstallContext>,
    pub(crate) working_directory: &'source Path,
}

/// Resolve CLI filesystem paths with this precedence, independently per path:
/// command-line argument > environment variable > install context > current
/// working directory. Installed distributions must publish an install context;
/// an unregistered old directory is never adopted implicitly.
pub(crate) fn resolve_install_paths(
    command_line_config_directory: Option<PathBuf>,
    command_line_data_directory: Option<PathBuf>,
) -> Result<ResolvedInstallPaths> {
    let environment_config_directory = non_empty_environment_path("AETHER_CONFIG_PATH");
    let environment_data_directory = non_empty_environment_path("AETHER_DATA_PATH");

    let context_path = non_empty_environment_path("AETHER_INSTALL_CONTEXT_PATH")
        .unwrap_or_else(|| PathBuf::from(DEFAULT_INSTALL_CONTEXT_PATH));
    let install_context = InstallContext::load_optional(&context_path)?;

    let working_directory =
        std::env::current_dir().context("failed to determine current directory")?;
    Ok(resolve_install_paths_from_sources(InstallPathSources {
        command_line_config_directory,
        command_line_data_directory,
        environment_config_directory,
        environment_data_directory,
        install_context: install_context.as_ref(),
        working_directory: &working_directory,
    }))
}

fn make_absolute(directory: PathBuf, working_directory: &Path) -> PathBuf {
    if directory.is_absolute() {
        directory
    } else {
        working_directory.join(directory)
    }
}

fn non_empty_environment_path(variable_name: &str) -> Option<PathBuf> {
    std::env::var_os(variable_name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn resolve_install_paths_from_sources(
    sources: InstallPathSources<'_>,
) -> ResolvedInstallPaths {
    let working_data_directory = sources.working_directory.join("data");
    let context_config_directory = sources
        .install_context
        .map(|context| context.config_directory.clone());
    let context_data_directory = sources
        .install_context
        .map(|context| context.data_directory.clone());

    let config_directory = resolve_directory(
        sources.command_line_config_directory,
        sources.environment_config_directory,
        context_config_directory,
        &working_data_directory.join("config"),
    );
    let data_directory = resolve_directory(
        sources.command_line_data_directory,
        sources.environment_data_directory,
        context_data_directory,
        &working_data_directory,
    );

    ResolvedInstallPaths {
        config_directory: make_absolute(config_directory, sources.working_directory),
        data_directory: make_absolute(data_directory, sources.working_directory),
        install_mode: sources.install_context.map(|context| context.mode.clone()),
    }
}

fn resolve_directory(
    command_line_directory: Option<PathBuf>,
    environment_directory: Option<PathBuf>,
    context_directory: Option<PathBuf>,
    working_directory_default: &Path,
) -> PathBuf {
    command_line_directory
        .or(environment_directory)
        .or(context_directory)
        .unwrap_or_else(|| working_directory_default.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::{
        InstallContext, InstallPathSources, ResolvedInstallPaths,
        resolve_install_paths_from_sources,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    fn representative_install_context(root_directory: &Path) -> InstallContext {
        InstallContext {
            mode: "systemd".to_owned(),
            config_directory: root_directory.join("context-config"),
            data_directory: root_directory.join("context-data"),
            runtime_directory: root_directory.join("context-run"),
            channel: "stable".to_owned(),
            packs: vec!["generic-iot".to_owned()],
        }
    }

    fn resolve_with_all_sources(
        root_directory: &Path,
        command_line_config_directory: Option<PathBuf>,
        command_line_data_directory: Option<PathBuf>,
        environment_config_directory: Option<PathBuf>,
        environment_data_directory: Option<PathBuf>,
        install_context: Option<&InstallContext>,
    ) -> ResolvedInstallPaths {
        resolve_install_paths_from_sources(InstallPathSources {
            command_line_config_directory,
            command_line_data_directory,
            environment_config_directory,
            environment_data_directory,
            install_context,
            working_directory: root_directory,
        })
    }

    #[test]
    fn command_line_paths_override_every_other_source() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let root_directory = temporary_directory.path();
        let install_context = representative_install_context(root_directory);
        let command_line_config_directory = root_directory.join("command-line-config");
        let command_line_data_directory = root_directory.join("command-line-data");

        let resolved_paths = resolve_with_all_sources(
            root_directory,
            Some(command_line_config_directory.clone()),
            Some(command_line_data_directory.clone()),
            Some(root_directory.join("environment-config")),
            Some(root_directory.join("environment-data")),
            Some(&install_context),
        );

        assert_eq!(
            resolved_paths.config_directory,
            command_line_config_directory
        );
        assert_eq!(resolved_paths.data_directory, command_line_data_directory);
        assert_eq!(resolved_paths.install_mode.as_deref(), Some("systemd"));
    }

    #[test]
    fn environment_paths_override_install_context() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let root_directory = temporary_directory.path();
        let install_context = representative_install_context(root_directory);
        let environment_config_directory = root_directory.join("environment-config");
        let environment_data_directory = root_directory.join("environment-data");

        let resolved_paths = resolve_with_all_sources(
            root_directory,
            None,
            None,
            Some(environment_config_directory.clone()),
            Some(environment_data_directory.clone()),
            Some(&install_context),
        );

        assert_eq!(
            resolved_paths.config_directory,
            environment_config_directory
        );
        assert_eq!(resolved_paths.data_directory, environment_data_directory);
    }

    #[test]
    fn install_context_paths_define_the_installed_layout() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let root_directory = temporary_directory.path();
        let install_context = representative_install_context(root_directory);

        let resolved_paths = resolve_with_all_sources(
            root_directory,
            None,
            None,
            None,
            None,
            Some(&install_context),
        );

        assert_eq!(
            resolved_paths.config_directory,
            install_context.config_directory
        );
        assert_eq!(
            resolved_paths.data_directory,
            install_context.data_directory
        );
    }

    #[test]
    fn an_unregistered_old_install_is_ignored() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let root_directory = temporary_directory.path();
        let legacy_install_root = root_directory.join("legacy");
        fs::create_dir_all(legacy_install_root.join("config"))
            .expect("create legacy config fixture");
        fs::create_dir_all(legacy_install_root.join("data")).expect("create legacy data fixture");

        let resolved_paths = resolve_install_paths_from_sources(InstallPathSources {
            command_line_config_directory: None,
            command_line_data_directory: None,
            environment_config_directory: None,
            environment_data_directory: None,
            install_context: None,
            working_directory: root_directory,
        });

        assert_eq!(
            resolved_paths.config_directory,
            root_directory.join("data/config")
        );
        assert_eq!(resolved_paths.data_directory, root_directory.join("data"));
        assert_eq!(resolved_paths.install_mode, None);
    }

    #[test]
    fn checkout_defaults_match_the_compose_data_layout() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let root_directory = temporary_directory.path();

        let resolved_paths = resolve_install_paths_from_sources(InstallPathSources {
            command_line_config_directory: None,
            command_line_data_directory: None,
            environment_config_directory: None,
            environment_data_directory: None,
            install_context: None,
            working_directory: root_directory,
        });

        assert_eq!(
            resolved_paths.config_directory,
            root_directory.join("data/config")
        );
        assert_eq!(resolved_paths.data_directory, root_directory.join("data"));
    }

    #[test]
    fn config_and_data_paths_are_resolved_independently() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let root_directory = temporary_directory.path();
        let install_context = representative_install_context(root_directory);
        let command_line_config_directory = root_directory.join("command-line-config");
        let environment_data_directory = root_directory.join("environment-data");

        let resolved_paths = resolve_with_all_sources(
            root_directory,
            Some(command_line_config_directory.clone()),
            None,
            None,
            Some(environment_data_directory.clone()),
            Some(&install_context),
        );

        assert_eq!(
            resolved_paths.config_directory,
            command_line_config_directory
        );
        assert_eq!(resolved_paths.data_directory, environment_data_directory);
    }

    #[test]
    fn valid_context_file_is_deserialized_with_stable_defaults() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let context_path = temporary_directory.path().join("install.yaml");
        fs::write(
            &context_path,
            r#"
mode: systemd
config_dir: /etc/aether/config
data_dir: /var/lib/aether
runtime_dir: /run/aether
"#,
        )
        .expect("write install context fixture");

        let install_context = InstallContext::load_optional(&context_path)
            .expect("load install context")
            .expect("context should exist");

        assert_eq!(install_context.mode, "systemd");
        assert_eq!(
            install_context.config_directory,
            Path::new("/etc/aether/config")
        );
        assert_eq!(install_context.data_directory, Path::new("/var/lib/aether"));
        assert_eq!(install_context.runtime_directory, Path::new("/run/aether"));
        assert_eq!(install_context.channel, "stable");
        assert!(install_context.packs.is_empty());
    }

    #[test]
    fn malformed_context_file_returns_a_descriptive_error() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let context_path = temporary_directory.path().join("install.yaml");
        fs::write(
            &context_path,
            r#"
mode: systemd
config_dir: /etc/aether/config
data_dir: [not, a, path]
runtime_dir: /run/aether
"#,
        )
        .expect("write malformed install context fixture");

        let error = InstallContext::load_optional(&context_path)
            .expect_err("malformed context should fail");

        assert!(
            error
                .to_string()
                .contains("failed to parse install context")
        );
        assert!(error.to_string().contains("install.yaml"));
    }

    #[test]
    fn relative_context_directories_are_rejected() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let context_path = temporary_directory.path().join("install.yaml");
        fs::write(
            &context_path,
            r#"
mode: docker-compose
config_dir: data/config
data_dir: /opt/AetherEdge/data
runtime_dir: /run/aether
"#,
        )
        .expect("write relative install context fixture");

        let error = InstallContext::load_optional(&context_path)
            .expect_err("relative context path should fail");

        assert!(
            error
                .to_string()
                .contains("requires an absolute config_dir")
        );
    }

    #[test]
    fn missing_context_file_is_not_an_error() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let context_path = temporary_directory.path().join("missing-install.yaml");

        let install_context =
            InstallContext::load_optional(&context_path).expect("missing context is optional");

        assert!(install_context.is_none());
    }

    #[test]
    fn relative_command_line_paths_are_bound_to_the_working_directory() {
        let temporary_directory = tempfile::tempdir().expect("create temporary directory");
        let root_directory = temporary_directory.path();
        let resolved = resolve_with_all_sources(
            root_directory,
            Some(PathBuf::from("site-config")),
            Some(PathBuf::from("site-data")),
            None,
            None,
            None,
        );

        assert_eq!(
            resolved.config_directory,
            root_directory.join("site-config")
        );
        assert_eq!(resolved.data_directory, root_directory.join("site-data"));
    }
}
