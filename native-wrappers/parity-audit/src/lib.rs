mod c_evidence;
mod discover;
pub mod model;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use model::{Declaration, Disposition, Ledger};

pub const LEDGER: &str = "native-wrappers/wrapper-core/parity.json";
pub const MARKDOWN: &str = "native-wrappers/wrapper-core/PARITY.md";
pub const BEGIN: &str = "<!-- parity-audit:begin -->";
pub const END: &str = "<!-- parity-audit:end -->";

/// Read the versioned repository ledger.
///
/// # Errors
/// Returns file access and strict schema deserialization errors.
pub fn read_ledger(root: &Path) -> Result<Ledger, String> {
    let path = root.join(LEDGER);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// Discover source declarations and validate explicit macro supplements.
///
/// # Errors
/// Returns missing-input, syntax, duplicate and unreviewed/changed supplement errors.
pub fn inventory(root: &Path, ledger: &Ledger) -> Result<Vec<Declaration>, String> {
    let mut result = discover_sources(root, ledger)?;
    let mut anchors = BTreeSet::new();
    for supplement in &ledger.supplements {
        if !anchors.insert(&supplement.anchor) {
            return Err(format!("duplicate supplement {}", supplement.anchor));
        }
        let anchor = result
            .iter()
            .find(|d| d.id == supplement.anchor)
            .ok_or_else(|| format!("stale supplement {}", supplement.anchor))?;
        if anchor.kind != "macro"
            || anchor.shape != supplement.expected_shape
            || supplement.rationale.trim().is_empty()
        {
            return Err(format!(
                "changed or invalid supplement {}",
                supplement.anchor
            ));
        }
        for declaration in &supplement.declarations {
            if declaration.source != anchor.source || declaration.surface != anchor.surface {
                return Err(format!(
                    "supplement declaration outside anchor scope: {}",
                    declaration.id
                ));
            }
        }
        result.extend(supplement.declarations.clone());
    }
    for declaration in &result {
        if declaration.kind == "macro" && !anchors.contains(&declaration.id) {
            return Err(format!(
                "unreviewed macro; explicit supplement required: {}",
                declaration.id
            ));
        }
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    unique(&result)?;
    Ok(result)
}

/// Raw discovery never writes or accepts a baseline, including for new macros.
///
/// # Errors
/// Returns invalid schema, missing source, syntax and empty/duplicate surface errors.
pub fn discover_sources(root: &Path, ledger: &Ledger) -> Result<Vec<Declaration>, String> {
    if ledger.schema_version != 1 {
        return Err("unsupported ledger schema_version".into());
    }
    let mut names = BTreeSet::new();
    let mut result = Vec::new();
    for surface in &ledger.surfaces {
        if !names.insert(&surface.name) {
            return Err(format!("duplicate surface {}", surface.name));
        }
        let declarations = discover::discover(root, surface)?;
        if declarations.is_empty() {
            return Err(format!("empty inventory for {}", surface.name));
        }
        result.extend(declarations);
    }
    for required in ["native-v4", "native-v5", "core", "c"] {
        if !names.contains(&required.to_string()) {
            return Err(format!("missing required surface {required}"));
        }
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    unique(&result)?;
    Ok(result)
}

fn unique(declarations: &[Declaration]) -> Result<BTreeMap<&str, &Declaration>, String> {
    let mut map = BTreeMap::new();
    for declaration in declarations {
        if map.insert(declaration.id.as_str(), declaration).is_some() {
            return Err(format!("duplicate declaration {}", declaration.id));
        }
    }
    Ok(map)
}

/// Validate declaration snapshots, mappings and named behavior evidence.
///
/// # Errors
/// Returns aggregated missing, stale, changed, duplicate and invalid review diagnostics.
#[expect(
    clippy::too_many_lines,
    reason = "Accumulate independent review errors rather than stopping at the first gap"
)]
pub fn validate(root: &Path, ledger: &Ledger, actual: &[Declaration]) -> Result<(), String> {
    let expected = unique(&ledger.declarations)?;
    let actual = unique(actual)?;
    let mut errors = Vec::new();
    for (id, declaration) in &actual {
        match expected.get(id) {
            None => errors.push(format!("unreviewed declaration: {id}")),
            Some(reviewed) if reviewed != declaration => {
                errors.push(format!("changed declaration requires review: {id}"));
            }
            _ => {}
        }
    }
    for id in expected.keys() {
        if !actual.contains_key(id) {
            errors.push(format!("stale declaration: {id}"));
        }
    }
    let mut decision_ids = BTreeSet::new();
    let mut coverage = BTreeSet::new();
    let mut c_targets = BTreeSet::new();
    let mut evidence_cache = BTreeMap::new();
    for decision in &ledger.decisions {
        if !decision_ids.insert(&decision.id) {
            errors.push(format!("duplicate decision {}", decision.id));
        }
        if decision.sources.is_empty()
            || decision.rationale.trim().is_empty()
            || decision.restrictions.trim().is_empty()
            || decision.validation_limits.trim().is_empty()
        {
            errors.push(format!("incomplete decision {}", decision.id));
        }
        if !["native-to-core", "core-to-c", "c-contract"].contains(&decision.boundary.as_str()) {
            errors.push(format!("invalid boundary {}", decision.id));
        }
        let mapped = matches!(
            decision.disposition,
            Disposition::Supported | Disposition::Partial
        );
        if mapped && (decision.targets.is_empty() || decision.evidence.is_empty()) {
            errors.push(format!(
                "mapped decision needs targets and behavior evidence: {}",
                decision.id
            ));
        }
        let mut targets = BTreeSet::new();
        for target in &decision.targets {
            if !targets.insert(target) {
                errors.push(format!("duplicate target in {}: {target}", decision.id));
            }
            let Some(target_decl) = actual.get(target.as_str()) else {
                errors.push(format!("dangling target in {}: {target}", decision.id));
                continue;
            };
            let valid = match decision.boundary.as_str() {
                "native-to-core" | "c-contract" => target_decl.surface == "core",
                "core-to-c" => target_decl.surface == "c",
                _ => false,
            };
            if !valid {
                errors.push(format!(
                    "wrong target boundary in {}: {target}",
                    decision.id
                ));
            }
            if decision.boundary == "core-to-c" {
                c_targets.insert(target.as_str());
            }
        }
        for source in &decision.sources {
            let Some(source_decl) = actual.get(source.as_str()) else {
                errors.push(format!("dangling source in {}: {source}", decision.id));
                continue;
            };
            let boundary = match source_decl.surface.as_str() {
                "core" => "core-to-c",
                "c" => "c-contract",
                _ => "native-to-core",
            };
            if boundary != decision.boundary {
                errors.push(format!(
                    "wrong source boundary in {}: {source}",
                    decision.id
                ));
            }
            if !coverage.insert(source.as_str()) {
                errors.push(format!("duplicate decision for {source}"));
            }
        }
        for evidence in &decision.evidence {
            let valid = evidence_cache
                .entry((evidence.path.clone(), evidence.symbol.clone()))
                .or_insert_with(|| verify_evidence(root, &evidence.path, &evidence.symbol));
            if let Err(error) = valid {
                errors.push(format!("{}: {error}", decision.id));
            }
        }
    }
    for (id, declaration) in &actual {
        if !(coverage.contains(id) || declaration.surface == "c" && c_targets.contains(id)) {
            errors.push(format!("missing boundary decision: {id}"));
        }
    }
    let mut capabilities = BTreeSet::new();
    for capability in &ledger.capabilities {
        if !capabilities.insert(&capability.name)
            || capability.rationale.trim().is_empty()
            || capability.validation_limits.trim().is_empty()
        {
            errors.push(format!(
                "duplicate or incomplete capability {}",
                capability.name
            ));
        }
    }
    if ledger.scope.trim().is_empty() || ledger.exclusions.is_empty() {
        errors.push("scope and exclusions must be documented".into());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn rust_symbols(items: &[syn::Item], symbols: &mut BTreeSet<String>) {
    for item in items {
        match item {
            syn::Item::Fn(f) => {
                symbols.insert(f.sig.ident.to_string());
            }
            syn::Item::Mod(m) => {
                if let Some((_, items)) = &m.content {
                    rust_symbols(items, symbols);
                }
            }
            syn::Item::Impl(i) => {
                for member in &i.items {
                    if let syn::ImplItem::Fn(f) = member {
                        symbols.insert(f.sig.ident.to_string());
                    }
                }
            }
            _ => {}
        }
    }
}

fn verify_evidence(root: &Path, path: &str, symbol: &str) -> Result<(), String> {
    if symbol.is_empty()
        || !symbol
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(format!("invalid evidence symbol {symbol}"));
    }
    let path = root.join(path);
    let source =
        std::fs::read_to_string(&path).map_err(|e| format!("evidence {}: {e}", path.display()))?;
    if path.extension().is_some_and(|e| e == "rs") {
        let parsed = syn::parse_file(&source).map_err(|e| e.to_string())?;
        let mut symbols = BTreeSet::new();
        rust_symbols(&parsed.items, &mut symbols);
        if symbols.contains(symbol) {
            return Ok(());
        }
    } else if path.extension().is_some_and(|e| e == "c") {
        // C evidence is a named native fixture entry point. Declaration parsing is
        // delegated to the existing Clang-based ABI checks, not this audit.
        if c_evidence::has_function_identifier(&source, symbol) {
            return Ok(());
        }
    }
    Err(format!(
        "missing evidence symbol {symbol} in {}",
        path.display()
    ))
}

fn cell(value: &str) -> String {
    value.replace('|', "&#124;").replace('\n', " ")
}

fn symbol_label(id: &str) -> String {
    let (surface, path) = id.split_once('|').unwrap_or(("", id));
    let path = path.split('#').next().unwrap_or(path);
    let mut parts: Vec<_> = path.rsplit("::").take(2).collect();
    parts.reverse();
    format!("`{surface}: {}`", parts.join("::"))
}

/// Render only the authoritative decision-table section, without writing files.
#[must_use]
pub fn render(ledger: &Ledger) -> String {
    let mut result = format!(
        "{BEGIN}\n\nGenerated from `parity.json`; edit reviewed decisions there. Declaration coverage\nrequires behavior evidence and does not establish semantic or platform parity.\n\n"
    );
    result.push_str("| Decision | Boundary | Status | Sources / targets | Rationale and restrictions | Evidence / limits |\n| --- | --- | --- | --- | --- | --- |\n");
    for decision in &ledger.decisions {
        let status = decision.disposition.label();
        let targets = if decision.targets.is_empty() {
            "explicit omission".to_owned()
        } else {
            decision
                .targets
                .iter()
                .map(|id| symbol_label(id))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let evidence = decision
            .evidence
            .iter()
            .map(|e| format!("`{}::{}`", e.path, e.symbol))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            result,
            "| {} | {} | {} | {} sources → {} | {} {} | {} {} |",
            cell(&decision.id),
            decision.boundary,
            status,
            decision.sources.len(),
            targets,
            cell(&decision.rationale),
            cell(&decision.restrictions),
            cell(&evidence),
            cell(&decision.validation_limits)
        );
    }
    result.push_str(
        "\n| Capability | Status | Decision | Validation limits |\n| --- | --- | --- | --- |\n",
    );
    for capability in &ledger.capabilities {
        let status = capability.disposition.label();
        let _ = writeln!(
            result,
            "| {} | {} | {} | {} |",
            cell(&capability.name),
            status,
            cell(&capability.rationale),
            cell(&capability.validation_limits)
        );
    }
    let _ = write!(result, "\n{END}");
    result
}

/// Check the generated decision tables without changing surrounding prose.
///
/// # Errors
/// Returns file access, missing/duplicate marker and stale table errors.
pub fn check_markdown(root: &Path, ledger: &Ledger) -> Result<(), String> {
    let source = std::fs::read_to_string(root.join(MARKDOWN)).map_err(|e| e.to_string())?;
    if source.matches(BEGIN).count() != 1 || source.matches(END).count() != 1 {
        return Err("PARITY.md needs exactly one generated decision section".into());
    }
    let (Some(start), Some(end)) = (source.find(BEGIN), source.find(END)) else {
        return Err("PARITY.md decision markers are missing".into());
    };
    if source.get(start..end + END.len()) != Some(render(ledger).as_str()) {
        return Err(
            "PARITY.md decision tables are stale; review the ledger, then use `render`".into(),
        );
    }
    Ok(())
}
