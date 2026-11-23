create table if not exists tracker_issues (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    tracker_id text not null,
    tracker_type text not null,
    issue_key text,
    remote_id text,
    url text,
    title text not null default '',
    state text not null default '',
    category text not null default 'creating',
    request_id uuid,
    created_by text not null,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    closed_at timestamptz,
    synced_at timestamptz,
    next_sync_at timestamptz,
    sync_attempts integer not null default 0,
    sync_error text,
    lease_until timestamptz,
    etag text,
    unmute_suggested boolean not null default false,
    constraint tracker_issues_category_check check (category in ('creating', 'open', 'closed')),
    constraint tracker_issues_key_unique unique (project_id, tracker_id, issue_key),
    constraint tracker_issues_request_unique unique (project_id, request_id)
);
create index if not exists tracker_issues_due on tracker_issues (next_sync_at) where next_sync_at is not null;
create index if not exists tracker_issues_project on tracker_issues (project_id, created_at, id);
create index if not exists tracker_issues_url on tracker_issues (project_id, url);

create table if not exists tracker_issue_links (
    id uuid primary key,
    issue_id uuid not null references tracker_issues (id) on delete cascade,
    project_id uuid not null references projects (id) on delete cascade,
    launch_id uuid,
    result_id uuid,
    fingerprint text,
    series_ids uuid[] not null default '{}',
    created_by text not null,
    created_at timestamptz not null default now()
);
create index if not exists tracker_issue_links_issue on tracker_issue_links (issue_id);
create index if not exists tracker_issue_links_launch on tracker_issue_links (launch_id);
create index if not exists tracker_issue_links_result on tracker_issue_links (project_id, result_id) where result_id is not null;
