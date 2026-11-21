create table if not exists project_secrets (
    project_id uuid not null references projects (id) on delete cascade,
    name text not null,
    key_id text not null,
    nonce bytea not null,
    ciphertext bytea not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    updated_by text,
    primary key (project_id, name)
);
create index if not exists project_secrets_key on project_secrets (key_id);

create table if not exists integration_cursors (
    name text primary key,
    last_event_id bigint not null default 0,
    updated_at timestamptz not null default now()
);

create table if not exists launch_gates (
    launch_id uuid primary key references launches (id) on delete cascade,
    project_id uuid not null references projects (id) on delete cascade,
    state text not null default 'waiting',
    status text,
    report jsonb,
    queued_at timestamptz not null default now(),
    evaluated_at timestamptz,
    constraint launch_gates_state_check check (state in ('waiting', 'done'))
);
create index if not exists launch_gates_waiting on launch_gates (queued_at) where state = 'waiting';

create table if not exists integration_deliveries (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    launch_id uuid,
    channel text not null,
    target_id text not null,
    events text[] not null default '{}',
    payload jsonb not null,
    status text not null default 'pending',
    attempts integer not null default 0,
    max_attempts integer not null default 6,
    next_attempt_at timestamptz not null default now(),
    lease_until timestamptz,
    last_error text,
    response_status integer,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    delivered_at timestamptz,
    constraint integration_deliveries_status_check check (status in ('pending', 'sending', 'delivered', 'failed'))
);
create index if not exists integration_deliveries_due on integration_deliveries (next_attempt_at) where status in ('pending', 'sending');
create index if not exists integration_deliveries_project on integration_deliveries (project_id, id);
