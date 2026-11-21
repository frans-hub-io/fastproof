-- no-transaction
create index concurrently if not exists blob_deletions_prefix on blob_deletions (prefix);
