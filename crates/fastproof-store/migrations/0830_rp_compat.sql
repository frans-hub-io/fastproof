create sequence if not exists rp_item_num_seq;

create table if not exists rp_launches (
    project_id uuid not null references projects (id) on delete cascade,
    uuid text not null,
    launch_id uuid not null references launches (id) on delete cascade,
    creator text not null,
    item_count bigint not null default 0,
    log_seq bigint not null default 0,
    last_call_at timestamptz not null default now(),
    finish_requested_at timestamptz,
    finish_status text,
    finish_force boolean not null default false,
    hold boolean not null default false,
    claimed_until timestamptz,
    done_at timestamptz,
    created_at timestamptz not null default now(),
    primary key (project_id, uuid)
);
create index if not exists rp_launches_launch on rp_launches (launch_id);
create index if not exists rp_launches_idle on rp_launches (last_call_at) where finish_requested_at is null and done_at is null;
create index if not exists rp_launches_finishing on rp_launches (finish_requested_at) where finish_requested_at is not null and done_at is null;
create index if not exists rp_launches_done on rp_launches (done_at) where done_at is not null;

create table if not exists rp_items (
    project_id uuid not null references projects (id) on delete cascade,
    uuid text not null,
    launch_id uuid not null references launches (id) on delete cascade,
    rp_launch text not null,
    num_id bigint not null default nextval('rp_item_num_seq'),
    parent_uuid text,
    role text not null,
    depth integer not null default 0,
    owner_uuid text,
    log_key text not null,
    log_step text,
    seq_uuid text,
    error_uuid text,
    suite_path jsonb not null default '[]',
    item_type text not null,
    has_stats boolean not null default true,
    name text not null,
    code_ref text,
    test_case_id text,
    description text,
    params jsonb not null default '[]',
    attrs jsonb not null default '[]',
    start_ms bigint not null,
    stop_ms bigint,
    status text,
    issue jsonb,
    attachments jsonb not null default '[]',
    error jsonb,
    log_seq bigint not null default 0,
    retry boolean not null default false,
    retry_of text,
    running_sent boolean not null default false,
    state text not null default 'started',
    settle_at timestamptz,
    emitted_at timestamptz,
    triage_by text,
    created_by text not null,
    created_at timestamptz not null default now(),
    primary key (project_id, uuid),
    constraint rp_items_role_check check (role in ('result', 'container', 'step', 'fixture', 'pending')),
    constraint rp_items_state_check check (state in ('pending', 'started', 'finished', 'emitting', 'emitted'))
);
create unique index if not exists rp_items_num on rp_items (num_id);
create index if not exists rp_items_launch on rp_items (launch_id);
create index if not exists rp_items_owner on rp_items (project_id, owner_uuid) where owner_uuid is not null;
create index if not exists rp_items_open on rp_items (launch_id) where state in ('pending', 'started', 'finished', 'emitting') and role in ('result', 'fixture', 'pending');
create index if not exists rp_items_settle on rp_items (settle_at) where state in ('finished', 'emitting') and role in ('result', 'fixture');
create index if not exists rp_items_pending on rp_items (project_id, parent_uuid) where state = 'pending';
create index if not exists rp_items_triage on rp_items (emitted_at) where triage_by is not null and state = 'emitted';
