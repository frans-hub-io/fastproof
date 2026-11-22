alter table run_items add column if not exists case_name text;

alter table run_step_results add column if not exists execution_no integer not null default 1;

update run_step_results s set execution_no = ri.execution_no
from run_items ri
where ri.id = s.run_item_id and ri.execution_no <> 1 and s.execution_no = 1;

do $$
begin
    if not exists (
        select 1 from pg_constraint c
        join pg_attribute a on a.attrelid = c.conrelid and a.attnum = any (c.conkey)
        where c.conrelid = 'run_step_results'::regclass and c.contype = 'p' and a.attname = 'execution_no'
    ) then
        alter table run_step_results drop constraint if exists run_step_results_pkey;
        alter table run_step_results add constraint run_step_results_pkey primary key (run_item_id, execution_no, step_path);
    end if;
end
$$;

create table if not exists run_item_executions (
    run_item_id uuid not null references run_items (id) on delete cascade,
    execution_no integer not null,
    status text not null,
    case_version integer not null,
    comment text not null default '',
    executed_by text,
    started_at timestamptz,
    finished_at timestamptz,
    defect jsonb,
    rerun_by text not null,
    rerun_at timestamptz not null default now(),
    constraint run_item_executions_status_check check (status in ('untested', 'in_progress', 'passed', 'failed', 'blocked', 'skipped')),
    primary key (run_item_id, execution_no)
);

create index if not exists launches_test_plan on launches (test_plan_id, number) where test_plan_id is not null;
