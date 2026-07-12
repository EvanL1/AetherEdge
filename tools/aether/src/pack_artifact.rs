//! Build and install data-only Aether Pack artifacts.
//!
//! The artifact deliberately contains no installer executable. The already
//! installed Kernel CLI verifies the exact runtime binding, publishes the Pack
//! below the site data directory, and atomically updates `global.yaml`.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use aether_pack::{PackManifest, load_active_packs, load_pack_manifest, parse_active_packs_config};

const ARTIFACT_SCHEMA: &str = "aether.pack-artifact.v1";
const ARTIFACT_METADATA_FILE: &str = "pack-artifact.json";
const PAYLOAD_DIRECTORY: &str = "pack";
const SHA256_ALGORITHM: &str = "sha256";
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_PAYLOAD_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PAYLOAD_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PAYLOAD_FILES: usize = 4096;
const MAX_GLOBAL_CONFIG_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Subcommand)]
pub(crate) enum PackCommands {
    /// Build a data-only Pack bundle bound to one Kernel runtime manifest
    Build {
        /// Validated Pack root containing pack.yaml
        #[arg(long)]
        pack_root: PathBuf,

        /// Exact Kernel runtime-manifest.json shipped by the target artifact
        #[arg(long)]
        runtime_manifest: PathBuf,

        /// New bundle directory to publish
        #[arg(long)]
        output: PathBuf,
    },

    /// Verify, publish, and atomically activate a Pack bundle
    Install {
        /// Bundle directory containing pack-artifact.json and pack/
        #[arg(long)]
        artifact: PathBuf,

        /// Absolute Pack store; defaults to <data-path>/packs
        #[arg(long)]
        packs_dir: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ArtifactMetadata {
    schema: String,
    pack: ArtifactPack,
    kernel: KernelBinding,
    payload: PayloadMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ArtifactPack {
    id: String,
    version: String,
    compatibility: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct KernelBinding {
    version: String,
    target_triple: String,
    runtime_manifest_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PayloadMetadata {
    algorithm: String,
    digest: String,
    files: Vec<FileRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FileRecord {
    path: String,
    size: u64,
    digest: String,
}

#[derive(Debug)]
struct PayloadSnapshot {
    directories: Vec<PathBuf>,
    files: Vec<FileRecord>,
}

#[derive(Debug, Serialize)]
struct BuildReceipt {
    artifact: PathBuf,
    pack_id: String,
    pack_version: String,
    kernel_version: String,
    target_triple: String,
    runtime_manifest_digest: String,
    payload_digest: String,
    files: usize,
}

#[derive(Debug, Serialize)]
struct InstallReceipt {
    artifact: PathBuf,
    pack_id: String,
    pack_version: String,
    installed_root: PathBuf,
    active_config: PathBuf,
    runtime_manifest_digest: String,
    already_published: bool,
}

pub(crate) fn handle(
    command: PackCommands,
    config_directory: &Path,
    data_directory: &Path,
    json: bool,
) -> Result<()> {
    match command {
        PackCommands::Build {
            pack_root,
            runtime_manifest,
            output,
        } => {
            let receipt = build_artifact(&pack_root, &runtime_manifest, &output)?;
            if json {
                crate::output::print_success(&receipt);
            } else {
                println!(
                    "Built Pack-only artifact {} ({} {} for Kernel {}, {})",
                    receipt.artifact.display(),
                    receipt.pack_id,
                    receipt.pack_version,
                    receipt.kernel_version,
                    receipt.runtime_manifest_digest
                );
            }
        },
        PackCommands::Install {
            artifact,
            packs_dir,
        } => {
            let packs_directory = packs_dir.unwrap_or_else(|| data_directory.join("packs"));
            let receipt = install_artifact(&artifact, config_directory, &packs_directory)?;
            if json {
                crate::output::print_success(&receipt);
            } else {
                println!(
                    "Installed and activated Pack {} {} at {}",
                    receipt.pack_id,
                    receipt.pack_version,
                    receipt.installed_root.display()
                );
            }
        },
    }
    Ok(())
}

fn build_artifact(
    pack_root: &Path,
    runtime_manifest_path: &Path,
    output: &Path,
) -> Result<BuildReceipt> {
    ensure_new_destination(output, "artifact output")?;
    let output_parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_parent).with_context(|| {
        format!(
            "failed to create artifact output directory {}",
            output_parent.display()
        )
    })?;

    let canonical_pack_root = validate_directory_root(pack_root, "Pack source")?;
    let canonical_output_parent = fs::canonicalize(output_parent).with_context(|| {
        format!(
            "failed to resolve artifact output directory {}",
            output_parent.display()
        )
    })?;
    ensure!(
        !canonical_output_parent.starts_with(&canonical_pack_root),
        "artifact output must not be inside the Pack source {}",
        canonical_pack_root.display()
    );

    let runtime_manifest = aether_runtime_catalog::load_runtime_manifest_file(
        runtime_manifest_path,
        env!("CARGO_PKG_VERSION"),
    )
    .with_context(|| {
        format!(
            "failed to verify Kernel runtime manifest {}",
            runtime_manifest_path.display()
        )
    })?;
    let pack_runtime = runtime_manifest.pack_runtime()?;
    let source_manifest = load_pack_manifest(&canonical_pack_root, &pack_runtime)
        .context("Pack source is incompatible with the selected Kernel runtime")?;
    let source_snapshot = scan_payload(&canonical_pack_root)?;

    let stage = staging_sibling(output, "build")?;
    ensure_new_destination(&stage, "artifact staging directory")?;
    let build_result = (|| -> Result<(ArtifactMetadata, PayloadSnapshot)> {
        fs::create_dir(&stage).with_context(|| {
            format!(
                "failed to create artifact staging directory {}",
                stage.display()
            )
        })?;
        let staged_payload = stage.join(PAYLOAD_DIRECTORY);
        copy_snapshot(&canonical_pack_root, &staged_payload, &source_snapshot)?;
        let staged_snapshot = scan_payload(&staged_payload)?;
        let staged_manifest = load_pack_manifest(&staged_payload, &pack_runtime)
            .context("staged Pack failed validation")?;
        ensure_same_manifest_identity(&source_manifest, &staged_manifest)?;

        let payload_digest = aggregate_payload_digest(&staged_snapshot.files);
        let metadata = ArtifactMetadata {
            schema: ARTIFACT_SCHEMA.to_owned(),
            pack: ArtifactPack {
                id: staged_manifest.id().to_owned(),
                version: staged_manifest.version().to_string(),
                compatibility: staged_manifest.aether_requirement().to_string(),
            },
            kernel: KernelBinding {
                version: runtime_manifest.aether_version().to_owned(),
                target_triple: runtime_manifest.target_triple().to_owned(),
                runtime_manifest_digest: runtime_manifest.digest(),
            },
            payload: PayloadMetadata {
                algorithm: SHA256_ALGORITHM.to_owned(),
                digest: payload_digest,
                files: staged_snapshot.files.clone(),
            },
        };
        write_artifact_metadata(&stage, &metadata)?;
        verify_artifact_layout(&stage, &metadata)?;
        sync_directory(&stage)?;
        Ok((metadata, staged_snapshot))
    })();

    let (metadata, snapshot) = match build_result {
        Ok(result) => result,
        Err(error) => {
            remove_directory_if_present(&stage);
            return Err(error);
        },
    };

    if let Err(error) = fs::rename(&stage, output)
        .with_context(|| format!("failed to atomically publish artifact {}", output.display()))
    {
        remove_directory_if_present(&stage);
        return Err(error);
    }
    warn_on_directory_sync_failure(output_parent);

    Ok(BuildReceipt {
        artifact: absolute_path(output)?,
        pack_id: metadata.pack.id,
        pack_version: metadata.pack.version,
        kernel_version: metadata.kernel.version,
        target_triple: metadata.kernel.target_triple,
        runtime_manifest_digest: metadata.kernel.runtime_manifest_digest,
        payload_digest: metadata.payload.digest,
        files: snapshot.files.len(),
    })
}

fn install_artifact(
    artifact: &Path,
    config_directory: &Path,
    packs_directory: &Path,
) -> Result<InstallReceipt> {
    let artifact_root = validate_directory_root(artifact, "Pack artifact")?;
    let metadata = read_artifact_metadata(&artifact_root)?;
    verify_artifact_layout(&artifact_root, &metadata)?;

    let runtime_manifest = aether_runtime_catalog::load_runtime_manifest_for_current_process(
        config_directory,
        env!("CARGO_PKG_VERSION"),
    )
    .context("installed Kernel runtime manifest is invalid")?;
    verify_kernel_binding(&metadata.kernel, &runtime_manifest)?;
    let pack_runtime = runtime_manifest.pack_runtime()?;
    let payload_root = artifact_root.join(PAYLOAD_DIRECTORY);
    let artifact_manifest = load_pack_manifest(&payload_root, &pack_runtime)
        .context("artifact Pack is incompatible with the installed Kernel runtime")?;
    verify_pack_metadata(&metadata.pack, &artifact_manifest)?;

    load_active_packs(config_directory, &pack_runtime)
        .context("current active Pack configuration is invalid")?;

    ensure!(
        packs_directory.is_absolute(),
        "Pack installation directory must be absolute: {}",
        packs_directory.display()
    );
    fs::create_dir_all(packs_directory).with_context(|| {
        format!(
            "failed to create Pack installation directory {}",
            packs_directory.display()
        )
    })?;
    let canonical_packs_directory =
        validate_directory_root(packs_directory, "Pack installation directory")?;
    let identity_directory = canonical_packs_directory.join(artifact_manifest.id());
    fs::create_dir_all(&identity_directory).with_context(|| {
        format!(
            "failed to create Pack identity directory {}",
            identity_directory.display()
        )
    })?;
    validate_directory_root(&identity_directory, "Pack identity directory")?;

    let final_root = identity_directory.join(artifact_manifest.version().to_string());
    let artifact_snapshot = scan_payload(&payload_root)?;
    let already_published = path_exists(&final_root)?;
    let mut published_new = false;

    if already_published {
        let installed_snapshot = scan_payload(&final_root).with_context(|| {
            format!(
                "existing Pack version at {} cannot be verified",
                final_root.display()
            )
        })?;
        ensure_snapshot_matches_metadata(&installed_snapshot, &metadata.payload)?;
        let installed_manifest = load_pack_manifest(&final_root, &pack_runtime)
            .context("existing Pack version failed validation")?;
        ensure_same_manifest_identity(&artifact_manifest, &installed_manifest)?;
    } else {
        let stage = canonical_packs_directory.join(format!(
            ".{}-{}-install-{}",
            artifact_manifest.id(),
            artifact_manifest.version(),
            uuid::Uuid::new_v4()
        ));
        ensure_new_destination(&stage, "Pack installation staging directory")?;
        let publish_result = (|| -> Result<()> {
            copy_snapshot(&payload_root, &stage, &artifact_snapshot)?;
            let staged_snapshot = scan_payload(&stage)?;
            ensure_snapshot_matches_metadata(&staged_snapshot, &metadata.payload)?;
            let staged_manifest = load_pack_manifest(&stage, &pack_runtime)
                .context("staged installed Pack failed validation")?;
            ensure_same_manifest_identity(&artifact_manifest, &staged_manifest)?;
            sync_tree_files(&stage, &staged_snapshot)?;
            sync_directory(&stage)?;
            fs::rename(&stage, &final_root).with_context(|| {
                format!(
                    "failed to atomically publish Pack at {}",
                    final_root.display()
                )
            })?;
            // The rename is already the publication boundary. A directory
            // fsync failure affects crash durability, not the logical result;
            // treating it as a pre-publication error would strand an inactive
            // final directory while reporting that publication never happened.
            warn_on_directory_sync_failure(&identity_directory);
            Ok(())
        })();
        if let Err(error) = publish_result {
            remove_directory_if_present(&stage);
            return Err(error);
        }
        published_new = true;
    }

    let activation = activate_pack_atomically(
        config_directory,
        artifact_manifest.id(),
        &final_root,
        &pack_runtime,
    );
    if let Err(error) = activation {
        if published_new {
            let cleanup_result = fs::remove_dir_all(&final_root).with_context(|| {
                format!(
                    "failed to roll back unpublished Pack {}",
                    final_root.display()
                )
            });
            remove_empty_directory(&identity_directory);
            if let Err(cleanup_error) = cleanup_result {
                return Err(error.context(format!(
                    "activation failed and rollback also failed: {cleanup_error:#}"
                )));
            }
            warn_on_directory_sync_failure(&canonical_packs_directory);
        }
        return Err(error);
    }

    Ok(InstallReceipt {
        artifact: artifact_root,
        pack_id: artifact_manifest.id().to_owned(),
        pack_version: artifact_manifest.version().to_string(),
        installed_root: fs::canonicalize(&final_root).with_context(|| {
            format!("failed to resolve installed Pack {}", final_root.display())
        })?,
        active_config: config_directory.join("global.yaml"),
        runtime_manifest_digest: runtime_manifest.digest(),
        already_published,
    })
}

fn read_artifact_metadata(artifact_root: &Path) -> Result<ArtifactMetadata> {
    let path = artifact_root.join(ARTIFACT_METADATA_FILE);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("failed to inspect artifact metadata {}", path.display()))?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "artifact metadata must not be a symbolic link"
    );
    ensure!(
        metadata.is_file(),
        "artifact metadata must be a regular file"
    );
    ensure!(
        metadata.len() <= MAX_METADATA_BYTES,
        "artifact metadata exceeds the {} byte limit",
        MAX_METADATA_BYTES
    );
    let source = fs::read(&path)
        .with_context(|| format!("failed to read artifact metadata {}", path.display()))?;
    let artifact: ArtifactMetadata = serde_json::from_slice(&source)
        .with_context(|| format!("invalid closed artifact metadata {}", path.display()))?;
    validate_artifact_metadata(&artifact)?;
    Ok(artifact)
}

fn validate_artifact_metadata(metadata: &ArtifactMetadata) -> Result<()> {
    ensure!(
        metadata.schema == ARTIFACT_SCHEMA,
        "unsupported Pack artifact schema {:?}",
        metadata.schema
    );
    ensure!(
        metadata.payload.algorithm == SHA256_ALGORITHM,
        "unsupported payload digest algorithm {:?}",
        metadata.payload.algorithm
    );
    validate_sha256_digest(&metadata.payload.digest, "payload digest")?;
    validate_sha256_digest(
        &metadata.kernel.runtime_manifest_digest,
        "runtime manifest digest",
    )?;
    ensure!(
        !metadata.payload.files.is_empty(),
        "Pack payload inventory must not be empty"
    );
    ensure!(
        metadata.payload.files.len() <= MAX_PAYLOAD_FILES,
        "Pack payload inventory exceeds the {} file limit",
        MAX_PAYLOAD_FILES
    );
    let mut paths = BTreeSet::new();
    for file in &metadata.payload.files {
        validate_metadata_relative_path(&file.path)?;
        ensure!(
            paths.insert(&file.path),
            "duplicate payload path {:?}",
            file.path
        );
        ensure!(
            file.size <= MAX_PAYLOAD_FILE_BYTES,
            "payload file {:?} exceeds the {} byte limit",
            file.path,
            MAX_PAYLOAD_FILE_BYTES
        );
        validate_sha256_digest(&file.digest, "file digest")?;
    }
    ensure!(
        metadata
            .payload
            .files
            .windows(2)
            .all(|pair| pair[0].path < pair[1].path),
        "payload file inventory must be in canonical lexical order"
    );
    ensure!(
        aggregate_payload_digest(&metadata.payload.files) == metadata.payload.digest,
        "payload aggregate digest does not match its file inventory"
    );
    Ok(())
}

fn write_artifact_metadata(artifact_root: &Path, metadata: &ArtifactMetadata) -> Result<()> {
    validate_artifact_metadata(metadata)?;
    let path = artifact_root.join(ARTIFACT_METADATA_FILE);
    let serialized = serde_json::to_vec_pretty(metadata)
        .context("failed to serialize Pack artifact metadata")?;
    ensure!(
        serialized.len() as u64 <= MAX_METADATA_BYTES,
        "generated artifact metadata exceeds the {} byte limit",
        MAX_METADATA_BYTES
    );
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("failed to create artifact metadata {}", path.display()))?;
    file.write_all(&serialized)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .with_context(|| format!("failed to persist artifact metadata {}", path.display()))
}

fn verify_artifact_layout(artifact_root: &Path, metadata: &ArtifactMetadata) -> Result<()> {
    let entries = sorted_directory_entries(artifact_root)?;
    let names = entries
        .iter()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    ensure!(
        names == [PAYLOAD_DIRECTORY, ARTIFACT_METADATA_FILE],
        "Pack artifact top-level inventory must be exactly {:?}; found {:?}",
        [PAYLOAD_DIRECTORY, ARTIFACT_METADATA_FILE],
        names
    );
    let snapshot = scan_payload(&artifact_root.join(PAYLOAD_DIRECTORY))?;
    ensure_snapshot_matches_metadata(&snapshot, &metadata.payload)
}

fn ensure_snapshot_matches_metadata(
    snapshot: &PayloadSnapshot,
    metadata: &PayloadMetadata,
) -> Result<()> {
    ensure!(
        snapshot.files == metadata.files,
        "Pack payload file inventory or digest differs from pack-artifact.json"
    );
    ensure!(
        aggregate_payload_digest(&snapshot.files) == metadata.digest,
        "Pack payload aggregate digest differs from pack-artifact.json"
    );
    Ok(())
}

fn verify_kernel_binding(
    binding: &KernelBinding,
    runtime: &aether_runtime_catalog::KernelRuntimeManifest,
) -> Result<()> {
    ensure!(
        binding.version == runtime.aether_version(),
        "Pack artifact requires exact Kernel version {}, installed runtime is {}",
        binding.version,
        runtime.aether_version()
    );
    ensure!(
        binding.target_triple == runtime.target_triple(),
        "Pack artifact target {} does not match installed runtime target {}",
        binding.target_triple,
        runtime.target_triple()
    );
    let installed_digest = runtime.digest();
    ensure!(
        binding.runtime_manifest_digest == installed_digest,
        "Pack artifact runtime manifest digest {} does not match installed runtime {}",
        binding.runtime_manifest_digest,
        installed_digest
    );
    Ok(())
}

fn verify_pack_metadata(metadata: &ArtifactPack, manifest: &PackManifest) -> Result<()> {
    ensure!(
        metadata.id == manifest.id(),
        "artifact metadata Pack id {:?} does not match pack.yaml {:?}",
        metadata.id,
        manifest.id()
    );
    ensure!(
        metadata.version == manifest.version().to_string(),
        "artifact metadata Pack version {:?} does not match pack.yaml {}",
        metadata.version,
        manifest.version()
    );
    ensure!(
        metadata.compatibility == manifest.aether_requirement().to_string(),
        "artifact metadata compatibility {:?} does not match canonical pack.yaml requirement {}",
        metadata.compatibility,
        manifest.aether_requirement()
    );
    Ok(())
}

fn ensure_same_manifest_identity(expected: &PackManifest, actual: &PackManifest) -> Result<()> {
    ensure!(
        expected.id() == actual.id()
            && expected.version() == actual.version()
            && expected.aether_requirement() == actual.aether_requirement(),
        "Pack identity changed while copying the artifact payload"
    );
    Ok(())
}

fn scan_payload(root: &Path) -> Result<PayloadSnapshot> {
    validate_directory_root(root, "Pack payload")?;
    let mut pending = vec![PathBuf::new()];
    let mut directories = Vec::new();
    let mut files = Vec::new();
    let mut total_bytes = 0_u64;

    while let Some(relative_directory) = pending.pop() {
        let absolute_directory = root.join(&relative_directory);
        for entry in sorted_directory_entries(&absolute_directory)? {
            let relative = relative_directory.join(entry.file_name());
            validate_payload_path(&relative)?;
            let absolute = root.join(&relative);
            let metadata = fs::symlink_metadata(&absolute).with_context(|| {
                format!(
                    "failed to inspect Pack payload entry {}",
                    absolute.display()
                )
            })?;
            ensure!(
                !metadata.file_type().is_symlink(),
                "symbolic links are forbidden in Pack payloads: {}",
                relative.display()
            );
            if metadata.is_dir() {
                directories.push(relative.clone());
                pending.push(relative);
                continue;
            }
            ensure!(
                metadata.is_file(),
                "Pack payload entry must be a regular file: {}",
                relative.display()
            );
            ensure_non_executable(&absolute, &metadata)?;
            ensure!(
                metadata.len() <= MAX_PAYLOAD_FILE_BYTES,
                "Pack payload file {} exceeds the {} byte limit",
                relative.display(),
                MAX_PAYLOAD_FILE_BYTES
            );
            total_bytes = total_bytes
                .checked_add(metadata.len())
                .context("Pack payload byte count overflow")?;
            ensure!(
                total_bytes <= MAX_PAYLOAD_BYTES,
                "Pack payload exceeds the {} byte total limit",
                MAX_PAYLOAD_BYTES
            );
            ensure!(
                files.len() < MAX_PAYLOAD_FILES,
                "Pack payload exceeds the {} file limit",
                MAX_PAYLOAD_FILES
            );
            ensure_not_native_binary(&absolute, &relative)?;
            let path = portable_relative_path(&relative)?;
            let digest = digest_file(&absolute)?;
            files.push(FileRecord {
                path,
                size: metadata.len(),
                digest,
            });
        }
    }

    directories.sort();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(PayloadSnapshot { directories, files })
}

fn copy_snapshot(source: &Path, destination: &Path, snapshot: &PayloadSnapshot) -> Result<()> {
    ensure_new_destination(destination, "Pack payload destination")?;
    fs::create_dir(destination).with_context(|| {
        format!(
            "failed to create Pack payload destination {}",
            destination.display()
        )
    })?;
    for relative in &snapshot.directories {
        fs::create_dir(destination.join(relative)).with_context(|| {
            format!(
                "failed to create staged Pack directory {}",
                destination.join(relative).display()
            )
        })?;
    }
    for record in &snapshot.files {
        let relative = metadata_path_to_path(&record.path)?;
        let source_file = source.join(&relative);
        let destination_file = destination.join(&relative);
        if let Some(parent) = destination_file.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create staged directory {}", parent.display())
            })?;
        }
        let source_metadata = fs::symlink_metadata(&source_file).with_context(|| {
            format!("failed to re-inspect source file {}", source_file.display())
        })?;
        ensure!(
            source_metadata.is_file() && !source_metadata.file_type().is_symlink(),
            "Pack source changed while being copied: {}",
            source_file.display()
        );
        fs::copy(&source_file, &destination_file).with_context(|| {
            format!(
                "failed to copy Pack payload {} to {}",
                source_file.display(),
                destination_file.display()
            )
        })?;
        ensure_non_executable(
            &destination_file,
            &fs::symlink_metadata(&destination_file).with_context(|| {
                format!(
                    "failed to inspect copied file {}",
                    destination_file.display()
                )
            })?,
        )?;
    }
    Ok(())
}

fn sync_tree_files(root: &Path, snapshot: &PayloadSnapshot) -> Result<()> {
    for record in &snapshot.files {
        let path = root.join(metadata_path_to_path(&record.path)?);
        File::open(&path)
            .and_then(|file| file.sync_all())
            .with_context(|| format!("failed to persist installed Pack file {}", path.display()))?;
    }
    Ok(())
}

fn activate_pack_atomically(
    config_directory: &Path,
    pack_id: &str,
    installed_root: &Path,
    runtime: &aether_pack::PackRuntime,
) -> Result<()> {
    let global_path = config_directory.join("global.yaml");
    let global_metadata = fs::symlink_metadata(&global_path).with_context(|| {
        format!(
            "failed to inspect active configuration {}",
            global_path.display()
        )
    })?;
    ensure!(
        global_metadata.is_file() && !global_metadata.file_type().is_symlink(),
        "active configuration must be a regular non-symlink file: {}",
        global_path.display()
    );
    ensure!(
        global_metadata.len() <= MAX_GLOBAL_CONFIG_BYTES,
        "active configuration exceeds the {} byte limit",
        MAX_GLOBAL_CONFIG_BYTES
    );
    let current = fs::read_to_string(&global_path).with_context(|| {
        format!(
            "failed to read active configuration {}",
            global_path.display()
        )
    })?;
    let mut document: serde_yml::Value = serde_yml::from_str(&current)
        .with_context(|| format!("failed to parse {}", global_path.display()))?;
    update_active_pack_value(&mut document, pack_id, installed_root)?;
    let candidate = serde_yml::to_string(&document)
        .context("failed to serialize candidate active Pack configuration")?;
    parse_active_packs_config(&candidate, config_directory, runtime)
        .context("candidate active Pack configuration failed runtime validation")?;

    let temporary = config_directory.join(format!(
        ".global.yaml.pack-install-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| {
                format!("failed to create candidate config {}", temporary.display())
            })?;
        fs::set_permissions(&temporary, global_metadata.permissions()).with_context(|| {
            format!("failed to preserve permissions on {}", temporary.display())
        })?;
        file.write_all(candidate.as_bytes())
            .and_then(|()| file.sync_all())
            .with_context(|| {
                format!("failed to persist candidate config {}", temporary.display())
            })?;
        fs::rename(&temporary, &global_path).with_context(|| {
            format!(
                "failed to atomically activate Pack in {}",
                global_path.display()
            )
        })?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    warn_on_directory_sync_failure(config_directory);
    Ok(())
}

fn update_active_pack_value(
    document: &mut serde_yml::Value,
    pack_id: &str,
    installed_root: &Path,
) -> Result<()> {
    let root_string = installed_root
        .to_str()
        .context("installed Pack root is not valid UTF-8")?
        .to_owned();
    let mapping = document
        .as_mapping_mut()
        .context("global.yaml root must be a mapping")?;
    let packs_key = serde_yml::Value::String("packs".to_owned());
    let packs = mapping
        .entry(packs_key)
        .or_insert_with(|| serde_yml::Value::Sequence(Vec::new()));
    let sequence = packs
        .as_sequence_mut()
        .context("global.yaml packs must be a sequence")?;
    let id_key = serde_yml::Value::String("id".to_owned());
    let root_key = serde_yml::Value::String("root".to_owned());

    for entry in sequence.iter_mut() {
        let entry_mapping = entry
            .as_mapping_mut()
            .context("each global.yaml packs entry must be a mapping")?;
        if entry_mapping
            .get(&id_key)
            .and_then(serde_yml::Value::as_str)
            == Some(pack_id)
        {
            entry_mapping.insert(root_key, serde_yml::Value::String(root_string));
            return Ok(());
        }
    }

    let mut entry = serde_yml::Mapping::new();
    entry.insert(id_key, serde_yml::Value::String(pack_id.to_owned()));
    entry.insert(root_key, serde_yml::Value::String(root_string));
    sequence.push(serde_yml::Value::Mapping(entry));
    Ok(())
}

fn aggregate_payload_digest(files: &[FileRecord]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aether.pack-payload.v1\0");
    for file in files {
        let path_bytes = file.path.as_bytes();
        hasher.update((path_bytes.len() as u64).to_be_bytes());
        hasher.update(path_bytes);
        hasher.update(file.size.to_be_bytes());
        hasher.update((file.digest.len() as u64).to_be_bytes());
        hasher.update(file.digest.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn digest_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| format!("failed to open Pack payload file {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to hash Pack payload file {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn validate_sha256_digest(value: &str, field: &str) -> Result<()> {
    let Some(hexadecimal) = value.strip_prefix("sha256:") else {
        bail!("{field} must use the sha256:<lowercase-hex> form");
    };
    ensure!(
        hexadecimal.len() == 64
            && hexadecimal
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{field} must contain exactly 64 lowercase hexadecimal characters"
    );
    Ok(())
}

fn validate_directory_root(path: &Path, description: &str) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {description} {}", path.display()))?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "{description} must not be a symbolic link: {}",
        path.display()
    );
    ensure!(
        metadata.is_dir(),
        "{description} must be a directory: {}",
        path.display()
    );
    fs::canonicalize(path)
        .with_context(|| format!("failed to resolve {description} {}", path.display()))
}

fn sorted_directory_entries(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let mut entries = fs::read_dir(path)
        .with_context(|| format!("failed to read directory {}", path.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to enumerate directory {}", path.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn validate_payload_path(path: &Path) -> Result<()> {
    let mut components = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            bail!(
                "Pack payload path is not a confined relative path: {}",
                path.display()
            );
        };
        let component = component
            .to_str()
            .context("Pack payload path is not valid UTF-8")?;
        ensure!(
            !component.is_empty() && component != "." && component != "..",
            "Pack payload contains an invalid path component"
        );
        components.push(component.to_ascii_lowercase());
    }
    ensure!(
        !components.is_empty(),
        "Pack payload path must not be empty"
    );

    const FORBIDDEN_DIRECTORIES: [&str; 12] = [
        ".cargo",
        ".git",
        "bin",
        "crates",
        "extensions",
        "integrations",
        "libs",
        "services",
        "src",
        "target",
        "tools",
        "workspace-hack",
    ];
    for component in &components[..components.len().saturating_sub(1)] {
        ensure!(
            !FORBIDDEN_DIRECTORIES.contains(&component.as_str()),
            "Pack-only payload contains forbidden Kernel/code directory {:?}",
            component
        );
    }
    let file_name = components.last().context("Pack payload path is empty")?;
    const FORBIDDEN_FILE_NAMES: [&str; 8] = [
        "cargo.lock",
        "cargo.toml",
        "dockerfile",
        "pack-artifact.json",
        "runtime-manifest.json",
        "rust-toolchain",
        "rust-toolchain.toml",
        "workspace-hack.rs",
    ];
    ensure!(
        !FORBIDDEN_FILE_NAMES.contains(&file_name.as_str()),
        "Pack-only payload contains forbidden Kernel/build file {:?}",
        file_name
    );
    const CODE_EXTENSIONS: [&str; 17] = [
        "bat", "c", "cc", "cmd", "cpp", "go", "h", "java", "js", "kt", "ps1", "py", "rs", "sh",
        "ts", "wasm", "zig",
    ];
    if let Some(extension) = Path::new(file_name)
        .extension()
        .and_then(|value| value.to_str())
    {
        ensure!(
            !CODE_EXTENSIONS.contains(&extension),
            "Pack-only payload contains executable/source file {}",
            path.display()
        );
    }
    Ok(())
}

fn validate_metadata_relative_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty() && !path.starts_with('/') && !path.contains('\\'),
        "invalid portable payload path {:?}",
        path
    );
    let converted = metadata_path_to_path(path)?;
    validate_payload_path(&converted)?;
    ensure!(
        portable_relative_path(&converted)? == path,
        "payload path is not canonical: {:?}",
        path
    );
    Ok(())
}

fn metadata_path_to_path(path: &str) -> Result<PathBuf> {
    let mut converted = PathBuf::new();
    for component in path.split('/') {
        ensure!(
            !component.is_empty() && component != "." && component != "..",
            "invalid payload path component in {:?}",
            path
        );
        converted.push(component);
    }
    Ok(converted)
}

fn portable_relative_path(path: &Path) -> Result<String> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .context("Pack payload path is not valid UTF-8"),
            _ => Err(anyhow::anyhow!(
                "Pack payload path is not confined: {}",
                path.display()
            )),
        })
        .collect::<Result<Vec<_>>>()
        .map(|components| components.join("/"))
}

fn ensure_not_native_binary(path: &Path, relative: &Path) -> Result<()> {
    let mut file = File::open(path)
        .with_context(|| format!("failed to inspect Pack payload file {}", path.display()))?;
    let mut magic = [0_u8; 4];
    let read = file
        .read(&mut magic)
        .with_context(|| format!("failed to inspect Pack payload file {}", path.display()))?;
    let native_binary = read >= 4
        && matches!(
            magic,
            [0x7f, b'E', b'L', b'F']
                | [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
        )
        || (read >= 2 && magic[..2] == *b"MZ");
    ensure!(
        !native_binary,
        "Pack-only payload contains a native executable/library: {}",
        relative.display()
    );
    Ok(())
}

#[cfg(unix)]
fn ensure_non_executable(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    ensure!(
        metadata.permissions().mode() & 0o111 == 0,
        "Pack-only payload file must not be executable: {}",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
fn ensure_non_executable(_path: &Path, _metadata: &fs::Metadata) -> Result<()> {
    Ok(())
}

fn ensure_new_destination(path: &Path, description: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("{description} already exists: {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect {description} {}", path.display())),
    }
}

fn path_exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("failed to inspect {}", path.display())),
    }
}

fn staging_sibling(destination: &Path, purpose: &str) -> Result<PathBuf> {
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .context("artifact output name must be valid UTF-8")?;
    Ok(parent.join(format!(".{name}.{purpose}-{}", uuid::Uuid::new_v4())))
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .context("failed to determine current directory")?
            .join(path))
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to persist directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn warn_on_directory_sync_failure(path: &Path) {
    if let Err(error) = sync_directory(path) {
        tracing::warn!(path = %path.display(), error = %error, "directory sync failed after atomic rename");
    }
}

fn remove_directory_if_present(path: &Path) {
    if let Err(error) = fs::remove_dir_all(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %error, "failed to remove staging directory");
    }
}

fn remove_empty_directory(path: &Path) {
    if let Err(error) = fs::remove_dir(path)
        && error.kind() != std::io::ErrorKind::NotFound
        && error.kind() != std::io::ErrorKind::DirectoryNotEmpty
    {
        tracing::warn!(path = %path.display(), error = %error, "failed to remove empty Pack identity directory");
    }
}
