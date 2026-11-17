-- Why a duplicate review was closed: the words of whoever judged the two
-- records to be different people, kept so a later merge of the same pair
-- can say who judged them apart, when, and why.
alter table reviews add column resolution_reason text null;
create index reviews_dismissed_pairs on reviews (party_id) where status = 'dismissed' and kind = 'duplicate';
