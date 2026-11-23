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
        public_endpoint: None,
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

async fn put(url: &str, headers: &[(String, String)], body: &[u8]) -> reqwest::StatusCode {
    let mut rb = reqwest::Client::new().put(url).body(body.to_vec());
    for (k, v) in headers {
        rb = rb.header(k, v);
    }
    rb.send().await.unwrap().status()
}

#[tokio::test]
async fn s3_direct_put_is_bound_to_its_headers_and_copied_by_the_server() {
    let Some(cfg) = s3_config() else { return };
    let store = BlobStore::open(&BlobConfig::S3(cfg)).unwrap();
    let id = format!("{:032x}", rand::random::<u128>());
    let incoming = BlobKey::direct_incoming(&id).unwrap();
    let check = BlobKey::direct_check(&id).unwrap();
    let body = b"direct upload body".to_vec();
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("b");
    std::fs::write(&f, &body).unwrap();
    let (sha, _) = sha256_file(&f).unwrap();
    let p = store
        .presign_direct_put(&incoming, Duration::from_secs(300), Some(&sha))
        .await
        .unwrap()
        .expect("S3 signs URLs");
    assert!(p.url.contains("X-Amz-SignedHeaders="), "{}", p.url);
    assert!(
        p.headers
            .iter()
            .any(|(k, v)| k == "content-type" && v == fastproof_blob::DIRECT_CONTENT_TYPE)
    );

    let without: Vec<(String, String)> = p
        .headers
        .iter()
        .filter(|(k, _)| k != "content-type")
        .cloned()
        .collect();
    assert!(
        !put(&p.url, &without, &body).await.is_success(),
        "a PUT without the signed content type is refused"
    );
    let other_type: Vec<(String, String)> = p
        .headers
        .iter()
        .map(|(k, v)| {
            if k == "content-type" {
                (k.clone(), "text/html".into())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect();
    assert!(!put(&p.url, &other_type, &body).await.is_success());
    let tampered = put(&p.url, &p.headers, b"other content").await;
    eprintln!("a PUT of other content with the signed checksum answered {tampered}");
    assert!(
        put(&p.url, &p.headers, &body).await.is_success(),
        "the declared body is accepted"
    );
    assert_eq!(
        store.head(&incoming).await.unwrap(),
        Some(body.len() as u64)
    );
    store.copy(&incoming, &check).await.unwrap();
    let out = dir.path().join("out");
    store.get_to_file(&check, &out).await.unwrap();
    assert_eq!(sha256_file(&out).unwrap().0, sha);
    assert_eq!(
        BlobKey::parse(check.as_str()).unwrap(),
        check,
        "direct keys parse back"
    );
    assert!(
        store
            .delete_prefix(&BlobKey::direct_prefix())
            .await
            .unwrap()
            >= 2
    );
}

#[tokio::test]
async fn s3_public_endpoint_is_used_for_signing_only() {
    let Some(mut cfg) = s3_config() else { return };
    cfg.public_endpoint = Some("https://s3.example.com".into());
    let store = BlobStore::open(&BlobConfig::S3(cfg)).unwrap();
    store.check().await.unwrap();
    let p = store
        .presign_direct_put(
            &BlobKey::direct_incoming("abc").unwrap(),
            Duration::from_secs(60),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(p.url.starts_with("https://s3.example.com/"), "{}", p.url);
}
