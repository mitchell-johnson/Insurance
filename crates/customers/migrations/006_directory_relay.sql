-- Relationship changes waiting to be told to the channels directory, which
-- decides who sees which policy in the app. Removed once accepted.
create table directory_relay (
    id bigserial primary key,
    party_id text not null references parties (id),
    command jsonb not null,
    recorded_at timestamptz not null,
    attempts integer not null default 0,
    last_error text null
);
