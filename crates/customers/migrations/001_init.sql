-- The customer master's schema. Applied once, in one transaction, by the
-- service at start (see store.rs). Never edit a migration that has shipped:
-- add the next one.

-- One row per party: the public facts the service needs to find, lock and
-- sweep a party. Nothing personal.
create table parties (
    id text primary key,
    kind text not null check (kind in ('person', 'organisation')),
    lifecycle text not null,
    group_id text not null,
    -- The last sequence number in the party's stream, for optimistic
    -- concurrency, and the last link of its hash chain.
    version bigint not null,
    head_hash bytea not null,
    merged_into text null references parties (id),
    holds integer not null default 0,
    created_at timestamptz not null,
    updated_at timestamptz not null,
    shredded_at timestamptz null
);
create index parties_merged_into on parties (merged_into) where merged_into is not null;
create index parties_sweep on parties (updated_at) where shredded_at is null and merged_into is null and holds = 0;

-- The party's data key, wrapped by the key-encryption key in Key Vault.
-- Deleting the row is what erases the party.
create table party_keys (
    party_id text primary key references parties (id),
    key_id text not null,
    wrapped text not null,
    created_at timestamptz not null
);

-- Each party's append-only event stream. The payload is sealed under the
-- party's key; the hash chain covers a commitment to it, not the
-- ciphertext, so it survives erasure.
create table party_events (
    party_id text not null references parties (id),
    seq bigint not null,
    kind text not null,
    recorded_at timestamptz not null,
    caller text not null,
    fields text[] not null,
    commitment bytea not null,
    prev_hash bytea not null,
    hash bytea not null,
    ciphertext bytea null,
    primary key (party_id, seq)
);

-- Blind indexes: keyed hashes of normalised values, for exact lookups
-- without decrypting.
create table blind_index (
    party_id text not null references parties (id),
    kind text not null,
    digest bytea not null,
    primary key (party_id, kind, digest)
);
create index blind_index_lookup on blind_index (kind, digest);

-- Relationships as they stand: IDs, kinds and dates, for lookups by policy,
-- scheme or party. Rebuilt from the events if lost.
create table relationships (
    party_id text not null references parties (id),
    id text not null,
    kind text not null,
    target_kind text not null,
    target_id text not null,
    valid_from date not null,
    valid_to date null,
    recorded_at timestamptz not null,
    attributes jsonb not null,
    primary key (party_id, id)
);
create index relationships_target on relationships (target_kind, target_id);

-- Public events waiting to be published, in order.
create table outbox (
    id bigserial primary key,
    party_id text not null,
    seq bigint not null,
    priority boolean not null,
    body jsonb not null,
    created_at timestamptz not null,
    published_at timestamptz null
);
create index outbox_unpublished on outbox (priority desc, id) where published_at is null;

-- Every read and every decision about access, append only.
create table audit (
    id bigserial primary key,
    at timestamptz not null,
    caller text not null,
    actor text not null,
    purpose text not null,
    party_id text null,
    action text not null,
    fields text[] not null,
    decision text not null,
    policy_version text not null,
    break_glass boolean not null default false
);
create index audit_party on audit (party_id, at);
create index audit_break_glass on audit (at) where break_glass;

-- A repeated request returns the first answer.
create table idempotency (
    caller text not null,
    key text not null,
    request_hash bytea not null,
    response jsonb not null,
    created_at timestamptz not null,
    primary key (caller, key)
);

-- Possible duplicates found when a party was created anyway, for a person
-- to resolve by merging or dismissing.
create table reviews (
    id text primary key,
    party_id text not null references parties (id),
    candidates text[] not null,
    status text not null check (status in ('open', 'merged', 'dismissed')),
    created_at timestamptz not null,
    resolved_at timestamptz null,
    resolved_by text null
);
create index reviews_open on reviews (created_at) where status = 'open';

-- Append-only tables refuse change. The event stream may only have a
-- payload erased.
create function cm_append_only() returns trigger language plpgsql as $$
begin
    raise exception '% is append only', tg_table_name;
end
$$;
create trigger audit_append_only before update or delete on audit
    for each row execute function cm_append_only();

create function cm_events_guard() returns trigger language plpgsql as $$
begin
    if tg_op = 'DELETE' then
        raise exception 'party events are never deleted';
    end if;
    if new.party_id <> old.party_id or new.seq <> old.seq or new.kind <> old.kind
        or new.recorded_at <> old.recorded_at or new.caller <> old.caller or new.fields <> old.fields
        or new.commitment <> old.commitment or new.prev_hash <> old.prev_hash or new.hash <> old.hash
        or new.ciphertext is not null then
        raise exception 'a party event may only have its payload erased';
    end if;
    return new;
end
$$;
create trigger party_events_guard before update or delete on party_events
    for each row execute function cm_events_guard();
