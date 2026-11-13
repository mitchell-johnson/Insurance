# Traceability: persona tests to built tests

Hand-maintained. Each row says which built tests (paths under `crates/`) carry out a persona test from the matrix, and how far.

- **full**: the built tests check what the matrix row expects.
- **part**: the built tests check some of it; the rest is still to write.
- A matrix row not listed here has no built test yet.

`tools/persona_matrix.py` checks that every ID here is in the matrix and every test named here exists, and reports the gaps in `coverage.md`. When you write a test that carries out a matrix row, add it here.


## POL Policy model and lifecycle

| Matrix | Status | Built tests |
| --- | --- | --- |
| POL-001 | part | `sim/tests/scenarios.rs::issue_bills_the_first_period_and_earns_it_exactly` |
| POL-002 | part | `sim/tests/personas_spine.rs::a_couple_holds_a_single_cover_each_on_one_policy` |
| POL-003 | part | `sim/tests/scenarios.rs::anniversary_indexes_cover_and_re_rates_stepped_premium`<br>`sim/tests/reduce_cover.rs::a_reduction_takes_the_newest_layer_first` |
| POL-004 | part | `sim/tests/edges.rs::old_identical_layers_merge_without_changing_the_cover` |
| POL-005 | part | `sim/tests/edges.rs::a_member_is_identified_once_and_only_as_one_person`<br>`sim/tests/scenarios.rs::group_scheme_bills_in_chunks_and_adjusts_for_a_backdated_leaver` |
| POL-006 | full | `sim/tests/group_cover.rs::a_pool_is_billed_on_its_declared_aggregate` |
| POL-007 | full | `sim/tests/scenarios.rs::unpaid_premium_moves_through_grace_suspension_and_lapse_then_reinstates`<br>`sim/tests/claims.rs::the_cover_check_reads_the_policy_as_it_was_on_the_event_date`<br>`sim/tests/personas_spine.rs::paying_the_arrears_while_suspended_restores_the_standing_at_once` |
| POL-008 | full | `sim/tests/scenarios.rs::unpaid_premium_moves_through_grace_suspension_and_lapse_then_reinstates`<br>`sim/tests/scenarios.rs::reinstatement_window_closes` |
| POL-010 | full | `core/src/bitemporal.rs::backdated_change_is_visible_on_both_timelines`<br>`sim/tests/scenarios.rs::group_scheme_bills_in_chunks_and_adjusts_for_a_backdated_leaver` |
| POL-011 | part | `core/src/bitemporal.rs::correction_from_the_start_supersedes` |
| POL-012 | full | `sim/tests/edges.rs::lifecycle_commands_refuse_a_policy_in_the_wrong_state` |
| POL-013 | full | `underwriting/tests/lifecycle.rs::a_proposal_lapses_or_is_withdrawn_and_leaves_nothing_running` |
| POL-014 | full | `sim/tests/partition_views.rs::scheme_members_cover_follows_the_membership_file` |
| POL-015 | part | `sim/tests/year.rs::the_synthetic_insurer_runs_a_year_with_every_reconciliation_clean` |

## LED Ledger mapping and invariants

| Matrix | Status | Built tests |
| --- | --- | --- |
| LED-001 | full | `ledger/src/lib.rs::ledger_always_nets_to_zero`<br>`ledger/src/lib.rs::units_balance_separately_from_currency` |
| LED-002 | full | `sim/tests/scenarios.rs::commands_are_idempotent_and_only_accepted_from_the_right_module`<br>`api/tests/api.rs::issuing_twice_with_one_key_issues_once` |
| LED-003 | full | `sim/tests/scenarios.rs::a_recorded_log_replays_to_an_identical_state_hash_and_the_chain_verifies`<br>`sim/tests/year.rs::the_same_seed_gives_the_same_ledger_and_a_different_seed_does_not` |
| LED-004 | part | `ledger/src/lib.rs::chain_detects_tampering`<br>`ledger/src/lib.rs::the_chain_catches_reordering_resalting_and_rehashing`<br>`sim/tests/audit.rs::the_audit_names_each_kind_of_divergence` |
| LED-005 | full | `sim/tests/scenarios.rs::issue_bills_the_first_period_and_earns_it_exactly` |
| LED-006 | full | `sim/tests/personas_spine.rs::a_yearly_premium_is_earned_exactly_across_the_leap_day` |
| LED-007 | part | `api/tests/api.rs::a_payment_with_the_policy_number_is_applied_once` |
| LED-008 | full | `sim/tests/investment.rs::an_order_waits_for_its_price_and_executes_once` |
| LED-009 | full | `sim/tests/claims.rs::a_lump_sum_claim_runs_from_notification_to_the_bank`<br>`claims/tests/suite/decisions.rs::an_admission_within_authority_takes_effect_and_is_recorded` |
| LED-010 | full | `sim/tests/finance_feed.rs::posted_lines_rebuild_the_control_totals_of_every_close`<br>`sim/tests/integrity.rs::control_totals_are_the_role_totals_finance_expects`<br>`finance/tests/feed.rs::the_books_equal_the_core_every_day_and_tie_to_the_merkle_root`<br>`finance/tests/feed.rs::a_core_total_that_differs_turns_the_control_check_red` |
| LED-011 | full | `sim/tests/integrity.rs::reconciliation_notices_invoices_that_disagree_with_the_ledger`<br>`sim/tests/integrity.rs::reconciliation_notices_earning_that_disagrees_with_the_ledger`<br>`sim/tests/integrity.rs::reconciliation_notices_a_standing_that_does_not_fit_the_status` |
| LED-012 | full | `sim/tests/personas_spine.rs::two_refunds_of_one_credit_are_applied_in_log_order` |
| LED-013 | part | `finance/tests/close.rs::a_posting_after_sign_off_clears_it`<br>`finance/tests/journals.rs::a_journal_dated_into_an_earlier_period_is_a_prior_period_item` |
| LED-014 | full | `sim/tests/scenarios.rs::health_premium_splits_gst_to_the_tax_account`<br>`finance/tests/edges.rs::a_line_in_the_wrong_statutory_fund_turns_the_fund_check_red` |
| LED-015 | part | `sim/src/cutoff.rs::cutoff_is_midnight_in_auckland` |
| LED-016 | part | `sim/src/cutoff.rs::clock_changes_make_short_and_long_days` |

## PRD Products, rating and quotes

| Matrix | Status | Built tests |
| --- | --- | --- |
| PRD-001 | full | `sim/tests/integrity.rs::a_quote_equals_the_first_bill`<br>`sim/tests/underwriting_door.rs::a_quoted_issue_is_refused_when_the_product_has_changed` |
| PRD-002 | full | `product/tests/products.rs::term_life_exact_premium` |
| PRD-003 | part | `sim/tests/edges.rs::active_configuration_cannot_be_rewritten` |
| PRD-004 | full | `product/tests/products.rs::bad_products_are_rejected`<br>`product/tests/products.rs::each_product_rule_refuses_the_file_that_breaks_it` |
| PRD-005 | part | `sim/tests/scenarios.rs::issue_bills_the_first_period_and_earns_it_exactly` |
| PRD-006 | full | `sim/tests/integrity.rs::a_level_premium_does_not_rise_with_age`<br>`sim/tests/edges.rs::a_level_premium_keeps_each_layers_rating_age_and_such_layers_do_not_merge` |
| PRD-007 | full | `finance/tests/dry_run.rs::a_proposed_table_is_priced_against_the_book_in_force`<br>`finance/tests/dry_run.rs::the_comparison_counts_what_it_cannot_price_and_lists_the_largest_moves` |
| PRD-008 | part | `api/tests/api.rs::a_quote_prices_without_writing_anything` |
| PRD-010 | full | `sim/tests/scenarios.rs::health_premium_splits_gst_to_the_tax_account`<br>`product/tests/products.rs::health_premium_carries_gst` |
| PRD-011 | full | `customers/tests/api.rs::gender_is_recorded_in_the_persons_words_and_never_matched`<br>`reinsurance/tests/premiums.rs::a_life_whose_sex_the_table_does_not_hold_is_rated_at_the_lower_rate`<br>`product/tests/products.rs::a_life_whose_sex_the_table_does_not_hold_is_rated_at_the_lower_rate`<br>`sim/tests/personas_spine.rs::a_life_with_no_recorded_sex_is_issued_at_the_lower_premium` |
| PRD-012 | part | `product/tests/products.rs::rating_refuses_what_it_cannot_price` |

## BIL Billing and earning

| Matrix | Status | Built tests |
| --- | --- | --- |
| BIL-001 | part | `sim/tests/year.rs::the_synthetic_insurer_runs_a_year_with_every_reconciliation_clean`<br>`product/src/schedule.rs::every_frequency_steps_by_its_own_period` |
| BIL-002 | part | `documents/tests/render.rs::a_template_renders_values_lists_and_conditions`<br>`documents/tests/render.rs::money_and_dates_are_written_for_people` |
| BIL-005 | full | `sim/tests/underwriting_door.rs::an_accepted_increase_is_a_layer_from_today_billed_for_the_days_left` |
| BIL-007 | full | `sim/tests/scenarios.rs::money_paid_ahead_waits_in_suspense_and_is_consumed_by_billing` |
| BIL-008 | full | `sim/tests/scenarios.rs::cooling_off_refunds_everything_and_mid_term_refunds_the_unearned_part`<br>`distribution/tests/commission.rs::cancelling_in_cooling_off_reverses_all_commission_from_whoever_received_it` |
| BIL-009 | full | `sim/tests/scenarios.rs::cooling_off_refunds_everything_and_mid_term_refunds_the_unearned_part` |
| BIL-011 | full | `sim/tests/claims.rs::a_waived_premium_is_billed_and_paid_from_claims_expense` |
| BIL-012 | full | `product/src/schedule.rs::leap_day_anniversary` |
| BIL-013 | part | `sim/tests/scenarios.rs::issue_bills_the_first_period_and_earns_it_exactly`<br>`money/src/lib.rs::round_to_money_remainder_is_exact` |
| BIL-014 | part | `sim/tests/personas_spine.rs::a_premium_paid_three_times_leaves_two_as_a_credit_that_can_be_refunded` |
| BIL-015 | part | `sim/tests/scenarios.rs::group_scheme_bills_in_chunks_and_adjusts_for_a_backdated_leaver` |

## COL Collection, cash matching, standing and dunning

| Matrix | Status | Built tests |
| --- | --- | --- |
| COL-001 | full | `payments/tests/runs.rs::a_dishonour_is_presented_once_more_then_the_authority_is_suspended` |
| COL-002 | full | `sim/tests/scenarios.rs::a_dishonour_does_not_reset_the_lapse_clock` |
| COL-003 | part | `sim/tests/partition_views.rs::merged_partition_candidates_decide_as_one_read_model_does` |
| COL-004 | part | `sim/tests/partition_views.rs::merged_partition_candidates_decide_as_one_read_model_does`<br>`payments/tests/recon.rs::a_direct_credit_goes_to_cash_matching_and_stays_open_until_identified` |
| COL-007 | part | `sim/tests/scenarios.rs::unpaid_premium_moves_through_grace_suspension_and_lapse_then_reinstates`<br>`sim/tests/claims.rs::the_cover_check_reads_the_policy_as_it_was_on_the_event_date` |
| COL-008 | full | `sim/tests/scenarios.rs::unpaid_premium_moves_through_grace_suspension_and_lapse_then_reinstates`<br>`sim/tests/edges.rs::a_lapse_reverses_the_levy_on_unpaid_premium` |
| COL-009 | full | `sim/tests/reopen.rs::a_policy_does_not_lapse_before_its_lapse_notice_allows`<br>`documents/tests/delivery.rs::returned_post_makes_the_notice_undeliverable_until_retried`<br>`collections/tests/dunning.rs::an_undeliverable_mandatory_notice_raises_a_task` |
| COL-010 | part | `sim/tests/scenarios.rs::unpaid_premium_moves_through_grace_suspension_and_lapse_then_reinstates`<br>`sim/tests/underwriting_door.rs::a_long_lapse_is_reinstated_only_on_underwritings_decision_and_on_its_terms` |
| COL-011 | full | `sim/tests/scenarios.rs::reinstatement_window_closes` |
| COL-012 | full | `collections/tests/sequence.rs::mandatory_notices_cannot_be_configured_away`<br>`collections/tests/dunning.rs::each_step_is_an_event_with_its_notice_or_task`<br>`collections/tests/dunning.rs::a_late_arrival_gets_the_latest_reminder_and_every_mandatory_notice`<br>`collections/tests/sequence.rs::a_sequence_is_checked_before_it_is_used`<br>`documents/tests/collections_notices.rs::dunning_notices_are_issued_once_and_reported_sent` |
| COL-013 | part | `collections/tests/cases.rs::an_outcome_drives_the_next_step` |
| COL-014 | full | `sim/tests/hardship.rs::a_reduced_premium_is_billed_for_its_days_and_ends_early_without_charging_back`<br>`sim/tests/hardship.rs::a_premium_holiday_bills_nothing_and_one_ended_before_it_starts_never_applies`<br>`collections/tests/cases.rs::hardship_is_applied_for_decided_by_another_and_ended_early`<br>`collections/tests/cases.rs::a_hardship_application_is_checked`<br>`collections/tests/cases.rs::a_holiday_a_decline_and_a_refused_relief` |
| COL-015 | full | `sim/tests/arrangement.rs::an_arrangement_that_is_kept_holds_standing_and_stops_the_lapse`<br>`sim/tests/arrangement.rs::a_broken_arrangement_resumes_the_lapse_clock_from_where_it_stood`<br>`collections/tests/cases.rs::an_arrangement_holds_the_sequence_and_a_break_resumes_the_clock`<br>`collections/tests/cases.rs::an_arrangement_completes_or_is_ended`<br>`collections/tests/cases.rs::an_arrangement_is_checked_and_a_refusal_is_a_task` |
| COL-016 | full | `payments/tests/runs.rs::a_payer_in_hardship_is_talked_to_before_anything_is_presented_again` |
| COL-017 | part | `payments/tests/runs.rs::a_dishonour_is_presented_once_more_then_the_authority_is_suspended` |
| COL-018 | full | `sim/tests/personas_spine.rs::paying_the_arrears_while_suspended_restores_the_standing_at_once`<br>`collections/tests/dunning.rs::paying_stops_the_sequence_and_a_new_spell_starts_it_afresh`<br>`collections/tests/dunning.rs::an_ended_policy_leaves_the_sequence` |
| COL-019 | full | `collections/tests/queue.rs::the_queue_puts_what_is_about_to_lapse_and_matters_most_first`<br>`collections/tests/cases.rs::an_officer_does_not_read_health_in_a_hardship_note`<br>`collections/tests/dunning.rs::a_high_premium_is_called_earlier`<br>`collections/tests/dunning.rs::a_call_left_too_long_goes_to_the_team_lead` |
| COL-024 | part | `payments/tests/incident.rs::failures_in_an_incident_are_not_held_against_the_payers` |
| COL-025 | part | `payments/tests/runs.rs::a_collection_held_up_by_an_outage_goes_next_window_and_is_found`<br>`payments-service/tests/pump.rs::a_bank_that_is_down_or_says_no_leaves_the_money_unpaid_and_known` |
| COL-026 | full | `collections/tests/dunning.rs::a_customer_with_several_policies_gets_one_notice_and_one_task`<br>`collections/tests/dunning.rs::a_notice_already_taken_is_not_added_to` |
| COL-027 | full | `documents/tests/interests.rs::a_lapse_notice_must_be_copied_to_the_lender`<br>`documents/tests/interests.rs::a_suspension_notice_is_copied_once_to_each_party` |

## XFR Policy transfer between groups

| Matrix | Status | Built tests |
| --- | --- | --- |
| XFR-001 | full | `sim/tests/scenarios.rs::a_policy_transfers_between_groups_under_fault_injection_with_nothing_lost` |
| XFR-002 | full | `sim/tests/scenarios.rs::a_rejected_transfer_unfreezes_and_nothing_has_moved` |
| XFR-003 | full | `raft/tests/processes.rs::a_transfer_between_partitions_survives_a_leader_kill` |
| XFR-004 | part | `sim/tests/edges.rs::a_transfer_that_cannot_land_cleanly_does_not_begin` |
| XFR-005 | full | `sim/tests/claims.rs::a_policy_moves_partition_with_its_claim_in_payment` |
| XFR-006 | part | `sim/tests/partition_views.rs::a_partitions_own_view_follows_a_policy_that_moves` |
| XFR-010 | full | `sim/tests/edges.rs::a_schemes_own_rate_table_moves_with_it` |

## AUD Audience scoping, customer app and identity

| Matrix | Status | Built tests |
| --- | --- | --- |
| AUD-001 | part | `readmodel/src/scope.rs::scheme_member_sees_nothing_else_from_the_scheme`<br>`channels/tests/journeys.rs::scheme_members_and_the_employer` |
| AUD-002 | part | `readmodel/src/scope.rs::policyholder_sees_policy_but_not_another_adults_health_facts`<br>`channels/tests/journeys.rs::family_privacy_and_consent` |
| AUD-003 | part | `channels/tests/journeys.rs::family_privacy_and_consent`<br>`channels/tests/feed.rs::lost_access_purges_and_a_gone_row_deletes` |
| AUD-004 | full | `channels/tests/journeys.rs::a_childs_health_information_becomes_their_own` |
| AUD-005 | full | `channels/tests/journeys.rs::a_childs_health_information_becomes_their_own` |
| AUD-006 | full | `channels/tests/journeys.rs::one_person_in_three_roles_sees_each_slice` |
| AUD-008 | part | `channels/tests/journeys.rs::requests_are_asynchronous_and_owned`<br>`channels/tests/journeys.rs::replicas_agree_and_retries_are_safe` |
| AUD-009 | full | `channels/tests/identity.rs::a_lost_phone_is_recovered_by_fresh_verification` |
| AUD-010 | part | `readmodel/src/scope.rs::no_relationship_no_access_and_stale_data_hides_sensitive_rows`<br>`api/tests/api.rs::each_audience_sees_its_own_slice_of_the_policy` |
| AUD-011 | full | `channels/tests/journeys.rs::people_the_policyholder_lets_in`<br>`channels/tests/http.rs::a_relative_is_let_in_to_look_and_no_more` |
| AUD-013 | part | `channels/tests/journeys.rs::people_the_policyholder_lets_in` |
| AUD-014 | full | `documents/tests/channels.rs::sms_carries_a_pointer_and_asks_for_a_report` |
| AUD-015 | part | `channels/tests/feed.rs::snapshot_then_changes` |
| AUD-016 | full | `channels/tests/feed.rs::lost_access_purges_and_a_gone_row_deletes` |
| AUD-017 | part | `raft/tests/store.rs::log_lines_name_a_command_without_its_payload` |
| AUD-018 | part | `documents/tests/pdf.rs::large_print_is_set_at_eighteen_point` |
| AUD-019 | part | `channels/tests/journeys.rs::people_the_policyholder_lets_in` |

## RES Resilience, capacity and scale

| Matrix | Status | Built tests |
| --- | --- | --- |
| RES-001 | part | `sim/tests/year.rs::the_synthetic_insurer_runs_a_year_with_every_reconciliation_clean` |
| RES-002 | part | `sim/tests/scenarios.rs::group_scheme_bills_in_chunks_and_adjusts_for_a_backdated_leaver` |
| RES-004 | part | `raft/tests/cluster.rs::leader_crash_fails_over_and_a_retry_applies_once`<br>`raft/tests/cluster.rs::losing_quorum_stops_writes_and_loses_nothing` |
| RES-005 | part | `workflows/tests/temporal.rs::the_scheduled_close_catches_up_to_today_and_stops` |
| RES-006 | full | `workflows/tests/end_of_day.rs::a_worker_dying_at_any_step_changes_nothing`<br>`workflows/tests/end_of_day.rs::two_workers_on_the_same_day_equal_one` |
| RES-007 | full | `azure/tests/adapters.rs::sealed_segments_link_to_the_chain_and_replay_from_the_cold_tier`<br>`raft/tests/processes.rs::the_journal_is_sealed_once_across_a_leader_kill` |
| RES-011 | part | `raft/tests/cluster.rs::leader_crash_fails_over_and_a_retry_applies_once`<br>`raft/tests/processes.rs::killing_the_leader_process_loses_no_payment_and_doubles_none` |

## UW Underwriting, underwriting operations and delegated authority

| Matrix | Status | Built tests |
| --- | --- | --- |
| UW-001 | full | `underwriting/tests/journey.rs::a_clean_application_is_issued_straight_through`<br>`underwriting/tests/driver.rs::a_clean_application_becomes_a_policy_with_nobody_carrying` |
| UW-002 | full | `underwriting/tests/journey.rs::a_referred_loading_reaches_the_core_as_a_rate_and_a_reference_only`<br>`underwriting/tests/journey.rs::terms_set_by_the_rules_are_offered_and_issued_once_accepted` |
| UW-003 | full | `underwriting/tests/journey.rs::one_decision_on_flex_health_writes_different_terms_to_each_component`<br>`underwriting/tests/casework.rs::decisions_are_held_to_authority_and_declines_take_two_signatures` |
| UW-004 | full | `underwriting/tests/libraries.rs::cedar_policy_enforces_authority_limits`<br>`underwriting/tests/casework.rs::referral_queues_have_one_owner_limits_handovers_and_escalation` |
| UW-005 | full | `underwriting/tests/casework.rs::decisions_are_held_to_authority_and_declines_take_two_signatures` |
| UW-006 | part | `underwriting/tests/journey.rs::a_declined_cover_is_told_in_its_own_notice_and_the_rest_still_issues`<br>`underwriting/tests/libraries.rs::reason_list_is_validated_and_splits_for_the_customer`<br>`underwriting/tests/progress.rs::progress_is_marked_by_version_and_a_decline_is_shown_only_to_the_life`<br>`documents/tests/requested_letters.rs::a_requested_decline_is_issued_once_as_clinical` |
| UW-007 | full | `reinsurance/tests/capacity.rs::a_reservation_sees_cover_already_held_on_the_life` |
| UW-008 | full | `underwriting/tests/casework.rs::nothing_is_offered_above_the_treaty_limit_until_the_reinsurer_has_answered` |
| UW-009 | full | `reinsurance/tests/capacity.rs::two_proposals_on_one_life_see_each_other`<br>`reinsurance/tests/accumulation.rs::arrival_order_does_not_change_the_cessions` |
| UW-010 | full | `underwriting/tests/libraries.rs::a_case_tracks_ownership_escalation_and_priority`<br>`underwriting/tests/casework.rs::referral_queues_have_one_owner_limits_handovers_and_escalation` |
| UW-011 | full | `underwriting/tests/addendum.rs::an_adviser_sees_a_lifes_answers_only_with_that_lifes_consent`<br>`underwriting/tests/intake.rs::an_underwriter_on_the_case_reads_the_disclosure_and_the_reading_is_logged` |
| UW-012 | full | `underwriting/tests/addendum.rs::an_adviser_sees_a_lifes_answers_only_with_that_lifes_consent` |
| UW-013 | part | `channels/tests/http.rs::the_person_insured_signs_their_disclosure` |
| UW-014 | part | `underwriting/tests/lifecycle.rs::a_joint_application_is_settled_life_by_life` |
| UW-015 | full | `underwriting/tests/casework.rs::evidence_that_cannot_be_had_goes_to_a_person_with_the_reason` |
| UW-016 | part | `underwriting/tests/casework.rs::evidence_is_requested_chased_and_then_read_by_a_person` |
| UW-017 | full | `underwriting/tests/intake.rs::a_previous_insurers_history_is_ordered_tracked_and_relied_on` |
| UW-018 | full | `underwriting/tests/driver.rs::interim_cover_is_written_at_the_start_and_ended_when_the_policy_is_issued`<br>`sim/tests/underwriting_door.rs::underwriting_can_end_interim_cover` |
| UW-019 | full | `underwriting/tests/casework.rs::interview_answers_count_only_once_the_life_confirms_them` |
| UW-020 | part | `underwriting/tests/driver.rs::an_accepted_alteration_rewrites_the_cover_on_the_policy_in_force` |
| UW-021 | full | `underwriting/tests/special.rs::a_non_disclosure_review_needs_untouched_seniors_and_two_for_a_remedy`<br>`claims/tests/suite/decisions.rs::a_non_disclosure_review_holds_the_admission`<br>`sim/tests/nondisclosure.rs::an_avoidance_with_refund_undoes_every_premium_since_inception` |
| UW-022 | full | `underwriting/tests/config.rs::question_sets_follow_the_disclosure_regime_of_the_contract_date` |
| UW-023 | full | `underwriting/tests/libraries.rs::backtest_shows_how_a_rule_change_shifts_outcomes`<br>`underwriting/tests/config.rs::a_changed_rule_set_needs_its_backtest_and_its_references` |
| UW-024 | full | `underwriting/tests/special.rs::peer_review_samples_decisions_for_a_senior_who_never_touched_them` |
| UW-025 | full | `underwriting/tests/special.rs::a_delegated_decision_is_bound_only_inside_its_authority` |
| UW-026 | full | `underwriting/tests/special.rs::a_delegated_decision_is_bound_only_inside_its_authority` |
| UW-027 | full | `underwriting/tests/intake.rs::a_sampled_delegated_decision_brings_the_file_in_under_the_lifes_key`<br>`underwriting/tests/special.rs::a_delegated_decision_is_bound_only_inside_its_authority` |
| UW-028 | full | `underwriting/tests/journey.rs::an_offer_can_be_declined_expire_or_be_countered` |
| UW-029 | part | `underwriting/tests/lifecycle.rs::a_capacity_request_carries_the_risk_codes_declared_and_nothing_clinical` |

## CLM Claims and claims operations

| Matrix | Status | Built tests |
| --- | --- | --- |
| CLM-001 | full | `claims/tests/suite/intake.rs::notification_opens_a_claim_with_a_reserve`<br>`claims/tests/suite/intake.rs::a_late_notified_death_prompts_a_register_check`<br>`customers/tests/api.rs::deaths_are_marked_and_retracted`<br>`customers/tests/lifecycle.rs::deaths_are_told_to_claims_on_every_partition` |
| CLM-002 | part | `sim/tests/claims.rs::a_lender_recorded_on_the_policy_is_paid_before_the_claimant`<br>`claims/tests/suite/book.rs::recorded_interests_are_paid_before_the_claimant`<br>`claims/tests/suite/covers.rs::joint_and_linked_covers_count_what_the_others_have_paid` |
| CLM-003 | part | `sim/tests/claims.rs::a_lump_sum_claim_runs_from_notification_to_the_bank`<br>`sim/tests/claims.rs::an_admission_can_end_the_policy_or_take_the_premium_owing`<br>`claims/tests/suite/book.rs::a_payee_is_verified_before_anything_is_paid` |
| CLM-005 | full | `claims/tests/suite/intake.rs::a_death_with_no_certificate_is_registered_and_the_coroner_is_chased` |
| CLM-006 | full | `sim/tests/claims.rs::an_admission_can_end_the_policy_or_take_the_premium_owing`<br>`sim/tests/claims.rs::a_premium_paid_after_the_event_does_not_rewrite_the_event_date` |
| CLM-007 | full | `sim/tests/claims.rs::the_cover_check_reads_the_policy_as_it_was_on_the_event_date`<br>`claims/tests/suite/intake.rs::doubtful_cover_still_registers_and_a_refused_reserve_leaves_nothing` |
| CLM-008 | part | `sim/tests/claims.rs::a_scheme_members_claim_is_checked_identified_and_does_not_end_the_scheme` |
| CLM-009 | part | `claims/tests/suite/decisions.rs::lump_sums_on_one_cover_never_exceed_the_sum_insured`<br>`claims/tests/suite/covers.rs::joint_and_linked_covers_count_what_the_others_have_paid` |
| CLM-010 | full | `claims/tests/suite/intake.rs::health_invoice_goes_straight_through_inside_the_limits`<br>`sim/tests/claims.rs::a_health_invoice_goes_straight_through_to_the_provider` |
| CLM-011 | full | `claims/tests/suite/intake.rs::pre_approval_is_a_hold_not_a_posting` |
| CLM-012 | full | `claims/tests/suite/intake.rs::health_invoice_goes_straight_through_inside_the_limits`<br>`claims/tests/suite/covers.rs::the_yearly_limit_resets_at_the_anniversary_and_the_lifetime_limit_does_not` |
| CLM-013 | full | `claims/tests/suite/intake.rs::pre_approval_is_a_hold_not_a_posting` |
| CLM-014 | full | `sim/tests/claims.rs::a_health_invoice_goes_straight_through_to_the_provider`<br>`sim/tests/claims.rs::a_duplicate_provider_bill_is_recovered_from_the_providers_next_run` |
| CLM-015 | full | `claims/tests/suite/investigate.rs::a_service_billed_twice_is_investigated_quietly_and_recovered_from_the_provider`<br>`sim/tests/claims.rs::a_duplicate_provider_bill_is_recovered_from_the_providers_next_run` |
| CLM-016 | full | `claims/tests/suite/book.rs::a_benefit_run_is_prepared_by_one_person_and_approved_by_another`<br>`claims/tests/suite/book.rs::declared_income_replays_the_periods_already_paid`<br>`claims/tests/suite/covers.rs::a_benefit_starts_only_after_the_waiting_period` |
| CLM-017 | full | `claims/tests/suite/book.rs::declared_income_replays_the_periods_already_paid`<br>`claims/tests/suite/book.rs::other_income_shows_as_its_own_figure_on_each_benefit_line` |
| CLM-018 | full | `claims/tests/suite/book.rs::a_late_income_declaration_does_not_stop_the_benefit` |
| CLM-019 | full | `claims/tests/suite/book.rs::reviews_fall_due_by_date_and_never_stop_the_benefit` |
| CLM-020 | full | `claims/tests/suite/book.rs::a_benefit_ends_closes_and_can_recur` |
| CLM-021 | full | `claims/tests/suite/book.rs::declared_income_replays_the_periods_already_paid`<br>`sim/tests/claims.rs::an_offset_recovers_an_overpayment_across_policies` |
| CLM-023 | full | `claims/tests/suite/intake.rs::escalation_and_identification`<br>`sim/tests/claims.rs::a_scheme_members_claim_is_checked_identified_and_does_not_end_the_scheme` |
| CLM-024 | part | `claims/tests/suite/decisions.rs::admission_follows_the_cover_facts` |
| CLM-025 | full | `sim/tests/personas_lines.rs::a_pilots_claim_is_checked_on_the_automatic_amount_plus_any_underwritten_layer`<br>`sim/tests/claims.rs::a_member_above_the_automatic_limit_is_paid_the_excess_only_where_it_was_underwritten` |
| CLM-026 | part | `claims/tests/suite/views.rs::a_continuation_policy_claim_carries_the_scheme_history`<br>`sim/tests/claims.rs::a_continuation_policy_carries_its_scheme_membership_to_the_claim` |
| CLM-027 | full | `claims/tests/suite/views.rs::a_company_owned_income_claim_pays_the_company_and_is_about_the_life` |
| CLM-028 | part | `claims/tests/suite/book.rs::a_payee_is_verified_before_anything_is_paid` |
| CLM-029 | full | `claims/tests/suite/book.rs::a_death_benefit_goes_to_the_nomination_in_force_on_the_date_of_death`<br>`sim/tests/claims.rs::a_death_benefit_goes_to_whoever_was_nominated_on_the_date_of_death` |
| CLM-030 | full | `claims/tests/suite/decisions.rs::an_exclusion_stops_admission_unless_overridden_with_authority` |
| CLM-031 | full | `claims/tests/suite/decisions.rs::a_closed_claim_reopens_under_independent_review` |
| CLM-032 | full | `claims/tests/suite/decisions.rs::a_reserve_beyond_authority_is_borrowed_and_a_sharp_move_is_reviewed`<br>`claims/tests/suite/decisions.rs::a_large_admission_waits_for_a_second_signature` |
| CLM-033 | full | `claims/tests/suite/intake.rs::a_claim_is_offered_only_to_someone_who_may_work_it`<br>`claims/tests/suite/intake.rs::a_claim_is_never_offered_to_a_relative_in_the_queue` |
| CLM-035 | full | `claims/tests/suite/book.rs::a_returned_payment_is_owed_again`<br>`payments/tests/intake.rs::claims_is_told_when_the_bank_refuses_a_released_payment` |
| CLM-036 | full | `claims/tests/suite/decisions.rs::ex_gratia_is_decided_and_paid_apart_from_the_contract` |
| CLM-037 | full | `claims/tests/suite/book.rs::a_lump_sum_paid_late_carries_interest`<br>`sim/tests/claims.rs::late_payment_interest_is_its_own_posting_and_settles_with_the_benefit`<br>`claims/tests/suite/book.rs::late_interest_is_offered_and_declined_only_by_an_informed_payee`<br>`sim/tests/claims.rs::declined_late_interest_is_reversed_in_its_own_entry` |
| CLM-038 | full | `claims/tests/suite/views.rs::totals_by_policy_are_what_the_ledger_must_agree_with`<br>`sim/tests/claims.rs::the_core_refuses_instructions_its_own_record_does_not_support` |
| CLM-039 | part | `claims/tests/suite/book.rs::recorded_interests_are_paid_before_the_claimant`<br>`claims/tests/suite/book.rs::a_whole_interest_takes_all_of_the_proceeds`<br>`claims/tests/suite/covers.rs::a_policy_loan_is_repaid_from_a_death_benefit_before_any_interest`<br>`claims/tests/suite/covers.rs::a_loan_takes_no_more_than_the_benefit_and_only_on_death` |
| CLM-040 | full | `claims/tests/suite/book.rs::a_benefit_is_suspended_in_custody_and_resumes_on_release` |
| CLM-041 | part | `claims/tests/suite/intake.rs::triage_routes_by_skill_and_ranks_by_urgency` |

## RI Reinsurance

| Matrix | Status | Built tests |
| --- | --- | --- |
| RI-001 | full | `reinsurance/tests/treaties.rs::shipped_treaties_load_and_their_scenarios_pass`<br>`reinsurance/tests/replay.rs::cessions_balance_and_never_over_retain` |
| RI-002 | full | `reinsurance/tests/accumulation.rs::one_life_on_two_partitions_fills_one_retention`<br>`reinsurance/tests/core.rs::a_life_on_two_partitions_is_ceded_once_and_recorded_on_both` |
| RI-003 | full | `reinsurance/tests/premiums.rs::a_period_charges_each_reinsurer_its_share_at_its_rate`<br>`reinsurance/tests/core.rs::ceded_premium_reaches_the_ledger_and_the_two_balances_agree` |
| RI-004 | full | `reinsurance/tests/claims.rs::a_claim_recovers_each_reinsurers_share_as_it_is_paid` |
| RI-005 | full | `reinsurance/tests/accumulation.rs::a_lapse_suspends_the_cession_and_reinstatement_restores_it`<br>`reinsurance/tests/premiums.rs::lapse_and_unwind_take_ceded_premium_back` |
| RI-006 | full | `reinsurance/tests/accumulation.rs::layers_are_ceded_one_by_one_and_over_limit_cover_is_never_silent`<br>`reinsurance/tests/premiums.rs::a_backdated_change_posts_only_the_difference` |
| RI-007 | full | `reinsurance/tests/capacity.rs::facultative_terms_follow_the_proposal_to_the_issued_layer`<br>`reinsurance/tests/claims.rs::a_claim_on_facultative_cover_recovers_the_facultative_share` |
| RI-009 | full | `reinsurance/tests/accumulation.rs::membership_events_move_the_cession_on_their_effective_dates`<br>`reinsurance/tests/accumulation.rs::an_identified_member_joins_the_life_and_the_excess_is_ceded_facultatively` |
| RI-010 | full | `reinsurance/tests/claims.rs::catastrophe_and_stop_loss_respond_to_what_is_kept_across_claims`<br>`reinsurance/tests/accumulation.rs::the_underwritten_excess_path_is_flagged_once` |
| RI-011 | full | `reinsurance/tests/statements.rs::a_statement_nets_the_account_and_lists_only_what_the_treaty_allows`<br>`reinsurance/tests/carriers.rs::statements_and_the_release_log_are_filed_once`<br>`reinsurance/tests/statements.rs::a_long_listing_is_filed_in_parts_the_statement_names`<br>`reinsurance/tests/carriers.rs::statements_and_listings_are_left_for_their_reinsurer_to_collect`<br>`reinsurance/tests/reinsurer_api.rs::a_reinsurer_reads_only_its_own_statements_and_files` |
| RI-012 | full | `reinsurance/tests/core.rs::an_agreed_statement_sweeps_its_net_amount_to_settlement_clearing`<br>`reinsurance/tests/statements.rs::settlement_is_agreed_checked_and_paid_once`<br>`payments/tests/recon.rs::money_a_reinsurer_pays_is_matched_to_its_statement` |
| RI-013 | full | `reinsurance/tests/replay.rs::reconciliation_notices_damage` |
| RI-014 | full | `reinsurance/tests/claims.rs::large_claims_are_notified_and_consent_is_tracked`<br>`claims/tests/suite/controls.rs::a_reinsurers_consent_holds_admission_and_a_refusal_goes_to_a_senior` |
| RI-015 | full | `reinsurance/tests/claims.rs::periodic_and_ex_gratia_payments_follow_the_treaty` |
| RI-017 | full | `reinsurance/tests/statements.rs::a_treaty_comparison_looks_only_at_cover_on_that_treaty`<br>`reinsurance/tests/statements.rs::recapture_and_a_negotiated_move_change_cover_from_a_date` |

## INV Unit pricing and the investment ledger

| Matrix | Status | Built tests |
| --- | --- | --- |
| INV-001 | full | `sim/tests/investment.rs::an_order_waits_for_its_price_and_executes_once` |
| INV-002 | full | `pricing/src/fund.rs::pricing_date_follows_cutoff_and_calendar` |
| INV-003 | full | `sim/tests/investment.rs::orders_that_cannot_be_honoured_are_refused_when_placed` |
| INV-004 | full | `sim/tests/investment.rs::a_switch_sells_then_buys_and_a_failed_order_does_not_stop_the_run` |
| INV-006 | full | `pricing/src/strike.rs::units_on_issue_difference_blocks` |
| INV-007 | full | `pricing/src/strike.rs::failed_checks_hold_the_price`<br>`pricing/tests/prices.rs::held_and_blocked_prices` |
| INV-008 | full | `pricing/tests/prices.rs::price_runs_from_valuation_to_every_partition` |
| INV-009 | full | `workflows/tests/end_of_day.rs::the_run_executes_unit_orders_and_replays_a_corrected_price` |
| INV-010 | full | `pricing/tests/errors.rs::material_error_is_corrected_by_a_new_version`<br>`sim/tests/investment.rs::a_corrected_price_is_replayed_over_the_orders_executed_at_it` |
| INV-011 | full | `pricing/tests/errors.rs::immaterial_error_is_recorded_not_replayed` |
| INV-013 | part | `sim/tests/investment.rs::a_withdrawal_is_paid_through_benefit_payable` |
| INV-014 | part | `pricing/tests/prices.rs::date_order_and_suspension`<br>`sim/tests/investment.rs::orders_that_cannot_be_honoured_are_refused_when_placed` |
| INV-015 | part | `pricing/src/extract.rs::renders_holdings_and_flow`<br>`pricing/tests/errors.rs::movements_build_holdings_and_extracts` |
| INV-016 | full | `sim/tests/investment.rs::the_book_answers_its_readers_and_reconciles_to_the_ledger` |
| INV-017 | part | `channels/tests/journeys.rs::units_held_and_orders_waiting_reach_the_policyholder` |
| INV-024 | full | `money/src/lib.rs::cash_converts_to_units_at_a_price` |

## GRP Group scheme administration

| Matrix | Status | Built tests |
| --- | --- | --- |
| GRP-001 | full | `group/tests/intake.rs::twelve_monthly_files_on_a_scheme_of_five_thousand`<br>`group/tests/intake.rs::a_full_file_reaches_the_core_as_the_employer_approved_it`<br>`group/tests/driver.rs::an_uploaded_file_goes_from_quarantine_to_the_core` |
| GRP-002 | full | `group/src/diff.rs::change_file_rows_that_do_not_fit_are_rejected_with_reasons`<br>`group/src/mapping.rs::unreadable_rows_are_rejected_one_by_one`<br>`group/tests/apply.rs::one_refused_row_does_not_stop_the_rest_of_the_file` |
| GRP-003 | full | `group/tests/apply.rs::repeats_and_strays_change_nothing` |
| GRP-004 | full | `group/tests/intake.rs::a_file_that_removes_too_many_members_is_held_for_a_person`<br>`group/src/diff.rs::the_safety_check_trips_past_the_share` |
| GRP-005 | full | `sim/tests/scenarios.rs::group_scheme_bills_in_chunks_and_adjusts_for_a_backdated_leaver` |
| GRP-006 | full | `group/tests/intake.rs::approval_needs_a_second_person_and_staff_for_deep_backdating`<br>`group/src/diff.rs::dates_are_held_to_the_backdating_limits` |
| GRP-007 | full | `group/src/benefit.rs::each_eligibility_rule_refuses_its_own_case` |
| GRP-008 | full | `group/src/diff.rs::cover_follows_hours_across_the_minimum`<br>`group/tests/leavers.rs::cover_follows_a_member_whose_hours_cross_the_minimum` |
| GRP-009 | full | `group/src/benefit.rs::cover_splits_into_automatic_and_excess`<br>`group/tests/underwriting.rs::accepted_excess_goes_from_request_to_layer_and_back`<br>`sim/tests/personas_lines.rs::a_pilots_claim_is_checked_on_the_automatic_amount_plus_any_underwritten_layer` |
| GRP-010 | full | `group/tests/excess.rs::a_loading_is_itemised_to_the_employer_only_with_the_members_consent`<br>`sim/tests/group_cover.rs::a_loading_reaches_the_employer_only_in_a_total_unless_the_member_agreed` |
| GRP-011 | full | `group/tests/excess.rs::too_few_members_for_a_total_means_the_member_pays_the_loading`<br>`group/tests/excess.rs::staff_are_told_when_leavers_leave_too_few_members_in_the_total` |
| GRP-012 | full | `group/tests/billing.rs::the_invoice_view_agrees_with_the_core_by_billing_group` |
| GRP-013 | part | `group/tests/billing.rs::a_short_payment_is_brought_forward_on_the_next_invoice_and_falls_on_no_member` |
| GRP-014 | full | `group/tests/billing.rs::persistent_short_payment_suspends_joiners_until_paid` |
| GRP-016 | full | `sim/tests/voluntary_cover.rs::a_member_who_does_not_pay_loses_only_the_cover_they_pay_for`<br>`group/tests/excess.rs::a_member_who_stops_paying_their_loading_loses_only_that_cover` |
| GRP-018 | part | `sim/tests/voluntary_cover.rs::cover_the_member_pays_for_is_billed_and_paid_apart_from_the_scheme`<br>`group/tests/excess.rs::a_member_who_pays_their_loading_keeps_the_cover` |
| GRP-020 | part | `group/src/mapping.rs::mappings_are_checked_and_hashed_when_sealed`<br>`group/src/mapping.rs::a_file_that_does_not_fit_the_mapping_is_refused_whole`<br>`group/tests/driver.rs::payroll_changes_and_the_monthly_file_each_use_their_own_layout`<br>`group/tests/driver.rs::a_new_mapping_version_is_tried_on_a_sample_before_it_is_set` |
| GRP-019 | full | `group/tests/driver.rs::payroll_changes_and_the_monthly_file_each_use_their_own_layout` |
| GRP-021 | full | `group/tests/year.rs::agreed_terms_take_effect_on_the_renewal_date` |
| GRP-022 | full | `group/tests/year.rs::without_an_agreement_the_scheme_runs_on_old_rates_and_then_ends`<br>`group/tests/year.rs::a_rate_guarantee_rolls_the_year_without_an_agreement` |
| GRP-023 | full | `group/tests/leavers.rs::a_leaver_can_continue_their_cover_as_an_individual_policy`<br>`group/tests/leavers.rs::an_option_not_taken_up_expires` |
| GRP-024 | full | `group/tests/leavers.rs::a_seasonal_worker_serves_the_waiting_period_again_on_rejoining`<br>`group/src/model.rs::membership_follows_a_member_through_every_change` |
| GRP-025 | part | `group/tests/year.rs::a_pool_scheme_is_run_on_declarations`<br>`sim/tests/group_cover.rs::a_pool_is_billed_on_its_declared_aggregate` |
| GRP-026 | part | `group/tests/year.rs::a_pool_scheme_is_run_on_declarations` |
| GRP-027 | full | `channels/tests/journeys.rs::employer_changes_can_need_a_second_administrator` |
| GRP-030 | full | `group/tests/billing.rs::a_new_site_joins_with_its_own_billing_group_and_headcount` |
| GRP-028 | part | `channels/tests/http.rs::an_employer_administrator_hands_over_at_noon` |
| GRP-032 | full | `channels/tests/journeys.rs::scheme_members_and_the_employer`<br>`readmodel/src/scope.rs::adviser_and_employer_never_see_sensitive_rows`<br>`channels/tests/http.rs::advisers_and_employers_see_claim_status_and_nothing_more`<br>`claims/tests/suite/views.rs::the_policy_status_shows_an_adviser_each_claim_without_its_cause`<br>`channels/tests/http.rs::an_employer_runs_the_scheme_membership`<br>`channels/tests/group.rs::what_an_administrator_may_ask_and_send`<br>`group/tests/excess.rs::the_employers_view_of_a_member_shows_cover_and_never_health` |
| GRP-034 | part | `group/tests/year.rs::staff_end_a_scheme_from_a_date`<br>`sim/tests/voluntary_cover.rs::a_members_money_stays_the_members_when_the_scheme_ends` |
| GRP-035 | full | `group/tests/underwriting.rs::a_scheme_taken_over_keeps_members_cover_without_new_evidence`<br>`underwriting/tests/takeover.rs::a_taken_over_excess_is_accepted_without_evidence_up_to_what_was_accepted`<br>`group/src/benefit.rs::each_eligibility_rule_refuses_its_own_case` |
| GRP-038 | part | `group/tests/leavers.rs::absence_past_the_limit_ends_cover_on_the_terms_date` |

## HLT Composable health product

| Matrix | Status | Built tests |
| --- | --- | --- |
| HLT-001 | full | `sim/tests/health_flex.rs::each_life_holds_its_own_covers_and_the_combination_rules_hold_per_life`<br>`product/tests/products.rs::combination_rules_decide_which_sets_of_covers_a_life_may_hold` |
| HLT-002 | full | `sim/tests/health_flex.rs::any_valid_subset_is_billed_the_sum_of_its_covers_under_the_bundle_discount` |
| HLT-003 | part | `sim/tests/health_flex.rs::a_cover_can_be_added_after_issue_and_is_billed_for_the_days_left` |
| HLT-004 | full | `claims/tests/suite/covers.rs::an_episode_is_priced_line_by_line_against_each_covers_counters` |
| HLT-006 | part | `sim/tests/health_flex.rs::a_cover_can_be_added_after_issue_and_is_billed_for_the_days_left` |
| HLT-008 | part | `sim/tests/health_flex.rs::each_life_holds_its_own_covers_and_the_combination_rules_hold_per_life` |
| HLT-009 | full | `claims/tests/suite/covers.rs::counters_never_cross_covers_unless_the_shell_shares_one` |
| HLT-010 | full | `sim/tests/health_flex.rs::an_excess_level_prices_the_cover_and_a_change_starts_a_new_layer`<br>`sim/tests/claims.rs::the_cover_check_reports_the_excess_level_held_on_the_event_date` |

## JNT Joint lives, ownership, assignments and dependants

| Matrix | Status | Built tests |
| --- | --- | --- |
| JNT-001 | part | `underwriting/tests/lifecycle.rs::a_joint_application_is_settled_life_by_life` |
| JNT-007 | full | `sim/tests/interests.rs::an_interest_restricts_the_owner_until_its_holder_consents_or_releases` |
| JNT-008 | part | `sim/tests/interests.rs::an_interest_restricts_the_owner_until_its_holder_consents_or_releases`<br>`sim/tests/reduce_cover.rs::a_reduction_needs_the_consent_of_a_standing_interest` |
| JNT-009 | part | `sim/tests/interests.rs::an_interest_restricts_the_owner_until_its_holder_consents_or_releases` |
| JNT-011 | part | `sim/tests/interests.rs::an_interest_restricts_the_owner_until_its_holder_consents_or_releases`<br>`documents/tests/interests.rs::a_released_or_future_interest_is_owed_nothing` |
| JNT-013 | part | `channels/tests/journeys.rs::ownership_change_and_assignment` |
| JNT-023 | part | `customers/tests/api.rs::trustee_changes_are_dated_on_the_resolution` |
| JNT-025 | part | `customers/tests/api.rs::trustee_changes_are_dated_on_the_resolution` |
| JNT-029 | full | `sim/tests/interests.rs::a_bankrupt_owners_policy_holds_its_value_until_a_legal_direction`<br>`payments/tests/intake.rs::a_held_payout_waits_only_once_directed_and_for_the_directed_party`<br>`claims/tests/suite/book.rs::proceeds_are_held_under_an_insolvency_interest_until_legal_directs` |
| JNT-030 | part | `customers/tests/api.rs::due_diligence_covers_an_owner_and_every_trustee` |
| JNT-031 | part | `customers/tests/api.rs::due_diligence_covers_an_owner_and_every_trustee` |

## ADV Advisers and commission

| Matrix | Status | Built tests |
| --- | --- | --- |
| ADV-001 | full | `distribution/tests/commission.rs::a_first_premium_pays_initial_and_renewal_commission_by_the_rule`<br>`distribution/tests/commission.rs::initial_commission_is_paid_once_and_renewal_on_every_premium` |
| ADV-002 | full | `distribution/tests/commission.rs::an_adviser_moves_with_the_book_and_the_previous_provider_keeps_a_tail` |
| ADV-003 | full | `distribution/tests/units.rs::a_tail_that_ends_flows_to_the_ongoing_parties_without_a_new_rule` |
| ADV-004 | full | `distribution/tests/commission.rs::a_lapse_claws_back_from_the_liable_party_and_reinstatement_restores_it` |
| ADV-005 | full | `distribution/tests/personas.rs::an_increase_after_a_move_belongs_to_the_new_arrangement_and_pays_no_tail` |
| ADV-006 | full | `distribution/tests/commission.rs::a_shared_fragment_pays_overrides_and_a_one_off_fee_on_top_of_base` |
| ADV-007 | full | `distribution/tests/personas.rs::a_provider_changing_aggregator_keeps_the_old_override_on_existing_books_only` |
| ADV-008 | full | `distribution/tests/personas.rs::a_second_move_carries_both_tails_each_ending_on_its_own_date` |
| ADV-009 | full | `distribution/tests/personas.rs::a_joint_policy_moves_only_the_share_of_the_adviser_who_leaves`<br>`distribution/tests/config.rs::a_layer_split_between_books_pays_each_book_its_share` |
| ADV-010 | full | `distribution/tests/personas.rs::a_cover_another_adviser_adds_earns_in_that_advisers_book_only` |
| ADV-011 | full | `distribution/tests/personas.rs::an_adviser_with_two_engagements_earns_each_policy_in_the_book_it_was_written_under` |
| ADV-012 | full | `distribution/tests/personas.rs::a_retiring_advisers_book_split_between_three_successors_carries_the_tail`<br>`distribution/tests/edges.rs::a_change_set_restructures_in_one_step_and_refuses_what_cannot_be_carried_out` |
| ADV-013 | full | `distribution/tests/commission.rs::a_customers_change_of_adviser_moves_servicing_at_once_and_commission_by_the_setting` |
| ADV-014 | full | `distribution/tests/commission.rs::a_dishonour_reverses_the_original_lines_whatever_the_rule_says_today` |
| ADV-015 | full | `distribution/tests/personas.rs::premium_handed_back_across_a_move_reverses_the_lines_each_month_recorded` |
| ADV-016 | full | `distribution/tests/commission.rs::a_lapse_claws_back_from_the_liable_party_and_reinstatement_restores_it` |
| ADV-017 | full | `distribution/tests/lifecycle.rs::clawback_by_cause_falls_on_the_engagement_that_submitted_the_alteration` |
| ADV-018 | full | `distribution/tests/personas.rs::a_wrong_commission_option_found_a_year_later_is_corrected_from_the_start` |
| ADV-019 | full | `distribution/tests/personas.rs::a_provider_losing_its_licence_stops_new_business_holds_commission_and_asks_for_servicing`<br>`distribution/tests/units.rs::a_licence_problem_cascades_through_the_dated_structure`<br>`channels/tests/journeys.rs::a_lost_licence_makes_the_adviser_read_only` |
| ADV-020 | full | `distribution/tests/workbench.rs::write_offs_follow_the_floor_the_age_and_the_second_signature`<br>`distribution/tests/workbench.rs::an_aged_debt_moves_to_the_guarantor_the_rule_names`<br>`distribution/tests/workbench.rs::a_release_to_a_liquidator_needs_the_appointment_account_and_legal` |
| ADV-021 | full | `distribution/tests/statements.rs::court_orders_and_assignments_are_paid_first_in_order_of_priority` |
| ADV-022 | full | `distribution/tests/statements.rs::court_orders_and_assignments_are_paid_first_in_order_of_priority` |
| ADV-023 | full | `distribution/tests/commission.rs::disputed_lines_and_accrual_holds_earn_into_held_with_their_reason`<br>`distribution/tests/workbench.rs::a_disputed_amount_is_split_between_the_parties_who_agreed_to_it` |
| ADV-024 | full | `distribution/tests/statements.rs::a_new_account_and_a_payment_hold_stop_payment_but_not_the_statement` |
| ADV-025 | full | `distribution/tests/personas.rs::a_referral_partners_fixed_fee_is_its_own_line_and_is_clawed_back`<br>`distribution/tests/commission.rs::a_shared_fragment_pays_overrides_and_a_one_off_fee_on_top_of_base` |
| ADV-026 | full | `distribution/tests/commission.rs::replacement_cover_earns_trail_only_and_the_house_book_earns_nothing`<br>`distribution/tests/lifecycle.rs::a_replaced_policys_clawback_carries_to_the_replacement` |
| ADV-027 | full | `underwriting/tests/driver.rs::a_replacement_with_a_top_up_underwrites_only_the_increase_on_the_same_cover` |
| ADV-028 | part | `underwriting/tests/replacement.rs::a_replacement_waits_for_the_signed_replacement_disclosure` |
| ADV-029 | full | `distribution/tests/replacement.rs::cover_rewritten_inside_the_group_after_a_cancellation_is_treated_as_a_replacement`<br>`distribution/tests/replacement.rs::the_rule_matches_the_same_life_the_same_cover_and_a_related_adviser_or_the_same_amount` |
| ADV-030 | full | `distribution/tests/personas.rs::a_scheme_changing_broker_mid_year_moves_its_book_with_a_dated_handover_split` |
| ADV-031 | full | `channels/tests/journeys.rs::an_incoming_broker_sees_the_scheme_from_the_appointment_day` |
| ADV-032 | full | `distribution/tests/units.rs::a_rule_allocates_every_cent_of_any_amount` |
| ADV-033 | full | `distribution/tests/units.rs::rule_validation_catches_every_way_a_rule_fails_to_add_up` |
| ADV-034 | full | `distribution/tests/statements.rs::one_net_payment_covers_credits_and_debts_on_different_partitions`<br>`distribution/tests/statements.rs::a_statement_shows_the_detail_and_its_payment_clears_the_ledger` |
| ADV-035 | part | `channels/tests/queries.rs::an_advisers_book_shows_standing_and_held_money`<br>`channels/tests/queries.rs::a_provider_and_its_group_see_the_book_under_them` |
| ADV-036 | full | `channels/tests/journeys.rs::access_ends_with_the_engagement`<br>`channels/tests/http.rs::an_advisers_request_needs_the_customers_signed_approval` |
| ADV-037 | full | `distribution/tests/statements.rs::a_new_account_and_a_payment_hold_stop_payment_but_not_the_statement`<br>`distribution/tests/config.rs::the_structure_refuses_duplicates_loops_and_the_wrong_kind_of_party`<br>`payments/tests/controls.rs::changed_details_wait_out_the_cooling_off_period` |
| ADV-038 | full | `distribution/tests/commission.rs::a_dry_run_shows_what_a_premium_would_earn_and_changes_nothing` |
| ADV-039 | full | `distribution/tests/personas.rs::premium_paused_in_hardship_earns_nothing_until_billing_resumes`<br>`distribution/tests/lifecycle.rs::waived_premium_earns_renewal_commission_unless_the_schedule_says_otherwise` |
| ADV-040 | full | `distribution/tests/personas.rs::a_retired_advisers_tail_passes_to_the_estate_from_the_date_of_death` |
| ADV-041 | full | `distribution/tests/personas.rs::an_authorised_body_that_gains_its_own_licence_leaves_its_providers_cascade` |
| ADV-042 | full | `distribution/tests/commission.rs::a_backdated_rule_moves_the_difference_between_payees_and_leaves_expense_alone` |
| ADV-043 | full | `distribution/tests/personas.rs::direct_business_sits_in_no_book_and_earns_nothing`<br>`distribution/tests/commission.rs::replacement_cover_earns_trail_only_and_the_house_book_earns_nothing` |
| ADV-044 | full | `distribution/tests/commission.rs::a_claim_never_triggers_clawback` |
| ADV-045 | full | `distribution/tests/personas.rs::a_provider_changing_aggregator_keeps_the_old_override_on_existing_books_only` |

## SVC Policy servicing

| Matrix | Status | Built tests |
| --- | --- | --- |
| SVC-001 | full | `sim/tests/integrity.rs::a_quote_equals_the_first_bill`<br>`sim/tests/underwriting_door.rs::an_accepted_increase_is_a_layer_from_today_billed_for_the_days_left` |
| SVC-002 | full | `sim/tests/reduce_cover.rs::a_reduction_takes_the_newest_layer_first`<br>`sim/tests/reduce_cover.rs::a_reduction_ends_layers_from_today_and_credits_the_days_left` |
| SVC-005 | part | `documents/tests/rules.rs::changes_to_cover_require_a_new_schedule` |
| SVC-008 | full | `sim/tests/investment.rs::a_full_surrender_sells_everything_pays_the_value_and_closes_the_policy` |
| SVC-015 | part | `sim/tests/edges.rs::a_cover_expires_at_its_age_limit_and_the_rest_of_the_policy_continues` |
| SVC-017 | part | `customers/tests/api.rs::a_name_change_reaches_every_relationship_and_groups_stay_apart` |
| SVC-018 | part | `channels/tests/http.rs::the_staff_console_asks_the_four_questions` |
| SVC-021 | full | `payments/tests/controls.rs::changed_details_wait_out_the_cooling_off_period`<br>`payments/tests/controls.rs::a_payee_passes_every_control_before_it_can_be_paid` |
| SVC-022 | part | `payments/tests/hold.rs::only_the_service_records_what_the_master_says`<br>`payments/tests/hold.rs::a_hold_stops_value_leaving_and_nothing_else`<br>`payments/tests/hold.rs::a_refund_is_value_going_out_and_a_claim_is_not`<br>`payments/tests/hold.rs::a_hold_heard_after_the_item_joined_holds_it_at_the_check`<br>`payments/tests/hold.rs::a_payout_waits_for_a_recent_answer_where_one_is_required`<br>`payments/tests/hold.rs::the_pump_watches_every_party_money_is_about_to_go_to`<br>`payments-service/tests/pump.rs::the_pump_hears_a_customers_own_hold_from_the_customer_master`<br>`customers/tests/lifecycle.rs::a_customers_own_spending_hold_ends_only_after_cooling_off`<br>`sim/tests/interests.rs::an_owners_own_hold_stops_value_leaving_until_the_master_ends_it` |
| SVC-025 | part | `distribution/tests/replacement.rs::cover_given_up_after_new_cover_was_written_is_matched_looking_back` |

## ANV The anniversary window

| Matrix | Status | Built tests |
| --- | --- | --- |
| ANV-002 | full | `sim/tests/scenarios.rs::anniversary_indexes_cover_and_re_rates_stepped_premium` |
| ANV-007 | full | `sim/tests/reopen.rs::a_decline_of_indexation_made_on_a_wrong_notice_is_set_aside_from_the_anniversary` |
| ANV-011 | part | `sim/tests/unwind.rs::a_death_notified_six_weeks_late_ends_the_policy_as_at_the_day_it_happened` |
| ANV-014 | full | `documents/tests/delivery.rs::returned_post_makes_the_notice_undeliverable_until_retried`<br>`documents/tests/delivery.rs::an_unconfirmed_send_times_out_and_the_chain_runs_to_its_end` |
| ANV-016 | part | `sim/tests/year.rs::the_synthetic_insurer_runs_a_year_with_every_reconciliation_clean` |

## ANN Annuities

| Matrix | Status | Built tests |
| --- | --- | --- |
| ANN-004 | part | `claims/tests/suite/book.rs::an_overdue_proof_of_life_holds_the_benefit_until_it_is_filed` |
| ANN-005 | full | `claims/tests/suite/book.rs::an_overdue_proof_of_life_holds_the_benefit_until_it_is_filed` |
| ANN-006 | part | `claims/tests/suite/book.rs::a_death_notified_late_ends_the_benefit_and_raises_the_overpayment` |
| ANN-010 | full | `payments/tests/controls.rs::changed_details_wait_out_the_cooling_off_period` |

## VUL Vulnerable customers, support people and representatives

| Matrix | Status | Built tests |
| --- | --- | --- |
| VUL-001 | full | `customers/tests/api.rs::vulnerable_people_are_routed_to_a_person` |
| VUL-002 | part | `customers/tests/api.rs::vulnerable_people_are_routed_to_a_person`<br>`channels/tests/http.rs::a_vulnerable_customers_request_goes_to_a_person` |
| VUL-003 | part | `customers/tests/api.rs::risky_acts_by_a_marked_person_are_paused_or_reviewed` |
| VUL-004 | full | `customers/tests/api.rs::a_third_party_concern_opens_a_prompt_and_discloses_nothing` |
| VUL-005 | part | `customers/tests/api.rs::risky_acts_by_a_marked_person_are_paused_or_reviewed` |
| VUL-006 | full | `customers/tests/api.rs::a_third_party_concern_opens_a_prompt_and_discloses_nothing` |
| VUL-007 | part | `customers/tests/api.rs::risky_acts_by_a_marked_person_are_paused_or_reviewed` |
| VUL-008 | part | `customers/tests/api.rs::representatives_act_only_within_their_authority` |
| VUL-009 | full | `customers/tests/api.rs::representatives_act_only_within_their_authority` |
| VUL-010 | full | `customers/tests/api.rs::representatives_act_only_within_their_authority` |
| VUL-011 | full | `customers/tests/api.rs::a_guardian_gets_a_report_of_their_actions` |
| VUL-012 | part | `customers/tests/api.rs::a_name_change_reaches_every_relationship_and_groups_stay_apart` |
| VUL-013 | full | `customers/tests/api.rs::representatives_act_only_within_their_authority` |
| VUL-014 | full | `customers/tests/api.rs::representatives_act_only_within_their_authority` |
| VUL-015 | full | `customers/tests/api.rs::a_death_prompts_a_person_to_consider_the_bereaved` |
| VUL-017 | part | `customers/tests/lifecycle.rs::markers_are_reviewed_when_due` |
| VUL-018 | part | `customers/tests/api.rs::a_capacity_question_pauses_representatives` |
| VUL-016 | part | `complaints/tests/timers.rs::a_vulnerable_complainant_gets_longer_and_the_insurer_does_not` |
| VUL-019 | part | `customers/tests/api.rs::presentation_preferences_are_per_person` |
| VUL-021 | part | `customers/tests/api.rs::presentation_preferences_are_per_person` |

## CMP Complaints and disputes

| Matrix | Status | Built tests |
| --- | --- | --- |
| CMP-001 | full | `complaints/tests/complaints.rs::a_complaint_is_recorded_by_one_command` |
| CMP-002 | full | `complaints/tests/complaints.rs::review_is_by_someone_who_was_not_involved` |
| CMP-003 | full | `complaints/tests/timers.rs::deadlock_arrives_by_time_or_by_declaration_and_owes_a_notice` |
| CMP-004 | full | `complaints/tests/complaints.rs::remediation_follows_an_outcome_through_ordinary_commands` |
| CMP-005 | full | `complaints/tests/conduct.rs::the_population_arrives_in_chunks_and_is_complete_only_when_the_counts_match`<br>`complaints/tests/conduct.rs::each_customer_is_told_or_excluded_for_a_reason` |
| CMP-006 | full | `complaints/tests/forums.rs::a_referral_puts_the_complaint_with_the_forum_under_its_deadlines`<br>`complaints/tests/forums.rs::a_binding_decision_is_implemented_before_the_complaint_closes` |
| CMP-007 | full | `complaints/tests/reporting.rs::advisers_and_complainants_each_see_only_their_view` |
| CMP-008 | full | `complaints/tests/complaints.rs::a_service_complaint_can_be_resolved_on_the_spot` |
| CMP-009 | full | `complaints/tests/complaints.rs::a_complaint_is_recorded_by_one_command` |

## STF Staff authorisation and permissions

| Matrix | Status | Built tests |
| --- | --- | --- |
| STF-001 | full | `channels/tests/staff.rs::the_four_questions_each_deny_on_their_own`<br>`channels/tests/http.rs::every_staff_read_is_in_the_access_log` |
| STF-002 | part | `channels/tests/staff.rs::personal_connections_and_flagged_attempts` |
| STF-003 | full | `channels/src/classes.rs::fields_above_the_callers_classes_are_masked_and_named` |
| STF-004 | full | `channels/tests/staff.rs::the_four_questions_each_deny_on_their_own` |
| STF-005 | full | `channels/tests/staff.rs::cover_and_uplift_grants` |
| STF-006 | part | `channels/tests/staff.rs::cover_and_uplift_grants` |
| STF-007 | full | `channels/tests/staff.rs::maker_checker_and_separation_of_duties`<br>`payments/tests/controls.rs::an_approver_cannot_also_maintain_payees_or_connectivity`<br>`payments/tests/runs.rs::the_maker_never_approves_their_own_run` |
| STF-008 | full | `channels/tests/staff.rs::maker_checker_and_separation_of_duties`<br>`underwriting/tests/special.rs::a_declared_conflict_removes_the_underwriter_from_the_case`<br>`underwriting/tests/special.rs::an_underwriter_never_acts_on_their_own_application` |
| STF-009 | full | `channels/tests/staff.rs::break_glass_is_immediate_loud_short_and_reviewed` |
| STF-010 | full | `channels/tests/staff.rs::leaving_and_recertification` |
| STF-012 | part | `channels/src/classes.rs::fields_above_the_callers_classes_are_masked_and_named` |
| STF-014 | full | `channels/tests/http.rs::the_staff_console_asks_the_four_questions`<br>`channels/tests/http.rs::every_staff_read_is_in_the_access_log` |
| STF-015 | full | `channels/tests/staff.rs::external_elevation_is_capped_and_fails_closed` |
| STF-016 | part | `channels/tests/staff.rs::external_source_drives_templates` |
| STF-017 | full | `channels/tests/staff.rs::classes_clinical_purpose_and_freshness`<br>`channels/tests/staff.rs::builders_and_operators_hold_no_personal_class` |
| STF-018 | part | `channels/tests/staff.rs::classes_clinical_purpose_and_freshness` |

## PAY Payment runs and bank connectivity

| Matrix | Status | Built tests |
| --- | --- | --- |
| PAY-001 | full | `payments/tests/runs.rs::a_claims_run_is_paid_settled_and_closed`<br>`payments/tests/runs.rs::the_maker_never_approves_their_own_run` |
| PAY-002 | full | `payments/tests/runs.rs::an_item_that_duplicates_or_contradicts_is_refused`<br>`payments/tests/controls.rs::a_repeated_command_is_answered_once`<br>`payments-service/tests/pump.rs::two_pumps_never_send_the_same_submission` |
| PAY-003 | full | `payments/tests/runs.rs::a_partial_acknowledgement_accounts_for_every_item` |
| PAY-004 | full | `payments/tests/runs.rs::a_route_is_redirected_to_the_second_bank`<br>`payments-service/tests/pump.rs::a_bank_that_is_down_or_says_no_leaves_the_money_unpaid_and_known` |
| PAY-005 | full | `payments/tests/refunds.rs::a_refund_the_bank_sends_back_is_owed_again`<br>`sim/tests/payments.rs::a_returned_payment_is_owed_again_and_names_what_it_reverses` |
| PAY-006 | full | `payments/tests/runs.rs::the_check_holds_what_must_not_be_paid` |
| PAY-007 | full | `payments/tests/runs.rs::run_and_day_limits_cap_what_can_be_released`<br>`payments/tests/runs.rs::a_run_is_not_released_without_the_funds_to_cover_it` |
| PAY-008 | full | `payments/tests/runs.rs::an_urgent_run_needs_two_on_call_approvers` |
| PAY-009 | full | `payments/tests/recon.rs::settled_and_returned_money_matches_the_statement`<br>`payments/tests/recon.rs::what_the_statement_cannot_explain_becomes_an_owned_exception` |
| PAY-010 | full | `payments/tests/sweep.rs::money_applied_to_another_fund_is_moved_to_its_account`<br>`payments/tests/sweep.rs::a_reversal_or_a_returned_sweep_is_owed_the_right_way`<br>`payments/tests/sweep.rs::a_matched_direct_credit_is_followed_too` |
| PAY-011 | full | `payments/tests/runs.rs::a_collection_run_draws_on_active_authorities`<br>`payments/tests/runs.rs::a_dishonour_is_presented_once_more_then_the_authority_is_suspended` |
| PAY-012 | full | `payments/tests/refunds.rs::a_refund_goes_back_to_the_account_it_came_from` |
| PAY-013 | full | `payments/tests/intake.rs::a_large_admitted_claim_is_flagged_and_forecast`<br>`payments/tests/runs.rs::run_and_day_limits_cap_what_can_be_released` |
| PAY-014 | full | `payments/tests/runs.rs::the_bank_cutoff_and_calendar_are_respected`<br>`payments/tests/runs.rs::a_run_prepared_after_the_cutoff_goes_next_window_and_the_date_is_known` |

## FIN Finance close, statutory funds and regulatory reporting

| Matrix | Status | Built tests |
| --- | --- | --- |
| FIN-001 | full | `finance/tests/close.rs::a_month_closes_once_its_checks_are_green_and_then_never_changes`<br>`finance/tests/driver.rs::a_month_goes_from_its_last_day_to_closed`<br>`finance/tests/close.rs::months_close_in_order_and_a_quarter_needs_the_actuary` |
| FIN-002 | full | `finance/tests/suspense.rs::suspense_is_aged_from_the_feed_and_needs_a_documented_action`<br>`finance/tests/close.rs::a_held_balance_with_no_documented_action_blocks_the_close` |
| FIN-003 | full | `finance/tests/close.rs::a_waiver_is_on_the_record_and_never_clears_the_item` |
| FIN-004 | full | `finance/tests/mart.rs::every_kind_of_change_keeps_the_book_and_its_movements_in_step`<br>`finance/tests/mart.rs::extracts_reconcile_are_frozen_and_satisfy_the_close` |
| FIN-005 | full | `finance/tests/close.rs::an_extract_is_frozen_and_tied_to_the_published_root`<br>`finance/tests/prior_period.rs::a_late_notified_death_is_a_prior_period_movement_and_closed_months_stay_frozen` |
| FIN-006 | full | `finance/tests/journals.rs::an_actuarial_journal_names_its_extract_and_is_signed_by_the_actuary` |
| FIN-007 | full | `finance/tests/journals.rs::a_journal_dated_into_an_earlier_period_is_a_prior_period_item` |
| FIN-008 | full | `finance/tests/mart.rs::a_rebuilt_mart_cuts_the_same_files`<br>`finance/tests/feed.rs::replaying_the_log_rebuilds_the_same_books` |
| FIN-009 | full | `finance/tests/mart.rs::the_mart_and_its_files_hold_ids_and_rating_facts_only` |
| FIN-010 | full | `finance/tests/returns.rs::a_return_is_filed_from_a_closed_period_and_restated_as_a_new_version`<br>`finance/tests/returns.rs::a_changed_line_definition_is_a_new_mapping_version_from_a_period` |
| FIN-011 | full | `finance/tests/returns.rs::any_figure_on_a_filed_return_opens_to_what_is_behind_it` |
| FIN-013 | full | `finance/tests/journals.rs::money_moves_between_statutory_funds_only_by_an_approved_transfer` |
| FIN-014 | part | `finance/tests/edges.rs::tax_accounts_reconcile_to_the_posting_types_that_may_move_them` |
| FIN-017 | part | `finance/tests/mart.rs::claims_that_leave_the_policy_in_force_are_on_the_experience_extract`<br>`finance/tests/mart.rs::the_real_claims_module_feeds_the_experience_extract`<br>`finance/tests/mart.rs::claims_by_cause_are_categories_with_small_rows_folded_or_withheld` |
| FIN-018 | part | `finance/tests/returns.rs::an_overturned_decline_is_reported_apart_from_the_declines`<br>`finance/tests/mart.rs::claims_decisions_lapses_and_their_turnaround_are_counted_from_the_feed` |

## CFG Configuration operations and the actuary's interface

| Matrix | Status | Built tests |
| --- | --- | --- |
| CFG-001 | full | `underwriting/tests/config.rs::activation_needs_two_other_approvers_and_a_change_record` |
| CFG-002 | full | `distribution/tests/config.rs::a_change_needs_two_approvals_from_two_people_who_did_not_prepare_it`<br>`channels/tests/staff.rs::configuration_and_profiles_need_two_people` |
| CFG-003 | full | `finance/tests/tables.rs::shape_range_precision_and_manifest_faults_block_with_the_cell_named`<br>`finance/tests/tables.rs::every_shipped_table_passes_its_schema` |
| CFG-004 | full | `finance/tests/tables.rs::a_cell_out_by_a_factor_of_ten_fails_the_adjacent_age_checks`<br>`finance/tests/tables.rs::a_new_version_is_compared_cell_by_cell_with_the_one_it_replaces` |
| CFG-005 | part | `sim/tests/edges.rs::a_rate_table_validation_is_recorded_against_the_content_it_was_run_on` |
| CFG-006 | full | `sim/tests/scenarios.rs::a_scoped_rate_table_applies_only_to_its_scheme` |
| CFG-012 | part | `finance/tests/edges.rs::a_rate_table_activated_without_a_validation_shows_at_the_close` |
| CFG-013 | full | `claims/tests/suite/book.rs::the_actuarys_valuation_sets_the_reserve_for_claims_in_payment`<br>`sim/tests/claims.rs::a_valuation_sets_the_reserve_of_a_claim_in_payment_on_the_ledger` |

## INC Insurance incident runbooks

| Matrix | Status | Built tests |
| --- | --- | --- |
| INC-003 | part | `sim/tests/group_cover.rs::a_member_removed_in_error_comes_back_with_no_gap` |
| INC-004 | full | `pricing/tests/errors.rs::material_error_is_corrected_by_a_new_version` |
| INC-005 | full | `reinsurance/tests/statements.rs::a_change_after_the_listing_was_sent_needs_a_supplementary_listing`<br>`reinsurance/tests/statements.rs::settlement_is_agreed_checked_and_paid_once`<br>`reinsurance/tests/replay.rs::reconciliation_notices_damage` |
| INC-007 | full | `payments/tests/runs.rs::a_failed_file_is_replaced_never_resent` |
| INC-008 | full | `documents/tests/templates.rs::a_disabled_template_issues_nothing_more`<br>`documents/tests/defects.rs::the_population_is_found_by_template_version_and_period` |
| INC-009 | part | `sim/tests/reopen.rs::a_policy_does_not_lapse_before_its_lapse_notice_allows` |
| INC-013 | full | `documents/tests/defects.rs::a_defect_closes_only_when_every_notice_is_corrected_or_excluded`<br>`sim/tests/unwind.rs::a_remediation_unwind_holds_the_policy_and_replays_when_resumed` |

## DOC Policy documents and correspondence

| Matrix | Status | Built tests |
| --- | --- | --- |
| DOC-001 | full | `documents/tests/render.rs::rendering_is_deterministic`<br>`documents/tests/driver.rs::verification_detects_a_changed_document` |
| DOC-002 | full | `documents/tests/templates.rs::every_product_version_and_language_needs_a_passing_case`<br>`documents/tests/languages.rs::a_translation_is_rendered_from_the_same_data_and_says_what_controls` |
| DOC-003 | full | `documents/tests/templates.rs::submission_checks_structure_readability_and_translations`<br>`documents/tests/render.rs::reading_grade_ranks_plain_words_below_dense_ones`<br>`documents/tests/templates.rs::a_translation_is_versioned_with_its_primary` |
| DOC-004 | full | `documents/tests/render.rs::a_missing_or_mistyped_value_fails_the_render`<br>`documents/tests/templates.rs::a_failing_rendering_run_blocks_activation_and_is_reported` |
| DOC-005 | full | `documents/tests/delivery.rs::proven_delivery_starts_the_notice_clock`<br>`documents/tests/delivery.rs::a_bounced_notice_falls_back_to_post_and_is_deemed_delivered` |
| DOC-006 | full | `documents/tests/relationships.rs::one_person_is_written_to_in_each_role_where_she_chose` |
| DOC-007 | full | `documents/tests/relationships.rs::joint_owners_are_each_addressed_by_their_own_relationship` |
| DOC-008 | full | `documents/tests/delivery.rs::a_correction_is_a_new_version_and_the_earlier_one_is_kept`<br>`documents/tests/defects.rs::a_defective_notice_is_voided_corrected_and_its_choices_reopened` |
| DOC-009 | full | `documents/tests/channels.rs::contact_points_come_from_the_customer_master`<br>`documents/tests/relationships.rs::a_changed_contact_point_reaches_every_relationship_that_uses_it`<br>`customers/tests/api.rs::a_contact_point_can_be_chosen_for_one_relationship` |
| DOC-010 | full | `documents/tests/templates.rs::a_template_is_composed_from_parts_and_hashed_with_them`<br>`documents/tests/templates.rs::a_changed_clause_shows_every_product_it_reaches` |

## MIG Migration

| Matrix | Status | Built tests |
| --- | --- | --- |
| MIG-002 | full | `underwriting/tests/special.rs::migrated_terms_are_carried_as_data_and_never_defaulted` |
| MIG-004 | part | `sim/tests/underwriting_door.rs::only_the_migration_load_issues_a_migrated_policy` |
| MIG-008 | part | `finance/tests/edges.rs::a_migration_control_account_must_stay_still_after_cutover` |
| MIG-011 | part | `reinsurance/tests/statements.rs::migrated_cessions_and_balances_are_taken_as_data` |
| MIG-013 | full | `claims/tests/suite/intake.rs::a_migrated_claim_in_payment_carries_on_where_it_was`<br>`sim/tests/claims.rs::a_migrated_claim_opens_against_the_migration_control_account` |
