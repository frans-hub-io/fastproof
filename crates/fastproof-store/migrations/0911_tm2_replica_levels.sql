create table if not exists replica_levels (
    instance text primary key,
    tm_level integer not null,
    version text not null,
    started_at timestamptz not null default now(),
    seen_at timestamptz not null default now()
);
create index if not exists replica_levels_seen on replica_levels (seen_at);
