use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Surface {
    pub name: String,
    pub crate_name: String,
    pub path: String,
    /// Empty selects the crate's public exports; otherwise these are definition paths.
    pub roots: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub id: String,
    pub surface: String,
    pub kind: String,
    pub shape: String,
    pub conditions: Vec<String>,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub path: String,
    /// A Rust test/function or C fixture entry point, never a line number.
    pub symbol: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Supported,
    Partial,
    Pending,
    IntentionallyExcluded,
    NotApplicable,
}

impl Disposition {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Partial => "partial",
            Self::Pending => "pending",
            Self::IntentionallyExcluded => "intentionally_excluded",
            Self::NotApplicable => "not_applicable",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub id: String,
    pub boundary: String,
    pub sources: Vec<String>,
    pub targets: Vec<String>,
    pub disposition: Disposition,
    pub rationale: String,
    pub restrictions: String,
    pub evidence: Vec<Evidence>,
    pub validation_limits: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Supplement {
    /// The scanner's macro declaration/invocation identity.
    pub anchor: String,
    pub expected_shape: String,
    pub rationale: String,
    pub declarations: Vec<Declaration>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub name: String,
    pub disposition: Disposition,
    pub rationale: String,
    pub validation_limits: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub schema_version: u32,
    pub scope: String,
    pub exclusions: Vec<String>,
    pub surfaces: Vec<Surface>,
    pub supplements: Vec<Supplement>,
    pub declarations: Vec<Declaration>,
    pub decisions: Vec<Decision>,
    pub capabilities: Vec<Capability>,
}
