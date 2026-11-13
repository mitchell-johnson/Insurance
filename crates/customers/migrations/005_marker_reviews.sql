-- When each vulnerability marker is due for review. Only the date is kept
-- here, so the sweep can find them without opening every record; the
-- marker itself stays sealed in the party's events.
create table marker_reviews (
    party_id text primary key references parties (id),
    review_on date not null,
    prompted_at timestamptz null
);
create index marker_reviews_due on marker_reviews (review_on) where prompted_at is null;
