-- Deaths waiting to be told to claims on every core partition. A row is
-- written with the death and removed once every partition has accepted
-- it, or when the death is retracted or the party erased before then.
create table death_relay (
    party_id text not null references parties (id),
    date_of_death date not null,
    recorded_at timestamptz not null,
    attempts integer not null default 0,
    last_error text null,
    primary key (party_id, date_of_death)
);
