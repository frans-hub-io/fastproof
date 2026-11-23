//! Topics that disappear while FastProof runs (deleted, or lost with the broker's data): with
//! topic creation on, producers and readers create them again and carry on; offsets beyond the
//! end of the new log read it from the start. Gated on `FASTPROOF_TEST_KAFKA_BROKERS`
//! (a plaintext broker).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use fastproof_stream::kafka::{Outgoing, Signer, TOPIC_RESULTS, is_missing_topic};
use fastproof_stream::{Kafka, KafkaConfig, Vars};

fn config(prefix: &str, create: bool) -> Option<KafkaConfig> {
    let brokers = std::env::var("FASTPROOF_TEST_KAFKA_BROKERS")
        .ok()
        .filter(|v| !v.is_empty())?;
    let vars: Vec<(&str, String)> = vec![
        ("FASTPROOF_KAFKA_BROKERS", brokers),
        ("FASTPROOF_KAFKA_TOPIC_PREFIX", prefix.to_string()),
        ("FASTPROOF_KAFKA_CREATE_TOPICS", create.to_string()),
        ("FASTPROOF_KAFKA_PARTITIONS", "3".into()),
    ];
    let get = move |k: &str| vars.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    Some(KafkaConfig::from_vars(&Vars(&get)).unwrap().unwrap())
}

fn records(tag: &str, n: usize) -> Vec<Outgoing> {
    (0..n)
        .map(|i| Outgoing {
            key: format!("key-{}", i % 7).into_bytes(),
            value: format!("{tag}-{i}").into_bytes(),
        })
        .collect()
}

/// Every record of `topic` from `from` (per partition), and the end offsets.
async fn read_all(
    k: &Kafka,
    topic: &str,
    from: &BTreeMap<i32, i64>,
) -> anyhow::Result<(Vec<String>, BTreeMap<i32, i64>)> {
    let n = k.partition_count(topic).await?;
    let mut values = Vec::new();
    let mut ends = BTreeMap::new();
    for p in 0..n {
        let mut offset = from.get(&p).copied().unwrap_or(0);
        loop {
            let (recs, hwm, start) = k.fetch(topic, p, offset, 1 << 20, 100).await?;
            let recs: Vec<_> = recs.into_iter().filter(|r| r.offset >= start).collect();
            offset = recs.last().map_or(start, |r| r.offset + 1);
            values.extend(
                recs.iter()
                    .map(|r| String::from_utf8(r.record.value.clone().unwrap()).unwrap()),
            );
            if offset >= hwm {
                ends.insert(p, hwm);
                break;
            }
        }
    }
    values.sort();
    Ok((values, ends))
}

async fn delete_and_wait(admin: &Kafka, topic: &str) {
    admin.delete_topic(topic).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match admin.partition_count(topic).await {
            Err(e) if is_missing_topic(&e) => return,
            _ => {}
        }
        assert!(Instant::now() < deadline, "{topic} was not deleted");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

#[tokio::test]
async fn missing_topics_are_created_again_and_offsets_past_the_end_read_from_the_start() {
    let prefix = format!("fptopics{:08x}.", rand::random::<u32>());
    let Some(cfg) = config(&prefix, true) else {
        eprintln!("FASTPROOF_TEST_KAFKA_BROKERS is not set: skipped");
        return;
    };
    let signer = Signer::new(vec![9; 32]);
    let k = Kafka::connect(&cfg, Some(signer.clone())).await.unwrap();
    k.ensure_topics().await.unwrap();
    let admin = Kafka::connect(&config(&prefix, false).unwrap(), None)
        .await
        .unwrap();
    let topic = k.topic(TOPIC_RESULTS);

    k.produce(&topic, records("old", 60)).await.unwrap();
    let (old, committed) = read_all(&k, &topic, &BTreeMap::new()).await.unwrap();
    assert_eq!(old.len(), 60);
    assert!(committed.values().all(|o| *o > 0), "{committed:?}");

    // The topic goes away behind the back of a client that has it cached.
    delete_and_wait(&admin, &topic).await;
    k.produce(&topic, records("new", 30)).await.unwrap();
    assert_eq!(admin.partition_count(&topic).await.unwrap(), 3);

    // Offsets committed against the old log are past the end of the new one: reading resumes
    // at the start, so nothing written since the topic came back is skipped.
    let (new, _) = read_all(&k, &topic, &committed).await.unwrap();
    let want: Vec<String> = sorted((0..30).map(|i| format!("new-{i}")).collect());
    assert_eq!(new, want);
    for r in k.fetch(&topic, 0, 0, 1 << 20, 100).await.unwrap().0 {
        assert!(
            signer.verify(&topic, &r.record),
            "records sent again stay signed"
        );
    }

    // A reader sees the topic missing, and the topic is created again for it.
    delete_and_wait(&admin, &topic).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_missing = false;
    loop {
        match k.fetch(&topic, 1, 5, 1 << 20, 100).await {
            Ok((recs, hwm, start)) => {
                assert_eq!((recs.len(), hwm, start), (0, 0, 0));
                break;
            }
            Err(e) => saw_missing |= is_missing_topic(&e),
        }
        assert!(Instant::now() < deadline, "the reader did not recover");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(saw_missing, "the fetch reported the missing topic");
    k.produce(&topic, records("after", 9)).await.unwrap();
    let (after, _) = read_all(&k, &topic, &BTreeMap::new()).await.unwrap();
    assert_eq!(after.len(), 9);

    // Without topic creation the topic stays missing and the error says so.
    delete_and_wait(&admin, &topic).await;
    let err = admin.partition_count(&topic).await.unwrap_err();
    assert!(is_missing_topic(&err), "{err:#}");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(is_missing_topic(
        &admin.partition_count(&topic).await.unwrap_err()
    ));

    for t in ["ingest.results", "ingest.logs", "ingest.uploads", "events"] {
        let _ = admin.delete_topic(&admin.topic(t)).await;
    }
}
