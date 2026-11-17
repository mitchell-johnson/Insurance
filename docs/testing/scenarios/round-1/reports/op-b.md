# Operator op-b: SCN-05, SCN-07, SCN-09, SCN-19

Scripts and screenshots were kept outside the repository.
What I created, all prefixed with my scenario and name:
- schemes `scn05-opb` (`pol-scn05-opb`, `WPL-SCN05-OPB`, administrator e1), `scn07-opb` (pool scheme, administrator e3) and `scn19-opb` (administrator e3)
- one customer for SCN-09, with a master record, a sign-in and a phone
- files named `SCN05-*`, `SCN07-*`, `SCN09-*` and `SCN19-*`

I did not touch kaifresh, closed no day, and left no scheme frozen.

Parts I played for missing drivers:
- the Defender scan result on every uploaded file (`group.scan`)
- issuing each scheme policy on the API as new business. There is no screen for this.

## Set-up corrections (the scenario text against the real system)
- **Scheme registration is not a form.** It is a 4 KB JSON blob pasted into "Act → Register a scheme": the policy, the group, the employer party, the whole product definition, the terms and the founders. The scheme can't be "found from the policy number" during registration. See F-SCN19-1.
- **A scheme's files are full membership lists** (`kind: full` in the shipped mapping). A "second file with the two corrected rows" (SCN-05 step 7) means removing everyone else, so the safety check holds it. See F-SCN05-8.
- **A pool scheme cannot have named members.** `register_scheme` takes pools or members, never both (`crates/group/src/state.rs:994`), and an upload to a pool scheme is refused with `pool_scheme`. SCN-07's "a pool for the warehouse plus a small named file for the office" can't be done on one scheme. I registered `scn07-opb` as a pool scheme: policy life `pool:warehouse`, category `staff`, because the product has no "warehouse" category. See F-SCN07-5.
- **The employer's "Ask for a change" form does nothing.** No module consumes it, so SCN-05 step 6 and SCN-09 step 2 (remove a member with a backdated date, second administrator approves) can't be done. I removed the SCN-09 member with a full file that left them out. The removal took effect on the file date (2027-11-15), not "last Friday": this mapping has no effective-date column. See F-SCN05-5.
- **`hr-e3` and `hr-e1` have no customer-master record.** The first-administrator picker finds nobody, so I typed the reference `hr-e3` into the field (SCN-19).
- **The customer's link needs the internal scheme ID** (`scn05-opb`). The scheme number the employer sees (`WPL-SCN05-OPB`) is refused.

---

## SCN-05: employer's monthly file. Outcome: **partly**
**What I did:**
1. Registered `scn05-opb` and applied a first file: E001, E002, and E003 (the SCN-09 customer).
2. As e1, named e1b an administrator and ticked "Removing a member" as needing a second administrator.
3. Uploaded `SCN05-2027-11.csv`. It held:
   - three good joiners (one executive above the automatic limit)
   - a US-order date (E007) and the salary `sixty thousand` (E008)
   - a look-alike of E002 under a new number (E009)
   - a salary change for E002, and E001 twice
4. Read the preview and approved. st7 worked the possible match.
5. e1 asked to remove a member, first with a wrong ID, then with the right one. e1 tried to approve her own request, and e1b looked for it.
6. Sent the two corrected rows as a second file, then the first file again under the same name and under another name.
7. Checked Members, a member's record and the invoice.

**What worked:**
- Good rows were applied (Applied 4 = 3 joiners + 1 change, equal to the preview's counts).
- Cover is 2× / 4× salary capped at $750,000 automatic. E006 on $250,000 shows $1,000,000 by formula, $750,000 automatic and $250,000 pending.
- The outstanding balance ($207.12) equals the independent calculation (oracle 20712 cents for the six members on cover).
- The employer's member record shows category, salary and cover, and nothing about health or matching.
- A re-sent file with the same content under another name is refused (`duplicate_file`).

**Findings:**
- **F-SCN05-1 (major) – The employer has to type an internal party reference to name a colleague, and any text is accepted.**
  - Hurts: employer administrators.
  - Steps: `/employer` as `sub-e1`, open WPL-SCN05-OPB, "Changes and administrators", Administrators. Type `Kai Fresh second administrator`, Access = Administrator, Save.
  - Happened: "Saved." A non-existent "person" became an administrator. The same happens by MCP `set_scheme_admin` with party `SCN07-nobody`.
  - Should: offer the colleague by name, or refuse an unknown person.
  - Also: the page never lists the current administrators, so the mistake can't be seen.
  - Cause: `crates/channels/ui/employer.html` (`#who` is a free text input); `crates/channels/src/http.rs:1456` `scheme_admin` passes the path party straight to `SetEmployerAdmin` with no check that the party exists.
  - I removed the bogus administrator afterwards.
- **F-SCN05-2 (major) – The preview gives counts only, not who joins, leaves or changes.**
  - Hurts: employer HR.
  - Steps: upload a file, then "Show preview" after the scan.
  - Happened: Joiners 3, Leavers 0, Changes 1, … with no names or employee IDs, except for refused rows.
  - Should: list each joiner, leaver and change.
  - The same holds for the staff held-file view and the mapping trial (F-SCN19-4).
  - Cause: `employer.html` `preview()` dumps the group service's `summary`. The file answer in `crates/group/src/host.rs` (around line 189) carries no per-row changes.
- **F-SCN05-3 (major) – A look-alike joiner is refused as "held for review", but nobody is asked to review it.**
  - Hurts: the joiner (left without cover) and group administration.
  - Steps: a full file with a new employee ID whose name and DOB equal an existing member's (E009 = E002).
  - Happened: the employer sees row 9 E009 "held for review" under *Rejected* and "likely duplicate E009 E002" under Ambiguities. st7's Work waiting shows nothing for it. E009 is not a member.
  - Should: put it to group administration as an exception, and tell the employer in words what happens next.
  - Cause: `crates/group/src/diff.rs:296-316` pushes `RowReject "held_for_review"`. `crates/group/src/event.rs:65` `ExceptionKind` has no kind for an in-file or in-scheme duplicate.
- **F-SCN05-4 (major) – Possible-match candidates are shown as opaque party IDs, with no name or birth date.**
  - Hurts: group administrators deciding identity.
  - Steps: `/group` as `sub-st7`, Work waiting, scheme `scn05-opb`, "Decide" on E003.
  - Happened: "Might be: pty_e68a6ba2b1324104b46c3cc51637e2a1 [It is this person]". The member view doesn't show the member's name or DOB either.
  - Should: show the candidates by name and birth date next to the member's, so the person can judge them.
  - Cause: `crates/group/src/host.rs:283-290` answers `candidates` as bare party IDs. `group.html` `rm-find` renders them.
  - Note: the look-alike raised in SCN-05 was actually E003. That customer's name and DOB had been put in the master before the first file, so the master put it to staff instead of linking it automatically. I decided "same person".
- **F-SCN05-5 (blocker for the flow) – "Ask for a change" requests go nowhere, the second-administrator rule never applies, and a wrong employee ID is accepted.**
  - Hurts: employers and members.
  - Steps:
    1. As e1, with remove_member ticked as needing a second administrator, go to "Ask for a change", choose Remove a member, Member reference `E0O4`, Send.
    2. Repeat with `E004`.
  - Happened:
    - Both requests: "Sent." and status `pending` with `route_to_person: true`, never "awaiting a second administrator".
    - e1b's "Your requests" says "You have asked for no changes", so there is no Approve button to press.
    - When e1 approves her own request by reference: "that request is not awaiting approval", which doesn't say she cannot approve her own.
    - E004 is still on cover and nothing consumes `module: "group-admin"` requests. `remove_member` appears nowhere in `crates/group`.
  - Should:
    - refuse the wrong ID by name
    - route the request to the second administrator
    - remove the member on the date once approved.
  - Cause:
    - `crates/channels/src/state.rs:868-879`: the Policyholder branch is checked before EmployerAdmin, and in this stack the HR person is also the policyholder (`e2e/lib/group.js:24` issues the scheme with `policyholder: employer.party`). So the second-administrator rule can never fire for the first administrator.
    - No consumer for the requests in the group service.
    - The request list for e1b only shows the requester's own requests (`/app/v1/requests`).
- **F-SCN05-6 (major) – The employer's group refusals show "The request failed (422)." with no reason.**
  - Hurts: employers. This affects every refusal on the employer's group routes: duplicate file, frozen scheme, approving a quarantined file, unknown member.
  - Steps: upload the same CSV under a new name, or upload while the scheme is frozen (SCN-19).
  - Happened: "The request failed (422)." The body is `{"error":{"code":"duplicate_file","message":"the same content was received as file SCN05-2027-11.csv"}}`, a good message that never shows. An unknown member shows the generic "Nothing was found with that reference…" instead of "E009 is not a member".
  - Cause: `crates/channels/src/http.rs:1534` `grouped()` passes the group service's `{error:{code,message}}` through unchanged. `crates/channels/ui/app.js:224-227` reads only `detail`. Staff routes convert it (`group_staff_answer`, http.rs:1621); employer routes don't. The same gap shows in MCP (F-SCN07-2).
- **F-SCN05-7 (minor) – Re-sending the same file under the same name reports success.**
  - Steps: upload `SCN05-2027-11.csv` again.
  - Happened: "Uploaded. It is being checked; show its preview in a moment." The route answered 202 with status `applied` and nothing changed.
  - Should: say it was already received and applied, and that nothing changed.
- **F-SCN05-8 (major) – The page never tells the employer that each file must list every member.**
  - Hurts: employers.
  - Steps: upload a file of only the two corrected rows.
  - Happened: "The file is held." with Leavers 6, and no explanation.
  - Should: say that every file must list all current members, and that this one would remove 6 of 6.
- **F-SCN05-9 (minor) – Preview while the file is in quarantine.**
  - Happened: a full summary of zeros, including "Members before 0".
  - Should: say "Being checked for viruses; try again in a minute."
  - Also: the "scanned" status reads as a final state.
- **F-SCN05-10 (minor) – Refused rows don't explain themselves well enough.**
  - Codes such as "bad date", "bad number" and "duplicate employee id" are not shown with the value that was wrong.
  - Row numbers count from the first data row, so they are one less than the spreadsheet line.
  - Both E001 rows are refused, and nothing says that E001 stays on cover.
- **F-SCN05-11 (minor) – Codes and raw values on the employer screens.**
  - The scheme list shows `group-life` and `in_force`.
  - The members table has an always-empty "Held at" column and shows `group-life`.
  - The file view shows the SHA-256 content hash.
  - Billing shows a "notice 1" of $11.00 (the founder at inception) beside $207.12 outstanding, while Invoices says "No invoices yet". The employer can't tell what they owe from the bill.
- **F-SCN05-12 (minor) – Staff member view shows raw labels and inconsistent money.**
  - Labels are `salary minor` and `hours hundredths` (shows `4000`).
  - Cover shows "144000.00" with no `$` beside a salary shown as "NZ$72,000.00".
  - The scheme view shows `cover minor Group-life: 168600000` in raw cents and `outstanding minor 193.92`.
  - Cause: `group.html` `pairs()` and `money()`.
- **F-SCN05-13 (minor, test-stack related) – Date fields default to the computer's date.**
  - Every date field defaults to the browser's date (2026-11-17), not the business date (2027-11-15). This comes from `portal.today()`.

## SCN-07: labour-hire assistant by MCP. Outcome: **partly** (the named file is impossible on a pool scheme)
**What I did:**
- As `sub-e3`, called `initialize`, `tools/list`, `my_account`, `employer_schemes`, and `scheme_question` for scheme and invoices.
- Declared the warehouse pool: 140 people, $7,840,000. Then tried headcount 0, and the salary entered 100 times too large.
- Uploaded the named file, approved a file still in quarantine (on scn05-opb as e1), asked for a member and an unknown member, and ran the overreach probes.

**(a) Tools listed:** 46. All are customer-realm tools plus `status`, including the adviser tools (`adviser_book`, `adviser_statement`, …) and the member tools.
**(b) No staff tools listed:** confirmed. For example, `first_scheme_admin` and `group_command` are absent.
**(c) Outside her job:** `adviser_book` is refused with "the adviser portal is for advisers and advice providers, and you are not engaged as either". Good.

**What worked:**
- The other employer's scheme (`pol-kaifresh`, read-only probes) answers "no such resource", exactly like a policy that doesn't exist.
- Removing herself as the only administrator is refused: "last_admin: a scheme keeps at least one administrator".
- `policy_claims` on her own scheme returns rows with status only.
- After the correct declaration, the bill matches: warehouse $1,437.33 + office $73.33 = $1,510.66, the same as the calculation.

**Findings:**
- **F-SCN07-1 (major) – The `scheme_command` description gives the wrong body shape.**
  - Hurts: assistants.
  - Steps: `tools/call scheme_command {policy, body:{command:"declare_pool", pool, category, effective, headcount, total_salary_minor}}`, following the schema text `{"command": "approve_file" | "withdraw_file" | "declare_pool", ...}`.
  - Happened: "Refused (400 bad_request): a command is one object naming the command". The working shape is `{"declare_pool": {...}}` / `{"approve_file": {"file": ...}}`.
  - Also: the description never lists the fields (`pool`, `category`, `effective`, `headcount`, `total_salary_minor`), so I had to guess every one. `approve_file` needs `{file}`, which isn't documented either.
  - Cause: `crates/channels/src/mcp.rs:305-306`.
- **F-SCN07-2 (major) – Group-service refusals reach the assistant with empty text.**
  - Steps: `declare_pool` with headcount 0; `upload_membership_file` to a pool scheme; `approve_file` in quarantine; `scheme_question member` for an unknown ID.
  - Happened:
    - the text is "Refused (422 ): " with an empty reason
    - the reason ("a declaration needs a headcount and a total salary", "a pool scheme declares aggregates; it has no membership files", "file … is Quarantined", "ZZ99 is not a member") is only inside `structuredContent.error`.
  - Cause: `crates/channels/src/mcp.rs:774-776` reads only `detail`/`title`. The root cause is the same as F-SCN05-6 (`http.rs:1534`).
- **F-SCN07-3 (major) – A pool salary entered 100 times too large is accepted and billed without a warning.**
  - Hurts: employer money.
  - Steps: `declare_pool {pool:"warehouse", headcount:140, total_salary_minor:78400000000}` (the dollars typed as cents, then multiplied again).
  - Happened: applied. Cover became $105,000,000 (capped per head at $750,000) and the outstanding balance jumped from $1,026.67 to $9,698.33.
  - Should: refuse or confirm an average salary of $5.6M a head. Money should also be shown in dollars in the answer.
  - I put the correct declaration back.
- **F-SCN07-4 (major) – A mistyped pool name creates a new insured pool, and it can't be undone.**
  - Steps: `declare_pool {pool:"office", ...}` on a scheme registered with only `warehouse`.
  - Happened: applied. The core now has life `pool:office` with $800,000 cover, and the bill includes it. A follow-up declaration of headcount 0 to end it is refused.
  - Should: refuse a pool that isn't on the scheme, or let a mistaken pool be ended.
  - `scn07-opb` therefore still carries `pool:office` (5 heads, $400,000 salary). It is my own scheme.
- **F-SCN07-5 (major, product gap) – A scheme can't have both a pool and named members.**
  - The persona's goal ("pool for the warehouse, named file for office staff") needs two schemes.
  - Cause: `crates/group/src/state.rs:994`. Upload refusal: `pool_scheme`.
- **F-SCN07-6 (minor) – Amounts and invoices are hard to read.**
  - Amounts are cents with a `_minor` suffix everywhere (`total_minor: 102667`, `outstanding_minor: 969833`), with no dollar form.
  - The invoice reads `members: 0` and `category: ""` for a pool of 100/140 and has no period dates.
  - `invoices` didn't change after any declaration (still `102667`), while the outstanding balance did.
- **F-SCN07-7 (minor) – The quarantine refusal doesn't say to wait.**
  - It says "file X is Quarantined" instead of telling the person to wait for the virus check (as in F-SCN05-9).
- **F-SCN07-8 (minor) – `my_profile` for an employer administrator gives no explanation.**
  - It answers "Refused (404 ): " (no such party). The tool is listed but there's nothing behind it for this sign-in.

## SCN-09: a leaver keeps their cover. Outcome: **blocked at take-up**
**What I did:**
- Created a customer (master record, sign-in, phone) and applied them as E003 on `scn05-opb`. st7 linked them in SCN-05.
- Enrolled the browser, then tried to link by:
  - the scheme number
  - a colleague's ID
  - an unknown ID
  - another scheme
  - the customer's own ID.
- The employer removed E003 by full file. The customer then saw the offer and asked for more than allowed, then the allowed amount. st7 looked them up.

**What worked:**
- The wrong employee ID, another scheme, or an unknown ID all give the same answer: "no member of that scheme has that employee ID and your birth date". It doesn't reveal anything.
- The offer shows "You may keep up to NZ$144,000.00 until 2028-01-14": the limit in dollars, 60 days, as a date.
- Asking for too much is refused: "that is more than the cover you held when you left".
- The employer's members table drops E003 and shows nothing about the continuation.

**Findings:**
- **F-SCN09-1 (blocker; test-stack configuration) – Taking up the offer fails.**
  - Steps: `/customer` as the customer, browser enrolled. Under "To approve", "Keep your cover from scheme scn05-opb", amount $144,000, Monthly, "Keep this cover".
  - Happened: the page shows "The request failed (403)." The body is `{"error":{"code":"role_not_held","message":"the token does not carry the role member-portal"}}`.
  - Cause: `e2e/stack/stack.js:43` gives `ic-portal` only `employer-portal` and `group-admin` on the group audience. The deployment (`deploy/entra/group-app.sh:38`, `deploy/k8s/portal.yaml:77`) also gives it `member-portal`.
  - Because of this I couldn't check:
    - that no health questions are asked
    - that a new policy appears under Policies with its premium
    - that the offer can't be taken twice
    - that group administration sees it taken.
  - Product side: the message shown is unreadable (F-SCN05-6 pattern).
- **F-SCN09-2 (major) – The customer must know an internal scheme code.**
  - Steps: link with `WPL-SCN05-OPB` / E003.
  - Happened: "no member of that scheme has that employee ID and your birth date". Only `scn05-opb` works, and nowhere does the employer see that code. Their portal shows `WPL-SCN05-OPB`.
  - Should: accept the scheme number. The refusal also wrongly suggests the employee ID is at fault.
- **F-SCN09-3 (major) – A linked member gets an error banner on arrival.**
  - Steps: `/customer` as the linked member.
  - Happened: "Nothing was found with that reference. Check it for a slip…" at the top of Your policies. The page asks `GET /app/v1/policies/pol-scn05-opb/claims`, which is 404 for a member.
  - Should: show no error for a scheme the member belongs to.
- **F-SCN09-4 (minor) – Codes and internal IDs in the offer and policy list.**
  - The offer says "scheme scn05-opb" instead of the employer's name.
  - Product `term-life` and cover `life` are shown as codes.
  - The policy list shows the product `group-life`.
  - The too-much refusal doesn't name the limit.
- **F-SCN09-5 (minor) – Staff can't see a member's continuation offer.**
  - st7's member view for E003 shows "left 2027-11-15, in force no" but nothing about the offer. Only the scheme view counts "open offers 1".
- **F-SCN09-6 (note; key storage is per browser, as designed)** – The signing key lives in the browser's storage, so a new browser session must be enrolled again. Expected.

## SCN-19: taking on a new scheme. Outcome: **completed with workarounds**
**What I did, as st7:**
1. Registered `scn19-opb` (5 founders, 3 akl and 2 wlg) through Act → Register a scheme (JSON).
2. Looked it up by number and by policy.
3. Tried the Kai Fresh layout against a reordered sample with `Date of Birth`, adjusted it, tried again, and set it.
4. Named `hr-e3` as first administrator, then tried a second "first" administrator.

**Then:**
- e3 uploaded a one-site file, which was held. st7 opened it from Work waiting without confirming. HR withdrew it and sent the full file, which was applied.
- st7 froze the scheme (with and without a reason). The employer's upload was refused, and st7 unfroze it.
- st7r looked at the scheme. st7 checked a member, the loadings-in-total view and the bill.

**What worked:**
- The old layout is refused naming the column: `the file has no column headed "DOB"`.
- The trial stores nothing (5 members, no open file afterwards) and lists the bad row: "6 S006 bad date date of birth".
- A second first administrator is refused: "this scheme already has an administrator, and its administrators manage their own list from the employer portal".
- The one-site file is held and appears in Work waiting as "file held", with an Open button that lands on it. Nothing changed until a decision.
- A freeze without a reason is refused ("Say why the scheme is frozen.").
- st7r is offered no changing buttons and no Act tab.
- Member cover is correct: $65,000 gives $130,000.
- The outstanding balance of $69.11 equals the independent calculation.

**Findings:**
- **F-SCN19-1 (major) – Registering a scheme means hand-writing a 4 KB JSON blob.**
  - Hurts: group administrators.
  - Steps: `/group` as `sub-st7`, Act, "Register a scheme", one textarea "The registration (JSON)".
  - Happened: the person must type the policy ID, group ID, employer party, inception, the full product definition, terms and founders.
  - Should: register from the policy number, picking it up from the issued policy. The staff-UI rule says nobody types a reference they can't know.
  - Cause: `crates/channels/ui/group.html` `ACTIONS.register_scheme`. The same holds for the "Split members" form (employer party, new policy).
- **F-SCN19-2 (major) – A scheme can't be found from its policy number.**
  - Steps: Schemes, "or its policy" = `WPL-SCN19-OPB`, Show.
  - Happened: "No scheme is written on that policy." Only the internal `pol-scn19-opb` works. Also, the old red banner stays on screen after a successful second lookup.
  - Cause: `group.html` `showScheme()` asks `scheme_of` with the raw text. `crates/group/src/host.rs:292` looks up `by_policy` by ID only.
- **F-SCN19-3 (major) – Setting an edited mapping fails unless the person copies a SHA-256 hash.**
  - Steps: Files, File layouts. Paste the adjusted mapping JSON and the sample, press "Set this mapping".
  - Happened: "the mapping's content hash does not match its content" (`mapping_not_sealed`). It only worked after I copied the 64-character "Content hash" from the trial's output into the JSON.
  - Also: a new scheme has no layout to start from ("Mapping –"), and "Try it" with an empty box says "The mapping is not valid JSON.".
  - Should: seal the mapping in the portal or the service on Set. Better, offer a column-picking form.
  - Cause: `crates/group/src/mapping.rs:226-256` (`seal`/`is_sealed`); `group.html` `mp-set` sends the typed mapping.
- **F-SCN19-4 (minor) – The trial shows counts, not what each row would do.**
  - It gives Joiners 0, Leavers 0, Rejected 1, and the rejected row, but not what each sample row would do (stay, join, change).
- **F-SCN19-5 (major) – The first administrator can't be found by name and birth date.**
  - Hurts: group administrators.
  - Steps: Schemes, "Name a scheme's first administrator", type `Southline`, `hr-e3` or `e3` in the picker.
  - Happened: "Nothing matches that." The HR person has no customer-master record (cast gap), so I typed the reference `hr-e3`, which the field accepts.
  - Partly a cast/test-stack limit (Things noticed #5), but it means the "found, never typed" promise in `group.html` can't hold for the seeded employers.
- **F-SCN19-6 (major) – The held mass-removal shows no proportion, no names and no plain question.**
  - Steps: Work waiting, "file held", Open.
  - Happened: "SCN19-wlg-only.csv: held" with leavers 3 and members before 5 among a dozen counts. Nothing says "This file would remove 3 of 5 members (60%). Confirm only if they have all left." The employer only sees "The file is held." / "The file is scanned." with no reason.
- **F-SCN19-7 (major) – A frozen scheme's refusal isn't shown to the employer.**
  - Steps: st7 freezes with the reason "SCN19 query open with Southline payroll". e3 uploads a file.
  - Happened: "The request failed (422)." The employer's standing line doesn't mention the freeze either.
  - Should: "Your scheme is on hold while we look into a query; files can't be sent until it is lifted."
  - Cause: F-SCN05-6.
- **F-SCN19-8 (minor) – The bill and invoices disagree on the employer page.**
  - The first invoice can't be compared on the employer page: "Invoices: No invoices yet" while the outstanding balance is $69.11 (correct to the cent).
- **F-SCN19-9 (minor) – Raw labels in the group console.**
  - `#me` ("You are … view only") renders empty in the top chip for st7 and st7r, so the reader isn't told they are view-only.
  - The member view shows `hours hundredths 4000`.

## "Things noticed while designing", items I touched
- **#3, MCP lists by sign-in: confirmed.** e3 is offered 46 customer-realm tools, including adviser and member tools. No staff tools are offered. The adviser tool is refused with words that say why.
- **#5, cast gaps: confirmed.**
  - `e3` has no scheme.
  - `hr-e1` and `hr-e3` have no customer-master record (not findable by name).
  - `scheme_command` is sketched as `...`, and worse, the sketch has the wrong shape (F-SCN07-1).
- **#2, missing drivers: touched.** I played Defender's scan result.

## Clean-up state
- `scn05-opb`:
  - one held file (`SCN05-2027-11-fix.csv`) was withdrawn
  - two unprocessable `remove_member` requests stay pending (they can't be withdrawn from the screen)
  - the bogus administrator was removed
- `scn07-opb` still has the mistaken `pool:office` (F-SCN07-4).
- `scn19-opb` is unfrozen.
- kaifresh was not changed. Only read-only probes were sent to it; the write probes against kaifresh were withheld.
