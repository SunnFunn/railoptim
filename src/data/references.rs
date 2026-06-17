//! Справочники из `data/references.json` (несколько объектов в массиве).

use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use serde_json::Value;

use super::esr::normalize_esr6;

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

/// Allowlist «своих» ёмкостей отстоя (`data/reserve_owners.json`).
///
/// Файл — плоский JSON-массив объектов вида
/// `{"railway", "station", "station_code", "owner", "owner_okpo"}`. Возвращает множество
/// пар `(код станции ЕСР-6, ОКПО владельца)`; фильтрация БД отстоя выполняется строго по
/// этим двум полям (`station_code` нормализуется до 6 цифр, `owner_okpo` — trim).
/// Текстовые наименования (`railway`, `station`, `owner`) — справочные, в ключ не входят.
pub fn load_reserve_owners_allowlist(
    path: impl AsRef<Path>,
) -> anyhow::Result<HashSet<(String, String)>> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("чтение {}", path.display()))?;
    let entries: Vec<Value> =
        serde_json::from_str(&text).context("разбор reserve_owners.json")?;
    let mut out = HashSet::new();
    for e in entries {
        let Some(obj) = e.as_object() else {
            continue;
        };
        let station_code = obj
            .get("station_code")
            .and_then(|v| v.as_str())
            .map(normalize_esr6)
            .filter(|s| !s.is_empty());
        let owner_okpo = obj
            .get("owner_okpo")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let (Some(sc), Some(okpo)) = (station_code, owner_okpo) {
            out.insert((sc, okpo));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Загрузчик allowlist: ключ — (ЕСР-6, ОКПО), наименования игнорируются,
    /// дубли пар схлопываются, неполные записи пропускаются.
    #[test]
    fn reserve_owners_allowlist_parses_station_and_okpo() {
        let dir = std::env::temp_dir().join(format!("railoptim_reserve_owners_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("reserve_owners.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(
            r#"[
                {"railway":"СВР","station":"Березники-Сортировочная","station_code":"769002","owner":"ЕвроХим","owner_okpo":"37011412"},
                {"railway":"СВР","station":"Березники-Сортировочная","station_code":"769002","owner":"Уралкалий","owner_okpo":"00203944"},
                {"railway":"СВР","station":"Соликамск 2","station_code":"769500","owner":"Уралкалий","owner_okpo":"00203944"},
                {"railway":"СВР","station":"Дубль","station_code":"769500","owner":"Уралкалий (другое имя)","owner_okpo":"00203944"},
                {"railway":"СВР","station":"БезОКПО","station_code":"769999"}
            ]"#
            .as_bytes(),
        )
        .unwrap();

        let allow = load_reserve_owners_allowlist(&path).unwrap();
        // 3 уникальные пары (дубль 769500+00203944 схлопнут, запись без ОКПО пропущена).
        assert_eq!(allow.len(), 3);
        assert!(allow.contains(&("769002".to_string(), "37011412".to_string())));
        assert!(allow.contains(&("769002".to_string(), "00203944".to_string())));
        assert!(allow.contains(&("769500".to_string(), "00203944".to_string())));
        assert!(!allow.contains(&("769999".to_string(), String::new())));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
