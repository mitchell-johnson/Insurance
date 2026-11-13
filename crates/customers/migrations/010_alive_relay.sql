-- Identity checks that saw the person alive, waiting to be told to claims
-- (ConfirmAlive on every partition) to complete any open proof of life.
-- Removed once every partition accepts.
create table alive_relay (
    id bigserial primary key,
    party_id text not null references parties (id),
    source text not null,
    evidence_ref text not null,
    recorded_at timestamptz not null,
    attempts integer not null default 0,
    last_error text null
);
