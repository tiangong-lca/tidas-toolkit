//! Integrity-locked runtime methodology/ruleset catalog.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tidas_assets::{AssetKind, bundled_asset};

const RUNTIME_RULESETS_PATH: &str = "assets/tidas/methodologies/runtime_rulesets.json";
const RUNTIME_RULESETS_SCHEMA_PATH: &str =
    "assets/tidas/methodologies/runtime_rulesets.schema.json";
const RUNTIME_PROFILES_PATH: &str = "assets/tidas/methodologies/runtime_profiles.v1.json";
const RUNTIME_PROFILES_SCHEMA_PATH: &str =
    "assets/tidas/methodologies/runtime_profiles.v1.schema.json";
const PUBLIC_RULES_PATH: &str = "assets/tidas/rules/public-rules.v1.json";
const PUBLIC_RULES_SCHEMA_PATH: &str = "assets/tidas/rules/public-rules.v1.schema.json";
const PUBLIC_RULES_SOURCE_PATH: &str = "assets/tidas/rules/public-rules.source.v1.json";
pub const RULESET_DESCRIPTION_SCHEMA_V1: &str = "tidas.ruleset-description.v1";
pub const METHODOLOGY_VALIDATION_REPORT_SCHEMA_V1: &str = "tidas.methodology-validation-report.v1";
pub const RULESET_DESCRIPTION_JSON_SCHEMA_V1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contracts/ruleset-description.v1.schema.json"
));
pub const METHODOLOGY_VALIDATION_REPORT_JSON_SCHEMA_V1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contracts/methodology-validation-report.v1.schema.json"
));

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RulesetDescriptionV1 {
    pub schema_version: String,
    pub ruleset_version: String,
    pub catalog_sha256: String,
    pub ruleset_count: u64,
    pub rule_count: u64,
    pub ruleset_ids: Vec<String>,
    pub methodology_file_count: u64,
    pub methodology_warning_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MethodologyValidationReportV1 {
    pub schema_version: String,
    pub ok: bool,
    pub file_count: u64,
    pub error_count: u64,
    pub warning_count: u64,
    pub files: Vec<MethodologyFileReportV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MethodologyFileReportV1 {
    pub methodology_file: String,
    pub schema_file: String,
    pub status: String,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct RulesetCatalog {
    metadata: Value,
    rules_by_id: BTreeMap<String, Value>,
    profile_rule_ids: BTreeMap<String, Vec<String>>,
    description: RulesetDescriptionV1,
    methodology_report: MethodologyValidationReportV1,
}

impl RulesetCatalog {
    pub fn load() -> Result<Self, RulesetError> {
        let metadata_asset = required_asset(RUNTIME_RULESETS_PATH)?;
        let schema_asset = required_asset(RUNTIME_RULESETS_SCHEMA_PATH)?;
        let compatibility_metadata: Value = serde_json::from_slice(metadata_asset.bytes)?;
        let schema: Value = serde_json::from_slice(schema_asset.bytes)?;
        let validator = jsonschema::draft202012::new(&schema)
            .map_err(|error| RulesetError::SchemaCompile(error.to_string()))?;
        if let Some(error) = validator.iter_errors(&compatibility_metadata).next() {
            return Err(RulesetError::SchemaValidation(error.to_string()));
        }
        let metadata = compose_runtime_catalog()?;
        if metadata != compatibility_metadata {
            return Err(RulesetError::CompatibilityProjectionDrift);
        }

        let rules = metadata
            .get("rules")
            .and_then(Value::as_array)
            .ok_or(RulesetError::MissingRules)?;
        let mut rules_by_id = BTreeMap::new();
        for rule in rules {
            let id = required_id(rule, "rule")?;
            if rules_by_id.insert(id.clone(), rule.clone()).is_some() {
                return Err(RulesetError::DuplicateRule(id));
            }
        }
        let profiles = metadata
            .get("rulesets")
            .and_then(Value::as_array)
            .ok_or(RulesetError::MissingRulesets)?;
        let mut profile_rule_ids = BTreeMap::new();
        for profile in profiles {
            let id = required_id(profile, "ruleset")?;
            let rule_ids = profile
                .get("rule_ids")
                .and_then(Value::as_array)
                .ok_or_else(|| RulesetError::MissingRuleIds(id.clone()))?
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| RulesetError::InvalidRuleId(id.clone()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            for rule_id in &rule_ids {
                if !rules_by_id.contains_key(rule_id) {
                    return Err(RulesetError::UnknownRule {
                        ruleset: id.clone(),
                        rule: rule_id.clone(),
                    });
                }
            }
            if profile_rule_ids.insert(id.clone(), rule_ids).is_some() {
                return Err(RulesetError::DuplicateRuleset(id));
            }
        }
        let ruleset_version = metadata
            .get("ruleset_version")
            .and_then(Value::as_str)
            .ok_or(RulesetError::MissingVersion)?
            .to_owned();
        let canonical = serde_json::to_vec(&metadata)?;
        let methodology_report = validate_methodologies()?;
        let description = RulesetDescriptionV1 {
            schema_version: RULESET_DESCRIPTION_SCHEMA_V1.to_owned(),
            ruleset_version,
            catalog_sha256: digest_hex(&Sha256::digest(canonical)),
            ruleset_count: u64::try_from(profile_rule_ids.len())
                .map_err(|_| RulesetError::SizeOverflow)?,
            rule_count: u64::try_from(rules_by_id.len()).map_err(|_| RulesetError::SizeOverflow)?,
            ruleset_ids: profile_rule_ids.keys().cloned().collect(),
            methodology_file_count: methodology_report.file_count,
            methodology_warning_count: methodology_report.warning_count,
        };
        Ok(Self {
            metadata,
            rules_by_id,
            profile_rule_ids,
            description,
            methodology_report,
        })
    }

    #[must_use]
    pub fn metadata(&self) -> &Value {
        &self.metadata
    }

    #[must_use]
    pub const fn description(&self) -> &RulesetDescriptionV1 {
        &self.description
    }

    #[must_use]
    pub const fn methodology_report(&self) -> &MethodologyValidationReportV1 {
        &self.methodology_report
    }

    pub fn rules_for(&self, ruleset_id: &str) -> Result<Vec<&Value>, RulesetError> {
        let ids = self
            .profile_rule_ids
            .get(ruleset_id)
            .ok_or_else(|| RulesetError::UnknownRuleset(ruleset_id.to_owned()))?;
        Ok(ids
            .iter()
            .map(|id| {
                self.rules_by_id
                    .get(id)
                    .expect("referential integrity was checked while loading")
            })
            .collect())
    }
}

fn compose_runtime_catalog() -> Result<Value, RulesetError> {
    let (public_rules, profile) = load_composition_inputs()?;
    compose_runtime_values(&public_rules, &profile)
}

fn load_composition_inputs() -> Result<(Value, Value), RulesetError> {
    let public_rules_asset = required_asset(PUBLIC_RULES_PATH)?;
    let public_schema_asset = required_asset(PUBLIC_RULES_SCHEMA_PATH)?;
    let source_identity: Value =
        serde_json::from_slice(required_asset(PUBLIC_RULES_SOURCE_PATH)?.bytes)?;
    verify_public_source_identity(
        &source_identity,
        public_rules_asset.bytes,
        public_schema_asset.bytes,
    )?;
    let public_rules: Value = serde_json::from_slice(public_rules_asset.bytes)?;
    let public_schema: Value = serde_json::from_slice(public_schema_asset.bytes)?;
    let public_validator = jsonschema::draft7::new(&public_schema)
        .map_err(|error| RulesetError::PublicSchemaCompile(error.to_string()))?;
    if let Some(error) = public_validator.iter_errors(&public_rules).next() {
        return Err(RulesetError::PublicSchemaValidation(error.to_string()));
    }
    let profile: Value = serde_json::from_slice(required_asset(RUNTIME_PROFILES_PATH)?.bytes)?;
    let profile_schema: Value =
        serde_json::from_slice(required_asset(RUNTIME_PROFILES_SCHEMA_PATH)?.bytes)?;
    let profile_validator = jsonschema::draft202012::new(&profile_schema)
        .map_err(|error| RulesetError::ProfileSchemaCompile(error.to_string()))?;
    if let Some(error) = profile_validator.iter_errors(&profile).next() {
        return Err(RulesetError::ProfileSchemaValidation(error.to_string()));
    }
    Ok((public_rules, profile))
}

fn compose_runtime_values(public_rules: &Value, profile: &Value) -> Result<Value, RulesetError> {
    let public_by_id = values_by_id(public_rules, "rules", "public rule")?;
    let policy_by_id = values_by_id(profile, "public_rule_policy", "public rule policy")?;
    let local_by_id = values_by_id(profile, "local_rules", "local rule")?;
    if public_by_id.keys().collect::<Vec<_>>() != policy_by_id.keys().collect::<Vec<_>>() {
        return Err(RulesetError::PublicPolicyMismatch);
    }
    if public_by_id.keys().any(|id| local_by_id.contains_key(id)) {
        return Err(RulesetError::PublicLocalOverlap);
    }

    let order = profile
        .get("rule_order")
        .and_then(Value::as_array)
        .ok_or(RulesetError::MissingRuleOrder)?;
    let mut seen = BTreeSet::new();
    let mut rules = Vec::with_capacity(order.len());
    for id_value in order {
        let id = id_value.as_str().ok_or(RulesetError::InvalidRuleOrder)?;
        if !seen.insert(id.to_owned()) {
            return Err(RulesetError::DuplicateRuleOrder(id.to_owned()));
        }
        if let Some(definition) = public_by_id.get(id) {
            let policy = policy_by_id
                .get(id)
                .expect("public definition/policy key equality was checked");
            rules.push(serde_json::json!({
                "id": id,
                "dataset_type": required_member(definition, "dataset_type", id)?,
                "summary": required_member(definition, "statement", id)?,
                "severity": required_member(policy, "severity", id)?,
                "phases": required_member(policy, "phases", id)?,
                "default_blocker": required_member(policy, "default_blocker", id)?,
                "field_paths": required_member(definition, "locations", id)?,
                "source_rule_refs": required_member(definition, "source_refs", id)?,
            }));
        } else if let Some(local) = local_by_id.get(id) {
            rules.push((*local).clone());
        } else {
            return Err(RulesetError::UnknownRuleOrder(id.to_owned()));
        }
    }
    if seen.len() != public_by_id.len() + local_by_id.len() {
        return Err(RulesetError::IncompleteRuleOrder);
    }
    let known: BTreeSet<&str> = seen.iter().map(String::as_str).collect();
    let rulesets = profile
        .get("rulesets")
        .and_then(Value::as_array)
        .ok_or(RulesetError::MissingRulesets)?;
    for ruleset in rulesets {
        let ruleset_id = required_id(ruleset, "ruleset")?;
        for rule_id in ruleset
            .get("rule_ids")
            .and_then(Value::as_array)
            .ok_or_else(|| RulesetError::MissingRuleIds(ruleset_id.clone()))?
        {
            let rule_id = rule_id
                .as_str()
                .ok_or_else(|| RulesetError::InvalidRuleId(ruleset_id.clone()))?;
            if !known.contains(rule_id) {
                return Err(RulesetError::UnknownRule {
                    ruleset: ruleset_id,
                    rule: rule_id.to_owned(),
                });
            }
        }
    }
    Ok(serde_json::json!({
        "$schema": "runtime_rulesets.schema.json",
        "schema_version": 1,
        "ruleset_version": required_member(profile, "ruleset_version", "runtime profile")?,
        "purpose": "Compatibility projection composed from exact public definitions and toolkit-owned runtime profile policy.",
        "source_assets": [
            {"asset": "public-rules.v1.json", "kind": "public rule definitions"},
            {"asset": "runtime_profiles.v1.json", "kind": "toolkit runtime profile policy"}
        ],
        "rulesets": rulesets,
        "rules": rules,
    }))
}

fn verify_public_source_identity(
    identity: &Value,
    index_bytes: &[u8],
    schema_bytes: &[u8],
) -> Result<(), RulesetError> {
    for (field, expected) in [
        ("index_sha256", digest_hex(&Sha256::digest(index_bytes))),
        ("schema_sha256", digest_hex(&Sha256::digest(schema_bytes))),
    ] {
        if identity.get(field).and_then(Value::as_str) != Some(expected.as_str()) {
            return Err(RulesetError::PublicSourceIdentity(field));
        }
    }
    let commit = identity
        .get("commit")
        .and_then(Value::as_str)
        .ok_or(RulesetError::PublicSourceIdentity("commit"))?;
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(RulesetError::PublicSourceIdentity("commit"));
    }
    Ok(())
}

fn values_by_id<'a>(
    root: &'a Value,
    member: &'static str,
    kind: &'static str,
) -> Result<BTreeMap<String, &'a Value>, RulesetError> {
    let values = root
        .get(member)
        .and_then(Value::as_array)
        .ok_or(RulesetError::MissingCompositionMember(member))?;
    let mut by_id = BTreeMap::new();
    for value in values {
        let id = required_id(value, kind)?;
        if by_id.insert(id.clone(), value).is_some() {
            return Err(RulesetError::DuplicateRule(id));
        }
    }
    Ok(by_id)
}

fn required_member<'a>(
    value: &'a Value,
    member: &'static str,
    id: &str,
) -> Result<&'a Value, RulesetError> {
    value
        .get(member)
        .ok_or_else(|| RulesetError::MissingComposedField {
            id: id.to_owned(),
            field: member,
        })
}

fn validate_methodologies() -> Result<MethodologyValidationReportV1, RulesetError> {
    let mut files = Vec::new();
    let mut total_errors = 0_u64;
    let mut total_warnings = 0_u64;
    for asset in tidas_assets::bundled_assets().into_iter().filter(|asset| {
        asset.kind == AssetKind::Methodology
            && std::path::Path::new(&asset.path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("yaml"))
    }) {
        let report = validate_methodology_asset(&asset)?;
        total_errors = total_errors
            .checked_add(
                u64::try_from(report.errors.len()).map_err(|_| RulesetError::SizeOverflow)?,
            )
            .ok_or(RulesetError::SizeOverflow)?;
        total_warnings = total_warnings
            .checked_add(
                u64::try_from(report.warnings.len()).map_err(|_| RulesetError::SizeOverflow)?,
            )
            .ok_or(RulesetError::SizeOverflow)?;
        files.push(report);
    }
    files.sort_by(|left, right| left.methodology_file.cmp(&right.methodology_file));
    let file_count = u64::try_from(files.len()).map_err(|_| RulesetError::SizeOverflow)?;
    Ok(MethodologyValidationReportV1 {
        schema_version: METHODOLOGY_VALIDATION_REPORT_SCHEMA_V1.to_owned(),
        ok: total_errors == 0,
        file_count,
        error_count: total_errors,
        warning_count: total_warnings,
        files,
    })
}

fn validate_methodology_asset(
    asset: &tidas_assets::BundledAsset,
) -> Result<MethodologyFileReportV1, RulesetError> {
    let methodology_file = asset
        .path
        .rsplit('/')
        .next()
        .expect("embedded asset paths are non-empty")
        .to_owned();
    let stem = std::path::Path::new(&methodology_file)
        .file_stem()
        .and_then(|value| value.to_str())
        .expect("embedded methodology filenames are controlled UTF-8");
    let schema_file = format!("{stem}.json");
    let schema_path = format!("assets/tidas/schemas/{schema_file}");
    let Some(schema_asset) = bundled_asset(&schema_path) else {
        return Ok(MethodologyFileReportV1 {
            methodology_file,
            schema_file,
            status: "error".to_owned(),
            errors: vec!["No corresponding schema file found".to_owned()],
            warnings: Vec::new(),
        });
    };
    let yaml_text =
        std::str::from_utf8(asset.bytes).map_err(|error| RulesetError::MethodologyParse {
            path: asset.path.clone(),
            reason: error.to_string(),
        })?;
    let yaml: Value = noyalib::compat::serde_yaml::from_str(yaml_text).map_err(|error| {
        RulesetError::MethodologyParse {
            path: asset.path.clone(),
            reason: error.to_string(),
        }
    })?;
    let schema: Value = serde_json::from_slice(schema_asset.bytes)?;
    let normalized_yaml: BTreeMap<String, String> = extract_methodology_paths(&yaml)
        .into_iter()
        .map(|path| (normalize_path(&path), path))
        .collect();
    let normalized_schema: BTreeMap<String, String> = extract_schema_paths(&schema)
        .into_iter()
        .map(|path| (normalize_path(&path), path))
        .collect();
    let mut warnings = methodology_only_warnings(&normalized_yaml, &normalized_schema);
    warnings.extend(schema_only_warnings(&normalized_yaml, &normalized_schema));
    warnings.sort();
    Ok(MethodologyFileReportV1 {
        methodology_file,
        schema_file,
        status: if warnings.is_empty() {
            "ok".to_owned()
        } else {
            "warning".to_owned()
        },
        errors: Vec::new(),
        warnings,
    })
}

fn methodology_only_warnings(
    methodology: &BTreeMap<String, String>,
    schema: &BTreeMap<String, String>,
) -> Vec<String> {
    methodology
        .iter()
        .filter(|(key, _)| !schema.contains_key(*key))
        .map(|(_, path)| format!("Field '{path}' in YAML methodology not found in schema"))
        .collect()
}

fn schema_only_warnings(
    methodology: &BTreeMap<String, String>,
    schema: &BTreeMap<String, String>,
) -> Vec<String> {
    const IMPORTANT: [&str; 5] = [
        "processDataSet",
        "processInformation",
        "modellingAndValidation",
        "administrativeInformation",
        "exchanges",
    ];
    schema
        .iter()
        .filter(|(key, _)| !methodology.contains_key(*key))
        .filter(|(_, path)| {
            IMPORTANT.iter().any(|important| {
                path.contains(important) && path.split('.').next() == Some(important)
            })
        })
        .map(|(_, path)| format!("Schema field '{path}' not covered in YAML methodology"))
        .collect()
}

fn extract_methodology_paths(value: &Value) -> BTreeSet<String> {
    fn visit(value: &Value, current: &str, output: &mut BTreeSet<String>) {
        let Some(object) = value.as_object() else {
            return;
        };
        for (key, child) in object {
            if matches!(key.as_str(), "<rules>" | "metadata" | "global_rules") {
                continue;
            }
            let path = if current.is_empty() {
                key.clone()
            } else {
                format!("{current}.{key}")
            };
            output.insert(path.clone());
            visit(child, &path, output);
        }
    }
    let mut paths = BTreeSet::new();
    visit(value, "", &mut paths);
    paths
}

fn extract_schema_paths(value: &Value) -> BTreeSet<String> {
    fn visit(value: &Value, current: &str, output: &mut BTreeSet<String>) {
        let Some(object) = value.as_object() else {
            return;
        };
        if object.get("type").and_then(Value::as_str) == Some("array") {
            match object.get("items") {
                Some(Value::Object(item)) => visit(&Value::Object(item.clone()), current, output),
                Some(Value::Array(items)) => {
                    for item in items {
                        visit(item, current, output);
                    }
                }
                _ => {}
            }
        }
        if let Some(properties) = object.get("properties").and_then(Value::as_object) {
            for (name, schema) in properties {
                let clean = name.replace("common:", "").replace('@', "");
                let path = if current.is_empty() {
                    clean
                } else {
                    format!("{current}.{clean}")
                };
                output.insert(path.clone());
                visit(schema, &path, output);
            }
        }
    }
    let mut paths = BTreeSet::new();
    visit(value, "", &mut paths);
    paths
}

fn normalize_path(path: &str) -> String {
    path.replace("common:", "")
        .replace('@', "")
        .replace("UUID", "uuid")
        .replace("timeStamp", "timestamp")
        .replace("dataSetVersion", "datasetversion")
        .to_ascii_lowercase()
}

fn required_asset(path: &str) -> Result<tidas_assets::BundledAsset, RulesetError> {
    let asset = bundled_asset(path).ok_or_else(|| RulesetError::MissingAsset(path.to_owned()))?;
    if !matches!(
        asset.kind,
        AssetKind::RuntimeRuleset | AssetKind::PublicRule
    ) {
        return Err(RulesetError::UnexpectedAssetKind(path.to_owned()));
    }
    Ok(asset)
}

fn required_id(value: &Value, kind: &'static str) -> Result<String, RulesetError> {
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(ToOwned::to_owned)
        .ok_or(RulesetError::MissingId(kind))
}

fn digest_hex(digest: &[u8]) -> String {
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

#[derive(Debug, Error)]
pub enum RulesetError {
    #[error("required ruleset asset is missing: {0}")]
    MissingAsset(String),
    #[error("ruleset asset has an unexpected kind: {0}")]
    UnexpectedAssetKind(String),
    #[error("runtime ruleset schema failed to compile: {0}")]
    SchemaCompile(String),
    #[error("public rule schema failed to compile: {0}")]
    PublicSchemaCompile(String),
    #[error("public rule index failed schema validation: {0}")]
    PublicSchemaValidation(String),
    #[error("runtime profile schema failed to compile: {0}")]
    ProfileSchemaCompile(String),
    #[error("runtime profile failed schema validation: {0}")]
    ProfileSchemaValidation(String),
    #[error("public rule source identity is invalid or stale for {0}")]
    PublicSourceIdentity(&'static str),
    #[error("runtime ruleset metadata failed schema validation: {0}")]
    SchemaValidation(String),
    #[error("runtime ruleset metadata has no rules array")]
    MissingRules,
    #[error("runtime ruleset metadata has no rulesets array")]
    MissingRulesets,
    #[error("runtime profile has no rule_order array")]
    MissingRuleOrder,
    #[error("runtime profile rule_order contains a non-string value")]
    InvalidRuleOrder,
    #[error("runtime profile rule_order contains duplicate rule {0}")]
    DuplicateRuleOrder(String),
    #[error("runtime profile rule_order references unknown rule {0}")]
    UnknownRuleOrder(String),
    #[error("runtime profile rule_order does not cover every public and local rule exactly once")]
    IncompleteRuleOrder,
    #[error("runtime composition is missing required member {0}")]
    MissingCompositionMember(&'static str),
    #[error("rule {id} is missing composed field {field}")]
    MissingComposedField { id: String, field: &'static str },
    #[error("public rule definitions and toolkit policy do not have identical IDs")]
    PublicPolicyMismatch,
    #[error("a rule cannot be both public and toolkit-local")]
    PublicLocalOverlap,
    #[error("the checked-in runtime compatibility projection differs from the composed catalog")]
    CompatibilityProjectionDrift,
    #[error("runtime ruleset metadata has no ruleset_version")]
    MissingVersion,
    #[error("{0} entry has no non-empty id")]
    MissingId(&'static str),
    #[error("duplicate rule id: {0}")]
    DuplicateRule(String),
    #[error("duplicate ruleset id: {0}")]
    DuplicateRuleset(String),
    #[error("ruleset {0} has no rule_ids array")]
    MissingRuleIds(String),
    #[error("ruleset {0} contains a non-string rule id")]
    InvalidRuleId(String),
    #[error("ruleset {ruleset} references unknown rule {rule}")]
    UnknownRule { ruleset: String, rule: String },
    #[error("unknown ruleset id: {0}")]
    UnknownRuleset(String),
    #[error("ruleset catalog size cannot be represented safely")]
    SizeOverflow,
    #[error("methodology asset {path} is invalid: {reason}")]
    MethodologyParse { path: String, reason: String },
    #[error("ruleset JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_rulesets_are_schema_valid_and_referentially_complete() {
        let catalog = RulesetCatalog::load().unwrap();
        assert_eq!(catalog.description.ruleset_count, 7);
        assert!(catalog.description.rule_count > 10);
        assert!(
            catalog
                .rules_for("process-authoring/strict")
                .unwrap()
                .iter()
                .any(|rule| rule["id"] == "tidas.process.quantitative-reference.required")
        );
        assert!(matches!(
            catalog.rules_for("missing/default"),
            Err(RulesetError::UnknownRuleset(_))
        ));
        assert_eq!(catalog.methodology_report.file_count, 2);
        assert!(catalog.methodology_report.ok);
    }

    #[test]
    fn warning_and_blocker_severities_survive_the_rust_catalog() {
        let catalog = RulesetCatalog::load().unwrap();
        let severities: BTreeSet<&str> = catalog
            .rules_for("process-authoring/strict")
            .unwrap()
            .into_iter()
            .filter_map(|rule| rule["severity"].as_str())
            .collect();
        assert!(severities.contains("warning"));
        assert!(severities.contains("blocker"));
    }

    #[test]
    fn public_ruleset_reports_match_their_checked_in_contracts() {
        let catalog = RulesetCatalog::load().unwrap();
        for (schema, instance) in [
            (
                RULESET_DESCRIPTION_JSON_SCHEMA_V1,
                serde_json::to_value(catalog.description()).unwrap(),
            ),
            (
                METHODOLOGY_VALIDATION_REPORT_JSON_SCHEMA_V1,
                serde_json::to_value(catalog.methodology_report()).unwrap(),
            ),
        ] {
            let schema: Value = serde_json::from_str(schema).unwrap();
            let validator = jsonschema::draft202012::new(&schema).unwrap();
            let errors: Vec<_> = validator
                .iter_errors(&instance)
                .map(|error| error.to_string())
                .collect();
            assert!(errors.is_empty(), "{errors:?}");
        }
    }
}
