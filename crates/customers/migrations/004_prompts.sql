-- Reviews are also prompts for a trained person to consider a
-- vulnerability marker, opened by a third party's concern.
alter table reviews add column kind text not null default 'duplicate' check (kind in ('duplicate', 'consider_marker'));
alter table reviews add column reason text null;
