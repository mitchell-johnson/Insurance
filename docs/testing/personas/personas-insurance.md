# Insurance histories for the persona library

This file extends `persona-library.md` (written for the bank) into the synthetic insurer described in the design notes under "Testing with the persona library". Every library persona keeps its ID. Each gains an insurance history that fits its life story, and the cast is widened with the people the insurance design needs and the bank library does not have: advisers and advice businesses, reinsurers, large employers, providers, staff, and the gap personas the design names.

Everything here is invented. Any resemblance to a real person or business is coincidental. Clinical detail is deliberately absent from this file: where a persona has a loading, exclusion or claim, the reason lives only in the clinical store fixtures, so the health boundary is exercised by the tests and not bypassed.

Simulated time starts on 1 January 2027. The Contracts of Insurance Act commencement date used in scenarios is 15 November 2027. The first leap day is 29 February 2028.

## Product catalogue used by the personas

| Code | Product | Line | Notes |
| --- | --- | --- | --- |
| TERM0 | Simple Term Life | Life | Direct, no commission, no reinsurance. The spine product for Phase A |
| LIFE1 | Advised Life and Trauma | Life | Life, trauma, total permanent disability, stepped or level premium, indexation, waiver rider, future insurability option. Commission and surplus treaty |
| IP1 | Income Protection | Life | Monthly benefit, waiting and benefit periods, offsets, partial disability, waiver of premium |
| HLTH1 | Mix and Match Health | Health | Composable shell: hospital and surgical (core), specialist, dental, optical, mental health, physio, maternity, cancer care, overseas treatment |
| GRPL | Group Life and Income Protection | Group | Benefit categories, automatic acceptance limit, continuation option. Named, unnamed and pool bases |
| GRPH | Group Health | Group | Employer-paid core, member-paid options and dependants as voluntary cover |
| UL1 | Investment Bond | Investment | Unit-linked. Funds F-CASH, F-BAL, F-GRO in NZD and F-USEQ in USD |
| CGA1 | Capital Guaranteed Account | Investment | Declared crediting rate on the accrual engine, no units |
| WOL0 | Whole of Life (closed book) | Life | Participating, surrender value, policy loan, automatic premium loan, paid-up option. Migrated |
| END0 | Endowment (closed book) | Life | Matures at term end. Migrated |
| ANN1 | Annuity | Annuity | Immediate, term certain, deferred, joint and impaired-life, guarantee period, escalation |

## Part A: Individuals

| ID | Persona | Insurance history | What it stresses |
| --- | --- | --- | --- |
| P1 | Aroha Ngata, 19 | TERM0 for $100,000 bought in the app, fortnightly direct debit timed to payday. Below minimum hours for her employer's scheme at E1 until her hours rise | Low balance and frequent dishonours, retry-once, grace without lapse, scheme eligibility by hours, outage on a collection day |
| P2 | Liam O'Connor, 24 | TERM0 and IP1 paid by card. Card replaced twice. Occupation class changes when he qualifies. Recorded as a non-smoker | Card token updates, dishonour that must not reset the lapse clock, change of rating fact, misstated smoker status found later |
| P3 | Priya Raman, 28 | UL1 with monthly contributions and switches, HLTH1 quoted many ways, LIFE1 with a wellness tier. Uses every feature | Forward pricing, switches across funds, quote equals issue, live composition dry runs, wellness entitlement, definitions versions and pass-back |
| P4 | Tama Hohepa, 31 | Life insured and adult on the family's LIFE1 and HLTH1 policies owned by Mere. Children aged 4 and 1 | Arrears and catch-up each winter, suspension and reinstatement, adult dependant privacy from the policyholder |
| P5 | Mere Hohepa, 30 | Policyholder of the family LIFE1 (two single covers) and HLTH1. Runs the budget to the dollar | Billing day and frequency, policy fee and allocation order, dishonour fee, newborn added backdated to birth, premium notice accuracy to the cent |
| P6 | Graham Whitfield, 67 | WOL0 with bonuses, END0 maturing in 2027, late wife's claim in history. Prefers the phone | Maturity options and reinvestment into ANN1, bonus declaration, assisted channel prompt, payee change cooling-off |
| P7 | Sione Taufa, 42 | Joint first-event LIFE1 with Ana, lender's interest recorded for the home loan. Life on the family HLTH1 | Joint first event, lender notices and consent, assisted service at a counter, five children with the charge-for-first-children rule |
| P8 | Ana Taufa, 40 | Joint owner with Sione. Policyholder of the family HLTH1. Manages everything in the app in short bursts | Joint owner signing rules, async writes on a phone, notices with no sign-in links, straight-through health claims |
| P9 | Losa Taufa, 17 | Child on the family HLTH1. Turns 18 in the scenario year | Becoming an adult dependant, own sign-in, parent visibility ending, later ageing out with a continuation offer |
| P10 | Margaret Chen, 74 | Own WOL0, paid up. Acts under an enduring power of attorney for her sister N10. Desktop only | Authorised representative scope, statement export that reconciles, 50-year-old migrated policy, legacy wording |
| P11 | Josh Harding, 35 | IP1 with a 13-week wait, LIFE1 trauma, yearly premium often paid ahead from his business account | Pay-ahead credit, payer who is not the owner, income offsets and partial disability, trauma event overseas, card declines abroad |
| P12 | Fatima Al-Sayed, 38 | Family HLTH1 with four children, TERM0 on herself and Omar. Prefers Arabic for documents. Avoids interest | Translated documents with the primary version controlling, per-party presentation preference, interest on a late payment she may decline, evidence gaps from overseas history |
| P13 | Dave Pākura, 55 | Level-premium LIFE1 with a lender's interest, WOL0 with a surrender value. Redundant and falling behind | Dunning sequence, call reaching the other owner, hardship override, automatic premium loan before lapse, early resumption |
| P14 | Chloe Bennett, 22 | Named member of the public service scheme E2 with voluntary top-up cover she pays herself | Member registration, voluntary cover with its own receivable and standing, premium paid to the wrong reference |
| P15 | Rangi Walker, 48 | TERM0 in his own name after a messy split of a joint policy. Pool-basis cover through the farm owner. Ex-partner still nominated | Offline app used weekly, beneficiary nomination on the date of death, migrated ownership history |
| P16 | Ruth Abrahams, 81 | WOL0 paid up and a small ANN1. Post only, no devices | Post channel with proof of delivery, undeliverable mandatory notice, proof of life by signed declaration, family asking without authority |
| P17 | Ben Kowalski, 29 | LIFE1 written by D1 on 1 March 2027. Key person cover owned by B3 and assigned to the overdraft lender. Calls the API from scripts | The commission worked example with a lapse in January 2029, idempotent retries, assignment to a lender |
| P18 | Hemi and Kiri Parata, 36 and 34 | Joint first-event LIFE1 with a lender's interest, family HLTH1 | Separation: split without underwriting, both owners to sign, lender consent, policy transfer between groups, a second interest by notice date |
| P19 | Oliver Trent, 52 | Large LIFE1 and IP1, part owned personally and part by B20. Investment property lender's interest | Dual sign-off, facultative reinsurance, per-life accumulation across partitions, service timers |
| P20 | Sarah Mitchell, 44 | Level-premium LIFE1 split out of a joint policy at divorce. Member of a teachers' GRPH scheme with her teenagers as voluntary dependants | Irregular income and dishonours, voluntary dependant cover, minors as beneficiaries paid through the estate |
| P21 | Kevin Zhang, 61 | Large UL1 across three funds with regular withdrawals, three LIFE1 policies each with a lender's interest. Wife Lin prefers Mandarin | Price error after a withdrawal, several interests ordered by notice date, single premium top-ups and AML rules, tax residency reporting |
| P22 | Jess Morgan, 26 | HLTH1 with the mental health component, TERM0 bought on impulse and cancelled in the free-look period | Self-declared vulnerability marker and adjustments, cooling-off refund, per-cover underwriting terms, pre-billing alerts |
| P23 | Tane Rikihana, 39 | IP1 and LIFE1 with an occupational loading. Partner holds a limited written authority. Blended children on HLTH1 | Limited-scope representative, pay-ahead while at sea, event with no body recovered, hazardous occupation class |
| P24 | Emily Foster, 33 | LIFE1 and HLTH1 with maternity. On parental leave | Premium holiday, future insurability option on a birth, newborn window, component waiting periods |
| P25 | Patrick Doyle, 58 | Life insured on policies owned by the partnership B12, with the rural lender's interest. Yearly premium after the lamb cheque | Partnership ownership, yearly frequency, succession adding a life and an owner |
| P26 | Nikau Pene, 20 | Unnamed member record at labour hire employer E3, hours vary | Eligibility by hours week to week, leaver and continuation window, mistaken refund paid to him and recovered |
| P27 | Dr Anjali Desai, 46 | LIFE1 written by adviser D3 years ago, IP1 owned by B7, UL1, child at university on HLTH1. Appoints D4 in 2028 | Replacement under a new adviser with a top-up, company-owned cover, accountant with read-only view, student dependant |
| P28 | Wiremu Tipene, 70 | Small WOL0. No branch, poor coverage, mokopuna helps with the app | Support person who helps but cannot decide, named owner instead of a call queue, cyclone relief by region |
| P29 | Hannah Lee, 30 | IP1, LIFE1 written by D1, UL1 owned jointly with Matt | Future insurability option on marriage in 2029 creating a layer in the adviser's new book, joint owners both to sign |
| P30 | Frank Russo, 63 | LIFE1 with trauma, key person cover owned by B13, yearly premium | Partial trauma payment that reduces linked life cover, cyclone hardship, cover ending when the business is sold |
| P31 | Grace Okafor, 41 | LIFE1 on herself and on N3, IP through the hospital's scheme, mother overseas as beneficiary | Non-resident life insured, overseas evidence, overseas payee verification and screening |
| P32 | Toby Nash, 16 | Child on his parents' HLTH1 with dental. Reaches the configured age for controlling his own health information | Age-of-control setting, parents seeing counters but not claims |
| P33 | Lorraine Brady, 59 | Own TERM0. Welfare guardian for N11. Seasonal member at E4 | Court-ordered representative, accountability report of actions taken, seasonal joiner and leaver |
| P34 | Arjun Mehta, 34 | $5 million LIFE1, UL1 in F-USEQ with single premium top-ups at bonus time. Will leave over one bad experience | Facultative referral, underwriting service timers, USD fund and non-pricing days, replacement monitoring |
| P35 | Moana Kahu, 27 | TERM0 fortnightly on benefit day. Often short | Hardship offered proactively, billing day change, no re-presentment without a conversation, fees waived after an outage |
| P36 | Stuart Campbell, 65 | IP1 expiring at 65, ANN1 bought with business sale proceeds, UL1 in drawdown, key person policy on Dan Reeves assigned to him | Expiry with notice, annuity quote equals issue, regular withdrawals, a non-lender interest |
| P37 | Yuki Tanaka, 23 | Unnamed member at B24, small TERM0. Leaves New Zealand in the scenario year | Planned cancellation, refund to an overseas account, payment returned after closure, plain-language and translated documents |
| P38 | Pita and Lagi Fuimaono, 70 and 68 | Joint last-survivor WOL0 with a policy loan, lender's interest for the home loan | Large surrender paused for a conversation, children without authority, view-only family member, first death that pays nothing |
| P39 | Rebecca Sinclair, 37 | Joint owner with James of LIFE1 and HLTH1, either to sign | Equal authority for a non-earning partner, correspondence to both, cover for a partner with no income |
| P40 | Hone Waititi, 45 | Named life on B4's scheme, policyholder of a family HLTH1 and a LIFE1, trustee of B21 which owns a policy | One person, several hats: contact points per relationship, one device with every role, discount that excludes the scheme |
| P41 | Claire Dubois, 52 | UL1 funded in tranches from a French inheritance, IP1 as a sole trader | Source-of-funds case on each tranche, dual tax residency in the reporting file, previous insurer evidence from overseas |
| P42 | Mike Thompson, 49 | Policyholder of a blended family HLTH1: Donna, her two teenagers, his adult daughter. LIFE1 with a new nomination | Health slice exit journey: pre-approval, excess, limit reached, aged-out child, family privacy. Future insurability on marriage |
| P43 | Sophie Williams, 31 | IP1 with a 13-week wait, paid monthly | Payment arrangement on arrears, billing day change, benefit offset against accident compensation |
| P44 | Raj and Sunita Patel, 58 and 56 | LIFE1 owned by B22 with the commercial lender's interest, premiums by card, son Vikram a student dependant on HLTH1 | Card compromise and reissue, trustee-owned policy, paper schedules |
| P45 | Isaac Brown, 25 | TERM0 and membership of a defence scheme. Deploys overseas for six months. Mother holds a limited authority | Overseas posting rule, exclusion code that may apply to an event, delegated authority, everything automated |
| P46 | Beverley Simmons, 77 | WOL0, ANN1. Daughter Fiona holds an enduring power of attorney not yet activated | Duplicate premium payment, activation of the authority, abuse indicator on a beneficiary change, proof of life |
| P47 | Dion Reihana, 33 | IP1, key person cover owned by B6 and assigned to the overdraft lender | Repeated dishonours suspending an authority, assignment, small scheme for two apprentices |
| P48 | Helen Doyle, 57 | Own LIFE1 and HLTH1 apart from the farm, member of a health employer's scheme. Joint owner in B12 | Own identity and contact points, multi-life discount, correspondence that never assumes who the farmer is |
| P49 | Alex Rivera, 29 | TERM0, partner as beneficiary and adult dependant on HLTH1. Legal name changed | Name and title change with a reissued schedule, sex as a rating factor for a non-binary customer, unmarried partner |
| P50 | Barry Hughes, 71 | UL1 and WOL0 with large values | Romance scam: surrender, payee account change to overseas, beneficiary and ownership change to a stranger, daughter without authority |
| P51 | Mei Lin Wong, 36 | Family HLTH1 with newborn twins, UL1 for education owned by B23, consolidated billing across policies. Husband Steven holds large IP1 | Newborns, trust ownership, multi-policy billing view, maternity claims |
| P52 | Caleb Stone, 21 | TERM0. Overpays and asks for refunds, cancels and rebuys | AML rule for overpayment then refund, cooling-off used repeatedly, friction without judgement |
| P53 | Ngaire Thompson, 63 | IP1 and LIFE1 with a migrated premium holiday from 2020. Satellite internet | Unreliable connection during a signed step, frequency change at anniversary, migrated override |
| P54 | David Park, 44 | LIFE1 assigned to the bank before a term loan draws. Administrator of B15's scheme | Commands arriving at the business date cutoff, adding a site as a bulk join, employer admin rights |
| P55 | Lucy Hartley, 19 | Aged out of her parents' family health plan, took the continuation offer. Parents pay her premium | Continuation without underwriting with terms and counters carried, payer who is not the owner, device loss and recovery |
| P56 | Ahmed Hassan, 50 | IP1 as a self-employed driver, TERM0. Pays premiums for several relatives' policies. Beneficiaries overseas | Payer-not-owner rule and its false positives, who can be a dependant, overseas beneficiaries |
| P57 | Vanessa Clarke, 47 | Joint first-event LIFE1 with Tom (lender's interest), family HLTH1 owned by Tom, Tom's sole LIFE1 with a loading written by D1 | Retail life exit journey and S1: death claim, survivorship, lender paid first, estate with an overseas executor, surviving adult becomes policyholder |
| P58 | Jordan Lee, 38 | LIFE1 accepted, IP1 declined after a financial referral. UL1 with large irregular contributions | Financial underwriting, decline with two signatures, repeated source-of-funds, complaint about how it felt |
| P59 | Elsie Marsh, 88 | ANN1 with a guarantee period. Niece in Australia holds the power of attorney | Proof of life by letter, overseas representative, death notified late and recovery from the estate |
| P60 | Marcus Fonoti, 32 | LIFE1 and IP1 with terms for a professional athlete, named life on his club's scheme. Related to staff member N7 | Occupational terms, evidence from an overseas insurer, public figure access monitoring, staff conflict routing |

## Part B: Businesses

| ID | Business | Insurance history | What it stresses |
| --- | --- | --- | --- |
| B1 | Kaiti Corner Dairy | Owner Hemi Walker holds IP1 with a long wait. Premiums arrive by direct credit with poor references | Cash matching by payer account and amount, suspense ageing |
| B2 | Bright Sparks Early Learning | GRPH scheme, 12 named staff, tiers single, couple and family. Two directors approve above a threshold | Small named scheme, second administrator rule, employer never seeing dependants |
| B3 | Tidewave Software Ltd | GRPH menu scheme through a payroll API, key person cover on P17 | API intake with preview, employer-paid core with member-paid options, founder guarantee and assignment |
| B4 | Waititi Construction Ltd | GRPL scheme covering directors and staff as named lives, P40 is administrator and member | The several-hats worked example, occupational classes on site |
| B5 | Campbell Plumbing Ltd | GRPL for ten staff, key person policy whose ownership follows the shareholding | Ownership transfer in stages, administrator changes, interest held by the vendor |
| B6 | Reihana Electrical Ltd | Three-member scheme, key person cover | Employer remittance short payments, scheme standing before member impact |
| B7 | Desai Anaesthesia Ltd | Owns IP1 and business expenses cover on P27 | Company as policyholder, accountant as a read-only relationship |
| B8 | Sophie Williams Design | Pays P43's premiums from the business account | Payer-not-owner rule cleared by relationship |
| B9 | Dubois Translations | Premiums occasionally paid in euros from a French account | Foreign currency receipt and matching, reference data mangled in transit |
| B10 | Patel Superette | GRPL scheme, premiums by card | Card collection, two schemes with common owners |
| B11 | Patel Liquor | Second scheme. Its terminal is the point of compromise in S5 | Bulk card compromise affecting premium collection |
| B12 | Doyle Farming Partnership | Owns joint life policies on P25 and P48 with the rural lender's interest. Connor joins as partner and life | Partnership admission, seasonal yearly premium, lender consent to changes |
| B13 | Russo Vineyard | Key person cover on P30, seasonal facility lender's interest | Sale of the business, regional hardship |
| B14 | Westcoast Hideaway B&B | Partnership-owned cover on P53 and Gary | Migrated hardship override, seasonal income |
| B15 | Park Hospitality Group Ltd | GRPL scheme, 30 members across two sites becoming three, weekly payroll integration | Billing groups per site, concentration per site, bulk join |
| B16 | Whangārei Community Foodbank | Pool-basis GRPL for volunteers by declared headcount, deposit premium | Declarations with a due date, adjustment premium, administrator handover every two years |
| B17 | Ngāti Kahu ki Whaingaroa Trust | Affinity scheme for registered members on unnamed member records, hundreds of members, delegated underwriting through TPA1 | Delegated authority ceilings, batch member intake, governance approvals |
| B18 | Taranaki Netball Club | Asked for a quote for player cover and did not proceed | Proposal that lapses with notice, nothing left in suspense |
| B19 | Ridgeline Property Developments Ltd | Key person cover on director Mark Ellison assigned to the construction lender. He is also insured through a second company | One life under policyholders on different partitions, accumulation, facultative |
| B20 | Trent Family Trust | Owns a LIFE1 policy on P19. Three trustees, any two to sign | Trustee-owned policy, signing rules, independent trustee |
| B21 | Waititi Whānau Trust | Owns a policy on its chair. Trustees change in S13 | Change of trustee inside the group, remote verification of a trustee in Sydney |
| B22 | Patel Property Trust | Owns LIFE1 on P44 with the lender's interest | Accountant as trustee, interest and release |
| B23 | Wong Family Trust | Newly formed, owns UL1 for the twins' education | New entity onboarding and due diligence before issue |
| B24 | Southern Lakes Adventure Tours Ltd | GRPL scheme, 40 staff with monthly seasonal joiners and leavers, pilots above the automatic acceptance limit | Truncated membership file, concentration in one aircraft, backdated leavers |
| B25 | Pacific Freight Logistics Ltd | GRPL scheme, 120 named members in two categories, files submitted host to host, dual approval | File with rejected rows, administrator lifecycle, remittance recall, audit extract |

## Part C: Situations as insurance journeys

Each library situation keeps its number and becomes a scripted journey. S16 onward are the journeys the insurance design names that the bank library has no equivalent for.

| ID | Situation | Insurance journey |
| --- | --- | --- |
| S1 | The estate of Tom Clarke | Tom dies on 14 June 2028. Death notified twice, his sole policies restricted, joint first-event claim pays the lender then Vanessa, sole policy pays the estate with an executor in Perth, Vanessa becomes policyholder of the health plan, his direct debit authority does not transfer, reinsurance recovery posts |
| S2 | Barry's romance scam | Two partial surrenders, then a full surrender to a new overseas payee, a beneficiary change and an attempted ownership transfer. Conversation, cooling-off, specialist review, daughter without authority |
| S3 | Dave's hardship | Two missed premiums, dunning steps, call reaches Kath, hardship application, six-month reduced premium, early resumption, lender notified throughout |
| S4 | Losa turns 18 | Overnight she becomes an adult dependant with her own sign-in, her parents lose claim detail unless she consents, and all parties are told |
| S5 | The Patel skimming incident | 300 compromised cards include many used for recurring premiums. Tokens die, collections fail in bulk, the lapse clock and fees must be fair, chargebacks arrive on card premiums |
| S6 | Beverley's declining capacity | Premium paid three times, lockouts, a beneficiary change in favour of a tradesperson, power of attorney activated with a medical certificate |
| S7 | The Fuimaono family intervention | Children ask the insurer to stop their parents' policy loans. Refused. The parents agree to a conversation step and a view-only daughter |
| S8 | Caleb's gambling | Overpay and refund loop, cancel and rebuy, a self-requested pause on refunds with a cooling-off on removing it |
| S9 | Cyclone recovery | Regional premium relief applied in bulk, undeliverable notices pausing lapses, a surge of claims, concentration and the catastrophe treaty, fake relief claims, bulk remittances with poor references |
| S10 | Yuki leaves the country | Leaver processed, cancellation, refund overseas, a refund returned after the bank account closed, non-resident flag |
| S11 | Hemi and Kiri separate | One tries to cancel alone, the other asks for a freeze, joint cover split into two single covers, health plan split, lender consent, policy transfer to a new group |
| S12 | Pacific Freight's finance team | Administrator leaves and is replaced the same day, 400-row file with three rejected rows, remittance recalled as a duplicate, auditors ask for a year of approvals |
| S13 | The Waititi trust changes trustees | A trustee dies, one resigns, two are appointed with one in Sydney. A servicing request needs two valid signatures during the change |
| S14 | Mistaken payment | Chloe pays her voluntary premium to another policy's reference. Separately a refund is paid to Nikau's account in error and partly spent |
| S15 | The day the processor goes down | Card and bank channels fail for four hours on a collection day. Dishonours caused by the outage are identified, fees waived and standing corrected through a remediation case |
| S16 | A death notified late | An annuitant's death is notified four months late. Payments since death become a recovery against the estate, the guarantee balance is paid |
| S17 | A leaver claims continuation | N9 left B25, took the continuation option inside the window and later claims on the individual policy |
| S18 | An adviser moves mid-trail | D1 moves from D6 to D7 on 1 November 2028 with a 25% tail for two years. The worked example in the design, with exact shares |
| S19 | A price error after withdrawals | F-BAL is struck wrong for one date. Orders are replayed, remaining investors adjusted in units, leavers paid cash, the fund made whole |
| S20 | A disputed book | D1 and D6 dispute part of a book. The disputed share is held, everyone else is paid, and a signed agreement releases it |
| S21 | A child ages out | P55 and Mike Thompson's daughter reach the age limit. Continuation offers start before the anniversary |
| S22 | A FAP loses its licence | D9 is frozen, new business refused, its lines held, servicing reassigned, later wound up with a clawback debt |
| S23 | The disclosure law changes | Contracts entered before and after 15 November 2027 run under different question sets and remedies, chosen by contract date |
| S24 | A truncated membership file | B24 uploads a file missing most members. The safety check holds it. In a second run it is confirmed in error and remediated |
| S25 | A wrong indexation rate | A cohort's anniversaries apply the wrong rate. Rollback, replay, corrected notices, declined customers asked again |
| S26 | Migration rehearsal | A synthetic legacy book built from the closed-book personas is loaded, with the exceptions it should produce |
| S27 | Quarter-end close | A full close with sweeps, extracts, actuarial journals and a waiver |
| S28 | An outage during a benefit run | The workflow service is unavailable on a benefit date. Catch-up order is honoured and nobody is paid twice |
| S29 | Replacement under a new adviser with a top-up | P27 appoints D4, who replaces the cover and increases it. The second worked example in the design |
| S30 | The 5,000-member year | E1 runs twelve monthly files, backdated leavers, an underwritten excess, a renewal and a claim. The group slice exit journey |

## Part D: Distribution cast

| ID | Party | Role in the tests |
| --- | --- | --- |
| D1 | Tessa Maddox, adviser, paid through Maddox Advice Ltd | Writes P17, P29 and Tom Clarke at D6. Moves to D7 on 1 November 2028 and later to D8 with overlapping tails |
| D2 | Viliami Fifita, adviser | Jointly writes business 60 and 40 with D1. Holds two engagements at once. Adds a cover to another adviser's policy |
| D3 | Colin Baird, adviser | Wrote P27's policy. Retires, book split among three successors with a tail, later dies |
| D4 | Anika Sharma, adviser at D7 | The new adviser in S29. Bought a book with finance from D15 |
| D5 | Joe Tane, adviser | Under investigation, payment hold, pattern of disguised replacements |
| D6 | Harbourline Advice Ltd | FAP One. Keeps 25% tail, bears clawback for two years |
| D7 | Kōwhai Financial Ltd | FAP Two. Changes aggregator from D10 to D11 |
| D8 | Tussock Partners Ltd | FAP Three. An authorised body under it gains its own licence |
| D9 | Clearwater Advice Ltd | The FAP that loses its licence and is wound up |
| D10 | Northlight Dealer Group | Aggregator with override on existing business |
| D11 | Longbay Dealer Group | New aggregator from the change date |
| D12 | Ridgeway Employee Benefits Ltd | Broker on group schemes, replaced mid-year on E1 by D13 |
| D13 | Summit Workplace Benefits Ltd | Incoming broker |
| D14 | Keystone Home Loans Ltd | Referral partner paid a fixed fee, no engagement and no customer access |
| D15 | Anchor Business Finance Ltd | Financier with an assignment over D4's commission |

## Part E: Reinsurers, employers, providers and delegates

| ID | Party | Role in the tests |
| --- | --- | --- |
| R1 | Kestrel Re | Lead on the surplus treaty at 60%, NZD |
| R2 | Halyard Re | 40% on the surplus treaty, facultative market, settled overseas in USD |
| R3 | Stormline Re | Catastrophe and stop loss covers, longevity quota share on annuities |
| E1 | Kai Fresh Supermarkets Ltd | 5,000 named members, two benefit categories, broker D12. The group slice exit scheme |
| E2 | Te Ara Public Service Scheme | 50,000 members across several billing-group policies. Capacity and apply budget |
| E3 | Southline Labour Hire Ltd | Unnamed member records, heavy churn |
| E4 | Heretaunga Packhouse Ltd | Seasonal employer whose payroll file format changes every year |
| TPA1 | Benefit Admin Partners Ltd | Delegated underwriting authority on B17 and E2 |
| PR1 | Riverside Surgical Centre | Provider with an agreement, paid directly, sends pre-approval requests |
| PR2 | BrightSmile Dental | Provider with a double-billing pattern |
| FA1 | Custodia Fund Services | Simulated fund administrator: valuations, units on issue, restatements |
| BK1 | Primary banking partner | File and API channels, acknowledgement and return files |
| BK2 | Second banking partner | Continuity channel |

## Part F: Gap personas

The design names six gaps. The bank library lists four more. Where a library persona already fills one, it is noted.

| ID | Persona | Fills |
| --- | --- | --- |
| N1 | Wendy Aperahama, 44, unnamed member at E3 | An unnamed scheme member who claims: identification at claim |
| N3 | Chidi Okafor, 43, Lagos | A non-resident life insured on P31's policy |
| N4 | Daniel Kereama, 37, wheelchair user who uses a screen reader | A person who cannot take part in a step-count wellness programme. Also assistive technology |
| N5 | Rob Sutherland, 51, undischarged bankrupt with a WOL0 surrender value | Bankruptcy. The design does not yet say what happens to ownership and proceeds |
| N6 | Shane Poutama, 34, in prison for two years, holds TERM0 and IP1 | A customer in prison: post only, representative, benefit eligibility |
| N7 | Leilani Fonoti, 35, claims assessor at the insurer | A staff member who is also a customer and is related to P60 |
| N9 | Petra Novak, 41, former B25 employee | A member who left and claims continuation |
| N10 | Dorothy Chen, 79, in care | Policy from 1974 and an annuity, represented by P10 |
| N11 | Michael Brady, 31 | Adult under welfare guardianship, represented by P33 |

The trustee-owned policy is B20. The reinsurer is R1. The FAP that loses its licence is D9.

## Part G: Staff

| ID | Role | Profile used in tests |
| --- | --- | --- |
| ST1 | Junior underwriter | Accept alone to $500,000, no loadings, no declines |
| ST2 | Senior underwriter | Loadings, exclusions, second signature on declines and large sums |
| ST3 | Health claims assessor | Admit to $5,000, no declines alone |
| ST4 | Senior claims assessor and case manager | Lump sum and income protection, second signature |
| ST5 | Collections officer | Ordinary and financial classes only |
| ST6 | Hardship and vulnerable customer specialist | Routed actions, accredited |
| ST7 | Group administrator | Assigned schemes only |
| ST8 | Unit pricing maker | Prepares prices |
| ST9 | Unit pricing checker | Approves prices |
| ST10 | Finance operator | Journals, payment run preparation |
| ST11 | Finance lead | Sign-off and waivers |
| ST12 | Appointed actuary | Required approver on tables, read-only extracts |
| ST13 | Compliance and fraud investigator | Restricted cases |
| ST14 | Complaints reviewer | Reports outside the functions reviewed |
| ST15 | Product owner | Configuration author, no customer data |
| ST16 | Platform engineer and incident commander | Operations console only |
| ST17 | Treasury and payment run approver | Approves runs, does not maintain payees |
