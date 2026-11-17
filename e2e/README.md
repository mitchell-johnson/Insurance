# Persona end-to-end tests

More than three hundred journeys through the system's own web pages, each one
a persona from `docs/testing/personas/` doing a piece of their work against a
real stack: three ledger nodes, the API, the portal, the group service, the
customer master on PostgreSQL, and a stand-in for Azure's sign-in and storage. Nothing inside the system is replaced by a test double.

## Running it

```
e2e/build.sh                 # the system's binaries, the suite's two tools, the browser
node e2e/stack/stack.js      # starts and seeds the stack; leave it running
node e2e/run.js              # all of them; results in e2e/.run/results/results.md
```

`ONLY=E2E-014,E2E-020`, `AREA=Claims`, `WORKERS=4` and `RETRY=0` narrow or
tune a run. The stack keeps its state under `e2e/.run` and wipes it on start.
The portal is at http://127.0.0.1:17090 while the stack runs, with every
persona's sign-in available from the stand-in sign-in page.

The customer master needs PostgreSQL. The stack starts a server of its own
from the PostgreSQL installed on the machine (`IC_E2E_PG_BIN`, or the newest
under `/usr/lib/postgresql`, or the `PATH`), or uses a running one named by
`IC_E2E_POSTGRES_URL`, as the workflow does.

On GitHub the suite is the `e2e` workflow, started by hand. It is not part of
`ci.yml`: it needs a browser, it takes far longer than a commit should wait,
and a failing journey is a finding about the product to be read by a person.

## How it is put together

| Where | What |
|---|---|
| `stack/stack.js` | Starts the real binaries from `target/debug` and waits for them. |
| `stack/seed.js` | Sets the insurer up. Each module's configuration (complaints timers, the finance chart, claims rules, payments banks and channels, the underwriting libraries and underwriters' profiles, the reinsurance treaties) is written as a set-up file and sent by the operator's tool, `ic-admin apply`; staff, the distribution structure, commission books, rules and schedules, payees, collections' dunning sequences and the group schemes go in through the portal's own routes, as the people whose work they are. |
| `stack/play.js` | The parts outside the insurer's screens a scenario operator plays: new business opening a proposal or issuing a policy and saying who advised it, and an evidence provider answering an order (see below). |
| `lib/cast.js` | Who signs in: each persona, what they are and the roles the insurer gave them. |
| `lib/suite.js` | The harness: a `Journey` (the set-up a test starts from) and a `Screen` (one signed-in browser page). |
| `lib/oracle.js` | The independent calculator (see below). |
| `lib/payments.js` | The payments register as the payments team keeps it: a payee entered, confirmed by a second maintainer and screened. Claims pick their payees from it. |
| `lib/group.js` | A group scheme set up as the insurer sets one up, and a membership file taken through its steps. |
| `tests/NN-area.js` | The tests. Each carries its persona, user story, the matrix rows it bears on, its purpose and why it is worth having. |
| `seed/` | Two small Rust tools: `ic-e2e-config` turns the shipped Starlark configuration into JSON for the seed (with an application template for each retail product, the AIA range included, and each product's version and cover types), and `ic-e2e-close` closes a business day. |
| `run.js` | Runs the tests four at a time, retries a failure once (a pass on retry is reported as flaky), and writes the report. |

A matrix reference on a test means the test bears on that row of
`docs/testing/personas/test-matrix.csv` through the screens. It does not mean
the row is fully proven here; `docs/testing/personas/traceability.md` remains
the record of that.

## The independent calculator

Tests that check money or dates do not ask the system whether it is right.
`lib/oracle.js` works the answer out separately: it reads the shipped product,
rate and fund files as text, the way an actuary would read them, and does its
own arithmetic in whole cents with its own rounding. It shares no code with
the system. A test then compares what the screen shows with what the
calculator gives, and a failure names both figures.

It covers premiums (stepped and per-life rates, age last birthday, modal
loading, multi-cover discount, policy fee, GST), billing periods, the days on
which cover is suspended and a policy lapses, the arrears a reinstatement
needs, what is payable on a claim after unpaid premium, a fund's buy and
sell prices, and a group life member's cover and premium.

## Policy lifecycles and closing days

The area "Policy lifecycle" (`tests/08-lifecycle.js`) follows policies through
time: issue, first notice, payment in full and in part, money paid ahead,
duplicate receipts, grace and suspension, lapse, reinstatement, cancellation,
and the first anniversary with indexation and re-rating at the new age.

Time moves by closing business days. `ic-e2e-close` runs the same end-of-day
function the Temporal worker runs (`ic_workflows::run_end_of_day`) against the
real nodes, about a third of a second a day. Temporal itself is not part of
this stack, so the durability of the workflow (retries, crash recovery) is not
exercised here; the failure drill covers that.

Tests that move the date carry `lock: "business-date"`. The runner runs them
last and one at a time, because every other test assumes the opening date
(2027-11-15). After a run that included them, restart the stack before running
other areas again.

## Group schemes and the customer master

The area "Group schemes" (`tests/11-group.js`) drives the group console as
group administration staff and the employer portal as an employer's scheme
administrators. Behind them run the group service with its driver and the
customer master, which is asked who each joiner in a membership file is.

The seed registers one standing scheme (`kaifresh`) that tests only read. A
test that changes a scheme registers its own with `lib/group.js`: the policy
is issued on the core as new business issues it, and the scheme and its file
layout are registered through the group console's route.

An uploaded file waits in quarantine for its virus scan. Defender for Storage
is not part of the stack, so the test writes the scan result on the file as
Defender would, and from there the system works by itself: the driver reads
the file, the customer master matches the joiners, the core takes the members
and bills them. The first bill is compared with the independent calculator.

The customer master matches people on name and date of birth, across every
scheme in the stack. Tests therefore give their members random names and
birth dates, except the one test that wants two records to look alike.

## The customer page

The area "Customer" (`tests/12-customer.js`) drives `/customer`, the page
Mitchell decided on 14 November 2026 that customers should have in this
repository. Each journey has a customer of its own (`lib/customer.js`): a
record opened in the customer master by its own route, as new business opens
one, a sign-in bound to it, and a phone enrolled after identity verification.
Approving, changing details and taking a copy of one's record need a signing
browser: the page makes the key, and the test signs it with the phone's key as
the phone app would.

## Playing new business and an evidence provider

No channel opens a proposal yet (`docs/underwriting.md`): the new business
officer can price an application (the `quote` route and MCP tool) but nothing
on a screen keeps it, and the adviser portal has no application. A scenario
therefore plays new business with `node e2e/stack/play.js`, which sends what
new business's service sends, as that service, and prints what it did:

```
# a proposal for AIA Living from a shipped template, advised by Tessa Maddox under Harbourline
node e2e/stack/play.js propose '{"ref":"SCN202-1","template":"aia_living","party":"<party>","born":"1986-04-02","adviser":"adv-d1","fap":"fap-d6"}'
# what underwriting holds on it: each life's stage, cases, and the evidence requests a provider answers
node e2e/stack/play.js proposal '{"proposal":"prop-scn202-1"}'
# the provider acknowledges the GP report, then sends it with its cost
node e2e/stack/play.js evidence '{"request":"<request>","outcome":"acknowledged","provider_ref":"GP-77"}'
node e2e/stack/play.js evidence '{"request":"<request>","outcome":"received","report":"BP 150/95 on 3 Nov, 135/85 on 10 Nov","cost_minor":12500}'
# a term life policy issued straight from the template, written by Tessa (commission is told who advised it)
node e2e/stack/play.js issue '{"ref":"SCN221-1","template":"term_life","party":"<party>","adviser":"adv-d1","fap":"fap-d6"}'
```

`propose` takes the core's quote of the application (`POST /v1/quotes`) and
sends `open_proposal` to the underwriting module on the ledger nodes as the
caller `new-business`; AIA Interim Cover is written at once where the product
has it, and the customer then answers and signs on `/customer`. `evidence`
posts to the underwriting intake's `POST /v1/evidence` with a token for the
stand-in provider (`ic-e2e-evidence-provider`, role `evidence`). `issue`
posts the application to `POST /v1/policies` and, with an adviser, sends
commission `advise_new_business` as new business does once a policy is
issued; `advise` does the second part alone. Templates are under
`applications` in `e2e/.run/shipped.json`: `term_life`, `income_protection`,
`health_family`, `invest_plan`, `group_scheme`, and `aia_essentials_life`,
`aia_living`, `aia_living_business`, `aia_cover_for_life`, `aia_starter`,
`aia_start_up_income`, `aia_private_health`, `aia_cancer_care`. Amounts are
in cents (`"covers":{"life":40000000}` is $400,000). The engagements are
`adv-d1` and `adv-d2` under `fap-d6`, `adv-d9a` under `fap-d9`, and `adv-d4`
and `adv-d5` under `fap-kowhai`, each with a commission book.

## What it does not cover

- The customer's phone app. The customer page is driven in the browser, and
  the phone's part (holding the key that approves a new browser) is played by
  the test with a key it makes; every check of a signature is the system's.
- Defender for Storage: a test writes the scan result on an uploaded file as
  Defender would.
- Group schemes beyond one product and one set of terms: pools, health menus,
  renewal, takeover, scheme split and billing over time are covered by the
  group module's own tests, not here.
- The unit pricing driver and the bank pump. The stack runs the documents
  service, the underwriting intake and the underwriting and reinsurance
  drivers (added 17 November 2026 after the scenario run found whole consoles
  that could not work without them); the unit pricing file reader and the
  bank are played by the tests where they are needed, which say so.
- The finance end-of-day feed before a day closes: the books have no period
  until the first business day closes, so journals and the close are tried
  only by tests that close a day.

## Reading a failure

A failing test is one of three things: the product is wrong (the report says
what the screen showed and what was expected), the stack is stale (restart
it), or the test's expectation is wrong. Screenshots of the failing page are
under `e2e/.run/results/shots`. A test that fails because the product is wrong
is left failing and the owner is told; it passes when the defect is fixed. A
test reported as flaky failed once and passed on its second attempt.
