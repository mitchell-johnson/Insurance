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
| `stack/seed.js` | Sets the insurer up through the system's own routes: staff and their roles, complaints timers, the finance chart, claims rules, payments banks and channels, the distribution structure. |
| `lib/cast.js` | Who signs in: each persona, what they are and the roles the insurer gave them. |
| `lib/suite.js` | The harness: a `Journey` (the set-up a test starts from) and a `Screen` (one signed-in browser page). |
| `lib/oracle.js` | The independent calculator (see below). |
| `lib/group.js` | A group scheme set up as the insurer sets one up, and a membership file taken through its steps. |
| `tests/NN-area.js` | The tests. Each carries its persona, user story, the matrix rows it bears on, its purpose and why it is worth having. |
| `seed/` | Two small Rust tools: `ic-e2e-config` turns the shipped Starlark configuration into JSON for the seed, and `ic-e2e-close` closes a business day. |
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

## What it does not cover

- The customer app: the repository has its API and sync protocol but no
  screens, so there is nothing for a browser to drive.
- The customer master has no screens of its own. It is exercised through
  group schemes, where it decides who each joiner is. A person's own record,
  consents and holds are reached only by the customer app.
- Defender for Storage: a test writes the scan result on an uploaded file as
  Defender would.
- Group schemes beyond one product and one set of terms: pools, health menus,
  renewal, takeover, scheme split and billing over time are covered by the
  group module's own tests, not here.
- The underwriting intake, the document service, and the drivers that carry
  instructions between modules (reinsurance, unit pricing, the bank pump).
  Tests in those areas cover the consoles and their rules and say where a
  driver's part is missing.

## Reading a failure

A failing test is one of three things: the product is wrong (the report says
what the screen showed and what was expected), the stack is stale (restart
it), or the test's expectation is wrong. Screenshots of the failing page are
under `e2e/.run/results/shots`. A test that fails because the product is wrong
is left failing and the owner is told; it passes when the defect is fixed. A
test reported as flaky failed once and passed on its second attempt.
