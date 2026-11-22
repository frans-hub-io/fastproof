-- no-transaction
create index concurrently if not exists cases_folder_id on cases (folder_id) where folder_id is not null;
