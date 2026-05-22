//! Справочник станций с координатами (`data/stations/stations_geo.sqlite`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use thiserror::Error;

use super::esr::normalize_esr6;

pub const DEFAULT_DB_PATH: &str = "data/stations/stations_geo.sqlite";

/// Станция с координатами (основные поля для карты и lookup).
#[derive(Debug, Clone, PartialEq)]
pub struct StationGeo {
    pub esr6: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub country_hint: Option<String>,
    pub region_group: Option<String>,
    pub source: String,
    pub match_method: String,
    pub osm_id: Option<i64>,
    pub name_osm: Option<String>,
    pub confidence: f64,
}

#[derive(Debug, Error)]
pub enum StationGeoError {
    #[error("sqlite ({path}): {source}")]
    Sqlite {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("некорректная строка esr6={esr6}: {message}")]
    InvalidRow { esr6: String, message: String },
}

/// In-memory каталог станций, keyed by normalized esr6.
#[derive(Debug, Clone)]
pub struct StationGeoCatalog {
    by_esr6: HashMap<String, StationGeo>,
    path: PathBuf,
}

impl StationGeoCatalog {
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            by_esr6: HashMap::new(),
            path: path.into(),
        }
    }

    /// Загрузка SQLite; файл должен существовать.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, StationGeoError> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open(&path).map_err(|source| StationGeoError::Sqlite {
            path: path.clone(),
            source,
        })?;

        let mut stmt = conn
            .prepare(
                "SELECT esr6, name, lat, lon, country_hint, region_group,
                        source, match_method, osm_id, name_osm, confidence
                 FROM stations_geo",
            )
            .map_err(|source| StationGeoError::Sqlite {
                path: path.clone(),
                source,
            })?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, f64>(10)?,
                ))
            })
            .map_err(|source| StationGeoError::Sqlite {
                path: path.clone(),
                source,
            })?;

        let mut by_esr6 = HashMap::new();
        for row in rows {
            let (
                esr6_raw,
                name,
                lat,
                lon,
                country_hint,
                region_group,
                source,
                match_method,
                osm_id,
                name_osm,
                confidence,
            ) = row.map_err(|source| StationGeoError::Sqlite {
                path: path.clone(),
                source,
            })?;

            let esr6 = normalize_esr6(&esr6_raw);
            if esr6.len() != 6 || !esr6.chars().all(|c| c.is_ascii_digit()) {
                return Err(StationGeoError::InvalidRow {
                    esr6: esr6_raw,
                    message: "esr6 должен быть 6 цифр".into(),
                });
            }
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return Err(StationGeoError::InvalidRow {
                    esr6: esr6.clone(),
                    message: format!("координаты вне диапазона: lat={lat}, lon={lon}"),
                });
            }

            by_esr6.insert(
                esr6.clone(),
                StationGeo {
                    esr6,
                    name,
                    lat,
                    lon,
                    country_hint,
                    region_group,
                    source,
                    match_method,
                    osm_id,
                    name_osm,
                    confidence,
                },
            );
        }

        Ok(Self { by_esr6, path })
    }

    /// `STATIONS_GEO_DB` или [`DEFAULT_DB_PATH`]. При ошибке / отсутствии файла — пустой каталог + warn.
    pub fn load_from_env() -> Self {
        let path = std::env::var("STATIONS_GEO_DB").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string());
        let path_buf = PathBuf::from(&path);

        if !path_buf.is_file() {
            eprintln!(
                "stations_geo: файл не найден ({}), справочник пуст",
                path_buf.display()
            );
            return Self::empty(path_buf);
        }

        match Self::load(&path_buf) {
            Ok(cat) if cat.is_empty() => {
                eprintln!(
                    "stations_geo: 0 записей в {}, справочник пуст",
                    path_buf.display()
                );
                cat
            }
            Ok(cat) => {
                let hint = cat.coverage_hint();
                if hint.is_empty() {
                    println!(
                        "stations_geo: {} записей, {}",
                        cat.len(),
                        path_buf.display()
                    );
                } else {
                    println!(
                        "stations_geo: {} записей, {} ({})",
                        cat.len(),
                        path_buf.display(),
                        hint
                    );
                }
                cat
            }
            Err(e) => {
                eprintln!(
                    "stations_geo: не загружен ({}), справочник пуст",
                    e
                );
                Self::empty(path_buf)
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.by_esr6.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_esr6.is_empty()
    }

    pub fn contains(&self, esr6: &str) -> bool {
        self.by_esr6.contains_key(&normalize_esr6(esr6))
    }

    pub fn get(&self, esr6: &str) -> Option<&StationGeo> {
        self.by_esr6.get(&normalize_esr6(esr6))
    }

    /// Краткая сводка для логов (число станций по region_group).
    pub fn coverage_hint(&self) -> String {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for s in self.by_esr6.values() {
            let rg = s.region_group.as_deref().unwrap_or("unknown");
            *counts.entry(rg).or_insert(0) += 1;
        }
        let mut parts: Vec<_> = counts.into_iter().collect();
        parts.sort_by_key(|(k, _)| *k);
        parts
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_sample_db(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE stations_geo (
              esr6 TEXT PRIMARY KEY NOT NULL,
              name TEXT NOT NULL,
              lat REAL NOT NULL,
              lon REAL NOT NULL,
              country_hint TEXT,
              region_group TEXT,
              source TEXT NOT NULL,
              match_method TEXT NOT NULL,
              osm_id INTEGER,
              name_osm TEXT,
              confidence REAL NOT NULL DEFAULT 1.0,
              built_at TEXT NOT NULL
            );
            INSERT INTO stations_geo VALUES
              ('194013', 'Москва-Пассажирская-Казанская', 55.7558, 37.6173,
               'RU', 'ru', 'osm_pbf', 'ref', 100, 'Moscow', 1.0, '2026-01-01T00:00:00+00:00'),
              ('160001', 'Брест-Центральный', 52.0976, 23.7341,
               'BY', 'cis', 'osm_pbf', 'ref', 200, 'Brest', 1.0, '2026-01-01T00:00:00+00:00');
            ",
        )
        .unwrap();
    }

    #[test]
    fn load_and_lookup() {
        let dir = std::env::temp_dir().join(format!("railoptim_stations_geo_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sample.sqlite");
        write_sample_db(&db);

        let cat = StationGeoCatalog::load(&db).unwrap();
        assert_eq!(cat.len(), 2);
        assert!(cat.contains("194013"));
        assert!(cat.contains(" 194013 "));

        let msk = cat.get("194013").unwrap();
        assert_eq!(msk.name, "Москва-Пассажирская-Казанская");
        assert!((msk.lat - 55.7558).abs() < 1e-4);

        let hint = cat.coverage_hint();
        assert!(hint.contains("ru=1"));
        assert!(hint.contains("cis=1"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_catalog_missing_file() {
        let cat = StationGeoCatalog::empty("/nonexistent/stations_geo.sqlite");
        assert!(cat.is_empty());
        assert!(!cat.contains("194013"));
    }
}
