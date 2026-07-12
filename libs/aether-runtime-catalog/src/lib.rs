//! Feature-exact runtime metadata for Pack compatibility checks.
//!
//! A Pack declares requirements. A composition root records what one concrete
//! Aether artifact actually contains in `runtime-manifest.json`. Automation,
//! MCP, and Pack installation tooling all use this loader; there is no implicit
//! "full distribution" fallback.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use aether_pack::PackRuntime;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// The only runtime-manifest schema understood by this release.
pub const RUNTIME_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Name shared by runtime processes and Pack installation tooling.
pub const RUNTIME_MANIFEST_FILE_NAME: &str = "runtime-manifest.json";

/// Upper bound for one composition manifest read by a runtime process.
pub const MAX_RUNTIME_MANIFEST_BYTES: u64 = 256 * 1024;

/// Services in the standard six-process edge composition.
pub const SHIPPED_SERVICES: [&str; 6] = [
    "aether-alarm",
    "aether-api",
    "aether-automation",
    "aether-history",
    "aether-io",
    "aether-uplink",
];

/// Protocol-affecting `aether-io` features enabled by its default Cargo build.
///
/// MQTT and HTTP are intentionally absent: they are opt-in features and must
/// never be advertised by the default artifact merely because an Energy Pack
/// can use them.
pub const DEFAULT_IO_PROTOCOL_FEATURES: [&str; 5] =
    ["aether_485", "can", "gpio", "iec61850", "modbus"];

/// Compatibility alias for release tooling.
pub const SHIPPED_IO_PROTOCOL_FEATURES: [&str; 5] = DEFAULT_IO_PROTOCOL_FEATURES;

const KNOWN_IO_PROTOCOL_FEATURES: [&str; 14] = [
    "aether_485",
    "ble",
    "can",
    "dl645",
    "gpio",
    "http",
    "iec104",
    "iec61850",
    "j1939",
    "matter",
    "modbus",
    "mqtt",
    "opcua",
    "zigbee",
];

/// Returns the `aether-io` default protocol feature set.
#[must_use]
pub const fn default_io_features() -> &'static [&'static str] {
    &DEFAULT_IO_PROTOCOL_FEATURES
}

/// Returns every protocol-affecting feature understood by manifest v1.
#[must_use]
pub const fn known_io_protocol_features() -> &'static [&'static str] {
    &KNOWN_IO_PROTOCOL_FEATURES
}

/// Digest over canonical JSON of every manifest field except `checksum`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestChecksum {
    algorithm: String,
    digest: String,
}

impl ManifestChecksum {
    /// Returns the digest algorithm identifier.
    #[must_use]
    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    /// Returns the lowercase hexadecimal digest without an algorithm prefix.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// A schema-, feature-, version-, and checksum-verified runtime composition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelRuntimeManifest {
    schema_version: u32,
    composition: String,
    aether_version: String,
    target_triple: String,
    target_os: String,
    services: Vec<String>,
    cargo_features: Vec<String>,
    capabilities: Vec<String>,
    protocols: Vec<String>,
    checksum: ManifestChecksum,
}

/// Shorter compatibility name for callers that already use runtime manifests.
pub type RuntimeManifest = KernelRuntimeManifest;

impl KernelRuntimeManifest {
    /// Produces standard six-service metadata from an explicit IO feature set.
    ///
    /// The feature set is the composition source. Protocols are derived, never
    /// supplied independently, so a trimmed build cannot over-advertise MQTT,
    /// HTTP, or another disabled adapter.
    pub fn from_io_features<I, S>(
        aether_version: impl Into<String>,
        target_triple: impl Into<String>,
        io_features: I,
    ) -> Result<Self, RuntimeManifestError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        RuntimeManifestBuilder::new("aether-edge-six-service", aether_version, target_triple)
            .with_services(SHIPPED_SERVICES)
            .with_io_protocol_features(io_features)
            .with_capabilities(
                aether_application::capability_catalog()
                    .iter()
                    .map(|descriptor| descriptor.name()),
            )
            .build()
    }

    /// Returns the verified schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the composition identity.
    #[must_use]
    pub fn composition(&self) -> &str {
        &self.composition
    }

    /// Returns the exact Aether kernel SemVer.
    #[must_use]
    pub fn aether_version(&self) -> &str {
        &self.aether_version
    }

    /// Returns the artifact target triple.
    #[must_use]
    pub fn target_triple(&self) -> &str {
        &self.target_triple
    }

    /// Returns the target OS used for platform-gated feature derivation.
    #[must_use]
    pub fn target_os(&self) -> &str {
        &self.target_os
    }

    /// Iterates services included by the composition root.
    pub fn services(&self) -> impl ExactSizeIterator<Item = &String> {
        self.services.iter()
    }

    /// Iterates exact package-qualified protocol-affecting Cargo features.
    pub fn cargo_features(&self) -> impl ExactSizeIterator<Item = &String> {
        self.cargo_features.iter()
    }

    /// Iterates exact application capability identifiers.
    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = &String> {
        self.capabilities.iter()
    }

    /// Iterates exact protocol adapter identifiers.
    pub fn protocols(&self) -> impl ExactSizeIterator<Item = &String> {
        self.protocols.iter()
    }

    /// Returns protocol adapters as a stable slice.
    #[must_use]
    pub fn protocol_adapters(&self) -> &[String] {
        &self.protocols
    }

    /// Returns the verified checksum.
    #[must_use]
    pub const fn checksum(&self) -> &ManifestChecksum {
        &self.checksum
    }

    /// Returns the digest with an explicit algorithm prefix.
    #[must_use]
    pub fn digest(&self) -> String {
        format!("{}:{}", self.checksum.algorithm, self.checksum.digest)
    }

    /// Converts verified runtime metadata into the Pack v1 compatibility view.
    pub fn pack_runtime(&self) -> Result<PackRuntime, RuntimeManifestError> {
        validate_aether_version(&self.aether_version)?;
        Ok(PackRuntime::new(self.aether_version.clone())
            .with_capabilities(self.capabilities.iter().cloned())
            .with_protocols(self.protocols.iter().cloned()))
    }

    /// Serializes stable, human-reviewable JSON.
    pub fn to_pretty_json(&self) -> Result<String, RuntimeManifestError> {
        serde_json::to_string_pretty(&ManifestDto::from(self))
            .map_err(|source| RuntimeManifestError::Serialize { source })
    }

    /// Writes the standard file atomically below one configuration directory.
    pub fn write_to_config_directory(
        &self,
        config_directory: impl AsRef<Path>,
    ) -> Result<PathBuf, RuntimeManifestError> {
        self.write_to_file(config_directory.as_ref().join(RUNTIME_MANIFEST_FILE_NAME))
    }

    /// Writes one manifest file atomically for build and installer tooling.
    pub fn write_to_file(
        &self,
        destination: impl AsRef<Path>,
    ) -> Result<PathBuf, RuntimeManifestError> {
        let path = destination.as_ref();
        let parent = match path.parent() {
            Some(parent) => parent,
            None => Path::new("."),
        };
        std::fs::create_dir_all(parent).map_err(|source| RuntimeManifestError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
        let temporary = parent.join(format!(".{RUNTIME_MANIFEST_FILE_NAME}.tmp"));
        let json = self.to_pretty_json()?;
        std::fs::write(&temporary, format!("{json}\n")).map_err(|source| {
            RuntimeManifestError::Write {
                path: temporary.clone(),
                source,
            }
        })?;
        std::fs::rename(&temporary, path).map_err(|source| RuntimeManifestError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(path.to_path_buf())
    }
}

/// Explicit composition builder for custom or trimmed artifacts.
#[derive(Debug, Clone)]
pub struct RuntimeManifestBuilder {
    composition: String,
    aether_version: String,
    target_triple: String,
    services: Vec<String>,
    io_features: Vec<String>,
    capabilities: Vec<String>,
}

impl RuntimeManifestBuilder {
    /// Starts an empty composition specification.
    #[must_use]
    pub fn new(
        composition: impl Into<String>,
        aether_version: impl Into<String>,
        target_triple: impl Into<String>,
    ) -> Self {
        Self {
            composition: composition.into(),
            aether_version: aether_version.into(),
            target_triple: target_triple.into(),
            services: Vec::new(),
            io_features: Vec::new(),
            capabilities: Vec::new(),
        }
    }

    /// Declares service artifacts included by the composition.
    #[must_use]
    pub fn with_services<I, S>(mut self, services: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.services = services.into_iter().map(Into::into).collect();
        self
    }

    /// Declares unqualified protocol-affecting `aether-io` features.
    #[must_use]
    pub fn with_io_protocol_features<I, S>(mut self, features: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.io_features = features.into_iter().map(Into::into).collect();
        self
    }

    /// Declares application capabilities reachable in this composition.
    #[must_use]
    pub fn with_capabilities<I, S>(mut self, capabilities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.capabilities = capabilities.into_iter().map(Into::into).collect();
        self
    }

    /// Validates inputs, derives adapters, and seals the payload checksum.
    pub fn build(self) -> Result<KernelRuntimeManifest, RuntimeManifestError> {
        validate_identifier("composition", &self.composition)?;
        validate_aether_version(&self.aether_version)?;
        validate_identifier("target_triple", &self.target_triple)?;
        let target_os = target_os_from_triple(&self.target_triple)?;

        let services = canonical_identifiers("services", self.services)?;
        if services.is_empty() {
            return Err(RuntimeManifestError::EmptyIdentifierList { field: "services" });
        }
        validate_standard_services(&self.composition, &services)?;
        let capabilities = canonical_capabilities(self.capabilities)?;
        validate_standard_capability_catalog(&self.composition, &capabilities)?;
        let io_features = canonical_io_features(self.io_features)?;
        let cargo_features = io_features
            .iter()
            .map(|feature| format!("aether-io/{feature}"))
            .collect::<Vec<_>>();
        let protocols = derive_protocols(&io_features, target_os);

        let payload = ManifestPayload {
            schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
            composition: &self.composition,
            aether_version: &self.aether_version,
            target_triple: &self.target_triple,
            target_os,
            services: &services,
            cargo_features: &cargo_features,
            capabilities: &capabilities,
            protocols: &protocols,
        };
        let checksum = checksum_payload(&payload)?;
        Ok(KernelRuntimeManifest {
            schema_version: RUNTIME_MANIFEST_SCHEMA_VERSION,
            composition: self.composition,
            aether_version: self.aether_version,
            target_triple: self.target_triple,
            target_os: target_os.to_string(),
            services,
            cargo_features,
            capabilities,
            protocols,
            checksum,
        })
    }
}

/// Typed fail-closed runtime metadata errors.
#[derive(Debug, Error)]
pub enum RuntimeManifestError {
    /// The mandatory runtime manifest cannot be read.
    #[error("cannot read runtime manifest {path}: {source}")]
    Read {
        /// Expected file.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The path is a symlink, non-regular file, or exceeds the size bound.
    #[error("unsafe runtime manifest file {path}: {message}")]
    UnsafeManifestFile {
        /// Rejected path.
        path: PathBuf,
        /// Stable rejection detail.
        message: &'static str,
    },
    /// JSON does not match the closed v1 shape.
    #[error("invalid runtime manifest: {source}")]
    InvalidManifest {
        /// Decoding failure.
        #[source]
        source: serde_json::Error,
    },
    /// The schema is newer or otherwise unsupported.
    #[error("unsupported runtime manifest schema {found}; this release supports {supported}")]
    UnsupportedSchema {
        /// Schema found in the file.
        found: u32,
        /// Schema supported by the loader.
        supported: u32,
    },
    /// Aether version is not SemVer.
    #[error("invalid Aether version {value:?}: {source}")]
    InvalidAetherVersion {
        /// Rejected version.
        value: String,
        /// Parser failure.
        #[source]
        source: semver::Error,
    },
    /// The file describes another Aether release.
    #[error("runtime manifest Aether {found} does not match required release {expected}")]
    AetherVersionMismatch {
        /// Expected version.
        expected: String,
        /// Manifest version.
        found: String,
    },
    /// An identifier is malformed.
    #[error("invalid runtime identifier in {field}: {value:?}")]
    InvalidIdentifier {
        /// Field name.
        field: &'static str,
        /// Rejected value.
        value: String,
    },
    /// A set-like field repeats an identifier.
    #[error("duplicate runtime identifier in {field}: {value:?}")]
    DuplicateIdentifier {
        /// Field name.
        field: &'static str,
        /// Repeated value.
        value: String,
    },
    /// A field that must identify at least one artifact is empty.
    #[error("runtime identifier list {field} must not be empty")]
    EmptyIdentifierList {
        /// Empty field.
        field: &'static str,
    },
    /// A set-like field is not stored in canonical lexical order.
    #[error("runtime identifiers in {field} are not in canonical lexical order")]
    NonCanonicalIdentifiers {
        /// Field name.
        field: &'static str,
    },
    /// A capability is absent from the live application catalog.
    #[error("runtime manifest declares unknown application capability {id:?}")]
    UnknownCapability {
        /// Unknown capability.
        id: String,
    },
    /// The standard six-service composition omitted or added a known capability.
    #[error(
        "runtime capabilities do not match the live standard composition catalog (expected {expected:?}, found {found:?})"
    )]
    CapabilityCatalogMismatch {
        /// Exact live catalog.
        expected: Vec<String>,
        /// Manifest claim.
        found: Vec<String>,
    },
    /// The standard composition identity was paired with another service set.
    #[error(
        "standard runtime services do not match the six-service composition (expected {expected:?}, found {found:?})"
    )]
    StandardServiceMismatch {
        /// Exact standard service set.
        expected: Vec<String>,
        /// Manifest claim.
        found: Vec<String>,
    },
    /// A protocol-affecting IO Cargo feature is unknown.
    #[error("runtime manifest declares unknown aether-io feature {id:?}")]
    UnknownIoFeature {
        /// Unknown feature.
        id: String,
    },
    /// The artifact target triple is unsupported or malformed.
    #[error("unsupported runtime target triple {target:?}")]
    UnsupportedTargetTriple {
        /// Rejected target.
        target: String,
    },
    /// Derived target OS disagrees with the recorded value.
    #[error("runtime target OS mismatch (expected {expected:?}, found {found:?})")]
    TargetMismatch {
        /// OS derived from the target triple or required by the caller.
        expected: String,
        /// Recorded OS.
        found: String,
    },
    /// The artifact target architecture differs from the running binary.
    #[error(
        "runtime target architecture mismatch (expected {expected:?}, manifest target {found:?})"
    )]
    TargetArchitectureMismatch {
        /// Architecture of the consuming binary.
        expected: String,
        /// Target triple recorded by the manifest.
        found: String,
    },
    /// Protocols differ from exact feature/platform derivation.
    #[error(
        "runtime protocols do not match IO features/platform (expected {expected:?}, found {found:?})"
    )]
    ProtocolFeatureMismatch {
        /// Derived adapter identifiers.
        expected: Vec<String>,
        /// Claimed adapter identifiers.
        found: Vec<String>,
    },
    /// Checksum metadata uses an unsupported shape or algorithm.
    #[error("invalid runtime manifest checksum {value:?}")]
    InvalidChecksum {
        /// Rejected checksum detail.
        value: String,
    },
    /// Canonical payload bytes do not match the recorded checksum.
    #[error("runtime manifest checksum mismatch (expected {expected}, found {found})")]
    ChecksumMismatch {
        /// Computed lowercase SHA-256.
        expected: String,
        /// Recorded digest.
        found: String,
    },
    /// Canonical serialization failed.
    #[error("cannot canonicalize runtime manifest: {source}")]
    CanonicalJson {
        /// Serialization failure.
        #[source]
        source: serde_json::Error,
    },
    /// Pretty serialization failed.
    #[error("cannot serialize runtime manifest: {source}")]
    Serialize {
        /// Serialization failure.
        #[source]
        source: serde_json::Error,
    },
    /// A manifest cannot be written atomically.
    #[error("cannot write runtime manifest {path}: {source}")]
    Write {
        /// Destination or temporary path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestDto {
    schema_version: u32,
    composition: String,
    aether_version: String,
    target_triple: String,
    target_os: String,
    services: Vec<String>,
    cargo_features: Vec<String>,
    capabilities: Vec<String>,
    protocols: Vec<String>,
    checksum: ManifestChecksum,
}

impl From<&KernelRuntimeManifest> for ManifestDto {
    fn from(manifest: &KernelRuntimeManifest) -> Self {
        Self {
            schema_version: manifest.schema_version,
            composition: manifest.composition.clone(),
            aether_version: manifest.aether_version.clone(),
            target_triple: manifest.target_triple.clone(),
            target_os: manifest.target_os.clone(),
            services: manifest.services.clone(),
            cargo_features: manifest.cargo_features.clone(),
            capabilities: manifest.capabilities.clone(),
            protocols: manifest.protocols.clone(),
            checksum: manifest.checksum.clone(),
        }
    }
}

#[derive(Serialize)]
struct ManifestPayload<'a> {
    schema_version: u32,
    composition: &'a str,
    aether_version: &'a str,
    target_triple: &'a str,
    target_os: &'a str,
    services: &'a [String],
    cargo_features: &'a [String],
    capabilities: &'a [String],
    protocols: &'a [String],
}

/// Loads `<config_directory>/runtime-manifest.json` and verifies its release.
pub fn load_runtime_manifest(
    config_directory: impl AsRef<Path>,
    expected_aether_version: &str,
) -> Result<KernelRuntimeManifest, RuntimeManifestError> {
    load_runtime_manifest_file(
        config_directory.as_ref().join(RUNTIME_MANIFEST_FILE_NAME),
        expected_aether_version,
    )
}

/// Loads one explicit manifest path, useful to Pack-only installer staging.
pub fn load_runtime_manifest_file(
    path: impl AsRef<Path>,
    expected_aether_version: &str,
) -> Result<KernelRuntimeManifest, RuntimeManifestError> {
    let path = path.as_ref();
    let metadata =
        std::fs::symlink_metadata(path).map_err(|source| RuntimeManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if metadata.file_type().is_symlink() {
        return Err(RuntimeManifestError::UnsafeManifestFile {
            path: path.to_path_buf(),
            message: "symbolic links are forbidden",
        });
    }
    if !metadata.is_file() {
        return Err(RuntimeManifestError::UnsafeManifestFile {
            path: path.to_path_buf(),
            message: "a regular file is required",
        });
    }
    if metadata.len() > MAX_RUNTIME_MANIFEST_BYTES {
        return Err(RuntimeManifestError::UnsafeManifestFile {
            path: path.to_path_buf(),
            message: "file exceeds the 256 KiB limit",
        });
    }
    let source = std::fs::read_to_string(path).map_err(|source| RuntimeManifestError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_runtime_manifest(&source, expected_aether_version)
}

/// Loads metadata and additionally requires the current process target OS.
pub fn load_runtime_manifest_for_current_process(
    config_directory: impl AsRef<Path>,
    expected_aether_version: &str,
) -> Result<KernelRuntimeManifest, RuntimeManifestError> {
    let manifest = load_runtime_manifest(config_directory, expected_aether_version)?;
    validate_current_process_target(&manifest)?;
    Ok(manifest)
}

/// Loads one explicit artifact and requires its target to match this process.
pub fn load_runtime_manifest_file_for_current_process(
    path: impl AsRef<Path>,
    expected_aether_version: &str,
) -> Result<KernelRuntimeManifest, RuntimeManifestError> {
    let manifest = load_runtime_manifest_file(path, expected_aether_version)?;
    validate_current_process_target(&manifest)?;
    Ok(manifest)
}

fn validate_current_process_target(
    manifest: &KernelRuntimeManifest,
) -> Result<(), RuntimeManifestError> {
    if manifest.target_os != std::env::consts::OS {
        return Err(RuntimeManifestError::TargetMismatch {
            expected: std::env::consts::OS.to_string(),
            found: manifest.target_os.clone(),
        });
    }
    let Some(manifest_arch) = manifest.target_triple.split('-').next() else {
        return Err(RuntimeManifestError::UnsupportedTargetTriple {
            target: manifest.target_triple.clone(),
        });
    };
    if manifest_arch != std::env::consts::ARCH {
        return Err(RuntimeManifestError::TargetArchitectureMismatch {
            expected: std::env::consts::ARCH.to_string(),
            found: manifest.target_triple.clone(),
        });
    }
    Ok(())
}

/// Parses a closed manifest and verifies checksum, release, features, and IDs.
pub fn parse_runtime_manifest(
    source: &str,
    expected_aether_version: &str,
) -> Result<KernelRuntimeManifest, RuntimeManifestError> {
    let dto: ManifestDto = serde_json::from_str(source)
        .map_err(|source| RuntimeManifestError::InvalidManifest { source })?;
    if dto.schema_version != RUNTIME_MANIFEST_SCHEMA_VERSION {
        return Err(RuntimeManifestError::UnsupportedSchema {
            found: dto.schema_version,
            supported: RUNTIME_MANIFEST_SCHEMA_VERSION,
        });
    }
    validate_checksum_shape(&dto.checksum)?;
    let payload = ManifestPayload {
        schema_version: dto.schema_version,
        composition: &dto.composition,
        aether_version: &dto.aether_version,
        target_triple: &dto.target_triple,
        target_os: &dto.target_os,
        services: &dto.services,
        cargo_features: &dto.cargo_features,
        capabilities: &dto.capabilities,
        protocols: &dto.protocols,
    };
    let expected_checksum = checksum_payload(&payload)?;
    if dto.checksum != expected_checksum {
        return Err(RuntimeManifestError::ChecksumMismatch {
            expected: expected_checksum.digest,
            found: dto.checksum.digest,
        });
    }

    validate_aether_version(expected_aether_version)?;
    validate_aether_version(&dto.aether_version)?;
    if dto.aether_version != expected_aether_version {
        return Err(RuntimeManifestError::AetherVersionMismatch {
            expected: expected_aether_version.to_string(),
            found: dto.aether_version,
        });
    }
    validate_identifier("composition", &dto.composition)?;
    validate_identifier("target_triple", &dto.target_triple)?;
    let derived_os = target_os_from_triple(&dto.target_triple)?;
    if dto.target_os != derived_os {
        return Err(RuntimeManifestError::TargetMismatch {
            expected: derived_os.to_string(),
            found: dto.target_os,
        });
    }
    validate_canonical_identifiers("services", &dto.services)?;
    if dto.services.is_empty() {
        return Err(RuntimeManifestError::EmptyIdentifierList { field: "services" });
    }
    validate_standard_services(&dto.composition, &dto.services)?;
    validate_canonical_order("cargo_features", &dto.cargo_features)?;
    validate_canonical_identifiers("capabilities", &dto.capabilities)?;
    validate_canonical_identifiers("protocols", &dto.protocols)?;
    validate_known_capabilities(&dto.capabilities)?;
    validate_standard_capability_catalog(&dto.composition, &dto.capabilities)?;

    let io_features = parse_package_features(&dto.cargo_features)?;
    let expected_protocols = derive_protocols(&io_features, derived_os);
    if dto.protocols != expected_protocols {
        return Err(RuntimeManifestError::ProtocolFeatureMismatch {
            expected: expected_protocols,
            found: dto.protocols,
        });
    }

    Ok(KernelRuntimeManifest {
        schema_version: dto.schema_version,
        composition: dto.composition,
        aether_version: dto.aether_version,
        target_triple: dto.target_triple,
        target_os: dto.target_os,
        services: dto.services,
        cargo_features: dto.cargo_features,
        capabilities: dto.capabilities,
        protocols: dto.protocols,
        checksum: dto.checksum,
    })
}

/// Builds the standard distribution manifest for one explicit target triple.
pub fn shipped_distribution_manifest(
    target_triple: impl Into<String>,
) -> Result<KernelRuntimeManifest, RuntimeManifestError> {
    KernelRuntimeManifest::from_io_features(
        env!("CARGO_PKG_VERSION"),
        target_triple,
        default_io_features().iter().copied(),
    )
}

/// Derives exact protocol identifiers for tooling and conformance tests.
pub fn protocol_adapters_for_io_features<I, S>(
    features: I,
    target_triple: &str,
) -> Result<Vec<String>, RuntimeManifestError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let target_os = target_os_from_triple(target_triple)?;
    let features = canonical_io_features(features.into_iter().map(Into::into).collect())?;
    Ok(derive_protocols(&features, target_os))
}

/// Validates, expands dependent protocol features, sorts, and de-duplicates
/// one explicit `aether-io` feature selection.
pub fn normalize_io_protocol_features<I, S>(
    features: I,
) -> Result<Vec<String>, RuntimeManifestError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    canonical_io_features(features.into_iter().map(Into::into).collect())
}

fn canonical_io_features(features: Vec<String>) -> Result<Vec<String>, RuntimeManifestError> {
    let mut resolved = BTreeSet::new();
    for feature in features {
        validate_identifier("io_features", &feature)?;
        if !KNOWN_IO_PROTOCOL_FEATURES.contains(&feature.as_str()) {
            return Err(RuntimeManifestError::UnknownIoFeature { id: feature });
        }
        if !resolved.insert(feature.clone()) {
            return Err(RuntimeManifestError::DuplicateIdentifier {
                field: "io_features",
                value: feature,
            });
        }
    }
    if resolved.contains("j1939") {
        resolved.insert("can".to_string());
    }
    Ok(resolved.into_iter().collect())
}

fn parse_package_features(cargo_features: &[String]) -> Result<Vec<String>, RuntimeManifestError> {
    let mut features = Vec::with_capacity(cargo_features.len());
    for qualified in cargo_features {
        let Some(feature) = qualified.strip_prefix("aether-io/") else {
            return Err(RuntimeManifestError::UnknownIoFeature {
                id: qualified.clone(),
            });
        };
        features.push(feature.to_string());
    }
    let canonical = canonical_io_features(features)?;
    let expected = canonical
        .iter()
        .map(|feature| format!("aether-io/{feature}"))
        .collect::<Vec<_>>();
    if cargo_features != expected {
        return Err(RuntimeManifestError::NonCanonicalIdentifiers {
            field: "cargo_features",
        });
    }
    Ok(canonical)
}

fn derive_protocols(features: &[String], target_os: &str) -> Vec<String> {
    let mut protocols = BTreeSet::from(["virtual".to_string()]);
    for feature in features {
        match feature.as_str() {
            "modbus" => {
                protocols.extend([
                    "modbus_rtu".to_string(),
                    "modbus_tcp".to_string(),
                    "sunspec_rtu".to_string(),
                    "sunspec_tcp".to_string(),
                ]);
            },
            "iec104" => insert(&mut protocols, "iec104"),
            "opcua" => insert(&mut protocols, "opcua"),
            "can" if target_os == "linux" => insert(&mut protocols, "can"),
            "j1939" if target_os == "linux" => insert(&mut protocols, "j1939"),
            "gpio" if target_os == "linux" => insert(&mut protocols, "di_do"),
            "dl645" => insert(&mut protocols, "dl645"),
            "aether_485" => insert(&mut protocols, "aether_485"),
            "mqtt" => insert(&mut protocols, "mqtt"),
            "http" => insert(&mut protocols, "http"),
            "ble" => insert(&mut protocols, "ble"),
            "zigbee" => insert(&mut protocols, "zigbee"),
            "matter" => insert(&mut protocols, "matter"),
            "iec61850" => insert(&mut protocols, "iec61850"),
            _ => {},
        }
    }
    protocols.into_iter().collect()
}

fn insert(values: &mut BTreeSet<String>, value: &str) {
    values.insert(value.to_string());
}

fn canonical_capabilities(capabilities: Vec<String>) -> Result<Vec<String>, RuntimeManifestError> {
    let capabilities = canonical_identifiers("capabilities", capabilities)?;
    validate_known_capabilities(&capabilities)?;
    Ok(capabilities)
}

fn validate_known_capabilities(capabilities: &[String]) -> Result<(), RuntimeManifestError> {
    let known = aether_application::capability_catalog()
        .iter()
        .map(|descriptor| descriptor.name())
        .collect::<BTreeSet<_>>();
    for capability in capabilities {
        if !known.contains(capability.as_str()) {
            return Err(RuntimeManifestError::UnknownCapability {
                id: capability.clone(),
            });
        }
    }
    Ok(())
}

fn validate_standard_capability_catalog(
    composition: &str,
    capabilities: &[String],
) -> Result<(), RuntimeManifestError> {
    if composition != "aether-edge-six-service" {
        return Ok(());
    }
    let mut expected = aether_application::capability_catalog()
        .iter()
        .map(|descriptor| descriptor.name().to_string())
        .collect::<Vec<_>>();
    expected.sort();
    if capabilities == expected {
        Ok(())
    } else {
        Err(RuntimeManifestError::CapabilityCatalogMismatch {
            expected,
            found: capabilities.to_vec(),
        })
    }
}

fn validate_standard_services(
    composition: &str,
    services: &[String],
) -> Result<(), RuntimeManifestError> {
    if composition != "aether-edge-six-service" {
        return Ok(());
    }
    let expected = SHIPPED_SERVICES.map(str::to_string).to_vec();
    if services == expected {
        Ok(())
    } else {
        Err(RuntimeManifestError::StandardServiceMismatch {
            expected,
            found: services.to_vec(),
        })
    }
}

fn canonical_identifiers(
    field: &'static str,
    values: Vec<String>,
) -> Result<Vec<String>, RuntimeManifestError> {
    let mut canonical = BTreeSet::new();
    for value in values {
        validate_identifier(field, &value)?;
        if !canonical.insert(value.clone()) {
            return Err(RuntimeManifestError::DuplicateIdentifier { field, value });
        }
    }
    Ok(canonical.into_iter().collect())
}

fn validate_canonical_identifiers(
    field: &'static str,
    values: &[String],
) -> Result<(), RuntimeManifestError> {
    for value in values {
        validate_identifier(field, value)?;
    }
    validate_canonical_order(field, values)
}

fn validate_canonical_order(
    field: &'static str,
    values: &[String],
) -> Result<(), RuntimeManifestError> {
    let canonical = values.iter().collect::<BTreeSet<_>>();
    if canonical.len() != values.len() {
        let mut seen = BTreeSet::new();
        let repeated = match values.iter().find(|value| !seen.insert(value.as_str())) {
            Some(repeated) => repeated.clone(),
            None => String::new(),
        };
        return Err(RuntimeManifestError::DuplicateIdentifier {
            field,
            value: repeated,
        });
    }
    if !values.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(RuntimeManifestError::NonCanonicalIdentifiers { field });
    }
    Ok(())
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), RuntimeManifestError> {
    let mut characters = value.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric());
    let valid_tail = characters.all(|character| {
        character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
    });
    if valid_start && valid_tail {
        Ok(())
    } else {
        Err(RuntimeManifestError::InvalidIdentifier {
            field,
            value: value.to_string(),
        })
    }
}

fn validate_aether_version(version: &str) -> Result<(), RuntimeManifestError> {
    Version::parse(version).map(|_| ()).map_err(|source| {
        RuntimeManifestError::InvalidAetherVersion {
            value: version.to_string(),
            source,
        }
    })
}

fn target_os_from_triple(target: &str) -> Result<&'static str, RuntimeManifestError> {
    let components = target.split('-').collect::<Vec<_>>();
    if components.len() < 3 || components.iter().any(|component| component.is_empty()) {
        return Err(RuntimeManifestError::UnsupportedTargetTriple {
            target: target.to_string(),
        });
    }
    if components.contains(&"linux") {
        Ok("linux")
    } else if components
        .windows(2)
        .any(|pair| pair == ["apple", "darwin"])
    {
        Ok("macos")
    } else if components.contains(&"windows") {
        Ok("windows")
    } else if components.contains(&"freebsd") {
        Ok("freebsd")
    } else {
        Err(RuntimeManifestError::UnsupportedTargetTriple {
            target: target.to_string(),
        })
    }
}

fn validate_checksum_shape(checksum: &ManifestChecksum) -> Result<(), RuntimeManifestError> {
    let valid_hex = checksum.digest.len() == 64
        && checksum
            .digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    if checksum.algorithm == "sha256" && valid_hex {
        Ok(())
    } else {
        Err(RuntimeManifestError::InvalidChecksum {
            value: format!("{}:{}", checksum.algorithm, checksum.digest),
        })
    }
}

fn checksum_payload(
    payload: &ManifestPayload<'_>,
) -> Result<ManifestChecksum, RuntimeManifestError> {
    let canonical = serde_json_canonicalizer::to_vec(payload)
        .map_err(|source| RuntimeManifestError::CanonicalJson { source })?;
    Ok(ManifestChecksum {
        algorithm: "sha256".to_string(),
        digest: format!("{:x}", Sha256::digest(canonical)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_gates_are_derived_from_the_target_triple() {
        let linux =
            protocol_adapters_for_io_features(["can", "gpio"], "aarch64-unknown-linux-musl")
                .expect("known Linux features");
        let macos = protocol_adapters_for_io_features(["can", "gpio"], "aarch64-apple-darwin")
            .expect("known macOS features");

        assert_eq!(linux, ["can", "di_do", "virtual"]);
        assert_eq!(macos, ["virtual"]);
    }

    #[test]
    fn round_trip_preserves_the_verified_checksum() {
        let manifest =
            shipped_distribution_manifest("aarch64-unknown-linux-musl").expect("shipped manifest");
        let source = manifest.to_pretty_json().expect("manifest JSON");
        let parsed = parse_runtime_manifest(&source, env!("CARGO_PKG_VERSION"))
            .expect("verified round trip");

        assert_eq!(parsed, manifest);
    }
}
