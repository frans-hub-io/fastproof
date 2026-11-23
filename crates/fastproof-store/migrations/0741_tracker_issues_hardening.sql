alter table tracker_issues add column if not exists lease_id uuid;
alter table tracker_issues add column if not exists last_attempt_at timestamptz;
alter table tracker_issues drop constraint if exists tracker_issues_category_check;
alter table tracker_issues add constraint tracker_issues_category_check check (category in ('creating', 'open', 'closed', 'failed'));
drop index if exists tracker_issues_url;
create index if not exists tracker_issues_url_md5 on tracker_issues (project_id, md5(url));

create table if not exists tracker_requests (
    id uuid primary key,
    project_id uuid not null references projects (id) on delete cascade,
    kind text not null,
    at timestamptz not null default now(),
    constraint tracker_requests_kind_check check (kind in ('create', 'link'))
);
create index if not exists tracker_requests_recent on tracker_requests (project_id, kind, at);
