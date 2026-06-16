//! Справочники из `data/references.json` (несколько объектов в массиве).

use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use serde_json::Value;

/// Нормализация кода ЕТСНГ для сравнения (цифры → 6 знаков с ведущими нулями).
pub fn normalize_etsng_code(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    if t.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = t.parse::<u64>() {
            return format!("{n:06}");
        }
    }
    t.to_string()
}

/// Короткие названия дорог, для которых промывка оплачивается клиентом на иностранной территории
/// (`NoCleaningRoads` в первом подходящем блоке JSON).
///
/// Вагоны, образовавшиеся на одной из этих дорог (`SupplyNode::railway_to`),
/// считаются «чистыми» с точки зрения российского планирования — промывка уже учтена клиентом.
pub fn load_no_cleaning_roads(path: impl AsRef<Path>) -> anyhow::Result<HashSet<String>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("чтение {}", path.display()))?;
    let blocks: Vec<Value> = serde_json::from_str(&text).context("разбор references.json")?;
    let mut out = HashSet::new();
    for b in blocks {
        let Some(obj) = b.as_object() else { continue };
        let Some(arr) = obj.get("NoCleaningRoads").and_then(|v| v.as_array()) else { continue };
        for v in arr {
            if let Some(s) = v.as_str() {
                let t = s.trim().to_string();
                if !t.is_empty() {
                    out.insert(t);
                }
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    Ok(out)
}

/// Текущие коды ЕТСНГ порожнего вагона, означающие, что вагон уже в цикле промывки/ремонта
/// и считается **чистым** (`WashedEmptyEtsngCodes` в первом подходящем блоке JSON).
///
/// Если текущий `FrETSNGCode` вагона входит в этот список, повторная промывка не назначается,
/// даже если предыдущий груз ([`crate::node::SupplyNode::prev_etsngs`]) был из `WashProductCodes`.
/// Типично: 421208 (для/из очистки, промывки или дезинфекции), 421195 (в/из ремонта).
pub fn load_washed_empty_codes(path: impl AsRef<Path>) -> anyhow::Result<HashSet<String>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("чтение {}", path.display()))?;
    let blocks: Vec<Value> = serde_json::from_str(&text).context("разбор references.json")?;
    let mut out = HashSet::new();
    for b in blocks {
        let Some(obj) = b.as_object() else {
            continue;
        };
        let Some(arr) = obj.get("WashedEmptyEtsngCodes").and_then(|v| v.as_array()) else {
            continue;
        };
        for v in arr {
            if let Some(s) = v.as_str() {
                let n = normalize_etsng_code(s);
                if !n.is_empty() {
                    out.insert(n);
                }
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    Ok(out)
}

/// Коды ЕТСНГ грузов, для которых требуется промывка (`WashProductCodes` в первом подходящем блоке JSON).
pub fn load_wash_product_codes(path: impl AsRef<Path>) -> anyhow::Result<HashSet<String>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("чтение {}", path.display()))?;
    let blocks: Vec<Value> = serde_json::from_str(&text).context("разбор references.json")?;
    let mut out = HashSet::new();
    for b in blocks {
        let Some(obj) = b.as_object() else {
            continue;
        };
        let Some(arr) = obj.get("WashProductCodes").and_then(|v| v.as_array()) else {
            continue;
        };
        for v in arr {
            if let Some(s) = v.as_str() {
                let n = normalize_etsng_code(s);
                if !n.is_empty() {
                    out.insert(n);
                }
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    Ok(out)
}
