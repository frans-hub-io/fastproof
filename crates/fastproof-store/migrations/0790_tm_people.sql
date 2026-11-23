create table if not exists tm_people (
    principal text primary key,
    display text,
    groups text[] not null default '{}',
    last_seen timestamptz not null default now()
);
create index if not exists tm_people_last_seen on tm_people (last_seen);
