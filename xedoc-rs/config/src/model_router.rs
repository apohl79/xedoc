//! Configuration and last-good loading for the experimental model router.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use xedoc_protocol::openai_models::ReasoningEffort;
use xedoc_utils_absolute_path::AbsolutePathBuf;
use xedoc_utils_path::resolve_symlink_write_paths;
use xedoc_utils_path::write_atomically;

/// Runtime mode for model-router decisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ModelRouterMode {
    #[default]
    Off,
    ShadowSubagents,
    ShadowFull,
    Subagents,
    Full,
}

/// A provider/model/reasoning route configured for reporting or fallback.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRouterRoute {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: ReasoningEffort,
}

/// Stable controls in `[model_router]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRouterConfigToml {
    #[serde(default)]
    pub mode: ModelRouterMode,
    /// Ask before applying an eligible route in an active router mode.
    #[serde(default)]
    pub approval: bool,
    pub baseline: Option<ModelRouterRoute>,
    pub fallback: Option<String>,
    pub policy_path: Option<AbsolutePathBuf>,
    #[serde(default = "default_max_prompt_bytes")]
    pub max_prompt_bytes: usize,
    pub report_url: Option<String>,
}

impl Default for ModelRouterConfigToml {
    fn default() -> Self {
        Self {
            mode: ModelRouterMode::Off,
            approval: false,
            baseline: None,
            fallback: None,
            policy_path: None,
            max_prompt_bytes: default_max_prompt_bytes(),
            report_url: None,
        }
    }
}

impl ModelRouterConfigToml {
    /// Resolve the standalone policy path, defaulting to the Xedoc home.
    pub fn resolved_policy_path(&self, xedoc_home: &Path) -> PathBuf {
        self.policy_path.as_ref().map_or_else(
            || xedoc_home.join("model-router.toml"),
            |path| path.to_path_buf(),
        )
    }
}

const fn default_max_prompt_bytes() -> usize {
    16_384
}

const MAX_POLICY_IDENTIFIER_BYTES: usize = 128;
const MAX_REVIEW_NOTE_BYTES: usize = 512;

/// Versioned policy loaded from the standalone model-router TOML file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRouterPolicy {
    pub schema_version: u32,
    pub policy_revision: String,
    pub embedding: ModelRouterEmbedding,
    pub classifier: ModelRouterClassifier,
    #[serde(default)]
    pub capabilities: Vec<ModelRouterCapability>,
    pub classes: Vec<ModelRouterClass>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRouterEmbedding {
    pub runtime: String,
    pub model: String,
    pub revision: String,
    pub artifact_sha256: String,
    pub dimensions: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRouterClassifier {
    pub revision: String,
    pub parameters_sha256: String,
    pub minimum_score: f32,
    pub minimum_margin: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRouterClass {
    pub id: String,
    pub minimum_reasoning_effort: ReasoningEffort,
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    #[serde(default)]
    pub weights: Option<Vec<f32>>,
    #[serde(default)]
    pub minimum_score: Option<f32>,
    #[serde(default)]
    pub minimum_margin: Option<f32>,
}

/// User-managed preference tags for one otherwise eligible model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRouterCapability {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Errors returned while reading or validating a standalone policy.
#[derive(Debug, thiserror::Error)]
pub enum ModelRouterPolicyError {
    #[error("could not read model-router policy at {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid model-router policy at {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("unsupported model-router policy schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("model-router policy must contain at least one class")]
    EmptyClasses,
    #[error("model-router policy field is invalid: {0}")]
    InvalidField(&'static str),
    #[error("model-router policy contains duplicate class id")]
    DuplicateClassId,
    #[error("could not write model-router policy at {path}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("model-router policy revision is invalid: {0}")]
    InvalidRevision(String),
    #[error("model-router policy revision already exists with different content: {0}")]
    RevisionConflict(String),
    #[error("model-router policy revision was not found: {0}")]
    RevisionNotFound(String),
    #[error("model-router policy revision has not been reviewed: {0}")]
    RevisionNotReviewed(String),
    #[error("model-router policy review is invalid")]
    InvalidReview,
    #[error("could not serialize model-router policy review")]
    SerializeReview {
        #[source]
        source: toml::ser::Error,
    },
}

/// Parse and validate a policy file without changing any active state.
pub fn load_model_router_policy(path: &Path) -> Result<ModelRouterPolicy, ModelRouterPolicyError> {
    let contents = fs::read_to_string(path).map_err(|source| ModelRouterPolicyError::Read {
        path: path.to_owned(),
        source,
    })?;
    let policy = parse_model_router_policy(path, &contents)?;
    validate_policy(&policy)?;
    Ok(policy)
}

/// Validate and atomically replace a user-managed model-router policy.
///
/// Callers are responsible for preserving any review or provenance records
/// required by their workflow before making the policy active.
pub fn write_model_router_policy(
    path: &Path,
    policy: &ModelRouterPolicy,
) -> Result<(), ModelRouterPolicyError> {
    validate_policy(policy)?;
    let contents = toml::to_string_pretty(policy)
        .map_err(|source| ModelRouterPolicyError::SerializeReview { source })?;
    write_policy_bytes(path, &contents)
}

fn parse_model_router_policy(
    path: &Path,
    contents: &str,
) -> Result<ModelRouterPolicy, ModelRouterPolicyError> {
    toml::from_str(contents).map_err(|source| ModelRouterPolicyError::Parse {
        path: path.to_owned(),
        source,
    })
}

fn validate_policy(policy: &ModelRouterPolicy) -> Result<(), ModelRouterPolicyError> {
    if policy.schema_version != 1 {
        return Err(ModelRouterPolicyError::UnsupportedSchemaVersion(
            policy.schema_version,
        ));
    }
    if !is_bounded_identifier(&policy.policy_revision) {
        return Err(ModelRouterPolicyError::InvalidField("policy_revision"));
    }
    if policy.classes.is_empty() {
        return Err(ModelRouterPolicyError::EmptyClasses);
    }
    let embedding = &policy.embedding;
    for (field, value) in [
        ("embedding.runtime", embedding.runtime.as_str()),
        ("embedding.model", embedding.model.as_str()),
        ("embedding.revision", embedding.revision.as_str()),
        (
            "embedding.artifact_sha256",
            embedding.artifact_sha256.as_str(),
        ),
    ] {
        if !is_bounded_identifier(value) {
            return Err(ModelRouterPolicyError::InvalidField(field));
        }
    }
    if embedding.dimensions == 0 {
        return Err(ModelRouterPolicyError::InvalidField("embedding.dimensions"));
    }
    if !is_sha256_hex(&embedding.artifact_sha256) {
        return Err(ModelRouterPolicyError::InvalidField(
            "embedding.artifact_sha256",
        ));
    }

    let classifier = &policy.classifier;
    for (field, value) in [
        ("classifier.revision", classifier.revision.as_str()),
        (
            "classifier.parameters_sha256",
            classifier.parameters_sha256.as_str(),
        ),
    ] {
        if !is_bounded_identifier(value) {
            return Err(ModelRouterPolicyError::InvalidField(field));
        }
    }
    if !is_sha256_hex(&classifier.parameters_sha256) {
        return Err(ModelRouterPolicyError::InvalidField(
            "classifier.parameters_sha256",
        ));
    }
    if !classifier.minimum_score.is_finite() || !(0.0..=1.0).contains(&classifier.minimum_score) {
        return Err(ModelRouterPolicyError::InvalidField(
            "classifier.minimum_score",
        ));
    }
    if !classifier.minimum_margin.is_finite() || !(0.0..=1.0).contains(&classifier.minimum_margin) {
        return Err(ModelRouterPolicyError::InvalidField(
            "classifier.minimum_margin",
        ));
    }

    let mut capability_routes = HashSet::with_capacity(policy.capabilities.len());
    for capability in &policy.capabilities {
        if !is_bounded_identifier(&capability.provider) {
            return Err(ModelRouterPolicyError::InvalidField(
                "capabilities.provider",
            ));
        }
        if !is_bounded_identifier(&capability.model) {
            return Err(ModelRouterPolicyError::InvalidField("capabilities.model"));
        }
        if capability
            .tags
            .iter()
            .any(|tag| !is_bounded_identifier(tag))
            || !capability_routes.insert((&capability.provider, &capability.model))
        {
            return Err(ModelRouterPolicyError::InvalidField("capabilities.tags"));
        }
    }

    let mut class_ids = HashSet::with_capacity(policy.classes.len());
    for class in &policy.classes {
        if !is_bounded_identifier(&class.id) {
            return Err(ModelRouterPolicyError::InvalidField("classes.id"));
        }
        if !class_ids.insert(&class.id) {
            return Err(ModelRouterPolicyError::DuplicateClassId);
        }
        if !matches!(
            class.minimum_reasoning_effort,
            ReasoningEffort::Low
                | ReasoningEffort::Medium
                | ReasoningEffort::High
                | ReasoningEffort::XHigh
        ) {
            return Err(ModelRouterPolicyError::InvalidField(
                "classes.minimum_reasoning_effort",
            ));
        }
        if class
            .required_capabilities
            .iter()
            .any(|capability| !is_bounded_identifier(capability))
        {
            return Err(ModelRouterPolicyError::InvalidField(
                "classes.required_capabilities",
            ));
        }
        if let Some(weights) = &class.weights {
            if weights.len() != embedding.dimensions
                || weights.iter().any(|value| !value.is_finite())
            {
                return Err(ModelRouterPolicyError::InvalidField("classes.weights"));
            }
        }
        for (field, value) in [
            ("classes.minimum_score", class.minimum_score),
            ("classes.minimum_margin", class.minimum_margin),
        ] {
            if value.is_some_and(|value| !value.is_finite() || !(0.0..=1.0).contains(&value)) {
                return Err(ModelRouterPolicyError::InvalidField(field));
            }
        }
    }
    Ok(())
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_bounded_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_POLICY_IDENTIFIER_BYTES
}

fn is_safe_revision(value: &str) -> bool {
    is_bounded_identifier(value)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PolicyFingerprint {
    digest: [u8; 32],
}

/// Atomically replaceable policy state that retains the last valid snapshot.
#[derive(Debug, Clone)]
pub struct ModelRouterPolicyStore {
    path: PathBuf,
    active: Arc<RwLock<Option<Arc<ModelRouterPolicy>>>>,
    fingerprint: Arc<RwLock<Option<PolicyFingerprint>>>,
    last_error: Arc<RwLock<Option<String>>>,
}

/// Health and identity information for the currently active policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRouterPolicyStatus {
    pub healthy: bool,
    pub active_revision: Option<String>,
    pub artifact_identity: Option<String>,
    pub last_reload_error: Option<String>,
}

impl ModelRouterPolicyStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            active: Arc::new(RwLock::new(None)),
            fingerprint: Arc::new(RwLock::new(None)),
            last_error: Arc::new(RwLock::new(None)),
        }
    }

    pub fn snapshot(&self) -> Option<Arc<ModelRouterPolicy>> {
        self.active.read().ok().and_then(|active| active.clone())
    }

    pub fn last_error(&self) -> Option<String> {
        self.last_error.read().ok().and_then(|error| error.clone())
    }

    pub fn status(&self) -> ModelRouterPolicyStatus {
        let active = self.snapshot();
        let last_reload_error = self.last_error();
        ModelRouterPolicyStatus {
            healthy: active.is_some() && last_reload_error.is_none(),
            active_revision: active.as_ref().map(|policy| policy.policy_revision.clone()),
            artifact_identity: active
                .as_ref()
                .map(|policy| policy.embedding.artifact_sha256.clone()),
            last_reload_error,
        }
    }

    pub fn reload_if_changed(&self) -> Result<bool, ModelRouterPolicyError> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(source) => {
                let error = ModelRouterPolicyError::Read {
                    path: self.path.clone(),
                    source,
                };
                self.record_error(&error);
                return Err(error);
            }
        };
        let digest = Sha256::digest(contents.as_bytes());
        let fingerprint = PolicyFingerprint {
            digest: digest.into(),
        };
        if self
            .fingerprint
            .read()
            .ok()
            .is_some_and(|current| *current == Some(fingerprint))
        {
            return Ok(false);
        }

        match parse_model_router_policy(&self.path, &contents).and_then(|policy| {
            validate_policy(&policy)?;
            Ok(policy)
        }) {
            Ok(policy) => {
                if let Ok(mut active) = self.active.write() {
                    *active = Some(Arc::new(policy));
                }
                if let Ok(mut current) = self.fingerprint.write() {
                    *current = Some(fingerprint);
                }
                if let Ok(mut error) = self.last_error.write() {
                    *error = None;
                }
                Ok(true)
            }
            Err(error) => {
                if let Ok(mut last_error) = self.last_error.write() {
                    *last_error = Some(error.to_string());
                }
                Err(error)
            }
        }
    }

    fn record_error(&self, error: &ModelRouterPolicyError) {
        if let Ok(mut last_error) = self.last_error.write() {
            *last_error = Some(error.to_string());
        }
    }
}

/// Immutable local revisions and reviewed activation for model-router policy.
///
/// Staging never changes the policy consumed by the router. Activation and
/// rollback replace that file atomically only after a matching review record
/// has been written.
#[derive(Debug, Clone)]
pub struct ModelRouterPolicyRevisionStore {
    active_path: PathBuf,
    revisions_dir: PathBuf,
}

/// A bounded reviewer attestation required before activating a policy revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRouterPolicyReview {
    pub reviewed_by: String,
    pub review_note: String,
    /// The reviewed proposal passed its frozen held-out calibration gates.
    pub calibration_gate_passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPolicyReview {
    revision: String,
    policy_sha256: String,
    reviewed_by: String,
    review_note: String,
    calibration_gate_passed: bool,
}

impl ModelRouterPolicyRevisionStore {
    /// Create a revision store beside the policy file it activates.
    pub fn new(active_path: impl Into<PathBuf>) -> Self {
        let active_path = active_path.into();
        let revisions_dir = active_path.parent().map_or_else(
            || PathBuf::from("model-router-revisions"),
            |parent| parent.join("model-router-revisions"),
        );
        Self {
            active_path,
            revisions_dir,
        }
    }

    /// Use an explicit local directory for staged policy revisions.
    pub fn with_revisions_dir(
        active_path: impl Into<PathBuf>,
        revisions_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            active_path: active_path.into(),
            revisions_dir: revisions_dir.into(),
        }
    }

    /// Parse and save a non-active candidate under its declared revision.
    ///
    /// # Errors
    ///
    /// Returns [`ModelRouterPolicyError`] if the candidate cannot be validated
    /// or a same-named revision has different contents.
    pub fn stage(
        &self,
        candidate_path: &Path,
    ) -> Result<ModelRouterPolicy, ModelRouterPolicyError> {
        let policy = load_model_router_policy(candidate_path)?;
        self.stage_bytes(&policy.policy_revision, &read_policy_bytes(candidate_path)?)?;
        Ok(policy)
    }

    /// Load one staged policy revision without changing the active policy.
    ///
    /// # Errors
    ///
    /// Returns [`ModelRouterPolicyError`] when the revision is missing or invalid.
    pub fn inspect(&self, revision: &str) -> Result<ModelRouterPolicy, ModelRouterPolicyError> {
        let path = self.revision_path(revision)?;
        let policy = load_model_router_policy(&path).map_err(|error| match error {
            ModelRouterPolicyError::Read { source, .. }
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                ModelRouterPolicyError::RevisionNotFound(revision.to_string())
            }
            error => error,
        })?;
        (policy.policy_revision == revision)
            .then_some(policy)
            .ok_or_else(|| ModelRouterPolicyError::InvalidRevision(revision.to_string()))
    }

    /// Persist an explicit reviewer attestation for a staged revision.
    ///
    /// # Errors
    ///
    /// Returns [`ModelRouterPolicyError`] if the review is not bounded or the
    /// named revision has not been staged.
    pub fn record_review(
        &self,
        revision: &str,
        review: &ModelRouterPolicyReview,
    ) -> Result<(), ModelRouterPolicyError> {
        if !is_bounded_identifier(&review.reviewed_by)
            || review.review_note.is_empty()
            || review.review_note.len() > MAX_REVIEW_NOTE_BYTES
        {
            return Err(ModelRouterPolicyError::InvalidReview);
        }
        self.inspect(revision)?;
        let stored = StoredPolicyReview {
            revision: revision.to_string(),
            policy_sha256: policy_fingerprint(&read_policy_bytes(&self.revision_path(revision)?)?),
            reviewed_by: review.reviewed_by.clone(),
            review_note: review.review_note.clone(),
            calibration_gate_passed: review.calibration_gate_passed,
        };
        let contents = toml::to_string_pretty(&stored)
            .map_err(|source| ModelRouterPolicyError::SerializeReview { source })?;
        write_policy_bytes(&self.review_path(revision)?, &contents)
    }

    /// Atomically activate a staged revision with a matching review attestation.
    ///
    /// The previous valid active policy is retained as an immutable revision
    /// before the active path is replaced.
    ///
    /// # Errors
    ///
    /// Returns [`ModelRouterPolicyError`] when review evidence is absent,
    /// stale, or a file operation fails.
    pub fn activate_reviewed(
        &self,
        revision: &str,
    ) -> Result<ModelRouterPolicy, ModelRouterPolicyError> {
        let policy = self.inspect(revision)?;
        self.validate_review(revision)?;
        self.archive_active_policy()?;
        let contents = read_policy_bytes(&self.revision_path(revision)?)?;
        write_policy_bytes(
            &self.active_path,
            std::str::from_utf8(&contents)
                .map_err(|_| ModelRouterPolicyError::InvalidRevision(revision.to_string()))?,
        )?;
        Ok(policy)
    }

    /// Atomically restore a previously reviewed staged revision.
    ///
    /// # Errors
    ///
    /// Returns [`ModelRouterPolicyError`] when the target revision is not
    /// staged and reviewed.
    pub fn rollback_reviewed(
        &self,
        revision: &str,
    ) -> Result<ModelRouterPolicy, ModelRouterPolicyError> {
        self.activate_reviewed(revision)
    }

    /// Whether the active policy exactly matches a reviewed calibrated revision.
    ///
    /// This is a read-only runtime gate. It deliberately rejects manually
    /// edited active policies, stale review attestations, and reviews that do
    /// not attest a successful held-out calibration.
    pub fn active_revision_is_calibrated(&self, revision: &str) -> bool {
        let staged_path = match self.revision_path(revision) {
            Ok(path) => path,
            Err(_) => return false,
        };
        let active = match read_policy_bytes(&self.active_path) {
            Ok(contents) => contents,
            Err(_) => return false,
        };
        let staged = match read_policy_bytes(&staged_path) {
            Ok(contents) => contents,
            Err(_) => return false,
        };
        if active != staged
            || load_model_router_policy(&self.active_path)
                .ok()
                .is_none_or(|policy| policy.policy_revision != revision)
        {
            return false;
        }
        self.validate_review(revision).is_ok()
    }

    fn archive_active_policy(&self) -> Result<(), ModelRouterPolicyError> {
        match load_model_router_policy(&self.active_path) {
            Ok(policy) => self.stage_bytes(
                &policy.policy_revision,
                &read_policy_bytes(&self.active_path)?,
            ),
            Err(ModelRouterPolicyError::Read { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn validate_review(&self, revision: &str) -> Result<(), ModelRouterPolicyError> {
        let review_path = self.review_path(revision)?;
        let contents = fs::read_to_string(&review_path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ModelRouterPolicyError::RevisionNotReviewed(revision.to_string())
            } else {
                ModelRouterPolicyError::Read {
                    path: review_path.clone(),
                    source: error,
                }
            }
        })?;
        let review: StoredPolicyReview =
            toml::from_str(&contents).map_err(|source| ModelRouterPolicyError::Parse {
                path: review_path.clone(),
                source,
            })?;
        let policy_sha256 = policy_fingerprint(&read_policy_bytes(&self.revision_path(revision)?)?);
        (review.revision == revision
            && review.policy_sha256 == policy_sha256
            && is_bounded_identifier(&review.reviewed_by)
            && !review.review_note.is_empty()
            && review.review_note.len() <= MAX_REVIEW_NOTE_BYTES
            && review.calibration_gate_passed)
            .then_some(())
            .ok_or_else(|| ModelRouterPolicyError::RevisionNotReviewed(revision.to_string()))
    }

    fn stage_bytes(&self, revision: &str, contents: &[u8]) -> Result<(), ModelRouterPolicyError> {
        let path = self.revision_path(revision)?;
        match fs::read(&path) {
            Ok(existing) if existing == contents => Ok(()),
            Ok(_) => Err(ModelRouterPolicyError::RevisionConflict(
                revision.to_string(),
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let contents = std::str::from_utf8(contents)
                    .map_err(|_| ModelRouterPolicyError::InvalidRevision(revision.to_string()))?;
                write_policy_bytes(&path, contents)
            }
            Err(source) => Err(ModelRouterPolicyError::Read { path, source }),
        }
    }

    fn revision_path(&self, revision: &str) -> Result<PathBuf, ModelRouterPolicyError> {
        is_safe_revision(revision)
            .then(|| self.revisions_dir.join(format!("{revision}.toml")))
            .ok_or_else(|| ModelRouterPolicyError::InvalidRevision(revision.to_string()))
    }

    fn review_path(&self, revision: &str) -> Result<PathBuf, ModelRouterPolicyError> {
        is_safe_revision(revision)
            .then(|| self.revisions_dir.join(format!("{revision}.review.toml")))
            .ok_or_else(|| ModelRouterPolicyError::InvalidRevision(revision.to_string()))
    }
}

fn read_policy_bytes(path: &Path) -> Result<Vec<u8>, ModelRouterPolicyError> {
    fs::read(path).map_err(|source| ModelRouterPolicyError::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn write_policy_bytes(path: &Path, contents: &str) -> Result<(), ModelRouterPolicyError> {
    let write_path = resolve_symlink_write_paths(path)
        .map_err(|source| ModelRouterPolicyError::Write {
            path: path.to_path_buf(),
            source,
        })?
        .write_path;
    write_atomically(&write_path, contents).map_err(|source| ModelRouterPolicyError::Write {
        path: write_path,
        source,
    })
}

fn policy_fingerprint(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}
