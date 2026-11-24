alter table cases add column if not exists state text;
alter table cases add column if not exists approved_version integer;
alter table cases add column if not exists review_state text;
do $$ begin
  if not exists (select 1 from pg_constraint where conname = 'cases_review_state_check') then
    alter table cases add constraint cases_review_state_check
      check (review_state in ('pending', 'changes_requested', 'approved')) not valid;
  end if;
end $$;
alter table cases validate constraint cases_review_state_check;
create table if not exists review_votes (
    project_id uuid not null,
    kind text not null,
    object_id uuid not null,
    version integer not null,
    reviewer text not null,
    decision text not null,
    comment text not null default '',
    created_at timestamptz not null default now(),
    constraint review_votes_kind_check check (kind in ('case', 'sharedStep')),
    constraint review_votes_decision_check check (decision in ('approve', 'request_changes')),
    primary key (kind, object_id, version, reviewer)
);
create index if not exists review_votes_reviewer on review_votes (project_id, reviewer, created_at desc);
