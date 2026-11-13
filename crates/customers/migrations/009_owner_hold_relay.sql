-- A person's own spending hold, told to the core policy by policy: placed,
-- ended, or moved when ownership changes while it stands. Removed once a
-- partition holding the policy accepts it.
create table owner_hold_relay (
    id bigserial primary key,
    party_id text not null references parties (id),
    policy text not null,
    held boolean not null,
    reference text not null,
    due_on date not null,
    recorded_at timestamptz not null,
    attempts integer not null default 0,
    last_error text null
);
