set local lock_timeout = '5s';

do $$
begin
    if exists (select 1 from results limit 1) then
        create index if not exists results_launch_name_key on only results (launch_id, left(full_name, 600));
        create index if not exists results_message_tsv on only results
            using gin (to_tsvector('simple', left(coalesce(message, ''), 65536)));
    else
        create index if not exists results_launch_name_key on results (launch_id, left(full_name, 600));
        create index if not exists results_message_tsv on results
            using gin (to_tsvector('simple', left(coalesce(message, ''), 65536)));
        drop index if exists results_launch_name;
        drop index if exists results_message_fts;
    end if;
end
$$;

create unique index if not exists case_folders_sibling_key
    on case_folders (project_id, coalesce(parent_id, '00000000-0000-0000-0000-000000000000'::uuid), md5(lower(name)));
drop index if exists case_folders_sibling_name;
alter index case_folders_sibling_key rename to case_folders_sibling_name;
