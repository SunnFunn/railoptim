#[cfg(test)]
mod api_tests {
    use std::fs;
    use std::path::PathBuf;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::data::StationGeoCatalog;
    use crate::web::config::WebConfig;
    use crate::web::routes::router;
    use crate::web::state::AppState;

    fn temp_geo_db(suffix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "railoptim_web_api_{}_{suffix}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let db = dir.join("geo.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE stations_geo (
              esr6 TEXT PRIMARY KEY, name TEXT, lat REAL, lon REAL,
              country_hint TEXT, region_group TEXT, source TEXT, match_method TEXT,
              osm_id INTEGER, name_osm TEXT, confidence REAL, built_at TEXT);
             INSERT INTO stations_geo VALUES
              ('194013','Москва',55.7558,37.6173,'RU','ru','t','m',NULL,NULL,1.0,'x');",
        )
        .unwrap();
        db
    }

    fn test_state(db: PathBuf) -> AppState {
        let config = WebConfig {
            bind_addr: "127.0.0.1:0".parse().unwrap(),
            stations_geo_db: db.clone(),
            optim_result_dir: PathBuf::from("tmp"),
            optim_result_file: None,
            cors_origins: vec!["*".into()],
        };
        let stations = StationGeoCatalog::load(&db).unwrap();
        AppState::new(config, stations).unwrap()
    }

    async fn get_station_status(app: axum::Router, path: &str) -> StatusCode {
        app.oneshot(
            axum::http::Request::get(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    }

    #[tokio::test]
    async fn health_ok() {
        let db = temp_geo_db("health");
        let app = router(test_state(db.clone()));
        assert_eq!(
            get_station_status(app, "/health").await,
            StatusCode::OK
        );
        let _ = fs::remove_dir_all(db.parent().unwrap());
    }

    #[tokio::test]
    async fn station_not_found() {
        let db = temp_geo_db("not_found");
        let app = router(test_state(db.clone()));
        assert_eq!(
            get_station_status(app, "/api/v1/stations/999999").await,
            StatusCode::NOT_FOUND
        );
        let _ = fs::remove_dir_all(db.parent().unwrap());
    }

    #[tokio::test]
    async fn station_found() {
        let db = temp_geo_db("found");
        let app = router(test_state(db.clone()));
        assert_eq!(
            get_station_status(app, "/api/v1/stations/194013").await,
            StatusCode::OK
        );
        let _ = fs::remove_dir_all(db.parent().unwrap());
    }
}
