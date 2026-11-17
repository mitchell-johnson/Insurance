# fix2-servicing

Merged to main on 18 Nov 2026.
Commits: 89ca1e7b (group rule sets), d218f9e4 (unit pricing), c19b1257 (spending hold), b7f24807 (servicing)

- F2-04-1: fixed (b7f24807, a_change_entered_for_a_customer_names_the_cover_and_can_be_taken_back). The staff policy view now carries `names` from the master. Left: staff.html does not yet use them in the cover picker.
- F2-04-2: fixed (b7f24807, a_change_entered_for_a_customer_names_the_cover_and_can_be_taken_back). Adds the route DELETE /staff/v1/requests/{id} and the MCP tool withdraw_change_for_customer. Left: staff.html has no Withdraw button yet.
- F2-04-3: fixed (b7f24807, a_change_entered_for_a_customer_names_the_cover_and_can_be_taken_back). Rows carry context.from_minor/life_party, and customer.html says "Reduce life cover on your life / the other life insured from $X to $Y".
- F2-04-4: fixed (b7f24807, a_change_is_shown_in_words_to_whoever_approves_it_and_stays_in_view). Rows show signed_by, and a co-owner who signed reads "You signed it; waiting for the other owner" with no buttons.
- F2-04-5: fixed (b7f24807, a_change_is_shown_in_words_to_whoever_approves_it_and_stays_in_view). A co-owner's list includes changes the other owner signed or declined, and reads "Declined by another owner".
- F2-04-6: fixed (b7f24807, a_change_entered_for_a_customer_names_the_cover_and_can_be_taken_back). A reduction is measured by the cover given up against the new limit assisted_cover_reduction; a profile without that limit sends every reduction to a second person.
- F2-04-7: left (UI). The backend queue rows carry on/context/cover_given_up_minor (b7f24807, tested). staff.html still shows entered_at and codes.
- F2-04-8: left. Needs customer.html forms for family consent and delegates; the routes exist. A customer would also need to see their own reference to give to a delegate.
- F2-04-9: left. Needs a signed "Ask for a change" form on customer.html (cancel, frequency, indexation, reduce, switch).
- F2-04-10: left. Customer view layout noise: subject in header, $0 hospital cover, empty Held at, investments on non-invest policies, ISO dates, "Pays –".
- F2-04-11: left. "Standing paid" within grace needs the core/portal to show "within grace: $X due".
- F2-04-12: left. No core-policy screen; the route takes the number not the id; MCP's wrong-argument message does not name the right one.
- F2-04-13: left (UI). staff.html still says "Request <uuid> recorded".
- F2-05-1: fixed (b7f24807, customers_changes_are_carried_to_the_core, servicing_requests_become_the_core_commands_they_name, a_switch_is_priced_on_the_next_day_a_price_can_be_struck). switch_funds is a kind on API and MCP, and unknown kinds are refused naming the kinds. Left: no switch form on customer.html.
- F2-05-2: fixed (c19b1257, sim interests hold test). A withdrawal order is refused under the owner's spending hold.
- F2-05-3: fixed (c19b1257, a_person_keeps_their_own_customer_record). Placing a spending hold or cancelling its removal needs no device signature.
- F2-05-4: not a defect in the product. The stack should seed fund prices up to its opening date (stack change for the lead).
- F2-05-5: left. Fund names, switch wording on orders, spread note, "Priced; units added tonight" on customer.html.
- F2-05-6: skipped (already fixed on main).
- F2-05-7: partly fixed (b7f24807). make_request lists reduce_cover and switch_funds. Left: my_policy units_micro/value_minor descriptions, cancelled orders still listed, my_account holdings.
- F2-06-1: fixed (d218f9e4, a_mistaken_suspension_is_withdrawn_and_a_suspended_day_is_not_prepared). A resume from the suspension's own day withdraws it.
- F2-06-2: fixed (d218f9e4, a_mistaken_suspension_is_withdrawn_and_a_suspended_day_is_not_prepared). Preparing a suspended day is refused, and suspending rejects open drafts.
- F2-06-3: fixed (d218f9e4, same test). The next date to price skips suspended days.
- F2-06-4: fixed (d218f9e4, a_price_for_a_day_that_has_not_come_is_not_prepared). The console sends the business date, and a later day is refused `not_yet`.
- F2-06-5: fixed (d218f9e4, the_unit_pricing_console). no_such_fund names the funds there are.
- F2-06-6: fixed (d218f9e4, the_unit_pricing_console). Full pricing tool descriptions, and late prices grouped per fund.
- F2-06-7: fixed (d218f9e4). The refusal names unit_pricing.view; the tool list was already fixed on main (F2-18-2).
- F2-22-3 follow-up: fixed (89ca1e7b, group_excess_is_judged_under_the_shipped_group_rule_set). Shipped rules_group.star and rules_group_2027.star for group-life/group-benefits/aia-corporate, with a decisions.md entry. Note: the individual sets already listed group-life, so the stack's failure may instead be member_not_linked.

Decisions not yet written in docs/decisions.md: unknown kinds refused at submission; the switch pricing date; assisted_cover_reduction; a same-day resume withdraws; the console sends the business date; protective holds need no device. Only the group rule-set entry is written.

Stack and e2e changes for the lead:
- E2E-229 (07-pricing.js): a prepared price is rejected when its fund is suspended, so no "suspend" 409 on Approve.
- E2E-230: a resume from the same day now withdraws the suspension.
- seed.js comment: group schemes use the group rule set.
- Seed fund prices to the opening date (F2-05-4).
- Optionally give team-lead profiles an assisted_cover_reduction limit, with a template ceiling.

Not run: clippy for ic-sim on c19b1257. docs/channels.md is not updated for the new route, kind and pricing row.
