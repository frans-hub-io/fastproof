//! S3 backend against a real S3-compatible server. Needs `FASTPROOF_TEST_S3_ENDPOINT`,
//! `_REGION`, `_BUCKET`, `_ACCESS_KEY` and `_SECRET_KEY`; skipped without them.

use std::time::Duration;

use fastproof_blob::{BlobConfig, BlobKey, BlobStore, S3Config, Secret, sha256_file};

fn s3_config() -> Option<S3Config> {
    let var = |k: &str| std::env::var(format!("FASTPROOF_TEST_S3_{k}")).ok();
    let Some(endpoint) = var("ENDPOINT") else {
        eprintln!("FASTPROOF_TEST_S3_ENDPOINT is not set: skipped");
        return None;
    };
    Some(S3Config {
        endpoint: Some(endpoint),
        region: var("REGION").unwrap_or_else(|| "us-east-1".into()),
        bucket: var("BUCKET").expect("FASTPROOF_TEST_S3_BUCKET"),
        path_style: true,
        ca_file: None,
        access_key: var("ACCESS_KEY").map(Secret::new),
        secret_key: var("SECRET_KEY").map(Secret::new),
    })
}

#[tokio::test]
async fn s3_round_trip_multipart_presign_and_prefix_delete() {
    let Some(cfg) = s3_config() else { return };
    let store = BlobStore::open(&BlobConfig::S3(cfg)).unwrap();
    store.check().await.unwrap();
    let project = format!("blobtest-{:012x}", rand::random::<u64>() >> 16);
    let dir = tempfile::tempdir().unwrap();

    let small = dir.path().join("small");
    std::fs::write(&small, b"hello from fastproof").unwrap();
    let (sha, _) = sha256_file(&small).unwrap();
    let key = BlobKey::attachment(&project, &sha).unwrap();
    store.put_file(&key, &small).await.unwrap();
    assert_eq!(store.head(&key).await.unwrap(), Some(20));

    let big = dir.path().join("big");
    let data: Vec<u8> = (0..(fastproof_blob::SINGLE_PUT_MAX * 2 + 777))
        .map(|i| (i % 253) as u8)
        .collect();
    std::fs::write(&big, &data).unwrap();
    let (big_sha, _) = sha256_file(&big).unwrap();
    let big_key = BlobKey::attachment(&project, &big_sha).unwrap();
    store.put_file(&big_key, &big).await.unwrap();
    let out = dir.path().join("out");
    assert_eq!(
        store.get_to_file(&big_key, &out).await.unwrap(),
        Some(data.len() as u64)
    );
    assert_eq!(sha256_file(&out).unwrap().0, big_sha);

    let url = store
        .presign_get(&key, Duration::from_secs(300))
        .await
        .unwrap()
        .expect("S3 signs URLs");
    assert!(url.contains("X-Amz-Signature="), "{url}");
    let put_url = store
        .presign_put(&key, Duration::from_secs(300))
        .await
        .unwrap()
        .unwrap();
    assert!(put_url.contains("X-Amz-Signature="));

    let missing = BlobKey::attachment(&project, &"0".repeat(64)).unwrap();
    assert_eq!(store.head(&missing).await.unwrap(), None);
    assert!(store.get(&missing).await.unwrap().is_none());

    let n = store
        .delete_prefix(&BlobKey::project_prefix(&project).unwrap())
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(store.head(&key).await.unwrap(), None);
}
