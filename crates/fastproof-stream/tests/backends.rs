//! Schema creation against real servers, run twice to show it is idempotent. Gated on
//! `FASTPROOF_TEST_CLICKHOUSE_URL` and `FASTPROOF_TEST_OPENSEARCH_URL` (with the `_USER`,
//! `_PASSWORD_FILE` and `_CA_FILE` variables of the server settings).

use fastproof_stream::sink::Sink;
use fastproof_stream::{ClickHouseConfig, OpenSearchConfig, Vars};

fn vars(prefix: &str, extra: Vec<(String, String)>) -> impl Fn(&str) -> Option<String> {
    let test = format!("FASTPROOF_TEST_{prefix}_");
    move |k: &str| {
        if let Some((_, v)) = extra.iter().find(|(n, _)| n == k) {
            return Some(v.clone());
        }
        let rest = k.strip_prefix(&format!("FASTPROOF_{prefix}_"))?;
        std::env::var(format!("{test}{rest}")).ok()
    }
}

#[tokio::test]
async fn clickhouse_schema_is_idempotent() {
    if std::env::var("FASTPROOF_TEST_CLICKHOUSE_URL").is_err() {
        return;
    }
    let db = format!("fptest_schema_{:08x}", rand::random::<u32>());
    let get = vars(
        "CLICKHOUSE",
        vec![("FASTPROOF_CLICKHOUSE_DATABASE".into(), db.clone())],
    );
    let ch = fastproof_stream::clickhouse::ClickHouse::new(
        ClickHouseConfig::from_vars(&Vars(&get)).unwrap().unwrap(),
    )
    .unwrap();
    ch.prepare().await.unwrap();
    ch.prepare().await.unwrap();
    let tables = ch
        .exec(
            &format!("SELECT name FROM system.tables WHERE database = '{db}' ORDER BY name"),
            &[],
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        tables.split_whitespace().collect::<Vec<_>>(),
        [
            "attempts",
            "defect_daily",
            "fingerprint_daily",
            "fingerprint_daily_mv",
            "logs",
            "results",
            "schema_version",
            "series_daily",
            "series_daily_mv",
            "steps",
            "triage"
        ]
    );
    ch.exec(&format!("DROP DATABASE {db}"), &[], None)
        .await
        .unwrap();
}

#[tokio::test]
async fn opensearch_schema_is_idempotent() {
    if std::env::var("FASTPROOF_TEST_OPENSEARCH_URL").is_err() {
        return;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let prefix = format!("fptest-schema-{:08x}-", rand::random::<u32>());
    let get = vars(
        "OPENSEARCH",
        vec![
            ("FASTPROOF_OPENSEARCH_INDEX_PREFIX".into(), prefix.clone()),
            ("FASTPROOF_OPENSEARCH_REPLICAS".into(), "0".into()),
        ],
    );
    let os = fastproof_stream::opensearch::OpenSearch::new(
        OpenSearchConfig::from_vars(&Vars(&get)).unwrap().unwrap(),
    )
    .unwrap();
    os.prepare().await.unwrap();
    os.prepare().await.unwrap();
}
