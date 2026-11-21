//! Streaming and big-data mode of FastProof (`31-streaming.md`): Kafka ingest through rskafka
//! with consumer groups managed in PostgreSQL, and the ClickHouse and OpenSearch sinks and
//! search backends over plain HTTP.

pub mod clickhouse;
pub mod config;
pub mod consumer;
pub mod groups;
pub mod http;
pub mod kafka;
pub mod messages;
pub mod opensearch;
pub mod sink;

pub use config::{ClickHouseConfig, KafkaConfig, OpenSearchConfig, SinkTuning, Vars};
pub use kafka::Kafka;
