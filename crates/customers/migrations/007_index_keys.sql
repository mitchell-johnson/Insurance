-- Blind indexes carry the key they were made under, so the index key can be
-- changed: entries under the next key are built beside the current ones,
-- the next key is promoted, and entries under a retired key are removed.
-- Rows made before this have key_id '' (made under the key in use then).
alter table blind_index add column key_id text not null default '';
create index blind_index_key on blind_index (key_id);

-- Which parties have been indexed under which key, so a rebuild knows who
-- is left even for a party with nothing to index.
create table index_state (
    party_id text not null references parties (id),
    key_id text not null,
    primary key (party_id, key_id)
);
create index index_state_key on index_state (key_id);
