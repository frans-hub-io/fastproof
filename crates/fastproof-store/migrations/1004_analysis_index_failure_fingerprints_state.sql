-- no-transaction
create index concurrently if not exists failure_fingerprints_state
    on failure_fingerprints (project_id, state, last_seen_at desc);
