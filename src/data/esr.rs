//! Нормализация кодов ЕСР-6 и классификация по сетевому району (первые 2 цифры).

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;

/// Нормализация кода ЕСР для сравнения (цифры → 6 знаков с ведущими нулями).
pub fn normalize_esr6(raw: &str) -> String {
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

/// Проверка контрольной цифры 6-значного ЕСР (алгоритм ТР4).
pub fn validate_esr6_checksum(code: &str) -> bool {
    let c = normalize_esr6(code);
    if c.len() != 6 || !c.chars().all(|ch| ch.is_ascii_digit()) {
        return false;
    }
    let digits: Vec<u32> = c.chars().map(|ch| ch.to_digit(10).unwrap()).collect();

    fn remainder(digits: &[u32], offset: u32) -> Option<u32> {
        let total: u32 = digits
            .iter()
            .take(5)
            .enumerate()
            .map(|(i, d)| d * (i as u32 + offset))
            .sum();
        let rem = total % 11;
        if rem == 10 { None } else { Some(rem) }
    }

    match remainder(&digits, 1).or_else(|| remainder(&digits, 3)) {
        Some(rem) => rem == digits[5],
        None => digits[5] == 0,
    }
}

/// Результат классификации по `data/stations/esr_country_prefixes.csv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsrClassification {
    pub esr6: String,
    pub country_hint: String,
    pub region_group: String,
    pub network_district: String,
}

/// Индекс префиксов сетевого района; неизвестный район → RU / `ru`.
#[derive(Debug)]
pub struct EsrCountryIndex {
    rules: HashMap<(u8, String), (String, String)>,
    default_country: String,
    default_region: String,
}

impl EsrCountryIndex {
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("чтение {}", path.display()))?;
        let mut rules = HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with("prefix_len,") {
                continue;
            }
            let cols: Vec<&str> = line.split(',').map(str::trim).collect();
            if cols.len() < 4 {
                continue;
            }
            let prefix_len: u8 = cols[0].parse().context("prefix_len")?;
            let prefix = cols[1].to_string();
            let country = cols[2].to_string();
            let region = cols[3].to_string();
            rules.insert((prefix_len, prefix), (country, region));
        }
        Ok(Self {
            rules,
            default_country: "RU".to_string(),
            default_region: "ru".to_string(),
        })
    }

    pub fn classify(&self, raw: &str) -> Option<EsrClassification> {
        let esr6 = normalize_esr6(raw);
        if esr6.len() != 6 || !esr6.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let district = esr6[..2].to_string();
        let (country_hint, region_group) = self
            .rules
            .get(&(2, district.clone()))
            .cloned()
            .unwrap_or_else(|| {
                (
                    self.default_country.clone(),
                    self.default_region.clone(),
                )
            });
        Some(EsrClassification {
            esr6,
            country_hint,
            region_group,
            network_district: district,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn parity_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts/stations/tests/fixtures/test_normalize_parity.json")
    }

    fn prefixes_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data/stations/esr_country_prefixes.csv")
    }

    #[test]
    fn normalize_esr6_parity_with_python_fixture() {
        let text = std::fs::read_to_string(parity_fixture()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        for pair in v["normalize"].as_array().unwrap() {
            let inp = if pair[0].is_number() {
                pair[0].as_i64().unwrap().to_string()
            } else {
                pair[0].as_str().unwrap_or("").to_string()
            };
            let want = pair[1].as_str().unwrap();
            assert_eq!(normalize_esr6(&inp), want, "input={inp:?}");
        }
    }

    #[test]
    fn checksum_fixture() {
        let text = std::fs::read_to_string(parity_fixture()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        for code in v["checksum_valid"].as_array().unwrap() {
            let c = code.as_str().unwrap();
            assert!(validate_esr6_checksum(c), "expected valid {c}");
        }
        for code in v["checksum_invalid"].as_array().unwrap() {
            let c = code.as_str().unwrap();
            assert!(!validate_esr6_checksum(c), "expected invalid {c}");
        }
    }

    #[test]
    fn country_index_foreign_and_ru_default() {
        let idx = EsrCountryIndex::load(prefixes_path()).unwrap();
        let by = idx.classify("160001").unwrap();
        assert_eq!(by.country_hint, "BY");
        assert_eq!(by.region_group, "cis");
        let msk = idx.classify("194013").unwrap();
        assert_eq!(msk.country_hint, "RU");
        assert_eq!(msk.region_group, "ru");
        let lv = idx.classify("210001").unwrap();
        assert_eq!(lv.country_hint, "LV");
        assert_eq!(lv.region_group, "baltic");
    }
}
