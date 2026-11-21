-- no-transaction
create index concurrently if not exists attachments_unlinked on attachments (last_seen_at);
