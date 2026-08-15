//! Typed adapter compatibility metadata and the compatibility-v2 evaluator.
//!
//! Compatibility v2 deliberately does not contain or compare the global
//! database-contract hash. That hash remains a legacy-v1 and provenance value.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::database_contract::{AdapterDatabaseContract, OwnedDatabaseComponentContract};

pub const ADAPTER_COMPATIBILITY_FORMAT_V2: &str = "ldgr.adapter-compatibility.v2";
pub const CORE_COMPATIBILITY_FORMAT_V2: &str = "ldgr.core-compatibility.v2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterCompatibilitySidecarV2 {
    pub format: String,
    pub adapter: String,
    pub compatibility: CompatibilityRequirementsV2,
    pub local_stores: Vec<LocalStoreDescriptorV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityRequirementsV2 {
    pub adapter_protocol_epoch: i32,
    pub minimum_core_schema: i32,
    pub required_core_capabilities: Vec<String>,
    pub central_components: Vec<CentralComponentRequirementV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CentralComponentRequirementV2 {
    pub namespace: String,
    pub schema_epoch: i32,
    pub minimum_schema_version: i32,
    pub accepted_lineage_digests: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalStoreDescriptorV2 {
    pub store_id: String,
    pub engine: String,
    pub schema_version: i32,
    pub migration_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreCompatibilityProfileV2 {
    pub format: String,
    pub core_schema_version: i32,
    pub supported_adapter_protocol_epochs: Vec<i32>,
    pub core_capabilities: Vec<String>,
    pub central_components: Vec<CentralComponentDescriptorV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CentralComponentDescriptorV2 {
    pub namespace: String,
    pub owner_adapter: String,
    pub schema_epoch: i32,
    pub schema_version: i32,
    pub lineage: Vec<ComponentLineageEntryV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentLineageEntryV2 {
    pub schema_version: i32,
    pub migration_digest: String,
}

/// Independently validated state read from the central database component
/// catalog. It is kept separate from the compiled/signed Core inventory so a
/// pending, missing, or tampered database migration cannot pass evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CentralComponentDatabaseStateV2 {
    pub namespace: String,
    pub schema_epoch: i32,
    pub schema_version: i32,
    pub lineage: Vec<ComponentLineageEntryV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParsedAdapterCompatibility {
    V2 {
        sidecar: AdapterCompatibilitySidecarV2,
    },
    LegacyV1 {
        contract: AdapterDatabaseContract,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityEvaluation {
    pub compatible: bool,
    pub reasons: Vec<CompatibilityReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityReason {
    pub code: CompatibilityReasonCode,
    pub subject: String,
    pub required: Value,
    pub actual: Value,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompatibilityReasonCode {
    #[serde(rename = "adapter.identity_mismatch")]
    AdapterIdentityMismatch,
    #[serde(rename = "compatibility.invalid_metadata")]
    InvalidMetadata,
    #[serde(rename = "compatibility.unsupported_format")]
    UnsupportedFormat,
    #[serde(rename = "compatibility.protocol_epoch_unsupported")]
    ProtocolEpochUnsupported,
    #[serde(rename = "compatibility.minimum_core_schema_unsatisfied")]
    MinimumCoreSchemaUnsatisfied,
    #[serde(rename = "compatibility.core_capability_missing")]
    CoreCapabilityMissing,
    #[serde(rename = "compatibility.central_component_missing")]
    CentralComponentMissing,
    #[serde(rename = "compatibility.central_component_owner_mismatch")]
    CentralComponentOwnerMismatch,
    #[serde(rename = "compatibility.central_component_epoch_mismatch")]
    CentralComponentEpochMismatch,
    #[serde(rename = "compatibility.central_component_schema_unsatisfied")]
    CentralComponentSchemaUnsatisfied,
    #[serde(rename = "compatibility.central_component_lineage_mismatch")]
    CentralComponentLineageMismatch,
    #[serde(rename = "compatibility.central_component_database_state")]
    CentralComponentDatabaseState,
    #[serde(rename = "compatibility.legacy_global_contract_mismatch")]
    LegacyGlobalContractMismatch,
    #[serde(rename = "compatibility.legacy_core_schema_mismatch")]
    LegacyCoreSchemaMismatch,
}

impl CompatibilityReasonCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdapterIdentityMismatch => "adapter.identity_mismatch",
            Self::InvalidMetadata => "compatibility.invalid_metadata",
            Self::UnsupportedFormat => "compatibility.unsupported_format",
            Self::ProtocolEpochUnsupported => "compatibility.protocol_epoch_unsupported",
            Self::MinimumCoreSchemaUnsatisfied => "compatibility.minimum_core_schema_unsatisfied",
            Self::CoreCapabilityMissing => "compatibility.core_capability_missing",
            Self::CentralComponentMissing => "compatibility.central_component_missing",
            Self::CentralComponentOwnerMismatch => "compatibility.central_component_owner_mismatch",
            Self::CentralComponentEpochMismatch => "compatibility.central_component_epoch_mismatch",
            Self::CentralComponentSchemaUnsatisfied => {
                "compatibility.central_component_schema_unsatisfied"
            }
            Self::CentralComponentLineageMismatch => {
                "compatibility.central_component_lineage_mismatch"
            }
            Self::CentralComponentDatabaseState => "compatibility.central_component_database_state",
            Self::LegacyGlobalContractMismatch => "compatibility.legacy_global_contract_mismatch",
            Self::LegacyCoreSchemaMismatch => "compatibility.legacy_core_schema_mismatch",
        }
    }
}

impl fmt::Display for CompatibilityReasonCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityMetadataError {
    pub reason: CompatibilityReason,
}

impl fmt::Display for CompatibilityMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.reason.code, self.reason.message)
    }
}

impl std::error::Error for CompatibilityMetadataError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyCoreProfileV1 {
    pub contract_hash: String,
    pub core_schema_version: i64,
    pub components: Vec<OwnedDatabaseComponentContract>,
}

impl AdapterCompatibilitySidecarV2 {
    pub fn validate(&self) -> Result<(), CompatibilityMetadataError> {
        if self.format != ADAPTER_COMPATIBILITY_FORMAT_V2 {
            return Err(metadata_error(
                CompatibilityReasonCode::UnsupportedFormat,
                "format",
                Value::String(ADAPTER_COMPATIBILITY_FORMAT_V2.to_owned()),
                Value::String(self.format.clone()),
                format!("unsupported adapter compatibility format {}", self.format),
            ));
        }
        validate_adapter_identifier("adapter", &self.adapter)?;
        self.compatibility.validate()?;
        validate_strictly_sorted_by("local_stores", &self.local_stores, |store| {
            store.store_id.as_str()
        })?;
        for store in &self.local_stores {
            validate_adapter_identifier("local_stores.store_id", &store.store_id)?;
            validate_adapter_identifier("local_stores.engine", &store.engine)?;
            validate_positive("local_stores.schema_version", store.schema_version)?;
            validate_digest("local_stores.migration_digest", &store.migration_digest)?;
        }
        Ok(())
    }

    /// RFC 8785/JCS bytes for the constrained v2 metadata model. No trailing LF
    /// is included; producers append one when writing a sidecar file.
    pub fn canonical_json(&self) -> Result<Vec<u8>, CompatibilityMetadataError> {
        self.validate()?;
        canonical_bytes(self)
    }

    pub fn canonical_file_json(&self) -> Result<Vec<u8>, CompatibilityMetadataError> {
        let mut bytes = self.canonical_json()?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn compatibility_sha256(&self) -> Result<String, CompatibilityMetadataError> {
        self.compatibility.compatibility_sha256()
    }
}

impl CompatibilityRequirementsV2 {
    pub fn validate(&self) -> Result<(), CompatibilityMetadataError> {
        validate_positive(
            "compatibility.adapter_protocol_epoch",
            self.adapter_protocol_epoch,
        )?;
        validate_positive(
            "compatibility.minimum_core_schema",
            self.minimum_core_schema,
        )?;
        validate_strictly_sorted(
            "compatibility.required_core_capabilities",
            &self.required_core_capabilities,
        )?;
        for capability in &self.required_core_capabilities {
            validate_capability_identifier("compatibility.required_core_capabilities", capability)?;
        }
        validate_strictly_sorted_by(
            "compatibility.central_components",
            &self.central_components,
            |component| component.namespace.as_str(),
        )?;
        for component in &self.central_components {
            validate_adapter_identifier(
                "compatibility.central_components.namespace",
                &component.namespace,
            )?;
            validate_positive(
                "compatibility.central_components.schema_epoch",
                component.schema_epoch,
            )?;
            validate_positive(
                "compatibility.central_components.minimum_schema_version",
                component.minimum_schema_version,
            )?;
            if component.accepted_lineage_digests.is_empty() {
                return Err(invalid_metadata(
                    "compatibility.central_components.accepted_lineage_digests",
                    "accepted lineage digests must not be empty",
                ));
            }
            validate_strictly_sorted(
                "compatibility.central_components.accepted_lineage_digests",
                &component.accepted_lineage_digests,
            )?;
            for digest in &component.accepted_lineage_digests {
                validate_digest(
                    "compatibility.central_components.accepted_lineage_digests",
                    digest,
                )?;
            }
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, CompatibilityMetadataError> {
        self.validate()?;
        canonical_bytes(self)
    }

    pub fn compatibility_sha256(&self) -> Result<String, CompatibilityMetadataError> {
        let canonical = self.canonical_json()?;
        Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
    }
}

impl CoreCompatibilityProfileV2 {
    pub fn validate(&self) -> Result<(), CompatibilityMetadataError> {
        if self.format != CORE_COMPATIBILITY_FORMAT_V2 {
            return Err(metadata_error(
                CompatibilityReasonCode::UnsupportedFormat,
                "format",
                Value::String(CORE_COMPATIBILITY_FORMAT_V2.to_owned()),
                Value::String(self.format.clone()),
                format!("unsupported Core compatibility format {}", self.format),
            ));
        }
        validate_positive("core_schema_version", self.core_schema_version)?;
        if self.supported_adapter_protocol_epochs.is_empty() {
            return Err(invalid_metadata(
                "supported_adapter_protocol_epochs",
                "supported adapter protocol epochs must not be empty",
            ));
        }
        validate_strictly_sorted_i32(
            "supported_adapter_protocol_epochs",
            &self.supported_adapter_protocol_epochs,
        )?;
        for epoch in &self.supported_adapter_protocol_epochs {
            validate_positive("supported_adapter_protocol_epochs", *epoch)?;
        }
        validate_strictly_sorted("core_capabilities", &self.core_capabilities)?;
        for capability in &self.core_capabilities {
            validate_capability_identifier("core_capabilities", capability)?;
        }
        validate_strictly_sorted_by(
            "central_components",
            &self.central_components,
            |component| component.namespace.as_str(),
        )?;
        for component in &self.central_components {
            component.validate()?;
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>, CompatibilityMetadataError> {
        self.validate()?;
        canonical_bytes(self)
    }
}

impl CentralComponentDescriptorV2 {
    fn validate(&self) -> Result<(), CompatibilityMetadataError> {
        validate_adapter_identifier("central_components.namespace", &self.namespace)?;
        validate_adapter_identifier("central_components.owner_adapter", &self.owner_adapter)?;
        validate_positive("central_components.schema_epoch", self.schema_epoch)?;
        validate_positive("central_components.schema_version", self.schema_version)?;
        validate_lineage(
            "central_components.lineage",
            &self.lineage,
            self.schema_version,
        )
    }
}

impl CentralComponentDatabaseStateV2 {
    pub fn validate(&self) -> Result<(), CompatibilityMetadataError> {
        validate_adapter_identifier("database_components.namespace", &self.namespace)?;
        validate_positive("database_components.schema_epoch", self.schema_epoch)?;
        validate_positive("database_components.schema_version", self.schema_version)?;
        validate_lineage(
            "database_components.lineage",
            &self.lineage,
            self.schema_version,
        )
    }
}

fn validate_lineage(
    field: &str,
    lineage: &[ComponentLineageEntryV2],
    current_version: i32,
) -> Result<(), CompatibilityMetadataError> {
    if lineage.is_empty() {
        return Err(invalid_metadata(
            field,
            "component lineage must not be empty",
        ));
    }
    let mut previous = None;
    for entry in lineage {
        validate_positive(&format!("{field}.schema_version"), entry.schema_version)?;
        if entry.schema_version > current_version {
            return Err(invalid_metadata(
                field,
                "component lineage version exceeds current component version",
            ));
        }
        if previous.is_some_and(|value| value >= entry.schema_version) {
            return Err(invalid_metadata(
                field,
                "component lineage versions must be unique and ascending",
            ));
        }
        validate_digest(
            &format!("{field}.migration_digest"),
            &entry.migration_digest,
        )?;
        previous = Some(entry.schema_version);
    }
    if previous != Some(current_version) {
        return Err(invalid_metadata(
            field,
            "component lineage must include the current component version",
        ));
    }
    Ok(())
}

/// Parse a v2 sidecar with strict field and collection validation.
pub fn parse_adapter_compatibility_v2(
    text: &str,
) -> Result<AdapterCompatibilitySidecarV2, CompatibilityMetadataError> {
    let sidecar: AdapterCompatibilitySidecarV2 = serde_json::from_str(text).map_err(|error| {
        invalid_metadata(
            "adapter-compatibility.json",
            format!("failed to parse adapter compatibility metadata: {error}"),
        )
    })?;
    sidecar.validate()?;
    Ok(sidecar)
}

pub fn parse_core_compatibility_v2(
    text: &str,
) -> Result<CoreCompatibilityProfileV2, CompatibilityMetadataError> {
    let profile: CoreCompatibilityProfileV2 = serde_json::from_str(text).map_err(|error| {
        invalid_metadata(
            "core-compatibility.json",
            format!("failed to parse Core compatibility metadata: {error}"),
        )
    })?;
    profile.validate()?;
    Ok(profile)
}

/// Parse installed compatibility metadata with the normative precedence rule:
/// when v2 is present it is authoritative, including when malformed. Legacy v1
/// is read only when no v2 sidecar exists.
pub fn parse_adapter_compatibility(
    v2_text: Option<&str>,
    legacy_v1_text: Option<&str>,
) -> Result<ParsedAdapterCompatibility, CompatibilityMetadataError> {
    if let Some(text) = v2_text {
        return parse_adapter_compatibility_v2(text)
            .map(|sidecar| ParsedAdapterCompatibility::V2 { sidecar });
    }
    let text = legacy_v1_text.ok_or_else(|| {
        invalid_metadata(
            "adapter compatibility sidecar",
            "neither adapter-compatibility.json nor adapter-database-contract.json is present",
        )
    })?;
    parse_legacy_adapter_contract_v1(text)
        .map(|contract| ParsedAdapterCompatibility::LegacyV1 { contract })
}

/// Strict syntax reader for the bounded v1 bridge. Compatibility remains exact
/// and is evaluated separately by [`evaluate_legacy_v1`].
pub fn parse_legacy_adapter_contract_v1(
    text: &str,
) -> Result<AdapterDatabaseContract, CompatibilityMetadataError> {
    let contract: AdapterDatabaseContract = serde_json::from_str(text).map_err(|error| {
        invalid_metadata(
            "adapter-database-contract.json",
            format!("failed to parse legacy adapter database contract: {error}"),
        )
    })?;
    if contract.format != crate::database_contract::ADAPTER_DATABASE_CONTRACT_FORMAT {
        return Err(metadata_error(
            CompatibilityReasonCode::UnsupportedFormat,
            "format",
            Value::String(crate::database_contract::ADAPTER_DATABASE_CONTRACT_FORMAT.to_owned()),
            Value::String(contract.format.clone()),
            format!(
                "unsupported legacy adapter database contract format {}",
                contract.format
            ),
        ));
    }
    validate_digest("contract_hash", &contract.contract_hash)?;
    validate_positive_i64("core_schema_version", contract.core_schema_version)?;
    validate_adapter_identifier("component.namespace", &contract.component.namespace)?;
    validate_positive_i64(
        "component.schema_version",
        contract.component.schema_version,
    )?;
    validate_positive_i64(
        "component.minimum_core_schema",
        contract.component.minimum_core_schema,
    )?;
    validate_digest(
        "component.migration_digest",
        &contract.component.migration_digest,
    )?;
    Ok(contract)
}

/// Inventory compiled into the current Core. Central component registration is
/// intentionally empty until a component is explicitly classified and
/// generated as central; legacy generated adapter components are not inferred.
pub fn core_compatibility_inventory() -> CoreCompatibilityProfileV2 {
    let profile: CoreCompatibilityProfileV2 =
        serde_json::from_str(crate::database_contract::GENERATED_CORE_COMPATIBILITY_JSON)
            .expect("generated Core compatibility inventory must be valid");
    profile
        .validate()
        .expect("generated Core compatibility inventory must conform");
    profile
}

/// Exact global contract inventory retained only for candidate evaluation of
/// legacy-v1 adapters during the bounded migration window.
pub fn legacy_core_compatibility_inventory() -> LegacyCoreProfileV1 {
    let components =
        serde_json::from_str::<Value>(crate::database_contract::database_release_set_json())
            .ok()
            .and_then(|value| value.get("components").cloned())
            .and_then(|value| {
                serde_json::from_value::<Vec<OwnedDatabaseComponentContract>>(value).ok()
            })
            .unwrap_or_default();
    LegacyCoreProfileV1 {
        contract_hash: crate::database_contract::DATABASE_RELEASE_SET_HASH.to_owned(),
        core_schema_version: crate::database_contract::GENERATED_CORE_SCHEMA_VERSION,
        components,
    }
}

/// Project the signed compiled central-component inventory to the state that
/// must exist after the candidate Core's migrations finish.
pub fn projected_database_components(
    profile: &CoreCompatibilityProfileV2,
) -> Vec<CentralComponentDatabaseStateV2> {
    profile
        .central_components
        .iter()
        .map(|component| CentralComponentDatabaseStateV2 {
            namespace: component.namespace.clone(),
            schema_epoch: component.schema_epoch,
            schema_version: component.schema_version,
            lineage: component.lineage.clone(),
        })
        .collect()
}

/// Evaluate a valid v2 sidecar against a Core inventory and independently
/// validated central database component state. Local stores and all global
/// contract hashes are intentionally absent from this decision.
pub fn evaluate_v2(
    sidecar: &AdapterCompatibilitySidecarV2,
    expected_adapter: &str,
    core: &CoreCompatibilityProfileV2,
    database_components: &[CentralComponentDatabaseStateV2],
) -> CompatibilityEvaluation {
    if let Err(error) = sidecar.validate() {
        return incompatible(vec![error.reason]);
    }
    if let Err(error) = core.validate() {
        return incompatible(vec![error.reason]);
    }
    if let Err(error) = validate_database_components(database_components) {
        return incompatible(vec![error.reason]);
    }

    let mut reasons = Vec::new();
    if sidecar.adapter != expected_adapter {
        reasons.push(reason(
            CompatibilityReasonCode::AdapterIdentityMismatch,
            "adapter",
            Value::String(expected_adapter.to_owned()),
            Value::String(sidecar.adapter.clone()),
            format!(
                "sidecar adapter {} does not match expected adapter {expected_adapter}",
                sidecar.adapter
            ),
        ));
    }
    reasons.extend(
        evaluate_requirements_v2(
            &sidecar.compatibility,
            &sidecar.adapter,
            core,
            database_components,
        )
        .reasons,
    );
    CompatibilityEvaluation {
        compatible: reasons.is_empty(),
        reasons,
    }
}

/// Evaluate a release-index compatibility object. Adapter identity is supplied
/// separately because it is excluded from the compatibility fingerprint.
pub fn evaluate_requirements_v2(
    requirements: &CompatibilityRequirementsV2,
    adapter: &str,
    core: &CoreCompatibilityProfileV2,
    database_components: &[CentralComponentDatabaseStateV2],
) -> CompatibilityEvaluation {
    if let Err(error) = requirements.validate() {
        return incompatible(vec![error.reason]);
    }
    if let Err(error) = validate_adapter_identifier("adapter", adapter) {
        return incompatible(vec![error.reason]);
    }
    if let Err(error) = core.validate() {
        return incompatible(vec![error.reason]);
    }
    if let Err(error) = validate_database_components(database_components) {
        return incompatible(vec![error.reason]);
    }

    let mut reasons = Vec::new();
    if !core
        .supported_adapter_protocol_epochs
        .contains(&requirements.adapter_protocol_epoch)
    {
        reasons.push(reason(
            CompatibilityReasonCode::ProtocolEpochUnsupported,
            "adapter_protocol_epoch",
            Value::from(requirements.adapter_protocol_epoch),
            json_i32_array(&core.supported_adapter_protocol_epochs),
            format!(
                "adapter protocol epoch {} is not supported by Core",
                requirements.adapter_protocol_epoch
            ),
        ));
    }

    if core.core_schema_version < requirements.minimum_core_schema {
        reasons.push(reason(
            CompatibilityReasonCode::MinimumCoreSchemaUnsatisfied,
            "core_schema_version",
            Value::from(requirements.minimum_core_schema),
            Value::from(core.core_schema_version),
            format!(
                "adapter requires Core schema {} or newer; active Core schema is {}",
                requirements.minimum_core_schema, core.core_schema_version
            ),
        ));
    }

    let capabilities = core
        .core_capabilities
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    for capability in &requirements.required_core_capabilities {
        if !capabilities.contains(capability.as_str()) {
            reasons.push(reason(
                CompatibilityReasonCode::CoreCapabilityMissing,
                capability,
                Value::String(capability.clone()),
                Value::Array(
                    core.core_capabilities
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
                format!("Core capability {capability} is required but not advertised"),
            ));
        }
    }

    let registered = core
        .central_components
        .iter()
        .map(|component| (component.namespace.as_str(), component))
        .collect::<BTreeMap<_, _>>();
    let database = database_components
        .iter()
        .map(|component| (component.namespace.as_str(), component))
        .collect::<BTreeMap<_, _>>();

    for requirement in &requirements.central_components {
        let subject = format!("central_component:{}", requirement.namespace);
        let Some(component) = registered.get(requirement.namespace.as_str()) else {
            reasons.push(reason(
                CompatibilityReasonCode::CentralComponentMissing,
                &subject,
                Value::String(requirement.namespace.clone()),
                Value::Null,
                format!(
                    "central component {} is not registered",
                    requirement.namespace
                ),
            ));
            continue;
        };

        let owner_mismatch = component.owner_adapter != adapter;
        if owner_mismatch {
            reasons.push(reason(
                CompatibilityReasonCode::CentralComponentOwnerMismatch,
                &subject,
                Value::String(adapter.to_owned()),
                Value::String(component.owner_adapter.clone()),
                format!(
                    "central component {} is owned by {}, not {adapter}",
                    requirement.namespace, component.owner_adapter
                ),
            ));
        }
        let epoch_mismatch = component.schema_epoch != requirement.schema_epoch;
        if epoch_mismatch {
            reasons.push(reason(
                CompatibilityReasonCode::CentralComponentEpochMismatch,
                &subject,
                Value::from(requirement.schema_epoch),
                Value::from(component.schema_epoch),
                format!(
                    "central component {} requires schema epoch {}; Core has epoch {}",
                    requirement.namespace, requirement.schema_epoch, component.schema_epoch
                ),
            ));
        }
        // Component lineages and versions are meaningful only within the
        // declared owner and schema epoch.
        if owner_mismatch || epoch_mismatch {
            continue;
        }
        if component.schema_version < requirement.minimum_schema_version {
            reasons.push(reason(
                CompatibilityReasonCode::CentralComponentSchemaUnsatisfied,
                &subject,
                Value::from(requirement.minimum_schema_version),
                Value::from(component.schema_version),
                format!(
                    "central component {} requires schema {} or newer; Core has {}",
                    requirement.namespace,
                    requirement.minimum_schema_version,
                    component.schema_version
                ),
            ));
            continue;
        }

        let registry_digest =
            lineage_digest(&component.lineage, requirement.minimum_schema_version);
        if registry_digest.is_none_or(|digest| {
            !requirement
                .accepted_lineage_digests
                .iter()
                .any(|accepted| accepted == digest)
        }) {
            reasons.push(reason(
                CompatibilityReasonCode::CentralComponentLineageMismatch,
                &subject,
                Value::Array(
                    requirement
                        .accepted_lineage_digests
                        .iter()
                        .cloned()
                        .map(Value::String)
                        .collect(),
                ),
                registry_digest
                    .map(|digest| Value::String(digest.to_owned()))
                    .unwrap_or(Value::Null),
                format!(
                    "central component {} has no accepted lineage at schema {}",
                    requirement.namespace, requirement.minimum_schema_version
                ),
            ));
        }

        let database_problem = match database.get(requirement.namespace.as_str()) {
            None => Some(Value::Null),
            Some(state)
                if state.schema_epoch != component.schema_epoch
                    || state.schema_version < requirement.minimum_schema_version =>
            {
                Some(serde_json::json!({
                    "schema_epoch": state.schema_epoch,
                    "schema_version": state.schema_version,
                }))
            }
            Some(state) => {
                let database_digest =
                    lineage_digest(&state.lineage, requirement.minimum_schema_version);
                if database_digest != registry_digest {
                    Some(
                        database_digest
                            .map(|digest| Value::String(digest.to_owned()))
                            .unwrap_or(Value::Null),
                    )
                } else {
                    None
                }
            }
        };
        if let Some(actual) = database_problem {
            reasons.push(reason(
                CompatibilityReasonCode::CentralComponentDatabaseState,
                &subject,
                serde_json::json!({
                    "schema_epoch": component.schema_epoch,
                    "minimum_schema_version": requirement.minimum_schema_version,
                    "lineage_digest": registry_digest,
                }),
                actual,
                format!(
                    "validated database state for central component {} does not match the Core registry",
                    requirement.namespace
                ),
            ));
        }
    }

    CompatibilityEvaluation {
        compatible: reasons.is_empty(),
        reasons,
    }
}

/// Exact v1 bridge evaluator. Unlike v2, this intentionally retains global
/// contract hash and exact Core schema equality for historical sidecars.
pub fn evaluate_legacy_v1(
    contract: &AdapterDatabaseContract,
    expected_adapter: &str,
    core: &LegacyCoreProfileV1,
) -> CompatibilityEvaluation {
    if let Err(error) = validate_adapter_identifier("adapter", expected_adapter) {
        return incompatible(vec![error.reason]);
    }
    let mut reasons = Vec::new();
    if contract.component.namespace != expected_adapter {
        reasons.push(reason(
            CompatibilityReasonCode::AdapterIdentityMismatch,
            "adapter",
            Value::String(expected_adapter.to_owned()),
            Value::String(contract.component.namespace.clone()),
            "legacy component namespace does not match the adapter identity".to_owned(),
        ));
    }
    if contract.contract_hash != core.contract_hash {
        reasons.push(reason(
            CompatibilityReasonCode::LegacyGlobalContractMismatch,
            "contract_hash",
            Value::String(core.contract_hash.clone()),
            Value::String(contract.contract_hash.clone()),
            "legacy adapter global database contract does not match active Core".to_owned(),
        ));
    }
    if contract.core_schema_version != core.core_schema_version {
        reasons.push(reason(
            CompatibilityReasonCode::LegacyCoreSchemaMismatch,
            "core_schema_version",
            Value::from(core.core_schema_version),
            Value::from(contract.core_schema_version),
            format!(
                "legacy adapter requires exact Core schema {}; active Core schema is {}",
                contract.core_schema_version, core.core_schema_version
            ),
        ));
    }
    match core
        .components
        .iter()
        .find(|component| component.namespace == contract.component.namespace)
    {
        Some(component) if component == &contract.component => {}
        Some(_) | None => reasons.push(reason(
            CompatibilityReasonCode::InvalidMetadata,
            format!("legacy_component:{}", contract.component.namespace),
            Value::Null,
            Value::Null,
            "legacy adapter component does not exactly match the generated Core component"
                .to_owned(),
        )),
    }
    CompatibilityEvaluation {
        compatible: reasons.is_empty(),
        reasons,
    }
}

fn validate_database_components(
    components: &[CentralComponentDatabaseStateV2],
) -> Result<(), CompatibilityMetadataError> {
    validate_strictly_sorted_by("database_components", components, |component| {
        component.namespace.as_str()
    })?;
    for component in components {
        component.validate()?;
    }
    Ok(())
}

fn lineage_digest(lineage: &[ComponentLineageEntryV2], version: i32) -> Option<&str> {
    lineage
        .iter()
        .find(|entry| entry.schema_version == version)
        .map(|entry| entry.migration_digest.as_str())
}

fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CompatibilityMetadataError> {
    let value = serde_json::to_value(value).map_err(|error| {
        invalid_metadata(
            "canonical_json",
            format!("failed to serialize compatibility metadata: {error}"),
        )
    })?;
    let mut output = Vec::new();
    write_canonical_value(&value, &mut output);
    Ok(output)
}

fn write_canonical_value(value: &Value, output: &mut Vec<u8>) {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(value) => output.extend_from_slice(if *value { b"true" } else { b"false" }),
        Value::Number(value) => output.extend_from_slice(value.to_string().as_bytes()),
        Value::String(value) => output.extend_from_slice(
            serde_json::to_string(value)
                .expect("string serialization")
                .as_bytes(),
        ),
        Value::Array(values) => {
            output.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                write_canonical_value(value, output);
            }
            output.push(b']');
        }
        Value::Object(values) => {
            output.push(b'{');
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                output.extend_from_slice(
                    serde_json::to_string(key)
                        .expect("object key serialization")
                        .as_bytes(),
                );
                output.push(b':');
                write_canonical_value(value, output);
            }
            output.push(b'}');
        }
    }
}

fn validate_adapter_identifier(field: &str, value: &str) -> Result<(), CompatibilityMetadataError> {
    if value.starts_with("ldgr-") || !valid_identifier_segment(value) {
        return Err(invalid_metadata(
            field,
            format!("{field} `{value}` is not a valid adapter identifier"),
        ));
    }
    Ok(())
}

fn valid_identifier_segment(value: &str) -> bool {
    if value.is_empty() || !value.is_ascii() {
        return false;
    }
    value.split('-').all(|segment| {
        !segment.is_empty()
            && segment.as_bytes()[0].is_ascii_lowercase()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn validate_capability_identifier(
    field: &str,
    value: &str,
) -> Result<(), CompatibilityMetadataError> {
    let Some((namespace, version)) = value.rsplit_once(".v") else {
        return Err(invalid_metadata(
            field,
            format!("{field} `{value}` is not a versioned capability identifier"),
        ));
    };
    let namespace_valid = !namespace.is_empty()
        && namespace.split('.').all(valid_identifier_segment)
        && !namespace
            .split('.')
            .any(|segment| segment.starts_with("ldgr-"));
    let version_valid = !version.is_empty()
        && version.bytes().all(|byte| byte.is_ascii_digit())
        && !version.starts_with('0')
        && version.parse::<i32>().is_ok_and(|value| value > 0);
    if !namespace_valid || !version_valid {
        return Err(invalid_metadata(
            field,
            format!("{field} `{value}` is not a versioned capability identifier"),
        ));
    }
    Ok(())
}

fn validate_digest(field: &str, value: &str) -> Result<(), CompatibilityMetadataError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(invalid_metadata(
            field,
            format!("{field} must be a lowercase sha256 digest"),
        ));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid_metadata(
            field,
            format!("{field} must be a lowercase sha256 digest"),
        ));
    }
    Ok(())
}

fn validate_positive(field: &str, value: i32) -> Result<(), CompatibilityMetadataError> {
    if value < 1 {
        return Err(invalid_metadata(
            field,
            format!("{field} must be in the range 1 through 2147483647"),
        ));
    }
    Ok(())
}

fn validate_positive_i64(field: &str, value: i64) -> Result<(), CompatibilityMetadataError> {
    if !(1..=i32::MAX as i64).contains(&value) {
        return Err(invalid_metadata(
            field,
            format!("{field} must be in the range 1 through 2147483647"),
        ));
    }
    Ok(())
}

fn validate_strictly_sorted(
    field: &str,
    values: &[String],
) -> Result<(), CompatibilityMetadataError> {
    if values
        .windows(2)
        .any(|pair| pair[0].as_bytes() >= pair[1].as_bytes())
    {
        return Err(invalid_metadata(
            field,
            format!("{field} must be unique and sorted in ascending ASCII order"),
        ));
    }
    Ok(())
}

fn validate_strictly_sorted_i32(
    field: &str,
    values: &[i32],
) -> Result<(), CompatibilityMetadataError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(invalid_metadata(
            field,
            format!("{field} must be unique and sorted in ascending order"),
        ));
    }
    Ok(())
}

fn validate_strictly_sorted_by<T, F>(
    field: &str,
    values: &[T],
    key: F,
) -> Result<(), CompatibilityMetadataError>
where
    F: Fn(&T) -> &str,
{
    if values
        .windows(2)
        .any(|pair| key(&pair[0]).as_bytes() >= key(&pair[1]).as_bytes())
    {
        return Err(invalid_metadata(
            field,
            format!("{field} must be unique and sorted in ascending ASCII order"),
        ));
    }
    Ok(())
}

fn json_i32_array(values: &[i32]) -> Value {
    Value::Array(values.iter().copied().map(Value::from).collect())
}

fn incompatible(reasons: Vec<CompatibilityReason>) -> CompatibilityEvaluation {
    CompatibilityEvaluation {
        compatible: false,
        reasons,
    }
}

fn reason(
    code: CompatibilityReasonCode,
    subject: impl Into<String>,
    required: Value,
    actual: Value,
    message: String,
) -> CompatibilityReason {
    CompatibilityReason {
        code,
        subject: subject.into(),
        required,
        actual,
        message,
    }
}

fn metadata_error(
    code: CompatibilityReasonCode,
    subject: impl Into<String>,
    required: Value,
    actual: Value,
    message: String,
) -> CompatibilityMetadataError {
    CompatibilityMetadataError {
        reason: reason(code, subject, required, actual, message),
    }
}

fn invalid_metadata(
    subject: impl Into<String>,
    message: impl Into<String>,
) -> CompatibilityMetadataError {
    metadata_error(
        CompatibilityReasonCode::InvalidMetadata,
        subject,
        Value::Null,
        Value::Null,
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn sidecar() -> AdapterCompatibilitySidecarV2 {
        AdapterCompatibilitySidecarV2 {
            format: ADAPTER_COMPATIBILITY_FORMAT_V2.to_owned(),
            adapter: "example".to_owned(),
            compatibility: CompatibilityRequirementsV2 {
                adapter_protocol_epoch: 1,
                minimum_core_schema: 4,
                required_core_capabilities: vec!["work.v1".to_owned()],
                central_components: vec![CentralComponentRequirementV2 {
                    namespace: "notes".to_owned(),
                    schema_epoch: 1,
                    minimum_schema_version: 1,
                    accepted_lineage_digests: vec![A.to_owned()],
                }],
            },
            local_stores: Vec::new(),
        }
    }

    fn profile() -> CoreCompatibilityProfileV2 {
        CoreCompatibilityProfileV2 {
            format: CORE_COMPATIBILITY_FORMAT_V2.to_owned(),
            core_schema_version: 5,
            supported_adapter_protocol_epochs: vec![1],
            core_capabilities: vec![
                "prompt.v1".to_owned(),
                "telemetry.v1".to_owned(),
                "work.v1".to_owned(),
            ],
            central_components: vec![CentralComponentDescriptorV2 {
                namespace: "notes".to_owned(),
                owner_adapter: "example".to_owned(),
                schema_epoch: 1,
                schema_version: 2,
                lineage: vec![
                    ComponentLineageEntryV2 {
                        schema_version: 1,
                        migration_digest: A.to_owned(),
                    },
                    ComponentLineageEntryV2 {
                        schema_version: 2,
                        migration_digest: B.to_owned(),
                    },
                ],
            }],
        }
    }

    fn database() -> Vec<CentralComponentDatabaseStateV2> {
        vec![CentralComponentDatabaseStateV2 {
            namespace: "notes".to_owned(),
            schema_epoch: 1,
            schema_version: 2,
            lineage: vec![
                ComponentLineageEntryV2 {
                    schema_version: 1,
                    migration_digest: A.to_owned(),
                },
                ComponentLineageEntryV2 {
                    schema_version: 2,
                    migration_digest: B.to_owned(),
                },
            ],
        }]
    }

    fn codes(evaluation: &CompatibilityEvaluation) -> Vec<CompatibilityReasonCode> {
        evaluation
            .reasons
            .iter()
            .map(|reason| reason.code)
            .collect()
    }

    #[test]
    fn parses_and_canonicalizes_normative_sidecar() {
        let text = r#"{
          "adapter": "research",
          "compatibility": {
            "adapter_protocol_epoch": 1,
            "central_components": [],
            "minimum_core_schema": 5,
            "required_core_capabilities": ["prompt.v1", "work.v1"]
          },
          "format": "ldgr.adapter-compatibility.v2",
          "local_stores": [{
            "engine": "sqlite",
            "migration_digest": "sha256:1795f5ad5d3dbb3b57b5f733beec88c7f7f2b394d85e10b1bfe97a826d21eda4",
            "schema_version": 4,
            "store_id": "research"
          }]
        }"#;
        let parsed = parse_adapter_compatibility_v2(text).unwrap();
        assert_eq!(
            parsed.compatibility_sha256().unwrap(),
            "sha256:01fbb829b5a5d858fce62ecc6e4ef0493a47197ec3e0e39e26552e92898afbbf"
        );
        let canonical = String::from_utf8(parsed.canonical_json().unwrap()).unwrap();
        assert_eq!(canonical, "{\"adapter\":\"research\",\"compatibility\":{\"adapter_protocol_epoch\":1,\"central_components\":[],\"minimum_core_schema\":5,\"required_core_capabilities\":[\"prompt.v1\",\"work.v1\"]},\"format\":\"ldgr.adapter-compatibility.v2\",\"local_stores\":[{\"engine\":\"sqlite\",\"migration_digest\":\"sha256:1795f5ad5d3dbb3b57b5f733beec88c7f7f2b394d85e10b1bfe97a826d21eda4\",\"schema_version\":4,\"store_id\":\"research\"}]}");
        assert_eq!(parsed.canonical_file_json().unwrap().last(), Some(&b'\n'));
    }

    #[test]
    fn parser_rejects_unknown_duplicate_unsorted_and_invalid_values() {
        let valid = serde_json::to_string(&sidecar()).unwrap();
        let unknown = valid.replacen("\"adapter\":", "\"unknown\":1,\"adapter\":", 1);
        let duplicate = valid.replacen(
            "\"adapter\":\"example\"",
            "\"adapter\":\"example\",\"adapter\":\"example\"",
            1,
        );
        for text in [unknown, duplicate] {
            let error = parse_adapter_compatibility_v2(&text).unwrap_err();
            assert_eq!(error.reason.code, CompatibilityReasonCode::InvalidMetadata);
        }

        let mut unsorted = sidecar();
        unsorted.compatibility.required_core_capabilities =
            vec!["work.v1".to_owned(), "prompt.v1".to_owned()];
        assert!(unsorted.validate().is_err());
        let mut duplicate_array = sidecar();
        duplicate_array.compatibility.required_core_capabilities =
            vec!["work.v1".to_owned(), "work.v1".to_owned()];
        assert!(duplicate_array.validate().is_err());
        let mut invalid_identifier = sidecar();
        invalid_identifier.adapter = "ldgr-example".to_owned();
        assert!(invalid_identifier.validate().is_err());
        let mut invalid_digest = sidecar();
        invalid_digest.compatibility.central_components[0].accepted_lineage_digests =
            vec!["sha256:ABC".to_owned()];
        assert!(invalid_digest.validate().is_err());
        let mut zero = sidecar();
        zero.compatibility.minimum_core_schema = 0;
        assert!(zero.validate().is_err());
    }

    #[test]
    fn additive_core_schema_and_unrelated_local_store_changes_are_compatible() {
        let mut core = profile();
        core.core_schema_version = 6;
        let mut adapter = sidecar();
        let original_fingerprint = adapter.compatibility_sha256().unwrap();
        adapter.local_stores.push(LocalStoreDescriptorV2 {
            store_id: "research".to_owned(),
            engine: "sqlite".to_owned(),
            schema_version: 99,
            migration_digest: C.to_owned(),
        });
        assert_eq!(
            adapter.compatibility_sha256().unwrap(),
            original_fingerprint
        );
        let result = evaluate_v2(&adapter, "example", &core, &database());
        assert!(result.compatible, "{:?}", result.reasons);
    }

    #[test]
    fn evaluator_returns_all_dimension_failures_in_stable_order() {
        let mut core = profile();
        core.core_schema_version = 3;
        core.supported_adapter_protocol_epochs = vec![2];
        core.core_capabilities = vec!["prompt.v1".to_owned()];
        let result = evaluate_v2(&sidecar(), "other", &core, &database());
        assert_eq!(
            codes(&result),
            vec![
                CompatibilityReasonCode::AdapterIdentityMismatch,
                CompatibilityReasonCode::ProtocolEpochUnsupported,
                CompatibilityReasonCode::MinimumCoreSchemaUnsatisfied,
                CompatibilityReasonCode::CoreCapabilityMissing,
            ]
        );
        assert_eq!(
            serde_json::to_value(&result.reasons[1]).unwrap()["code"],
            "compatibility.protocol_epoch_unsupported"
        );
    }

    #[test]
    fn central_component_failures_are_structured() {
        let mut old_core = profile();
        old_core.central_components[0].schema_version = 1;
        old_core.central_components[0].lineage.truncate(1);
        let mut future_component = sidecar();
        future_component.compatibility.central_components[0].minimum_schema_version = 2;
        future_component.compatibility.central_components[0].accepted_lineage_digests =
            vec![B.to_owned()];
        assert_eq!(
            codes(&evaluate_v2(
                &future_component,
                "example",
                &old_core,
                &database()
            )),
            vec![CompatibilityReasonCode::CentralComponentSchemaUnsatisfied]
        );

        let mut core = profile();
        core.central_components[0].owner_adapter = "other".to_owned();
        core.central_components[0].schema_epoch = 2;
        core.central_components[0].schema_version = 1;
        core.central_components[0].lineage = vec![ComponentLineageEntryV2 {
            schema_version: 1,
            migration_digest: C.to_owned(),
        }];
        let result = evaluate_v2(&sidecar(), "example", &core, &[]);
        assert_eq!(
            codes(&result),
            vec![
                CompatibilityReasonCode::CentralComponentOwnerMismatch,
                CompatibilityReasonCode::CentralComponentEpochMismatch,
            ]
        );

        let mut forked_core = profile();
        forked_core.central_components[0].lineage[0].migration_digest = C.to_owned();
        let mut forked_database = database();
        forked_database[0].lineage[0].migration_digest = C.to_owned();
        assert_eq!(
            codes(&evaluate_v2(
                &sidecar(),
                "example",
                &forked_core,
                &forked_database
            )),
            vec![CompatibilityReasonCode::CentralComponentLineageMismatch]
        );

        let mut missing = profile();
        missing.central_components.clear();
        assert_eq!(
            codes(&evaluate_v2(&sidecar(), "example", &missing, &[])),
            vec![CompatibilityReasonCode::CentralComponentMissing]
        );
    }

    #[test]
    fn database_component_must_independently_match_registry_lineage() {
        let mut database = database();
        database[0].lineage[0].migration_digest = C.to_owned();
        let result = evaluate_v2(&sidecar(), "example", &profile(), &database);
        assert_eq!(
            codes(&result),
            vec![CompatibilityReasonCode::CentralComponentDatabaseState]
        );
    }

    #[test]
    fn capability_versions_and_protocol_epochs_are_exact_not_inferred() {
        let mut adapter = sidecar();
        adapter.compatibility.required_core_capabilities = vec!["work.v2".to_owned()];
        adapter.compatibility.adapter_protocol_epoch = 2;
        let result = evaluate_v2(&adapter, "example", &profile(), &database());
        assert_eq!(
            codes(&result),
            vec![
                CompatibilityReasonCode::ProtocolEpochUnsupported,
                CompatibilityReasonCode::CoreCapabilityMissing,
            ]
        );
    }

    #[test]
    fn malformed_v2_never_falls_back_to_valid_v1() {
        let legacy = serde_json::json!({
            "format": crate::database_contract::ADAPTER_DATABASE_CONTRACT_FORMAT,
            "contract_hash": A,
            "core_schema_version": 5,
            "component": {
                "namespace": "example",
                "schema_version": 1,
                "minimum_core_schema": 5,
                "migration_digest": B,
                "migration_sources": []
            }
        })
        .to_string();
        let error =
            parse_adapter_compatibility(Some("{\"unknown\":true}"), Some(&legacy)).unwrap_err();
        assert_eq!(error.reason.code, CompatibilityReasonCode::InvalidMetadata);
        assert!(matches!(
            parse_adapter_compatibility(None, Some(&legacy)).unwrap(),
            ParsedAdapterCompatibility::LegacyV1 { .. }
        ));
    }

    #[test]
    fn legacy_v1_remains_an_exact_bounded_bridge() {
        let text = serde_json::json!({
            "format": crate::database_contract::ADAPTER_DATABASE_CONTRACT_FORMAT,
            "contract_hash": A,
            "core_schema_version": 5,
            "component": {
                "namespace": "example",
                "schema_version": 1,
                "minimum_core_schema": 5,
                "migration_digest": B,
                "migration_sources": []
            }
        })
        .to_string();
        let contract = parse_legacy_adapter_contract_v1(&text).unwrap();
        let core = LegacyCoreProfileV1 {
            contract_hash: A.to_owned(),
            core_schema_version: 5,
            components: vec![contract.component.clone()],
        };
        assert!(evaluate_legacy_v1(&contract, "example", &core).compatible);

        let changed = LegacyCoreProfileV1 {
            contract_hash: C.to_owned(),
            core_schema_version: 6,
            components: core.components,
        };
        assert_eq!(
            codes(&evaluate_legacy_v1(&contract, "example", &changed)),
            vec![
                CompatibilityReasonCode::LegacyGlobalContractMismatch,
                CompatibilityReasonCode::LegacyCoreSchemaMismatch,
            ]
        );
    }

    #[test]
    fn compiled_inventory_is_sorted_valid_and_has_no_inferred_v1_components() {
        let inventory = core_compatibility_inventory();
        inventory.validate().unwrap();
        assert_eq!(inventory.supported_adapter_protocol_epochs, vec![1]);
        assert_eq!(
            inventory.core_capabilities,
            vec!["prompt.v1", "telemetry.v1", "work.v1"]
        );
        assert!(inventory.central_components.is_empty());
    }
}
