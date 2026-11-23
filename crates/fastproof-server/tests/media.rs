//! Attachment serving (byte ranges, validators, sandbox headers) and signed media URLs, on the
//! filesystem and on S3 blob stores. Skipped without `FASTPROOF_TEST_DATABASE_URL`; the S3
//! variants also need `FASTPROOF_TEST_S3_*`.

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{ADMIN_TOKEN, Server, TestDb};
use fastproof_blob::BlobKey;
use fastproof_store::attachments::NewAttachment;
use reqwest::{Method, Response, StatusCode};
use serde_json::{Value, json};

const SIZE: usize = 1000;
const OUTSIDE_KEY: &str = "test-outside-secret-0123456789abcdef";

fn content() -> Vec<u8> {
    (0..SIZE).map(|i| (i * 7 % 251) as u8).collect()
}

struct Fx {
    s: Server,
    key: String,
    launch: String,
    id: String,
    sha: String,
    body: Vec<u8>,
}

async fn start(db: &TestDb, s3: bool, extra: &[(&str, &str)]) -> Option<Fx> {
    let mut vars: Vec<(String, String)> = Vec::new();
    if s3 {
        let Some(env) = common::s3_env() else {
            eprintln!("FASTPROOF_TEST_S3_* not set: skipped");
            return None;
        };
        vars.extend(env);
    }
    vars.push(("FASTPROOF_MEDIA_URL_KEY".into(), OUTSIDE_KEY.into()));
    vars.push(("FASTPROOF_MEDIA_PREPARE".into(), "false".into()));
    for (k, v) in extra {
        vars.push((k.to_string(), v.to_string()));
    }
    let refs: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let s = Server::start_with(db, &refs).await;
    let key = format!("m-{:012x}", rand::random::<u64>() >> 16);
    s.project(&key).await;
    let launch = s.launch(&key, None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let body = content();
    let (id, sha) = attach(&s, &key, &launch, body.clone(), "video/mp4").await;
    Some(Fx {
        s,
        key,
        launch,
        id,
        sha,
        body,
    })
}

async fn attach(s: &Server, key: &str, launch: &str, bytes: Vec<u8>, ct: &str) -> (String, String) {
    let r = s
        .req(
            Method::POST,
            &format!("/api/v1/projects/{key}/launches/{launch}/attachments"),
            Some(ADMIN_TOKEN),
        )
        .header("content-type", ct)
        .header("x-fastproof-attachment-name", "clip.mp4")
        .body(bytes)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let b: Value = r.json().await.unwrap();
    (
        b["id"].as_str().unwrap().to_string(),
        b["sha256"].as_str().unwrap().to_string(),
    )
}

impl Fx {
    fn content_path(&self) -> String {
        format!(
            "/api/v1/projects/{}/attachments/{}/content",
            self.key, self.id
        )
    }

    async fn direct(&self, method: Method, headers: &[(&str, &str)]) -> Response {
        let mut rb = self.s.req(method, &self.content_path(), Some(ADMIN_TOKEN));
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        rb.send().await.unwrap()
    }

    async fn media_url(&self) -> String {
        self.media_url_for(&self.key, &self.sha, ADMIN_TOKEN).await
    }

    async fn media_url_for(&self, key: &str, sha: &str, token: &str) -> String {
        let (st, b) = self
            .s
            .call(
                Method::POST,
                &format!("/api/v1/projects/{key}/attachments/{sha}/media-url"),
                Some(token),
                None,
            )
            .await;
        assert_eq!(st, StatusCode::OK, "{b}");
        b["url"].as_str().unwrap().to_string()
    }

    async fn media(&self, method: Method, url: &str, headers: &[(&str, &str)]) -> Response {
        let mut rb = self.s.http.request(method, format!("{}{url}", self.s.base));
        for (k, v) in headers {
            rb = rb.header(*k, *v);
        }
        rb.send().await.unwrap()
    }
}

fn sandboxed(r: &Response, media: bool) {
    let h = r.headers();
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["content-security-policy"], "sandbox; default-src 'none'");
    let cc = h["cache-control"].to_str().unwrap();
    if media {
        let age: u64 = cc
            .strip_prefix("private, max-age=")
            .unwrap_or_else(|| panic!("{cc}"))
            .parse()
            .unwrap();
        assert!(age <= 3600 && !cc.contains("immutable"), "{cc}");
    } else {
        assert_eq!(cc, "private, max-age=31536000, immutable");
    }
    assert_eq!(h["cross-origin-resource-policy"], "same-origin");
    assert_eq!(h["accept-ranges"], "bytes");
    assert!(h.contains_key("etag"), "{h:?}");
    assert!(h.contains_key("content-disposition"));
    assert_eq!(h["content-type"], "video/mp4");
}

/// The three range forms, 416, HEAD, validators and headers on every status, for one route.
async fn serving_checks<F, Fut>(f: &Fx, media: bool, send: F)
where
    F: Fn(Method, Vec<(&'static str, String)>) -> Fut,
    Fut: std::future::Future<Output = Response>,
{
    let etag = format!("\"{}\"", f.sha);
    let go = |m: Method, h: &[(&'static str, &str)]| {
        send(m, h.iter().map(|(k, v)| (*k, v.to_string())).collect())
    };

    let r = go(Method::GET, &[]).await;
    assert_eq!(r.status(), StatusCode::OK);
    sandboxed(&r, media);
    assert_eq!(r.headers()["etag"].to_str().unwrap(), etag);
    assert_eq!(r.headers()["content-length"], SIZE.to_string().as_str());
    assert_eq!(r.bytes().await.unwrap().as_ref(), f.body.as_slice());

    let r = go(Method::HEAD, &[]).await;
    assert_eq!(r.status(), StatusCode::OK);
    sandboxed(&r, media);
    assert_eq!(r.headers()["content-length"], SIZE.to_string().as_str());
    assert!(r.bytes().await.unwrap().is_empty());

    for (range, from, to) in [
        ("bytes=10-19", 10, 19),
        ("bytes=990-", 990, 999),
        ("bytes=-5", 995, 999),
        ("bytes=500-99999", 500, 999),
        ("bytes=0-0", 0, 0),
    ] {
        let r = go(Method::GET, &[("range", range)]).await;
        assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT, "{range}");
        sandboxed(&r, media);
        assert_eq!(
            r.headers()["content-range"].to_str().unwrap(),
            format!("bytes {from}-{to}/{SIZE}")
        );
        assert_eq!(
            r.headers()["content-length"].to_str().unwrap(),
            (to - from + 1).to_string()
        );
        assert_eq!(
            r.bytes().await.unwrap().as_ref(),
            &f.body[from..=to],
            "{range}"
        );
    }

    let r = go(Method::HEAD, &[("range", "bytes=10-19")]).await;
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.headers()["content-range"], "bytes 10-19/1000");
    assert_eq!(r.headers()["content-length"], "10");
    assert!(r.bytes().await.unwrap().is_empty());

    for range in ["bytes=1000-", "bytes=1000-2000", "bytes=-0", "bytes=5000-"] {
        let r = go(Method::GET, &[("range", range)]).await;
        assert_eq!(r.status(), StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
        sandboxed(&r, media);
        assert_eq!(r.headers()["content-range"], "bytes */1000");
        assert!(r.bytes().await.unwrap().is_empty());
    }

    for range in ["bytes=0-1,5-9", "bytes=9-3", "items=0-5", "bytes=x-"] {
        let r = go(Method::GET, &[("range", range)]).await;
        assert_eq!(r.status(), StatusCode::OK, "{range}");
        assert!(!r.headers().contains_key("content-range"));
        assert_eq!(r.bytes().await.unwrap().len(), SIZE);
    }

    for inm in [
        etag.clone(),
        format!("W/{etag}"),
        "*".into(),
        format!("\"x\", {etag}"),
    ] {
        let r = go(Method::GET, &[("if-none-match", &inm)]).await;
        assert_eq!(r.status(), StatusCode::NOT_MODIFIED, "{inm}");
        sandboxed(&r, media);
        assert_eq!(r.headers()["etag"].to_str().unwrap(), etag);
        assert!(r.bytes().await.unwrap().is_empty());
    }
    let r = go(Method::GET, &[("if-none-match", "\"other\"")]).await;
    assert_eq!(r.status(), StatusCode::OK);
    let r = go(
        Method::GET,
        &[("if-none-match", &etag), ("range", "bytes=0-9")],
    )
    .await;
    assert_eq!(r.status(), StatusCode::NOT_MODIFIED);

    let r = go(
        Method::GET,
        &[("range", "bytes=20-29"), ("if-range", &etag)],
    )
    .await;
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.bytes().await.unwrap().as_ref(), &f.body[20..30]);
    for ir in [
        "\"stale\"",
        "Wed, 21 Oct 2015 07:28:00 GMT",
        &format!("W/{etag}"),
    ] {
        let r = go(Method::GET, &[("range", "bytes=20-29"), ("if-range", ir)]).await;
        assert_eq!(r.status(), StatusCode::OK, "if-range {ir}");
        sandboxed(&r, media);
        assert!(!r.headers().contains_key("content-range"));
        assert_eq!(r.bytes().await.unwrap().len(), SIZE);
    }
}

async fn serving(s3: bool) {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(f) = start(&db, s3, &[]).await else {
        return;
    };
    serving_checks(&f, false, |m, h| {
        let f = &f;
        async move {
            let hs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
            f.direct(m, &hs).await
        }
    })
    .await;

    let r =
        f.s.http
            .get(format!("{}{}", f.s.base, f.content_path()))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r =
        f.s.http
            .get(format!("{}{}", f.s.base, f.content_path()))
            .header("range", "bytes=0-9")
            .send()
            .await
            .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::UNAUTHORIZED,
        "a range does not skip auth"
    );

    let url = f.media_url().await;
    serving_checks(&f, true, |m, h| {
        let (f, url) = (&f, url.clone());
        async move {
            let hs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (*k, v.as_str())).collect();
            f.media(m, &url, &hs).await
        }
    })
    .await;
}

#[tokio::test]
async fn serving_fs() {
    serving(false).await;
}

#[tokio::test]
async fn serving_s3() {
    serving(true).await;
}

async fn media_urls(s3: bool) {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(f) = start(&db, s3, &[("FASTPROOF_MEDIA_URL_TTL_SECONDS", "3")]).await else {
        return;
    };
    let (st, b) =
        f.s.call(
            Method::POST,
            &format!("/api/v1/projects/{}/attachments/{}/media-url", f.key, f.sha),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let url = b["url"].as_str().unwrap().to_string();
    assert!(url.starts_with("/api/v1/media/"), "{url}");
    let token = &url["/api/v1/media/".len()..];
    assert!(
        token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    );
    let exp = chrono::DateTime::parse_from_rfc3339(b["expiresAt"].as_str().unwrap()).unwrap();
    let left = (exp.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_milliseconds();
    assert!((1000..=3100).contains(&left), "expires in {left} ms");

    let r = f.media(Method::GET, &url, &[]).await;
    assert_eq!(r.status(), StatusCode::OK);
    sandboxed(&r, true);
    assert_eq!(r.bytes().await.unwrap().as_ref(), f.body.as_slice());
    let r =
        f.s.http
            .get(format!("{}{url}", f.s.base))
            .header("authorization", "Bearer not-a-token")
            .header("range", "bytes=1-2")
            .send()
            .await
            .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::PARTIAL_CONTENT,
        "headers are ignored"
    );

    let generic = f.media(Method::GET, "/api/v1/media/garbage", &[]).await;
    assert_eq!(generic.status(), StatusCode::NOT_FOUND);
    let generic_body = generic.text().await.unwrap();
    for bad in [
        "/api/v1/media/".to_string(),
        "/api/v1/media/a.b".to_string(),
        format!("/api/v1/media/{}", "A".repeat(5000)),
        format!("/api/v1/media/{token}x"),
        format!("/api/v1/media/{token}/extra"),
        format!("/api/v1/media/{}", token.replace('.', "..")),
    ] {
        let r = f.media(Method::GET, &bad, &[]).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{bad:.60}");
        assert_eq!(
            r.text().await.unwrap(),
            generic_body,
            "no detail: {bad:.60}"
        );
    }
    let r = f.media(Method::HEAD, "/api/v1/media/garbage", &[]).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);

    let mut flipped = 0;
    for i in 0..token.len() {
        let mut bytes = token.as_bytes().to_vec();
        bytes[i] = if bytes[i] == b'A' { b'B' } else { b'A' };
        let t = String::from_utf8(bytes).unwrap();
        if t == token {
            continue;
        }
        flipped += 1;
        if flipped % 7 != 0 && i != token.len() - 1 && i != 0 {
            continue;
        }
        let r = f
            .media(Method::GET, &format!("/api/v1/media/{t}"), &[])
            .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "byte {i} changed");
        assert_eq!(r.text().await.unwrap(), generic_body);
    }

    let proj_b = format!("b-{:012x}", rand::random::<u64>() >> 16);
    f.s.project(&proj_b).await;
    let lb = f.s.launch(&proj_b, None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, _) =
        f.s.call(
            Method::POST,
            &format!("/api/v1/projects/{proj_b}/attachments/{}/media-url", f.sha),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "the attachment is not in project b"
    );
    let (st, _) =
        f.s.call(
            Method::POST,
            &format!(
                "/api/v1/projects/{}/attachments/{}/media-url",
                f.key,
                "0".repeat(64)
            ),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) =
        f.s.call(
            Method::POST,
            &format!(
                "/api/v1/projects/{}/attachments/not-a-hash/media-url",
                f.key
            ),
            Some(ADMIN_TOKEN),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) =
        f.s.call(
            Method::POST,
            &format!("/api/v1/projects/{}/attachments/{}/media-url", f.key, f.sha),
            None,
            None,
        )
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let only_b = f.s.token("only-b", "viewer", &[&proj_b]).await;
    let (st, _) =
        f.s.call(
            Method::POST,
            &format!("/api/v1/projects/{}/attachments/{}/media-url", f.key, f.sha),
            Some(&only_b),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "no read access to the project");
    let uploader_in_b = f.s.token("uploader-b", "uploader", &[&proj_b]).await;
    let _ = lb;
    let (st, _) =
        f.s.call(
            Method::POST,
            &format!("/api/v1/projects/{}/attachments/{}/media-url", f.key, f.sha),
            Some(&uploader_in_b),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (other_id, other_sha) =
        attach(&f.s, &f.key, &f.launch, b"second".to_vec(), "text/plain").await;
    let _ = other_id;
    let other_url = f.media_url_for(&f.key, &other_sha, ADMIN_TOKEN).await;
    let r = f.media(Method::GET, &other_url, &[]).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "text/plain");

    tokio::time::sleep(Duration::from_millis(3300)).await;
    let r = f.media(Method::GET, &url, &[]).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND, "expired");
    assert_eq!(r.text().await.unwrap(), generic_body);

    let fresh = f.media_url().await;
    assert_eq!(
        f.media(Method::GET, &fresh, &[]).await.status(),
        StatusCode::OK
    );
    sqlx::query("delete from attachment_links where attachment_id = $1")
        .bind(uuid_of(&f.id))
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("delete from attachments where id = $1")
        .bind(uuid_of(&f.id))
        .execute(&db.pool)
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let r = f.media(Method::GET, &fresh, &[]).await;
        if r.status() == StatusCode::NOT_FOUND {
            assert_eq!(r.text().await.unwrap(), generic_body);
            break;
        }
        assert!(Instant::now() < deadline, "attachment deleted but served");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let r = f.media(Method::HEAD, &fresh, &[]).await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);

    let metrics = reqwest::get(format!("{}/metrics", f.s.ops))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics.contains("media_urls_issued_total"), "{metrics}");
    assert!(metrics.contains("media_url_rejections_total"));
    assert!(metrics.contains("route=\"/api/v1/media/{token}\""));
    assert!(!metrics.contains(token));
}

fn uuid_of(s: &str) -> uuid::Uuid {
    s.parse().unwrap()
}

#[tokio::test]
async fn media_urls_fs() {
    media_urls(false).await;
}

#[tokio::test]
async fn media_urls_s3() {
    media_urls(true).await;
}

#[tokio::test]
async fn media_url_follows_the_token_that_asked() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(f) = start(&db, false, &[]).await else {
        return;
    };
    let f = &f;
    let make = |name: &'static str| async move {
        let (st, t) =
            f.s.post(
                "/api/v1/tokens",
                ADMIN_TOKEN,
                json!({"name": name, "role": "viewer", "scopes": [&f.key], "expiresInDays": 1}),
            )
            .await;
        assert_eq!(st, StatusCode::CREATED, "{t}");
        (
            t["token"].as_str().unwrap().to_string(),
            t["id"].as_str().unwrap().to_string(),
        )
    };
    let revoke = |id: String| async move {
        let (st, b) =
            f.s.call(
                Method::DELETE,
                &format!("/api/v1/tokens/{id}"),
                Some(ADMIN_TOKEN),
                None,
            )
            .await;
        assert!(st.is_success(), "{st} {b}");
    };
    let (tok1, id1) = make("watcher-one").await;
    let never_used = f.media_url_for(&f.key, &f.sha, &tok1).await;
    revoke(id1).await;
    assert_eq!(
        f.media(Method::GET, &never_used, &[]).await.status(),
        StatusCode::NOT_FOUND,
        "the token was revoked before its URL was ever used"
    );

    let (tok2, id2) = make("watcher-two").await;
    let used = f.media_url_for(&f.key, &f.sha, &tok2).await;
    assert_eq!(
        f.media(Method::GET, &used, &[]).await.status(),
        StatusCode::OK
    );
    revoke(id2).await;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let st = f.media(Method::GET, &used, &[]).await.status();
        if st == StatusCode::NOT_FOUND {
            break;
        }
        assert_eq!(st, StatusCode::OK);
        assert!(
            Instant::now() < deadline,
            "a revoked token's URL kept working"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let (st, _) =
        f.s.call(
            Method::POST,
            &format!("/api/v1/projects/{}/attachments/{}/media-url", f.key, f.sha),
            Some(&tok2),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

fn pattern(i: u64) -> u8 {
    (i.wrapping_mul(2_654_435_761) >> 13) as u8
}

async fn big_object(s3: bool) {
    const MIB: u64 = 1024 * 1024;
    const TOTAL: u64 = 600 * MIB;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(f) = start(&db, s3, &[]).await else {
        return;
    };
    let st = &f.s.running.as_ref().unwrap().state;
    let project = st.store.project(&f.key).await.unwrap().unwrap();
    let sha = "c0".repeat(32);
    let file = f.s.dir.path().join("big.bin");
    {
        use std::io::Write;
        let mut w = std::io::BufWriter::new(std::fs::File::create(&file).unwrap());
        let mut block = vec![0u8; MIB as usize];
        for m in 0..600u64 {
            for (j, b) in block.iter_mut().enumerate() {
                *b = pattern(m * MIB + j as u64);
            }
            w.write_all(&block).unwrap();
        }
        w.flush().unwrap();
    }
    let bk = BlobKey::attachment(&f.key, &sha).unwrap();
    st.blobs.put_file(&bk, &file).await.unwrap();
    std::fs::remove_file(&file).unwrap();
    let launch: uuid::Uuid = f.launch.parse().unwrap();
    st.store
        .insert_attachments(
            project.id,
            launch,
            &[NewAttachment {
                sha256: sha.clone(),
                size: TOTAL as i64,
                content_type: "video/mp4".into(),
                original_name: Some("big.mp4".into()),
                blob_key: bk.to_string(),
            }],
        )
        .await
        .unwrap();
    let url = f.media_url_for(&f.key, &sha, ADMIN_TOKEN).await;

    let start_at = 590 * MIB;
    let before = st.blobs.bytes_streamed();
    let r = f
        .media(
            Method::GET,
            &url,
            &[("range", &format!("bytes={start_at}-"))],
        )
        .await;
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        r.headers()["content-range"].to_str().unwrap(),
        format!("bytes {start_at}-{}/{TOTAL}", TOTAL - 1)
    );
    let got = r.bytes().await.unwrap();
    assert_eq!(got.len() as u64, 10 * MIB);
    for k in [0u64, 1, 12345, 5 * MIB, 10 * MIB - 1] {
        assert_eq!(got[k as usize], pattern(start_at + k), "offset {k}");
    }
    let read = st.blobs.bytes_streamed() - before;
    assert_eq!(read, 10 * MIB, "only the range was read from the store");

    let before = st.blobs.bytes_streamed();
    let r = f.media(Method::GET, &url, &[("range", "bytes=-100")]).await;
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.bytes().await.unwrap().len(), 100);
    let r = f
        .media(
            Method::GET,
            &url,
            &[("range", &format!("bytes={}-{}", 300 * MIB, 300 * MIB + 99))],
        )
        .await;
    assert_eq!(r.bytes().await.unwrap().len(), 100);
    let r = f.media(Method::HEAD, &url, &[]).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        r.headers()["content-length"].to_str().unwrap(),
        TOTAL.to_string()
    );
    assert_eq!(st.blobs.bytes_streamed() - before, 200);
    st.blobs.delete(&bk).await.unwrap();
}

#[tokio::test]
async fn big_object_range_fs() {
    big_object(false).await;
}

#[tokio::test]
async fn big_object_range_s3() {
    big_object(true).await;
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn the_token_is_not_logged() {
    fastproof_blob::install_crypto_provider();
    let Some(db) = TestDb::new().await else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("tokens");
    std::fs::write(&tokens, format!("admin:admin:{ADMIN_TOKEN}\n")).unwrap();
    let log_path = dir.path().join("log");
    let log = std::fs::File::create(&log_path).unwrap();
    let (port, ops) = (free_port(), free_port());
    let mut child = Command::new(env!("CARGO_BIN_EXE_fastproof"))
        .arg("serve")
        .env_clear()
        .env("FASTPROOF_DATABASE_URL", &db.url)
        .env("FASTPROOF_LISTEN", format!("127.0.0.1:{port}"))
        .env("FASTPROOF_OPS_LISTEN", format!("127.0.0.1:{ops}"))
        .env("FASTPROOF_BLOB_DIR", dir.path().join("blobs"))
        .env("FASTPROOF_SPOOL_DIR", dir.path().join("spool"))
        .env("FASTPROOF_ADMIN_TOKENS_FILE", &tokens)
        .env("FASTPROOF_LOG_FORMAT", "text")
        .env("RUST_LOG", "debug")
        .stdout(Stdio::from(log.try_clone().unwrap()))
        .stderr(Stdio::from(log))
        .spawn()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !matches!(http.get(format!("http://127.0.0.1:{ops}/readyz")).send().await, Ok(r) if r.status().is_success())
    {
        assert!(Instant::now() < deadline, "the server did not start");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let post = |path: String, body: Option<Value>| {
        let mut rb = http.post(format!("{base}{path}")).bearer_auth(ADMIN_TOKEN);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        rb.send()
    };
    let r = post(
        "/api/v1/projects".into(),
        Some(json!({"key": "logs", "name": "logs"})),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let r = post(
        "/api/v1/projects/logs/launches".into(),
        Some(json!({"name": "l"})),
    )
    .await
    .unwrap();
    let lid = r.json::<Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = http
        .post(format!(
            "{base}/api/v1/projects/logs/launches/{lid}/attachments"
        ))
        .bearer_auth(ADMIN_TOKEN)
        .header("content-type", "video/mp4")
        .body(content())
        .send()
        .await
        .unwrap();
    let sha = r.json::<Value>().await.unwrap()["sha256"]
        .as_str()
        .unwrap()
        .to_string();
    let r = post(
        format!("/api/v1/projects/logs/attachments/{sha}/media-url"),
        None,
    )
    .await
    .unwrap();
    let url = r.json::<Value>().await.unwrap()["url"]
        .as_str()
        .unwrap()
        .to_string();
    let token = url["/api/v1/media/".len()..].to_string();
    let payload = token.split('.').next().unwrap().to_string();
    let ok = http
        .get(format!("{base}{url}?x=secret-query"))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let bad = http.get(format!("{base}{url}x")).send().await.unwrap();
    assert_eq!(bad.status(), StatusCode::NOT_FOUND);
    let blob = dir
        .path()
        .join("blobs/projects/logs/attachments")
        .join(&sha[..2])
        .join(&sha);
    std::fs::remove_file(&blob).unwrap();
    let gone = http.get(format!("{base}{url}")).send().await.unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    let gone_head = http.head(format!("{base}{url}")).send().await.unwrap();
    assert_eq!(gone_head.status(), StatusCode::NOT_FOUND);
    tokio::time::sleep(Duration::from_millis(500)).await;
    child.kill().unwrap();
    let _ = child.wait();
    let text = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        text.contains("attachment blob is missing"),
        "no event inside the request:\n{text}"
    );
    assert!(
        text.contains("/api/v1/media/_"),
        "the redacted path is logged:\n{text}"
    );
    assert!(!text.contains(&token), "the token is in the log");
    assert!(!text.contains(&payload), "the token payload is in the log");
    assert!(!text.contains("secret-query"));
    assert!(!text.contains("/api/v1/media/garbage"));
}

async fn project_id(db: &TestDb, key: &str) -> uuid::Uuid {
    sqlx::query_scalar("select id from projects where key = $1")
        .bind(key)
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

async fn db_secret(db: &TestDb) -> Vec<u8> {
    sqlx::query_scalar("select secret from stream_keys where name = 'media-url'")
        .fetch_one(&db.pool)
        .await
        .unwrap()
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[tokio::test]
async fn media_url_by_attachment_id() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(f) = start(&db, false, &[]).await else {
        return;
    };
    let url = f.media_url_for(&f.key, &f.id, ADMIN_TOKEN).await;
    let r = f.media(Method::GET, &url, &[]).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.bytes().await.unwrap().as_ref(), f.body.as_slice());

    let other = format!("o-{:012x}", rand::random::<u64>() >> 16);
    f.s.project(&other).await;
    let lo = f.s.launch(&other, None).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (id_o, _) = attach(
        &f.s,
        &other,
        &lo,
        b"in the other project".to_vec(),
        "text/plain",
    )
    .await;
    for ident in [
        id_o.as_str(),
        "00000000-0000-4000-8000-000000000000",
        "not-an-id",
    ] {
        let (st, _) =
            f.s.call(
                Method::POST,
                &format!("/api/v1/projects/{}/attachments/{ident}/media-url", f.key),
                Some(ADMIN_TOKEN),
                None,
            )
            .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{ident}");
    }
    let only_other = f.s.token("only-o", "viewer", &[&other]).await;
    let (st, _) =
        f.s.call(
            Method::POST,
            &format!("/api/v1/projects/{}/attachments/{}/media-url", f.key, f.id),
            Some(&only_other),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn tokens_need_the_outside_secret_and_a_sane_expiry() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(f) = start(&db, false, &[]).await else {
        return;
    };
    let pid = project_id(&db, &f.key).await;
    let secret = db_secret(&db).await;
    let mint = |outside: &[u8], exp: i64| {
        let t =
            fastproof_server::api::media::mint(&secret, outside, pid, &f.sha, exp, "t", "admin")
                .unwrap();
        format!("/api/v1/media/{t}")
    };
    for guess in [&b""[..], &secret[..], b"another-secret-0123456789"] {
        let r = f
            .media(Method::GET, &mint(guess, now_ms() + 60_000), &[])
            .await;
        assert_eq!(
            r.status(),
            StatusCode::NOT_FOUND,
            "forged with the database secret alone"
        );
    }
    let ok = f
        .media(
            Method::GET,
            &mint(OUTSIDE_KEY.as_bytes(), now_ms() + 60_000),
            &[],
        )
        .await;
    assert_eq!(ok.status(), StatusCode::OK, "the harness forges correctly");
    let ok = f
        .media(
            Method::GET,
            &mint(OUTSIDE_KEY.as_bytes(), now_ms() + 3_500_000),
            &[],
        )
        .await;
    assert_eq!(ok.status(), StatusCode::OK, "within the longest lifetime");
    for far in [3_700_000, 86_400_000, i64::MAX / 2] {
        let r = f
            .media(
                Method::GET,
                &mint(OUTSIDE_KEY.as_bytes(), now_ms().saturating_add(far)),
                &[],
            )
            .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "expiry {far} ms ahead");
    }
}

#[tokio::test]
async fn keys_rotate_without_a_restart() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let Some(f) = start(&db, false, &[("FASTPROOF_MEDIA_KEY_RELOAD_SECONDS", "1")]).await else {
        return;
    };
    let kid = |u: &str| {
        u["/api/v1/media/".len()..]
            .split('.')
            .next()
            .unwrap()
            .to_string()
    };
    let rotate = || async {
        let mut tx = db.pool.begin().await.unwrap();
        sqlx::query("delete from stream_keys where name = 'media-url-previous'")
            .execute(&mut *tx)
            .await
            .unwrap();
        sqlx::query("update stream_keys set name = 'media-url-previous' where name = 'media-url'")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    };
    let first = f.media_url().await;
    assert_eq!(
        f.media(Method::GET, &first, &[]).await.status(),
        StatusCode::OK
    );
    rotate().await;
    let deadline = Instant::now() + Duration::from_secs(20);
    let second = loop {
        let u = f.media_url().await;
        if kid(&u) != kid(&first) {
            break u;
        }
        assert!(Instant::now() < deadline, "the new key was not picked up");
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    assert_eq!(
        f.media(Method::GET, &first, &[]).await.status(),
        StatusCode::OK,
        "the previous key still verifies"
    );
    assert_eq!(
        f.media(Method::GET, &second, &[]).await.status(),
        StatusCode::OK
    );
    rotate().await;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let st = f.media(Method::GET, &first, &[]).await.status();
        if st == StatusCode::NOT_FOUND {
            break;
        }
        assert!(Instant::now() < deadline, "the dropped key still verifies");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert_eq!(
        f.media(Method::GET, &second, &[]).await.status(),
        StatusCode::OK,
        "the key it was signed with is now the previous one"
    );
}

#[tokio::test]
async fn a_changed_blob_store_is_refused_unless_declared() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut f = start(&db, false, &[]).await.unwrap();
    let cfg = (*f.s.running.as_ref().unwrap().state.cfg).clone();
    let recorded: String = sqlx::query_scalar("select identity from blob_identity")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(recorded.starts_with("fs:"), "{recorded}");
    f.s.stop().await;

    let again = fastproof_server::start(cfg.clone(), Some(common::prometheus()))
        .await
        .unwrap();
    again.shutdown().await;

    let elsewhere = tempfile::tempdir().unwrap();
    let mut changed = cfg.clone();
    changed.blob = fastproof_blob::BlobConfig::Fs {
        dir: elsewhere.path().join("empty"),
    };
    let err = match fastproof_server::start(changed.clone(), Some(common::prometheus())).await {
        Ok(_) => panic!("started on another blob store with attachments stored"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("FASTPROOF_BLOB_BACKEND_MIGRATED"), "{err}");
    let still: String = sqlx::query_scalar("select identity from blob_identity")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(still, recorded);

    sqlx::query("delete from blob_identity")
        .execute(&db.pool)
        .await
        .unwrap();
    let err = match fastproof_server::start(changed.clone(), Some(common::prometheus())).await {
        Ok(_) => panic!("started on an empty store without a record"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("empty store"), "{err}");
    let same = fastproof_server::start(cfg.clone(), Some(common::prometheus()))
        .await
        .unwrap();
    same.shutdown().await;
    let rec: String = sqlx::query_scalar("select identity from blob_identity")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        rec, recorded,
        "an existing store is recorded after the check"
    );

    changed.blob_migrated = true;
    let moved = fastproof_server::start(changed.clone(), Some(common::prometheus()))
        .await
        .unwrap();
    moved.shutdown().await;
    let rec: String = sqlx::query_scalar("select identity from blob_identity")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_ne!(rec, recorded);
    assert!(rec.contains("empty"));
    changed.blob_migrated = false;
    let next = fastproof_server::start(changed, Some(common::prometheus()))
        .await
        .unwrap();
    next.shutdown().await;
}
