//! Versioned, industry-neutral domain-pack contract.
//!
//! A pack is declarative data rooted in one directory. This crate validates
//! the manifest, release compatibility, required identifiers, fail-safe
//! example state, and asset-directory confinement. It does not install,
//! commission, or execute a pack.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use semver::{Version, VersionReq};
use serde::Deserialize;
use thiserror::Error;

/// The only pack-manifest schema understood by this release.
pub const PACK_SCHEMA_VERSION: u32 = 1;

/// The only closed asset-index schema understood by this release.
pub const PACK_ASSET_INDEX_SCHEMA: &str = "aether.pack.asset-index.v1";

/// Maximum size of one indexed Pack asset.
pub const MAX_PACK_ASSET_BYTES: usize = 1024 * 1024;

/// Maximum size of one asset `index.yaml`.
pub const MAX_PACK_ASSET_INDEX_BYTES: usize = 64 * 1024;

const INDEXED_ASSET_CATEGORIES: [(&str, &str, &str); 4] = [
    ("mappings", "mappings", "aether.pack.mapping-set.v1"),
    ("rules", "rules", "aether.pack.rule.v1"),
    (
        "evaluations",
        "evaluations",
        "aether.pack.evaluation-suite.v1",
    ),
    (
        "data_processing",
        "data_processing_tasks",
        "aether.data-processing-task.v1",
    ),
];

/// Capabilities and protocols supplied by one Aether composition.
#[derive(Debug, Clone)]
pub struct PackRuntime {
    aether_version: String,
    capabilities: BTreeSet<String>,
    protocols: BTreeSet<String>,
}

impl PackRuntime {
    /// Creates an empty compatibility catalog for one Aether release.
    #[must_use]
    pub fn new(aether_version: impl Into<String>) -> Self {
        Self {
            aether_version: aether_version.into(),
            capabilities: BTreeSet::new(),
            protocols: BTreeSet::new(),
        }
    }

    /// Adds capability identifiers available in the composition.
    #[must_use]
    pub fn with_capabilities<I, S>(mut self, capabilities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.capabilities
            .extend(capabilities.into_iter().map(Into::into));
        self
    }

    /// Adds protocol identifiers available in the composition.
    #[must_use]
    pub fn with_protocols<I, S>(mut self, protocols: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.protocols.extend(protocols.into_iter().map(Into::into));
        self
    }
}

/// One manifest-validated Pack selected by the shared site configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivePack {
    root: PathBuf,
    manifest: PackManifest,
}

impl ActivePack {
    /// Returns the identity verified against both configuration and manifest.
    #[must_use]
    pub fn id(&self) -> &str {
        self.manifest.id()
    }

    /// Returns the canonical Pack root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the validated Pack v1 manifest.
    #[must_use]
    pub const fn manifest(&self) -> &PackManifest {
        &self.manifest
    }

    /// Resolves one previously validated asset directory below the Pack root.
    #[must_use]
    pub fn asset_directory(&self, category: &str) -> Option<PathBuf> {
        self.manifest
            .asset_directory(category)
            .map(|relative| self.root.join(relative))
    }

    /// Resolves one manifest/index-validated asset by its Pack-local identity.
    ///
    /// Only an [`ActivePack`] can resolve a file through this API, so an empty
    /// active set cannot accidentally expose domain assets.
    #[must_use]
    pub fn asset_file(&self, category: &str, id: &str) -> Option<PathBuf> {
        let directory = self.manifest.asset_directory(category)?;
        let asset = self
            .manifest
            .asset_index(category)?
            .assets()
            .iter()
            .find(|asset| asset.id() == id)?;
        Some(self.root.join(directory).join(asset.path()))
    }
}

/// Ordered set of Packs explicitly activated for one site.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivePackSet {
    packs: Vec<ActivePack>,
}

impl ActivePackSet {
    /// Creates the fail-safe site state with no domain Pack activated.
    #[must_use]
    pub const fn empty() -> Self {
        Self { packs: Vec::new() }
    }

    /// Returns the number of activated Packs.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.packs.len()
    }

    /// Returns whether no Pack is activated.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.packs.is_empty()
    }

    /// Looks up one activated Pack by its validated identity.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ActivePack> {
        self.packs.iter().find(|pack| pack.id() == id)
    }

    /// Iterates activated Packs in configuration order.
    pub fn iter(&self) -> impl Iterator<Item = &ActivePack> {
        self.packs.iter()
    }

    /// Returns validated asset identities namespaced by active Pack identity.
    ///
    /// An empty active set therefore exposes no domain assets.
    #[must_use]
    pub fn namespaced_asset_ids(&self, category: &str) -> Vec<String> {
        self.packs
            .iter()
            .filter_map(|pack| {
                pack.manifest()
                    .asset_index(category)
                    .map(|index| (pack.id(), index))
            })
            .flat_map(|(pack_id, index)| {
                index
                    .assets()
                    .iter()
                    .map(move |asset| format!("{pack_id}/{category}/{}", asset.id()))
            })
            .collect()
    }
}

/// Fail-closed errors while selecting Packs from `<config>/global.yaml`.
#[derive(Debug, Error)]
pub enum ActivePackError {
    /// The shared global configuration cannot be read.
    #[error("cannot read active Pack configuration {path}: {source}")]
    ConfigRead {
        /// Expected global configuration path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The `packs` entry does not match the shared configuration shape.
    #[error("invalid active Pack configuration {path}: {source}")]
    InvalidConfig {
        /// Parsed global configuration path.
        path: PathBuf,
        /// YAML decoding failure.
        #[source]
        source: serde_yml::Error,
    },
    /// An active Pack identity is malformed.
    #[error("invalid active Pack identity {id:?}")]
    InvalidIdentity {
        /// Configured identity.
        id: String,
    },
    /// An active Pack root is empty or contains traversal.
    #[error("invalid root {path} for active Pack {id}: {message}")]
    InvalidRoot {
        /// Configured identity.
        id: String,
        /// Rejected root.
        path: PathBuf,
        /// Stable rejection reason.
        message: &'static str,
    },
    /// The same Pack identity appears more than once.
    #[error("active Pack identity {id:?} is duplicated")]
    DuplicateIdentity {
        /// Duplicated identity.
        id: String,
    },
    /// Two configured identities resolve to the same Pack root.
    #[error("active Pack root {root} is duplicated")]
    DuplicateRoot {
        /// Duplicated canonical root.
        root: PathBuf,
    },
    /// Configuration identity does not match the selected manifest.
    #[error("configured active Pack {configured:?} selected manifest {manifest:?}")]
    IdentityMismatch {
        /// Identity in `global.yaml`.
        configured: String,
        /// Identity in `pack.yaml`.
        manifest: String,
    },
    /// A selected Pack fails manifest, compatibility, or confinement checks.
    #[error("active Pack {id:?} at {root} is invalid: {source}")]
    InvalidPack {
        /// Configured identity.
        id: String,
        /// Resolved root.
        root: PathBuf,
        /// Manifest validation failure.
        #[source]
        source: Box<PackError>,
    },
}

#[derive(Debug, Deserialize)]
struct ActivePackConfigDto {
    #[serde(default)]
    packs: Vec<ActivePackDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivePackDto {
    id: String,
    root: String,
}

/// Loads the single active-Pack entry point from `<config_directory>/global.yaml`.
///
/// An omitted or empty `packs` list means no domain Pack is active. Every
/// selected root is then loaded through the regular Pack v1 validator before
/// it becomes visible to a runtime process.
pub fn load_active_packs(
    config_directory: impl AsRef<Path>,
    runtime: &PackRuntime,
) -> Result<ActivePackSet, ActivePackError> {
    let config_directory = config_directory.as_ref();
    let path = config_directory.join("global.yaml");
    let source = std::fs::read_to_string(&path).map_err(|source| ActivePackError::ConfigRead {
        path: path.clone(),
        source,
    })?;
    parse_active_packs_config(&source, config_directory, runtime)
}

/// Parses a candidate `global.yaml` before an installer atomically activates it.
///
/// Relative Pack roots are resolved exactly as they are by [`load_active_packs`]:
/// from `config_directory`. This lets installers validate the complete future
/// active set while the currently installed configuration remains untouched.
pub fn parse_active_packs_config(
    source: &str,
    config_directory: impl AsRef<Path>,
    runtime: &PackRuntime,
) -> Result<ActivePackSet, ActivePackError> {
    let config_directory = config_directory.as_ref();
    let path = config_directory.join("global.yaml");
    let dto: ActivePackConfigDto =
        serde_yml::from_str(source).map_err(|source| ActivePackError::InvalidConfig {
            path: path.clone(),
            source,
        })?;

    let mut identities = BTreeSet::new();
    let mut roots = BTreeSet::new();
    let mut packs = Vec::with_capacity(dto.packs.len());
    for configured in dto.packs {
        if validate_identifier("packs.id", &configured.id).is_err() {
            return Err(ActivePackError::InvalidIdentity { id: configured.id });
        }
        if !identities.insert(configured.id.clone()) {
            return Err(ActivePackError::DuplicateIdentity { id: configured.id });
        }

        let declared_root = PathBuf::from(&configured.root);
        validate_active_pack_root(&configured.id, &declared_root)?;
        let selected_root = if declared_root.is_absolute() {
            declared_root
        } else {
            config_directory.join(declared_root)
        };
        let canonical_root = std::fs::canonicalize(&selected_root).map_err(|source| {
            ActivePackError::InvalidPack {
                id: configured.id.clone(),
                root: selected_root.clone(),
                source: Box::new(PackError::PackRootUnavailable {
                    path: selected_root.clone(),
                    source,
                }),
            }
        })?;
        if !roots.insert(canonical_root.clone()) {
            return Err(ActivePackError::DuplicateRoot {
                root: canonical_root,
            });
        }

        let manifest = load_pack_manifest(&canonical_root, runtime).map_err(|source| {
            ActivePackError::InvalidPack {
                id: configured.id.clone(),
                root: canonical_root.clone(),
                source: Box::new(source),
            }
        })?;
        if manifest.id() != configured.id {
            return Err(ActivePackError::IdentityMismatch {
                configured: configured.id,
                manifest: manifest.id().to_string(),
            });
        }
        packs.push(ActivePack {
            root: canonical_root,
            manifest,
        });
    }

    Ok(ActivePackSet { packs })
}

fn validate_active_pack_root(id: &str, root: &Path) -> Result<(), ActivePackError> {
    let mut has_normal_component = false;
    for component in root.components() {
        match component {
            Component::Normal(_) => has_normal_component = true,
            Component::ParentDir => {
                return Err(ActivePackError::InvalidRoot {
                    id: id.to_string(),
                    path: root.to_path_buf(),
                    message: "parent traversal is forbidden",
                });
            },
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {},
        }
    }
    if has_normal_component {
        Ok(())
    } else {
        Err(ActivePackError::InvalidRoot {
            id: id.to_string(),
            path: root.to_path_buf(),
            message: "root must contain a normal path component",
        })
    }
}

/// Why an asset path is not confined to its pack root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetPathErrorKind {
    /// An absolute path was supplied.
    Absolute,
    /// A `..` component was supplied.
    ParentTraversal,
    /// A symlink resolved outside the pack root.
    EscapesRoot,
    /// An empty path or a path without a normal component was supplied.
    Empty,
}

impl std::fmt::Display for AssetPathErrorKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Absolute => "absolute paths are forbidden",
            Self::ParentTraversal => "parent traversal is forbidden",
            Self::EscapesRoot => "the resolved path escapes the pack root",
            Self::Empty => "the path must contain a normal relative component",
        };
        formatter.write_str(message)
    }
}

/// Typed pack loading and compatibility failures.
#[derive(Debug, Error)]
pub enum PackError {
    /// The pack root cannot be resolved.
    #[error("cannot resolve pack root {path}: {source}")]
    PackRootUnavailable {
        /// Requested pack root.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// `pack.yaml` cannot be read.
    #[error("cannot read pack manifest {path}: {source}")]
    ManifestRead {
        /// Manifest path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// YAML does not match the closed v1 shape.
    #[error("invalid pack manifest: {source}")]
    InvalidManifest {
        /// YAML decoding failure.
        #[source]
        source: serde_yml::Error,
    },
    /// The manifest schema is not supported.
    #[error("unsupported pack schema {found}; this Aether release supports {supported}")]
    UnsupportedSchema {
        /// Manifest schema version.
        found: u32,
        /// Supported schema version.
        supported: u32,
    },
    /// The pack version is not SemVer.
    #[error("invalid pack version {value:?}: {source}")]
    InvalidPackVersion {
        /// Invalid value.
        value: String,
        /// SemVer parsing failure.
        #[source]
        source: semver::Error,
    },
    /// The distribution version is not SemVer.
    #[error("invalid distribution version {value:?}: {source}")]
    InvalidDistributionVersion {
        /// Invalid value.
        value: String,
        /// SemVer parsing failure.
        #[source]
        source: semver::Error,
    },
    /// The host Aether version is not SemVer.
    #[error("invalid Aether version {value:?}: {source}")]
    InvalidAetherVersion {
        /// Invalid value.
        value: String,
        /// SemVer parsing failure.
        #[source]
        source: semver::Error,
    },
    /// The declared Aether range is not a valid SemVer requirement.
    #[error("invalid Aether compatibility range {value:?}: {source}")]
    InvalidAetherRequirement {
        /// Invalid value.
        value: String,
        /// SemVer requirement parsing failure.
        #[source]
        source: semver::Error,
    },
    /// The Aether release falls outside the pack's range.
    #[error("Aether {current} does not satisfy pack requirement {required}")]
    IncompatibleAether {
        /// Host release.
        current: Version,
        /// Pack requirement.
        required: VersionReq,
    },
    /// The composition does not advertise a required capability.
    #[error("required capability {id:?} is unavailable")]
    UnknownCapability {
        /// Missing stable identifier.
        id: String,
    },
    /// The composition does not advertise a required protocol.
    #[error("required protocol {id:?} is unavailable")]
    UnknownProtocol {
        /// Missing stable identifier.
        id: String,
    },
    /// A stable identifier is empty or contains unsupported characters.
    #[error("invalid identifier in {field}: {value:?}")]
    InvalidIdentifier {
        /// Manifest field.
        field: String,
        /// Invalid value.
        value: String,
    },
    /// One identifier is repeated within the same field.
    #[error("duplicate identifier in {field}: {value:?}")]
    DuplicateIdentifier {
        /// Manifest field.
        field: String,
        /// Repeated value.
        value: String,
    },
    /// An asset path is absolute, traversing, empty, or resolves outside root.
    #[error("invalid asset directory {field}={path}: {kind}")]
    InvalidAssetPath {
        /// Asset category.
        field: String,
        /// Declared path.
        path: PathBuf,
        /// Confinement failure.
        kind: AssetPathErrorKind,
    },
    /// A declared asset directory is absent, unreadable, or not a directory.
    #[error("asset directory {field} is unavailable at {path}: {message}")]
    AssetDirectoryUnavailable {
        /// Asset category.
        field: String,
        /// Resolved path.
        path: PathBuf,
        /// Filesystem detail.
        message: String,
    },
    /// An indexed asset directory does not contain a valid closed v1 index.
    #[error("invalid {category} asset index {path}: {message}")]
    InvalidAssetIndex {
        /// Manifest asset category.
        category: String,
        /// Index path.
        path: PathBuf,
        /// Stable decoding or contract failure.
        message: String,
    },
    /// The same logical asset identity appears more than once.
    #[error("duplicate asset id {id:?} in {category}")]
    DuplicateAssetId {
        /// Manifest asset category.
        category: String,
        /// Duplicate identity.
        id: String,
    },
    /// Two index entries select the same file.
    #[error("duplicate asset path {path} in {category}")]
    DuplicateAssetPath {
        /// Manifest asset category.
        category: String,
        /// Duplicate relative file path.
        path: PathBuf,
    },
    /// An indexed asset path is malformed or escapes its directory/root.
    #[error("invalid indexed asset path in {category}: {path}: {message}")]
    InvalidAssetFilePath {
        /// Manifest asset category.
        category: String,
        /// Rejected path.
        path: PathBuf,
        /// Stable rejection reason.
        message: &'static str,
    },
    /// Indexed assets must be regular files, never symbolic links.
    #[error("indexed asset in {category} is a symbolic link: {path}")]
    AssetFileSymlink {
        /// Manifest asset category.
        category: String,
        /// Rejected path.
        path: PathBuf,
    },
    /// An indexed asset exceeds the bounded Pack contract.
    #[error("indexed asset in {category} exceeds {max_bytes} bytes: {path}")]
    AssetFileTooLarge {
        /// Manifest asset category.
        category: String,
        /// Rejected path.
        path: PathBuf,
        /// Maximum accepted bytes.
        max_bytes: usize,
    },
    /// Manifest IDs, index IDs, and actual directory files differ.
    #[error("{category} asset inventory mismatch: {message}")]
    AssetInventoryMismatch {
        /// Manifest asset category.
        category: String,
        /// Exact mismatch detail.
        message: String,
    },
    /// Bundled examples may not arrive commissioned.
    #[error("pack examples must declare commissioned: false")]
    CommissionedExamples,
}

/// A validated distribution identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distribution {
    id: String,
    version: Version,
    composition: String,
}

impl Distribution {
    /// Returns the distribution identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the distribution release version.
    #[must_use]
    pub const fn version(&self) -> &Version {
        &self.version
    }

    /// Returns the composition identifier, not a filesystem path.
    #[must_use]
    pub fn composition(&self) -> &str {
        &self.composition
    }
}

/// A fully validated v1 pack manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackManifest {
    schema_version: u32,
    id: String,
    name: String,
    version: Version,
    status: String,
    description: String,
    distribution: Distribution,
    aether_requirement: VersionReq,
    required_capabilities: Vec<String>,
    required_protocols: Vec<String>,
    assets: BTreeMap<String, PathBuf>,
    capabilities: BTreeMap<String, Vec<String>>,
    asset_indexes: BTreeMap<String, PackAssetIndex>,
}

/// Validated v1 index for one Pack-owned asset category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackAssetIndex {
    category: String,
    assets: Vec<PackAssetDescriptor>,
}

impl PackAssetIndex {
    /// Returns the manifest category owned by this index.
    #[must_use]
    pub fn category(&self) -> &str {
        &self.category
    }

    /// Returns assets in deterministic index order.
    #[must_use]
    pub fn assets(&self) -> &[PackAssetDescriptor] {
        &self.assets
    }
}

/// One validated Pack-owned file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackAssetDescriptor {
    id: String,
    path: PathBuf,
    schema: String,
    media_type: String,
}

impl PackAssetDescriptor {
    /// Returns the Pack-local stable identity.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the path relative to its declared asset directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the versioned content schema identity.
    #[must_use]
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// Returns the declared media type.
    #[must_use]
    pub fn media_type(&self) -> &str {
        &self.media_type
    }
}

impl PackManifest {
    /// Returns the manifest schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the pack identifier.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the human-readable pack name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the pack release version.
    #[must_use]
    pub const fn version(&self) -> &Version {
        &self.version
    }

    /// Returns the lifecycle status declared by the distribution.
    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Returns the pack description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the owning distribution metadata.
    #[must_use]
    pub const fn distribution(&self) -> &Distribution {
        &self.distribution
    }

    /// Returns the compatible Aether release requirement.
    #[must_use]
    pub const fn aether_requirement(&self) -> &VersionReq {
        &self.aether_requirement
    }

    /// Returns required Aether capability identifiers.
    #[must_use]
    pub fn required_capabilities(&self) -> &[String] {
        &self.required_capabilities
    }

    /// Returns required protocol identifiers.
    #[must_use]
    pub fn required_protocols(&self) -> &[String] {
        &self.required_protocols
    }

    /// Returns a relative, pack-root-confined asset directory.
    #[must_use]
    pub fn asset_directory(&self, category: &str) -> Option<&Path> {
        self.assets.get(category).map(PathBuf::as_path)
    }

    /// Returns provided domain identifiers for a manifest category.
    #[must_use]
    pub fn capability_ids(&self, category: &str) -> Option<&[String]> {
        self.capabilities.get(category).map(Vec::as_slice)
    }

    /// Returns a fully validated index for a formal Pack asset category.
    #[must_use]
    pub fn asset_index(&self, category: &str) -> Option<&PackAssetIndex> {
        self.asset_indexes.get(category)
    }

    /// Version 1 accepts only explicitly uncommissioned examples.
    #[must_use]
    pub const fn examples_commissioned(&self) -> bool {
        false
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestDto {
    schema_version: u32,
    id: String,
    name: String,
    version: String,
    status: String,
    description: String,
    distribution: DistributionDto,
    compatibility: CompatibilityDto,
    assets: AssetDirectoriesDto,
    examples: ExamplesDto,
    capabilities: CapabilitiesDto,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DistributionDto {
    id: String,
    version: String,
    composition: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityDto {
    aether: String,
    required_capabilities: Vec<String>,
    required_protocols: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExamplesDto {
    commissioned: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetDirectoriesDto {
    #[serde(default)]
    config: Option<String>,
    #[serde(default)]
    data_processing: Option<String>,
    #[serde(default)]
    models: Option<String>,
    #[serde(default)]
    knowledge: Option<String>,
    #[serde(default)]
    rules: Option<String>,
    #[serde(default)]
    mappings: Option<String>,
    #[serde(default)]
    evaluations: Option<String>,
}

impl AssetDirectoriesDto {
    fn into_entries(self) -> impl Iterator<Item = (&'static str, String)> {
        [
            ("config", self.config),
            ("data_processing", self.data_processing),
            ("models", self.models),
            ("knowledge", self.knowledge),
            ("rules", self.rules),
            ("mappings", self.mappings),
            ("evaluations", self.evaluations),
        ]
        .into_iter()
        .filter_map(|(category, path)| path.map(|path| (category, path)))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapabilitiesDto {
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    rule_topics: Vec<String>,
    #[serde(default)]
    data_processing_tasks: Vec<String>,
    #[serde(default)]
    mappings: Vec<String>,
    #[serde(default)]
    rules: Vec<String>,
    #[serde(default)]
    evaluations: Vec<String>,
}

impl CapabilitiesDto {
    fn into_map(self) -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([
            ("models".to_string(), self.models),
            ("rule_topics".to_string(), self.rule_topics),
            (
                "data_processing_tasks".to_string(),
                self.data_processing_tasks,
            ),
            ("mappings".to_string(), self.mappings),
            ("rules".to_string(), self.rules),
            ("evaluations".to_string(), self.evaluations),
        ])
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetIndexDto {
    schema: String,
    category: String,
    assets: Vec<AssetDescriptorDto>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetDescriptorDto {
    id: String,
    path: String,
    schema: String,
    media_type: String,
}

#[derive(Debug, Deserialize)]
struct AssetIdentityDto {
    schema: String,
    id: String,
}

/// Loads and validates `<pack_root>/pack.yaml`.
pub fn load_pack_manifest(
    pack_root: impl AsRef<Path>,
    runtime: &PackRuntime,
) -> Result<PackManifest, PackError> {
    let pack_root = pack_root.as_ref();
    let path = pack_root.join("pack.yaml");
    let source = std::fs::read_to_string(&path)
        .map_err(|source| PackError::ManifestRead { path, source })?;
    parse_pack_manifest(&source, pack_root, runtime)
}

/// Parses and validates a manifest against one pack root and Aether catalog.
///
/// This entry point exists for conformance and mutation tests. Production code
/// normally uses [`load_pack_manifest`].
pub fn parse_pack_manifest(
    source: &str,
    pack_root: impl AsRef<Path>,
    runtime: &PackRuntime,
) -> Result<PackManifest, PackError> {
    let dto: ManifestDto =
        serde_yml::from_str(source).map_err(|source| PackError::InvalidManifest { source })?;
    if dto.schema_version != PACK_SCHEMA_VERSION {
        return Err(PackError::UnsupportedSchema {
            found: dto.schema_version,
            supported: PACK_SCHEMA_VERSION,
        });
    }

    validate_identifier("id", &dto.id)?;
    validate_identifier("distribution.id", &dto.distribution.id)?;
    validate_identifier("distribution.composition", &dto.distribution.composition)?;
    validate_nonempty("name", &dto.name)?;
    validate_nonempty("status", &dto.status)?;
    validate_nonempty("description", &dto.description)?;

    let version = Version::parse(&dto.version).map_err(|source| PackError::InvalidPackVersion {
        value: dto.version.clone(),
        source,
    })?;
    let distribution_version = Version::parse(&dto.distribution.version).map_err(|source| {
        PackError::InvalidDistributionVersion {
            value: dto.distribution.version.clone(),
            source,
        }
    })?;
    let aether_requirement = VersionReq::parse(&dto.compatibility.aether).map_err(|source| {
        PackError::InvalidAetherRequirement {
            value: dto.compatibility.aether.clone(),
            source,
        }
    })?;
    let aether_version = Version::parse(&runtime.aether_version).map_err(|source| {
        PackError::InvalidAetherVersion {
            value: runtime.aether_version.clone(),
            source,
        }
    })?;
    if !aether_requirement.matches(&aether_version) {
        return Err(PackError::IncompatibleAether {
            current: aether_version,
            required: aether_requirement,
        });
    }

    validate_requirements(
        "compatibility.required_capabilities",
        &dto.compatibility.required_capabilities,
    )?;
    validate_requirements(
        "compatibility.required_protocols",
        &dto.compatibility.required_protocols,
    )?;
    for id in &dto.compatibility.required_capabilities {
        if !runtime.capabilities.contains(id) {
            return Err(PackError::UnknownCapability { id: id.clone() });
        }
    }
    for id in &dto.compatibility.required_protocols {
        if !runtime.protocols.contains(id) {
            return Err(PackError::UnknownProtocol { id: id.clone() });
        }
    }
    if dto.examples.commissioned {
        return Err(PackError::CommissionedExamples);
    }

    let root = std::fs::canonicalize(pack_root.as_ref()).map_err(|source| {
        PackError::PackRootUnavailable {
            path: pack_root.as_ref().to_path_buf(),
            source,
        }
    })?;
    let mut assets = BTreeMap::new();
    for (field, declared) in dto.assets.into_entries() {
        let relative = validate_asset_directory(&root, field, &declared)?;
        assets.insert(field.to_string(), relative);
    }
    if assets.is_empty() {
        return Err(PackError::InvalidIdentifier {
            field: "assets".to_string(),
            value: String::new(),
        });
    }

    let capabilities = dto.capabilities.into_map();
    validate_capabilities(&capabilities)?;
    let asset_indexes = validate_asset_indexes(&root, &assets, &capabilities)?;
    Ok(PackManifest {
        schema_version: dto.schema_version,
        id: dto.id,
        name: dto.name,
        version,
        status: dto.status,
        description: dto.description,
        distribution: Distribution {
            id: dto.distribution.id,
            version: distribution_version,
            composition: dto.distribution.composition,
        },
        aether_requirement,
        required_capabilities: dto.compatibility.required_capabilities,
        required_protocols: dto.compatibility.required_protocols,
        assets,
        capabilities,
        asset_indexes,
    })
}

fn validate_identifier(field: &str, value: &str) -> Result<(), PackError> {
    let valid = value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'));
    if valid {
        Ok(())
    } else {
        Err(PackError::InvalidIdentifier {
            field: field.to_string(),
            value: value.to_string(),
        })
    }
}

fn validate_nonempty(field: &str, value: &str) -> Result<(), PackError> {
    if value.trim().is_empty() {
        Err(PackError::InvalidIdentifier {
            field: field.to_string(),
            value: value.to_string(),
        })
    } else {
        Ok(())
    }
}

fn validate_requirements(field: &str, values: &[String]) -> Result<(), PackError> {
    let mut unique = BTreeSet::new();
    for value in values {
        validate_identifier(field, value)?;
        if !unique.insert(value) {
            return Err(PackError::DuplicateIdentifier {
                field: field.to_string(),
                value: value.clone(),
            });
        }
    }
    Ok(())
}

fn validate_capabilities(values: &BTreeMap<String, Vec<String>>) -> Result<(), PackError> {
    for (category, identifiers) in values {
        validate_identifier("capabilities", category)?;
        let mut unique = BTreeSet::new();
        for identifier in identifiers {
            validate_nonempty(&format!("capabilities.{category}"), identifier)?;
            if !unique.insert(identifier) {
                return Err(PackError::DuplicateIdentifier {
                    field: format!("capabilities.{category}"),
                    value: identifier.clone(),
                });
            }
        }
    }
    Ok(())
}

fn validate_asset_indexes(
    root: &Path,
    asset_directories: &BTreeMap<String, PathBuf>,
    capabilities: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<String, PackAssetIndex>, PackError> {
    let mut indexes = BTreeMap::new();
    for (category, capability_category, asset_schema) in INDEXED_ASSET_CATEGORIES {
        let directory = asset_directories.get(category);
        let declared_ids = capabilities
            .get(capability_category)
            .map(Vec::as_slice)
            .unwrap_or_default();
        match (directory, declared_ids.is_empty()) {
            (None, true) => continue,
            (None, false) => {
                return Err(PackError::AssetInventoryMismatch {
                    category: category.to_string(),
                    message: "manifest capabilities declare assets without an asset directory"
                        .to_string(),
                });
            },
            (Some(_), true) => {
                return Err(PackError::AssetInventoryMismatch {
                    category: category.to_string(),
                    message: "asset directory is declared without manifest capability IDs"
                        .to_string(),
                });
            },
            (Some(relative), false) => {
                let index =
                    validate_asset_index(root, category, asset_schema, relative, declared_ids)?;
                indexes.insert(category.to_string(), index);
            },
        }
    }
    Ok(indexes)
}

fn validate_asset_index(
    root: &Path,
    category: &str,
    expected_asset_schema: &str,
    relative_directory: &Path,
    manifest_ids: &[String],
) -> Result<PackAssetIndex, PackError> {
    let directory = root.join(relative_directory);
    let directory_metadata = std::fs::symlink_metadata(&directory).map_err(|source| {
        PackError::AssetDirectoryUnavailable {
            field: category.to_string(),
            path: directory.clone(),
            message: source.to_string(),
        }
    })?;
    if directory_metadata.file_type().is_symlink() {
        return Err(PackError::AssetFileSymlink {
            category: category.to_string(),
            path: directory,
        });
    }

    let index_path = directory.join("index.yaml");
    let index_bytes = read_bounded_asset(category, &index_path, MAX_PACK_ASSET_INDEX_BYTES)?;
    let dto: AssetIndexDto =
        serde_yml::from_slice(&index_bytes).map_err(|source| PackError::InvalidAssetIndex {
            category: category.to_string(),
            path: index_path.clone(),
            message: source.to_string(),
        })?;
    if dto.schema != PACK_ASSET_INDEX_SCHEMA {
        return Err(PackError::InvalidAssetIndex {
            category: category.to_string(),
            path: index_path.clone(),
            message: format!("unsupported schema {:?}", dto.schema),
        });
    }
    if dto.category != category {
        return Err(PackError::InvalidAssetIndex {
            category: category.to_string(),
            path: index_path.clone(),
            message: format!("index category {:?} does not match manifest", dto.category),
        });
    }
    if dto.assets.is_empty() {
        return Err(PackError::InvalidAssetIndex {
            category: category.to_string(),
            path: index_path,
            message: "assets must not be empty".to_string(),
        });
    }

    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut descriptors = Vec::with_capacity(dto.assets.len());
    for declared in dto.assets {
        validate_identifier(&format!("assets.{category}.id"), &declared.id)?;
        validate_identifier(&format!("assets.{category}.schema"), &declared.schema)?;
        if declared.schema != expected_asset_schema {
            return Err(PackError::InvalidAssetIndex {
                category: category.to_string(),
                path: index_path.clone(),
                message: format!(
                    "asset {:?} declares schema {:?}; Pack v1 requires {:?}",
                    declared.id, declared.schema, expected_asset_schema
                ),
            });
        }
        if !ids.insert(declared.id.clone()) {
            return Err(PackError::DuplicateAssetId {
                category: category.to_string(),
                id: declared.id,
            });
        }
        let relative = validate_asset_file_path(category, &declared.path)?;
        if !paths.insert(relative.clone()) {
            return Err(PackError::DuplicateAssetPath {
                category: category.to_string(),
                path: relative,
            });
        }
        validate_media_type(category, &relative, &declared.media_type)?;
        let absolute = directory.join(&relative);
        let bytes = read_bounded_asset(category, &absolute, MAX_PACK_ASSET_BYTES)?;
        let resolved =
            std::fs::canonicalize(&absolute).map_err(|_| PackError::InvalidAssetFilePath {
                category: category.to_string(),
                path: relative.clone(),
                message: "asset cannot be resolved",
            })?;
        if !resolved.starts_with(&directory) || !resolved.starts_with(root) {
            return Err(PackError::InvalidAssetFilePath {
                category: category.to_string(),
                path: relative,
                message: "asset escapes its declared Pack directory",
            });
        }
        let identity: AssetIdentityDto = match declared.media_type.as_str() {
            "application/json" => {
                serde_json::from_slice(&bytes).map_err(|source| PackError::InvalidAssetIndex {
                    category: category.to_string(),
                    path: absolute.clone(),
                    message: source.to_string(),
                })?
            },
            "application/yaml" => {
                serde_yml::from_slice(&bytes).map_err(|source| PackError::InvalidAssetIndex {
                    category: category.to_string(),
                    path: absolute.clone(),
                    message: source.to_string(),
                })?
            },
            _ => {
                return Err(PackError::InvalidAssetIndex {
                    category: category.to_string(),
                    path: absolute,
                    message: format!("unsupported media_type {:?}", declared.media_type),
                });
            },
        };
        if identity.id != declared.id || identity.schema != declared.schema {
            return Err(PackError::AssetInventoryMismatch {
                category: category.to_string(),
                message: format!(
                    "{} metadata is id={:?}, schema={:?}; index declares id={:?}, schema={:?}",
                    relative.display(),
                    identity.id,
                    identity.schema,
                    declared.id,
                    declared.schema
                ),
            });
        }
        descriptors.push(PackAssetDescriptor {
            id: declared.id,
            path: relative,
            schema: declared.schema,
            media_type: declared.media_type,
        });
    }

    let manifest_ids = manifest_ids.iter().cloned().collect::<BTreeSet<_>>();
    if ids != manifest_ids {
        return Err(PackError::AssetInventoryMismatch {
            category: category.to_string(),
            message: format!("manifest IDs {manifest_ids:?} differ from index IDs {ids:?}"),
        });
    }

    let mut actual_files = BTreeSet::new();
    collect_asset_inventory(category, &directory, &directory, &mut actual_files)?;
    let mut expected_files = paths;
    expected_files.insert(PathBuf::from("index.yaml"));
    if actual_files != expected_files {
        return Err(PackError::AssetInventoryMismatch {
            category: category.to_string(),
            message: format!(
                "indexed files {expected_files:?} differ from actual files {actual_files:?}"
            ),
        });
    }

    Ok(PackAssetIndex {
        category: category.to_string(),
        assets: descriptors,
    })
}

fn validate_asset_file_path(category: &str, declared: &str) -> Result<PathBuf, PackError> {
    let path = PathBuf::from(declared);
    let portable_absolute =
        declared.starts_with(['/', '\\']) || declared.as_bytes().get(1) == Some(&b':');
    let non_normal_portable_component = declared
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."));
    let valid_components = path
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if declared.is_empty()
        || portable_absolute
        || declared.contains('\\')
        || non_normal_portable_component
        || !valid_components
    {
        return Err(PackError::InvalidAssetFilePath {
            category: category.to_string(),
            path,
            message: "asset path must be a normalized relative path without traversal",
        });
    }
    Ok(path)
}

fn validate_media_type(category: &str, path: &Path, media_type: &str) -> Result<(), PackError> {
    let extension = path.extension().and_then(|value| value.to_str());
    let matches = match media_type {
        "application/json" => extension == Some("json"),
        "application/yaml" => matches!(extension, Some("yaml" | "yml")),
        _ => false,
    };
    if matches {
        Ok(())
    } else {
        Err(PackError::InvalidAssetIndex {
            category: category.to_string(),
            path: path.to_path_buf(),
            message: format!("unsupported or mismatched media_type {media_type:?}"),
        })
    }
}

fn read_bounded_asset(category: &str, path: &Path, max_bytes: usize) -> Result<Vec<u8>, PackError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| PackError::InvalidAssetIndex {
            category: category.to_string(),
            path: path.to_path_buf(),
            message: source.to_string(),
        })?;
    if metadata.file_type().is_symlink() {
        return Err(PackError::AssetFileSymlink {
            category: category.to_string(),
            path: path.to_path_buf(),
        });
    }
    if !metadata.is_file() {
        return Err(PackError::InvalidAssetFilePath {
            category: category.to_string(),
            path: path.to_path_buf(),
            message: "asset must be a regular file",
        });
    }
    if metadata.len() > max_bytes as u64 {
        return Err(PackError::AssetFileTooLarge {
            category: category.to_string(),
            path: path.to_path_buf(),
            max_bytes,
        });
    }
    std::fs::read(path).map_err(|source| PackError::InvalidAssetIndex {
        category: category.to_string(),
        path: path.to_path_buf(),
        message: source.to_string(),
    })
}

fn collect_asset_inventory(
    category: &str,
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<(), PackError> {
    let entries = std::fs::read_dir(directory).map_err(|source| PackError::InvalidAssetIndex {
        category: category.to_string(),
        path: directory.to_path_buf(),
        message: source.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| PackError::InvalidAssetIndex {
            category: category.to_string(),
            path: directory.to_path_buf(),
            message: source.to_string(),
        })?;
        let path = entry.path();
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|source| PackError::InvalidAssetIndex {
                category: category.to_string(),
                path: path.clone(),
                message: source.to_string(),
            })?;
        if metadata.file_type().is_symlink() {
            return Err(PackError::AssetFileSymlink {
                category: category.to_string(),
                path,
            });
        }
        if metadata.is_dir() {
            collect_asset_inventory(category, root, &path, files)?;
        } else if metadata.is_file() {
            let relative =
                path.strip_prefix(root)
                    .map_err(|_| PackError::InvalidAssetFilePath {
                        category: category.to_string(),
                        path: path.clone(),
                        message: "inventory entry escapes its asset directory",
                    })?;
            files.insert(relative.to_path_buf());
        } else {
            return Err(PackError::InvalidAssetFilePath {
                category: category.to_string(),
                path,
                message: "inventory entry must be a regular file or directory",
            });
        }
    }
    Ok(())
}

fn validate_asset_directory(
    root: &Path,
    field: &str,
    declared: &str,
) -> Result<PathBuf, PackError> {
    let relative = PathBuf::from(declared);
    let bytes = declared.as_bytes();
    let portable_absolute = bytes
        .first()
        .is_some_and(|byte| matches!(byte, b'/' | b'\\'))
        || bytes.get(1) == Some(&b':');
    if relative.is_absolute() || portable_absolute {
        return Err(invalid_asset_path(
            field,
            relative,
            AssetPathErrorKind::Absolute,
        ));
    }
    let mut has_normal = false;
    for component in relative.components() {
        match component {
            Component::Normal(_) => has_normal = true,
            Component::ParentDir => {
                return Err(invalid_asset_path(
                    field,
                    relative,
                    AssetPathErrorKind::ParentTraversal,
                ));
            },
            Component::RootDir | Component::Prefix(_) => {
                return Err(invalid_asset_path(
                    field,
                    relative,
                    AssetPathErrorKind::Absolute,
                ));
            },
            Component::CurDir => {},
        }
    }
    if !has_normal {
        return Err(invalid_asset_path(
            field,
            relative,
            AssetPathErrorKind::Empty,
        ));
    }

    let candidate = root.join(&relative);
    let resolved = std::fs::canonicalize(&candidate).map_err(|source| {
        PackError::AssetDirectoryUnavailable {
            field: field.to_string(),
            path: candidate.clone(),
            message: source.to_string(),
        }
    })?;
    if !resolved.starts_with(root) {
        return Err(invalid_asset_path(
            field,
            relative,
            AssetPathErrorKind::EscapesRoot,
        ));
    }
    if !resolved.is_dir() {
        return Err(PackError::AssetDirectoryUnavailable {
            field: field.to_string(),
            path: resolved,
            message: "not a directory".to_string(),
        });
    }
    Ok(relative)
}

fn invalid_asset_path(field: &str, path: PathBuf, kind: AssetPathErrorKind) -> PackError {
    PackError::InvalidAssetPath {
        field: field.to_string(),
        path,
        kind,
    }
}
