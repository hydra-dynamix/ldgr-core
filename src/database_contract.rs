use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DatabaseComponentContract {
    pub namespace: &'static str,
    pub schema_version: i64,
    pub minimum_core_schema: i64,
    pub migration_digest: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterDatabaseContract {
    pub format: String,
    pub contract_hash: String,
    pub core_schema_version: i64,
    pub component: OwnedDatabaseComponentContract,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedDatabaseComponentContract {
    pub namespace: String,
    pub schema_version: i64,
    pub minimum_core_schema: i64,
    pub migration_digest: String,
    pub migration_sources: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OwnedDatabaseContract {
    components: Vec<OwnedDatabaseComponentContract>,
}

include!("generated_database_contract.rs");

pub fn database_contract_json() -> &'static str {
    GENERATED_DATABASE_CONTRACT_JSON
}

pub fn database_component(namespace: &str) -> Option<&'static DatabaseComponentContract> {
    GENERATED_DATABASE_COMPONENTS
        .iter()
        .find(|component| component.namespace == namespace)
}

pub fn parse_and_validate_adapter_contract(text: &str) -> anyhow::Result<AdapterDatabaseContract> {
    let contract: AdapterDatabaseContract =
        serde_json::from_str(text).context("failed to parse adapter database contract")?;
    anyhow::ensure!(
        contract.format == ADAPTER_DATABASE_CONTRACT_FORMAT,
        "unsupported adapter database contract format {}",
        contract.format
    );
    anyhow::ensure!(
        contract.contract_hash == DATABASE_CONTRACT_HASH,
        "adapter database contract hash does not match active Core contract"
    );
    anyhow::ensure!(
        contract.core_schema_version == GENERATED_CORE_SCHEMA_VERSION,
        "adapter requires Core schema {}; active Core schema is {}",
        contract.core_schema_version,
        GENERATED_CORE_SCHEMA_VERSION
    );
    let generated: OwnedDatabaseContract = serde_json::from_str(database_contract_json())
        .context("failed to parse generated database contract")?;
    let expected = generated
        .components
        .iter()
        .find(|component| component.namespace == contract.component.namespace)
        .with_context(|| {
            format!(
                "adapter schema component {} is not registered by Core",
                contract.component.namespace
            )
        })?;
    anyhow::ensure!(
        &contract.component == expected,
        "adapter schema component {} does not match the generated Core contract",
        contract.component.namespace
    );
    Ok(contract)
}

pub fn generated_adapter_contract_json(namespace: &str) -> anyhow::Result<String> {
    let generated: serde_json::Value = serde_json::from_str(database_contract_json())
        .context("failed to parse generated database contract")?;
    let component = generated["components"]
        .as_array()
        .context("generated database contract components are not an array")?
        .iter()
        .find(|component| component["namespace"] == namespace)
        .with_context(|| format!("unknown generated adapter component {namespace}"))?
        .clone();
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "format": ADAPTER_DATABASE_CONTRACT_FORMAT,
        "contract_hash": DATABASE_CONTRACT_HASH,
        "core_schema_version": GENERATED_CORE_SCHEMA_VERSION,
        "component": component,
    }))? + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[derive(Deserialize, Serialize)]
    struct OwnedContract {
        format: String,
        core_schema_version: i64,
        components: Vec<OwnedComponent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        contract_hash: Option<String>,
    }

    #[derive(Deserialize, Serialize)]
    struct OwnedComponent {
        minimum_core_schema: i64,
        migration_digest: String,
        migration_sources: Vec<String>,
        namespace: String,
        schema_version: i64,
    }

    #[test]
    fn generated_contract_hash_and_bindings_match() {
        let contract: OwnedContract = serde_json::from_str(database_contract_json()).unwrap();
        assert_eq!(contract.format, DATABASE_CONTRACT_FORMAT);
        assert_eq!(contract.core_schema_version, GENERATED_CORE_SCHEMA_VERSION);
        assert_eq!(
            contract.contract_hash.as_deref(),
            Some(DATABASE_CONTRACT_HASH)
        );
        let mut canonical_value: serde_json::Value =
            serde_json::from_str(database_contract_json()).unwrap();
        canonical_value
            .as_object_mut()
            .unwrap()
            .remove("contract_hash");
        let canonical = serde_json::to_string(&canonical_value).unwrap();
        let digest = format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()));
        assert_eq!(digest, DATABASE_CONTRACT_HASH);
        assert_eq!(
            contract.components.len(),
            GENERATED_DATABASE_COMPONENTS.len()
        );
        for generated in GENERATED_DATABASE_COMPONENTS {
            let owned = contract
                .components
                .iter()
                .find(|component| component.namespace == generated.namespace)
                .unwrap();
            assert_eq!(owned.schema_version, generated.schema_version);
            assert_eq!(owned.minimum_core_schema, generated.minimum_core_schema);
            assert_eq!(owned.migration_digest, generated.migration_digest);
        }
    }

    #[test]
    fn every_supported_adapter_has_one_generated_component() {
        for namespace in [
            "bench",
            "code",
            "conduct",
            "evidence",
            "example",
            "explore",
            "private-commercial",
            "programbench",
            "recall",
            "research",
            "security",
        ] {
            assert!(
                database_component(namespace).is_some(),
                "missing {namespace}"
            );
        }
    }

    #[test]
    fn adapter_contract_validation_is_exact_and_fail_closed() {
        let generated: serde_json::Value = serde_json::from_str(database_contract_json()).unwrap();
        let component = generated["components"]
            .as_array()
            .unwrap()
            .iter()
            .find(|component| component["namespace"] == "example")
            .unwrap()
            .clone();
        let valid = serde_json::json!({
            "format": ADAPTER_DATABASE_CONTRACT_FORMAT,
            "contract_hash": DATABASE_CONTRACT_HASH,
            "core_schema_version": GENERATED_CORE_SCHEMA_VERSION,
            "component": component,
        });
        let parsed = parse_and_validate_adapter_contract(&valid.to_string()).unwrap();
        assert_eq!(parsed.component.namespace, "example");

        let mut invalid_version = valid.clone();
        invalid_version["component"]["schema_version"] = 99.into();
        let mut invalid_minimum = valid.clone();
        invalid_minimum["component"]["minimum_core_schema"] = 99.into();
        let mut invalid_digest = valid.clone();
        invalid_digest["component"]["migration_digest"] = "sha256:wrong".into();
        let mut invalid_sources = valid.clone();
        invalid_sources["component"]["migration_sources"] =
            serde_json::json!(["private/user/path"]);
        for invalid in [
            invalid_version,
            invalid_minimum,
            invalid_digest,
            invalid_sources,
        ] {
            assert!(parse_and_validate_adapter_contract(&invalid.to_string()).is_err());
        }
        let mut wrong_hash = valid.clone();
        wrong_hash["contract_hash"] = "sha256:wrong".into();
        assert!(parse_and_validate_adapter_contract(&wrong_hash.to_string()).is_err());
        let mut unknown = valid;
        unknown["component"]["namespace"] = "unknown".into();
        assert!(parse_and_validate_adapter_contract(&unknown.to_string()).is_err());
    }
}
