-- A person's request to end their own spending hold, waiting out the
-- cooling-off period. Only the time is kept outside the sealed events, so
-- the sweep can end holds without opening anyone's record.
create table spending_hold_removals (
    party_id text primary key references parties (id),
    requested_at timestamptz not null
);
