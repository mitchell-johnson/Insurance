# Persona testing pack for the insurance core

This folder turns the persona library into a stress test for the insurance core. It follows the decision in the design notes under "Testing with the persona library": every persona gets an insurance history, every design scenario becomes a scripted journey for a named persona, and coverage is reported from tags on the journeys.

| File | What it is |
| --- | --- |
| `persona-library.md` | The source library as supplied: 60 people, 25 businesses, 15 situations, written for the bank |
| `personas-insurance.md` | An insurance history for every one of them, 15 more situations the insurance design names, and the cast the bank library lacks: advisers and advice businesses, reinsurers, large employers, providers, a delegate, banking partners, gap personas and staff |
| `user-stories.md` | 234 user stories, grouped by persona, each with an acceptance line |
| `test-matrix-1-spine.md` | Tests for the policy model, ledger, products, billing, collection, policy transfer, audience scoping and resilience |
| `test-matrix-2-lines.md` | Tests for underwriting, claims, reinsurance, unit pricing, group schemes, joint lives and ownership, commission, composable health and discounts |
| `test-matrix-3-operations.md` | Tests for servicing, the anniversary window, annuities, vulnerable customers, complaints, staff access, payment runs, finance close, configuration, incident runbooks, documents and migration |
| `traceability.md` | Hand-maintained. For each matrix test, the built tests under `crates/` that carry it out, marked full or part |
| `test-matrix.csv` | Generated. All 577 tests as one sheet for filtering, with how far each is built |
| `coverage.md` | Generated. Tests by area and phase, how far the built suite reaches, the priority 1 gaps by area, and the tests behind every persona, story and situation |

## How the pieces link

- A **persona** has an ID from the library (P, B, S) or from the added cast (D distribution, R reinsurers, E employers, N gap personas, ST staff, and TPA, PR, FA, BK for the delegate, providers, fund administrator and banks).
- A **story** is `US-<persona>-<n>`.
- A **test** is `<AREA>-<nnn>`, names the story it proves and the personas it runs on, and carries a type, a build phase and a priority.
- `traceability.md` maps a matrix test to the built tests that carry it out, as `file::test` paths under `crates/`.
- `tools/persona_matrix.py` checks every link and fails if a persona has no story or no test, a story has no test, a situation is not exercised, or the traceability names a matrix ID or a built test that does not exist. Run it after any edit: `python3 tools/persona_matrix.py`. CI runs it with `--check` on every push.

## What to run when

Phases follow the build roadmap in the design notes.

- **Phase A, the spine: 69 tests.** Filter the CSV on phase A. These need only a term life product, billing, earning, receipts, standing, lapse, reinstatement, cash matching, policy transfer, the read models and the simulated year. `RES-001` is the phase exit test.
- **Phase B, four slices: 142 tests.** Each slice has one exit journey:
  - Retail life: Tom Clarke (with P57). `UW-002`, `RI-001`, `RI-003`, `ADV-044`, `CLM-001`, `CLM-003`, `RI-004`.
  - Health: the Thompson family (P42). `CLM-011`, `CLM-012`, `HLT-004`, `JNT-018`, `AUD-002`.
  - Group: Kai Fresh Supermarkets (E1). `GRP-001`, `GRP-005`, `GRP-009`, `GRP-010`, `GRP-021`, `CLM-025`.
  - Investment: Priya Raman (P3) with Kevin Zhang (P21). `INV-001`, `INV-004`, `INV-010`, `INV-016`.
- **Phase C, widening: 346 tests.** The adviser scenarios, servicing, joint lives and ownership, annuities, operations and runbooks.
- **Phase D, migration: 20 tests**, run against the synthetic legacy book in S26.

Priority 1 (306 tests) means money, cover or privacy would be wrong if the test failed.

## What the stress comes from

The library was chosen for awkward lives, and the insurance histories lean into that:

- **Money that arrives badly**: fortnightly dishonours (P1, P35), poor references (B1), payments to the wrong policy (P14), duplicates (P46), foreign currency (B9), one transfer for four policies (P56), short employer remittances (B6).
- **Time that runs backwards**: backdated leavers, late-notified deaths, corrected dates of birth, late declarations, a move agreed late, a price corrected after people have left.
- **More than one person per policy**: joint owners who disagree (P18), adults who want privacy from each other (P4, P9, P32), trustees who change (B21), lenders with a say (P7), representatives with limits (P10, P23, P33).
- **People who cannot be rushed**: declining capacity (P46), a scam in progress (P50), hardship (P13), bereavement (P57), no digital channel at all (P16).
- **Scale and failure**: a 50,000-member scheme under the apply budget, 150,000 billings on one day, region loss during billing, a workflow outage on a benefit date, a bank channel down, a replay that must match to the hash.
- **Rules as code going wrong**: twelve incident runbooks, each ending in a remediation case.

## Design gaps the personas exposed

These are stories the design notes do not yet answer. Each has a test that is held open on purpose.

1. **Sex as a rating factor for a non-binary life** (P49, `PRD-011`).
2. **Whether a payee may decline late-payment interest**, and how that posts (P12, `CLM-037`).
3. **A customer-set hold on refunds and surrenders** with a cooling-off on removal. The design has a staff-recorded vulnerability pause but nothing the customer sets (P52, `SVC-022`).
4. **Bankruptcy**: how an insolvency official's claim on a surrender value is recorded and enforced (N5, `JNT-029`).
5. **Imprisonment** and its effect on an income protection claim (N6, `CLM-040`).

Four more are assumptions made here that the design should confirm:

6. **Relief applied in bulk by region** after a disaster. The design has hardship overrides one policy at a time. `INC-015` assumes a population query that applies them in bulk.
7. **Dishonours caused by someone else's outage or card compromise.** `COL-024` and `COL-025` assume a remediation case that waives fees and corrects standing.
8. **A court order freezing a policy** during a separation. The design relies on signing rules, which `JNT-003` tests, and does not model an order.
9. **Minimum age to own a policy**, and what a 16 or 17 year old may do alone.

## How far the build reaches

Read the "Built so far" section of `coverage.md`. On 13 Nov 2026 the built suite carried out 199 of the 577 tests in full and 119 in part; 166 priority 1 tests were not yet fully built. Most of the unbuilt ones are features not yet written rather than tests missing for features that exist: composable health and discounts (HLT, DSC), joint lives and ownership changes (JNT), vulnerable customers and representatives (VUL), surrender, maturity and loans (SVC), annuities (ANN), most of the dunning sequence (COL), the incident runbooks (INC) and migration cohorts (MIG). `coverage.md` lists them by area.

**When you write a test that carries out a matrix row, add a line for it to `traceability.md`.** A row marked full is a claim that the built tests check everything the matrix row expects; mark it part when they check some of it.

## Limits of this pack

- Expected results come from the design notes. Where a built test and the matrix disagree, the design notes and `docs/decisions.md` decide which is wrong.
- Dollar amounts, retentions, limits and day counts are fixtures chosen to make the arithmetic checkable. They are not product decisions.
- The tests are specifications. Turning each into an executable scenario with exact postings is the next step, starting with the 69 in Phase A.
- All names are invented.
