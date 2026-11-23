create table if not exists blob_identity (
    id boolean primary key default true check (id),
    identity text not null,
    recorded_at timestamptz not null default now()
);
