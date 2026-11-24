alter table run_items add column if not exists iterations integer not null default 0;
create table if not exists run_iterations (
    run_item_id uuid not null references run_items (id) on delete cascade,
    execution_no integer not null,
    row_id text not null,
    position integer not null,
    param_values jsonb not null,
    series_id uuid not null,
    status text not null default 'untested',
    comment text not null default '',
    result_id uuid,
    executed_by text,
    started_at timestamptz,
    finished_at timestamptz,
    updated_at timestamptz not null default now(),
    constraint run_iterations_status_check
        check (status in ('untested', 'in_progress', 'passed', 'failed', 'blocked', 'skipped')),
    primary key (run_item_id, execution_no, row_id)
);
create table if not exists run_iteration_step_results (
    run_item_id uuid not null,
    execution_no integer not null,
    row_id text not null,
    step_path text not null,
    status text not null,
    actual text not null default '',
    comment text not null default '',
    attachment_ids uuid[] not null default '{}',
    updated_by text not null,
    updated_at timestamptz not null default now(),
    constraint run_iteration_step_results_status_check
        check (status in ('untested', 'passed', 'failed', 'blocked', 'skipped')),
    primary key (run_item_id, execution_no, row_id, step_path),
    foreign key (run_item_id, execution_no, row_id)
        references run_iterations (run_item_id, execution_no, row_id) on delete cascade
);
create index if not exists run_iteration_step_results_attachments
    on run_iteration_step_results using gin (attachment_ids);
