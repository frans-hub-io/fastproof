set local lock_timeout = '5s';

alter table results
    add column if not exists label_values jsonb,
    add column if not exists suite_tree text[],
    add column if not exists suite_titled boolean;

create or replace function fastproof_label_values(labels jsonb) returns jsonb
language sql immutable parallel safe as $$
    select coalesce(jsonb_object_agg(n, v), '{}'::jsonb)
    from (
        select e->>'name' as n, jsonb_agg(coalesce(e->'value', '""'::jsonb) order by o) as v
        from jsonb_array_elements(case when jsonb_typeof(labels) = 'array' then labels else '[]'::jsonb end)
            with ordinality as x(e, o)
        where jsonb_typeof(e->'name') = 'string'
        group by 1
    ) s
$$;

create table if not exists explorer_gen (
    launch_id uuid primary key references launches (id) on delete cascade,
    gen bigint not null default 1
);

create table if not exists explorer_cache (
    launch_id uuid not null references launches (id) on delete cascade,
    key text not null,
    gen bigint not null,
    payload jsonb not null,
    created_at timestamptz not null default now(),
    primary key (launch_id, key)
);
create index if not exists explorer_cache_created on explorer_cache (created_at);

create or replace function fastproof_explorer_touch() returns trigger
language plpgsql as $$
declare
    l uuid;
    k text;
begin
    if tg_op = 'DELETE' then
        l := old.launch_id;
    else
        l := new.launch_id;
    end if;
    k := 'fastproof.explorer_gen_' || replace(l::text, '-', '');
    if current_setting(k, true) is distinct from 'done' then
        perform set_config(k, 'done', true);
        insert into explorer_gen (launch_id)
        select l where exists (select 1 from launches where id = l)
        on conflict (launch_id) do update set gen = explorer_gen.gen + 1;
    end if;
    return null;
end
$$;

create or replace function fastproof_results_keys() returns trigger
language plpgsql as $$
begin
    if current_setting('fastproof.explorer_keys', true) = 'app' then
        return new;
    end if;
    if new.labels is distinct from old.labels then
        new.label_values := fastproof_label_values(new.labels);
    end if;
    new.suite_tree := null;
    new.suite_titled := null;
    return new;
end
$$;

drop trigger if exists results_explorer_gen on results;
create constraint trigger results_explorer_gen after insert or update or delete on results
    deferrable initially deferred for each row execute function fastproof_explorer_touch();
drop trigger if exists results_explorer_keys on results;
create trigger results_explorer_keys before update on results for each row
    when (old.labels is distinct from new.labels or old.suite_path is distinct from new.suite_path
        or old.full_name is distinct from new.full_name)
    execute function fastproof_results_keys();
