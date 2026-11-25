alter table cases add column if not exists last_status text;
alter table cases add column if not exists last_status_at timestamptz;
alter table cases add column if not exists last_source text;
alter table cases add column if not exists last_launch_id uuid;
alter table cases add column if not exists last_result_id uuid;
do $$ begin
  if not exists (select 1 from pg_constraint where conname = 'cases_last_source_check') then
    alter table cases add constraint cases_last_source_check check (last_source in ('manual', 'automated')) not valid;
  end if;
end $$;
alter table cases validate constraint cases_last_source_check;
