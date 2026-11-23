//! A guard against a silent change of the blob store: the identity of the store the attachments
//! were written to is recorded in the database, and a server configured for another store refuses
//! to start while attachments exist, unless the operator states that the data was moved.

use anyhow::bail;
use fastproof_blob::{BlobConfig, BlobKey, BlobStore};
use fastproof_store::Store;

/// `fs:<directory>` or `s3:<endpoint>/<bucket>`.
pub fn identity(c: &BlobConfig) -> String {
    match c {
        BlobConfig::Fs { dir } => format!("fs:{}", dir.display()),
        BlobConfig::S3(s) => format!(
            "s3:{}/{}",
            s.endpoint.as_deref().unwrap_or("aws").trim_end_matches('/'),
            s.bucket
        ),
    }
}

fn refusal(configured: &str, recorded: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "the configured blob storage ({configured}) is not the one the attachments were stored in ({recorded}). \
         Restore the previous blob settings (FASTPROOF_BLOB_BACKEND and its directory or S3 settings), or, once the data has \
         been copied to the new store, start once with FASTPROOF_BLOB_BACKEND_MIGRATED=true (Helm: server.blobBackendMigrated)"
    )
}

/// Checks the configured store against the recorded identity and records it when it is new.
pub async fn check(
    store: &Store,
    blobs: &BlobStore,
    cfg: &BlobConfig,
    migrated: bool,
) -> anyhow::Result<()> {
    let id = identity(cfg);
    match store.blob_identity().await? {
        Some(r) if r == id => Ok(()),
        Some(r) => {
            if !migrated && !store.recent_blob_keys(1).await?.is_empty() {
                return Err(refusal(&id, &r));
            }
            tracing::warn!(from = %r, to = %id, "the blob store identity changed");
            store.set_blob_identity(&id).await
        }
        None => {
            let keys = store.recent_blob_keys(5).await?;
            if !keys.is_empty() && !migrated {
                let mut present = false;
                for k in &keys {
                    let Ok(k) = BlobKey::parse(k) else { continue };
                    match blobs.head(&k).await {
                        Ok(None) => {}
                        Ok(Some(_)) | Err(_) => {
                            present = true;
                            break;
                        }
                    }
                }
                if !present {
                    bail!(
                        "{}: none of the newest attachments exist in the configured blob storage ({id}); \
                         refusing to start with an empty store. Check the blob settings, or start once with \
                         FASTPROOF_BLOB_BACKEND_MIGRATED=true if the storage was changed on purpose",
                        "attachments are stored but the blob store looks new"
                    );
                }
            }
            store.set_blob_identity(&id).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn identities() {
        assert_eq!(
            identity(&BlobConfig::Fs {
                dir: PathBuf::from("/data/blobs")
            }),
            "fs:/data/blobs"
        );
        let c = BlobConfig::S3(fastproof_blob::S3Config {
            endpoint: Some("http://garage:3900/".into()),
            region: "r".into(),
            bucket: "b".into(),
            path_style: true,
            ca_file: None,
            access_key: None,
            secret_key: None,
            public_endpoint: None,
        });
        assert_eq!(identity(&c), "s3:http://garage:3900/b");
    }
}
