-- Retracted deaths are told to claims too, in the order things happened.
alter table death_relay add column retract boolean not null default false;
alter table death_relay add column id bigserial;
alter table death_relay drop constraint death_relay_pkey;
alter table death_relay add primary key (party_id, date_of_death, retract);
