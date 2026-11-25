alter table failure_fingerprints add column if not exists state text not null default 'active';
alter table failure_fingerprints add column if not exists state_at timestamptz;
alter table failure_fingerprints add column if not exists alias_of text;
alter table failure_fingerprints add column if not exists resolved_at timestamptz;
alter table failure_fingerprints add column if not exists regressed_count integer not null default 0;
alter table failure_fingerprints add column if not exists first_commit text;
alter table failure_fingerprints add column if not exists last_good_commit text;
do $$ begin
  if not exists (select 1 from pg_constraint where conname = 'failure_fingerprints_state_check') then
    alter table failure_fingerprints add constraint failure_fingerprints_state_check
      check (state in ('new', 'active', 'resolved', 'regressed', 'ignored')) not valid;
  end if;
end $$;
alter table failure_fingerprints validate constraint failure_fingerprints_state_check;
create table if not exists fingerprint_branches (
    project_id uuid not null references projects (id) on delete cascade,
    fingerprint text not null,
    branch text not null,
    first_seen_at timestamptz not null,
    first_launch_id uuid,
    last_seen_at timestamptz not null,
    last_launch_id uuid,
    occurrences bigint not null default 0,
    primary key (project_id, fingerprint, branch)
);
create index if not exists fingerprint_branches_branch on fingerprint_branches (project_id, branch, last_seen_at desc);
