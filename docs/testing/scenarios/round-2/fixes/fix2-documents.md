# fix2-documents

Merged to main on 17 Nov 2026.
Commits: ffa89139 (documents module), 36dff797 (console, page, docs, complaint_letters test fix).

Checks: `cargo clippy --all-targets` is clean for ic-channels and ic-documents. `cargo test -p ic-documents --no-fail-fast` passes (all 28 test binaries). `cargo test -p ic-channels` passes (http.rs 92 passed, 1 ignored: the `serve_for_a_browser` harness). The browser check (`tests/browser/check.js`) was updated, but it could not be run against the final build because the disk filled up during the run.
Also in 36dff797: two tests in `crates/documents/tests/complaint_letters.rs` had been failing on the base since e180a072, which lets a person declare deadlock only after the decide-by date. They now declare it on that day.

- F2-10-1: fixed in part (ffa89139, 36dff797; tests `a_sample_is_rendered_for_a_person_to_read_and_record`, `the_documents_console_finds_by_number_renders_samples_and_keeps_keys_back`). "Write a template" starts from any placeholder (the new `authoring` question lists them, plus a policy-wording placeholder) or from an earlier version, and each row has New version. Left: loading templates on the stack is a seed step. See the stack seed in the report.
- F2-10-2: fixed (36dff797; browser check plus `the_documents_console_finds_by_number_...` covering `authoring`). A form with a kind picker that lists each kind's fields, products, language, notice days, review date, formats and a wording editor. The JSON textarea is gone.
- F2-10-3: fixed (ffa89139 `a_sample_is_rendered_for_a_person_to_read_and_record`, `the_standard_sample_gives_what_a_policy_gives`; 36dff797 browser check). Render a sample shows each language from the standard sample, or from data the author gives. "These are right: record the test" records the run with the hash of what the person read and the product's fields.
- F2-10-4: fixed (ffa89139 `a_products_notice_uses_only_the_fields_the_policy_gives`). A product-bound template of an event kind is refused at submission for a field the policy does not give, naming the fields it is given. Decision recorded in decisions.md.
- F2-10-5: fixed (ffa89139 `one_long_sentence_is_refused_and_the_grade_is_kept`). A sentence over `max_sentence_words` (40) is refused. The grade is stored and shown. Decision recorded in decisions.md.
- F2-10-6: fixed (ffa89139 `the_list_says_which_versions_answer_a_policys_documents`). The template list carries products and `answers_events`, and the page shows "None: used only where it is asked for by its id".
- F2-10-7: fixed (36dff797). The tester reports pass or fail from the recorded run. The Act form's run_rendering_tests says the result through a `done` hook, not a bare "Done".
- F2-10-8: fixed (ffa89139 `approvals` test asserts the message; 36dff797). The refusal says another person who gives that approval does it, and the page appends the colleagues who hold it.
- F2-10-9: fixed (ffa89139 disable test). DisableTemplate withdraws a draft. After that, approve gives not_draft and activate gives config_conflict. The row has Withdraw.
- F2-10-10: decided (decisions.md "Activation may be backdated"). The page says beside Activate that a backdated date lets waiting documents issue.
- F2-10-11: fixed (36dff797). Look something up has "notices issued from a template version", and a defect row has Find its notices.
- F2-10-12: fixed in part (36dff797). Dates, settings and approvals are shown in words. Left: staff appear by their workforce ID because the staff directory holds no display names. It needs a name on the staff record.
- F2-11-1: fixed (36dff797 `the_documents_console_finds_by_number_renders_samples_and_keeps_keys_back`). A policy is picked by number (`policies?q=`), and `policy_documents` takes a policy number in any case.
- F2-11-2: fixed (ffa89139 `a_wrong_notice_is_corrected_from_the_new_wording`, `remediation_asks_for_corrections_and_another_person_closes`). CorrectDefectNotices, after which the pass issues `correction:<defect>:<notice>` from the newer in-force version. Decision recorded in decisions.md.
- F2-11-3: fixed (ffa89139 `a_defect_names_its_template_and_takes_only_its_notices`). A defect names its template version and takes only its notices.
- F2-11-4: fixed (ffa89139 `a_returned_letter_is_recorded_and_listed_until_sent_again`; 36dff797). RecordReturnedPost, plus Came back on the document row.
- F2-11-5: fixed (ffa89139 `a_returned_letter_...`, `documents_wait_for_templates_and_people_and_survive_outages`). Undeliverable documents are listed on Work, and each trigger says why it waits.
- F2-11-6: decided (decisions.md "Whoever raised a defect does not close it"). The message now says who closes it and the page names colleagues. Cast gap: give `documents.remediation` to a second person.
- F2-11-7: fixed in part (36dff797 `..._keeps_keys_back`). Rows show recipient party, relationship, channel, status and last tried, and wrapped keys and blob names are no longer sent. Left: a recipient's name, which the documents module does not hold (it needs a customer-master lookup on the page).
- F2-11-8: fixed (ffa89139 `a_defect_names_its_template_and_takes_only_its_notices`). The attach refusals say why: not proven, another template, or no such document.
- F2-11-9: fixed (36dff797). A form that makes a reference says it when done: "Done: Raise a notice defect. Its reference is DEF-...".
- F2-11-10: fixed (36dff797 `..._keeps_keys_back`). There is a Download button with a purpose on each document row, `me.download` hides it from those without the right, and the refusal names `documents.download` and who holds it.
- F2-11-11: fixed (ffa89139 `a_hold_says_who_placed_it_when_and_why`; 36dff797 `the_documents_console`). The console requires a reason, and holds show why, by whom and when.
- F2-11-12: fixed in part (36dff797). The overdue banner's date is shown as a day. Left: the product name ("Aia cancer care") needs the catalog's display name served to the customer page, and "held at level 0" needs the product's option labels. Both belong to the products/catalog owner.
