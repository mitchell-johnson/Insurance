# User scenario testing

The scripted e2e journeys (`e2e/tests`) each check one step. Scenario testing checks whole tasks the way people do them: a goal, a starting situation, a realistic complication, hand-offs between people, and mistakes along the way. Each round has three parts:

1. **Scenarios.** An advisor writes realistic tasks across the personas in `docs/testing/personas/`, on the portal screens and through the `/mcp` endpoint, with what "good" looks like for each.
2. **Operators.** Each scenario is carried out as that user against the e2e stack (`node e2e/stack/stack.js`), on the real screens with a browser and through `/mcp` with the user's own sign-in. Operators record every finding (`F-<scenario>-<n>`) with what they did, what happened, and what good would be.
3. **Fixes.** Findings are fixed by area, each with its tests in the same change, and merged to main. Each fix report gives every finding a line: fixed, partly fixed, decided (with the decision in `docs/decisions.md`), not a defect, or left with a reason.

## Round 1 (17 Nov 2026)

24 scenarios (SCN-01 to SCN-24) in [round-1/scenarios.md](round-1/scenarios.md), run by six operators ([round-1/reports](round-1/reports)) and fixed by four areas ([round-1/fixes](round-1/fixes)).

| Area | Findings | Fixed | Partly | Decided | Not a defect or not reproduced | Left |
|---|---|---|---|---|---|---|
| Portal, customers, advisers and MCP ([fix-portal](round-1/fixes/fix-portal.md)) | 67 | 46 | 6 | 1 | 1 | 7 (and 6 fixed earlier) |
| Group schemes and employers ([fix-group](round-1/fixes/fix-group.md)) | 36 | 25 | 2 | 1 | 2 | 6 |
| Claims, payments and underwriting ([fix-claims](round-1/fixes/fix-claims.md)) | 37 | 27 | 4 | 1 | 2 | 3 |
| Back office: finance, pricing, commission, complaints, records, reinsurance ([fix-back-office](round-1/fixes/fix-back-office.md)) | 52 | 34 | 9 | 1 | 2 | 6 |

Counts are as each area recorded them; a finding raised by two operators can appear in two areas.

**The most serious finding.** A customer's assistant, holding only the customer's sign-in, could cancel cover through `/mcp` without the phone signature a cancellation needs (SCN-10). Cancelling a policy and changing the payment frequency now need the customer's signature on every channel, like money movements (`needs_signature` in `crates/channels/src/state.rs`). In the same change, an adviser's prepared request no longer waits on the customer master, and a licence status recorded in error can be put right the same day (`record_licence` in `crates/distribution`).

**Other themes fixed across areas.**

- Refusals from other services reach the screens and assistants in words that say what to do, not as codes.
- Pages take today's date from the books' business date, not the browser clock.
- The MCP tool list offers each caller only the tools their sign-in and permissions allow, and every tool describes its fields, money in cents and which tools change nothing or cannot be undone.
- People are picked by name (customers, colleagues, complainants) rather than by a typed reference.
- The e2e stack now runs the documents, underwriting intake, underwriting and reinsurance services. It also seeds the providers, schemes and staff roles that the scenarios needed and the scripted suite lacked.

**Left after round 1** (each with its reason in the fix report):

- Commission books, schedules and attribution are not seeded on the e2e stack (F-20-1).
- The claims cover check does not yet show the core's cover facts (F-11-4).
- A complainant's own words need inbound storage in the documents vault (F-21-2).
- Registering a scheme from an issued policy needs its own route and terms form (F-SCN19-1).
- A reused idempotency key with a different body is not yet refused (F-SCN02-4).
- Employer administrators on the e2e stack have no customer-master record (F-SCN07-8).
