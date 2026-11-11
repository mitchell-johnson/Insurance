# Underwriting and new business

`crates/underwriting` (`ic-underwriting`) takes a proposal from quote to policy: the disclosure, the rules, the people who decide what the rules cannot, the offer, and the one command that issues the policy in the core. It is a deterministic state machine beside the core, built to the contract in `docs/modules.md`. Its shipped configuration is in `products/underwriting/` and its authority policy in `config/cedar/underwriting.cedar`.

## What it does

- **Configuration as code, activated through the log** (`config.rs`, `shipped.rs`). Exclusion wordings, reason codes, question sets, rule sets, authority profiles and settings are Starlark files loaded into typed values. Each is activated by a command that names an author, two other approvers and a change reference. Versions only go up, a code list never loses a code, a changed rule set needs its backtest digest, and nobody approves their own profile. Everything is looked up by date, so a proposal is judged by what was in force when it was signed.
- **Two disclosure regimes.** Question set 1 is the duty of disclosure; set 2 is the reasonable-care duty from 15 November 2027. The regime a life answered under is recorded on the file and travels with every later non-disclosure review.
- **No clinical content in the log.** Answers are sealed in the clinical store (`clinical.rs`): AES-256-GCM under a key per person, that key wrapped by a vault key, the record's reference bound into the seal, every read given a purpose and recorded. Destroying a person's key ends access to all their records. Commands carry hashes, vault references and the per-cover assessment that `prepare_disclosure` works out from the answers before the command is proposed.
- **Signed steps** (`signing.rs`). Disclosure, a tele-interview confirmation, an offer response, a consent and a re-quote are signed on the applicant's device with ed25519 over a message that names the step, so a signature for one step cannot be replayed as another.
- **Routing** (`flow.rs`). The worst disposition across a life's covers decides the path: decline or defer goes to a person for review, a referral opens a case in the right queue, a tele-interview is scheduled, evidence is requested only with the applicant's consent to obtain records, and clean or rule-rated answers are decided by the rules unless the sum insured is over the straight-through limit.
- **Capacity before terms.** Every accepted cover reserves capacity with reinsurance before a decision takes effect. An automatic answer lets it proceed, a facultative answer waits for the reinsurer's terms and applies them, and an excluded answer returns the case to a person. A reservation is released whenever the cover it was for is no longer wanted and confirmed when the policy is issued.
- **Case work under authority** (`casework.rs`, `authority.rs`). Underwriters take, hand over and move cases, request evidence, ask a medical officer's opinion, and propose terms. A Cedar policy and the underwriter's profile decide what each may do: sum insured, loading and exclusion limits by grade, no work on a file where they declared a conflict, no countersigning their own decision. A decline, a deferral, a sum over the countersign limit or a loading over 100% needs a second signature from someone senior who has not touched the file.
- **Offers** (`intake.rs`). Standard terms are accepted at once. Anything else is an offer with an expiry that the applicant accepts, declines or asks to have looked at again. When one of two joint lives drops out, the other is asked again before a single-life policy is issued, because their earlier acceptance was of a different contract.
- **Issue.** When every life is final the module asks the core to issue the policy with one command bound to the quoted product version. On confirmation it confirms the capacity, sends the rating factors to reinsurance and ends any interim cover. A refusal from the core is kept with its reason and can be retried.
- **Interim accidental death cover**, where configured, is requested when the proposal opens, capped, and ended when the proposal ends in any way.
- **The clock** (`external.rs`). One `Sweep` command per day does bounded work in ID order: lapses proposals past their expiry, chases evidence and marks it unobtainable after the configured chasers, escalates cases nobody has touched, and expires offers.
- **Existing cover from the life index (addendum entry 2).** With the applicant's consent, the existing-cover question is pre-filled from the life index across partitions. Without consent nothing is read. What was read, and that it was pre-filled, is recorded on the file, and the applicant's own answer is still theirs to give.
- **Adviser licence loss (addendum entry 4).** When an adviser's licence is suspended or lost, their access to the file ends at once and the applicant is told. The proposal continues without them.
- **Delegated underwriting** (`special.rs`). A delegate's decisions come in with the delegate's own reference, are checked against the delegated authority (products, sums, loadings, dates, tolerance for late reporting) and are refused or sent to a person where outside it. The delegate's file can be called for and its arrival recorded.
- **Migrated policies.** Terms on a migrated policy are carried as data from the old system. Nothing is defaulted.
- **After issue.** Claims asks for the terms on a cover, the underwriters who touched a life, and the file to be opened for a claim. A non-disclosure review needs seniors who never touched the file and two of them for a remedy. Peer review draws a sample of decisions for a senior who had no part in them.
- **Refusals leave no trace.** A refused command changes nothing. A repeated command gets its first result, marked as a repeat.

## How it talks to the rest

In: commands in an `Envelope` (`key`, `caller`, `at`, `today`, `command`). Callers are fixed names: `config`, `new-business`, `migration`, `adviser-portal`, `evidence`, `underwriting-console`, `reinsurance`, `issuer`, `timer`, `adviser-licensing`, `claims` and `delegate-api`.

Out, on each applied command: events for the audit trail, and `Effect`s, each with a stable key that is its idempotency key at the receiver. Effects are for reinsurance (reserve, release and confirm capacity, record risk factors), the core (issue the policy, issue and end interim cover, apply terms, a non-disclosure remedy), evidence providers (request, chase, cancel), a delegate (send the file) and people (notices).

Queries (`special::Query`, answered by `special::answer`): a proposal, the terms on a policy's cover, the underwriters of a life, a queue with ages, an underwriter's workload, a report over a period, and the exclusion library on a date.

For claims: `codes::ExclusionLibrary::matches` decides whether an exclusion applies to a coded cause, and `codes::CAUSE_ROOTS` lists the roots cause codes hang from. Claims owns the coding of a cause; this module owns the wordings and the match.

Hosting: `ic_underwriting::module_spec()` is in `ic_node::modules()`, so every ledger node hosts the module under the name `underwriting`. It starts empty and takes its configuration through the log.

## Known gaps

- **No drivers.** The module produces effects. The processes that carry them to reinsurance, the core and evidence providers, and bring the answers back as commands, are not built. Until they are, nothing here reaches a policy outside the tests.
- **The clinical store has no Azure adapters.** The store is written against two traits, a key vault that wraps and unwraps and a blob store. Only an in-memory blob store and a local vault exist. The Key Vault and Blob Storage implementations belong in `ic-azure` and are not written, so the store must not be used with real clinical records yet.
- **Core commands that do not exist yet.** The core has no command to add or replace a layer for revised terms after issue (`Effect::ApplyTerms`), none to vary or avoid a policy as a non-disclosure remedy, its cancel command accepts only `policy-servicing` so interim cover cannot be ended by this module, and issue carries no flag for a migrated policy.
- **Reserve capacity does not carry risk codes.** Reinsurance accepts optional `risk_codes` on a reservation; the effect does not send them, so a treaty exclusion by risk is found at the facultative step and not before.
- **The idempotency key set grows without bound.** The last 20,000 results are kept to answer repeats in full; older keys are remembered but never dropped. It needs a horizon agreed with the drivers.
- **Peer review draws by scanning every proposal**, which is fine for the book the tests build and too slow for a large one.
- **Staff grants are not modelled.** Whether a decision on a staff member's own policy needs a different approver is a channel rule today and is not checked here.
- **No Temporal workflow per proposal.** The design describes one; the state machine and the daily sweep do the same work with one fewer moving part. Timers are therefore accurate to the day, not the hour.
- **Proposal progress is not published to the portals.** The channels module can take rows; nothing sends them yet.
