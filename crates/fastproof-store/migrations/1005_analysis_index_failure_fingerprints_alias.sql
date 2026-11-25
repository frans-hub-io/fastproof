-- no-transaction
create index concurrently if not exists failure_fingerprints_alias
    on failure_fingerprints (project_id, alias_of) where alias_of is not null;
