# User stories by persona

Each story is written from the persona's point of view and carries an ID of the form `US-<persona>-<n>`. The line after "Done when" is the acceptance condition in plain words. The exact checks are the test cases in `test-matrix.md` that name the story. `coverage.md` lists the tests behind every story.

Stories describe behaviour the design notes already decide. Where a persona wants something the design does not yet answer, the story is marked **(design gap)** and repeated in the gap list in `README.md`.

## Individuals

### P1 Aroha Ngata
- **US-P01-1** As a student paid fortnightly, I want my premium taken on payday and retried once if it bounces, so that a short week does not cost me my cover. *Done when* a dishonour is retried once, I am told before the retry, my cover continues in grace and the lapse clock runs from the original due date.
- **US-P01-2** As a part-time worker, I want to know why I am not in my employer's scheme and to join when my hours rise, so that I am not left guessing. *Done when* the ineligible row is rejected with its reason in the employer's preview and a later hours change joins me from its effective date.
- **US-P01-3** As someone who checks her phone constantly, I want the app to show what I owe and when instantly, so that I am never surprised. *Done when* standing, next payment and any pending payment show from the device copy and update when the receipt event lands.

### P2 Liam O'Connor
- **US-P02-1** As a customer who keeps losing his card, I want my premium to keep working when the card is replaced, so that I do not lapse by accident. *Done when* a replaced card is picked up through the updater or I am asked for a new one before the next billing.
- **US-P02-2** As an apprentice who has qualified, I want my new occupation reflected, so that I pay the right premium. *Done when* the change goes through underwriting and applies from the decision date as a new layer, with nothing already billed edited.
- **US-P02-3** As a customer whose smoker status was recorded wrongly, I want it corrected under the rules and not by a system default, so that I am treated lawfully. *Done when* the correction re-rates by value date and any remedy is a workflow decision with approvals.

### P3 Priya Raman
- **US-P03-1** As an investor, I want my monthly contribution to buy units at the right day's price and to see it pending until then, so that I can trust the numbers. *Done when* cash waits in pending investment cash, the order takes the correct pricing date and the statement shows units and price.
- **US-P03-2** As a power user, I want to toggle health components and see the premium move, so that the quote I save is the price I pay. *Done when* the issued premium equals the saved quote to the cent under the same versions.
- **US-P03-3** As a careful reader, I want any improvement in wording passed back to me without losing my original terms, so that I get the better of the two at claim. *Done when* my cover is bound to both versions and a claim is assessed against both.
- **US-P03-4** As a walker, I want my wellness tier to reduce my premium from the next billing, so that the programme is worth it. *Done when* the tier arrives as an entitlement and the discount is its own line on the notice.

### P4 Tama Hohepa
- **US-P04-1** As an adult on my wife's policy, I want my own sign-in and my own claims kept private unless I say otherwise, so that my health is my business. *Done when* Mere sees counters and premium only until I consent, and consent can be withdrawn.
- **US-P04-2** As a seasonal earner, I want to catch up arrears in summer and have cover restored, so that a quiet winter does not end it. *Done when* paying all arrears while suspended restores cover from the payment date without underwriting.

### P5 Mere Hohepa
- **US-P05-1** As the household's budget keeper, I want every premium notice to add up to the cent and say what each line is, so that my notebook matches. *Done when* base, discounts, fee, tax and net reconcile to the receivable posted.
- **US-P05-2** As a new mother, I want my baby added from birth without questions, so that the first weeks are covered. *Done when* a newborn added inside the window is covered from the date of birth with the premium adjusted by value date.
- **US-P05-3** As someone fierce about fees, I want to see and question any fee, so that nothing is taken quietly. *Done when* a dishonour fee appears as its own line with notice, and allocation takes fees in the declared order.

### P6 Graham Whitfield
- **US-P06-1** As a retiree with a maturing endowment, I want to be contacted ahead with my options, so that I can decide calmly. *Done when* the maturity workflow contacts me before the date, pays or reinvests as I choose, and unpaid proceeds stay in benefit payable.
- **US-P06-2** As someone who rings up, I want staff to act for me with my approval, so that I do not need the app. *Done when* the action records both identities and I approve by the assisted-channel prompt.
- **US-P06-3** As a scam-wary customer, I want a change of my payout account to be slowed and checked, so that nobody can redirect my money. *Done when* a payee change needs a signed step, a name check and a cooling-off before first payment.

### P7 Sione Taufa
- **US-P07-1** As a joint owner with a mortgage, I want the lender's interest respected and visible, so that the cover does what the loan needs. *Done when* restricted changes are refused without the lender's consent and the lender receives arrears and lapse notices.
- **US-P07-2** As someone who prefers a person, I want to make changes in person with the same protections as the app, so that I am not second class. *Done when* an assisted change is quoted, approved by me and applied as the same typed command.

### P8 Ana Taufa
- **US-P08-1** As a busy parent, I want to photograph a receipt and be paid without a phone call, so that claiming takes a minute. *Done when* an eligible health claim is admitted by rule and paid in the next run with the excess and limits shown.
- **US-P08-2** As someone targeted by scam texts, I want messages from my insurer to never contain sign-in links, so that I can tell real from fake. *Done when* every notification is a pointer to the in-app inbox with no link.
- **US-P08-3** As a mother of five, I want the premium to charge only for the children the product says, so that a big family is affordable. *Done when* rating applies the product's rule and the notice shows each life.

### P9 Losa Taufa
- **US-P09-1** As a new adult, I want my health information to become mine on my birthday, so that my parents see only what I choose. *Done when* on the date my claim details leave my parents' view, I can sign in myself, and everyone is told.
- **US-P09-2** As a young adult nearing the age limit, I want an offer of my own policy without new medical questions, so that I stay covered. *Done when* the continuation offer starts before the anniversary and carries my terms, waiting periods and limits used.

### P10 Margaret Chen
- **US-P10-1** As an attorney for my sister, I want to act within the authority I hold and no further, so that I am protected too. *Done when* actions inside the scope succeed, actions outside are refused, and every action records me as the actor.
- **US-P10-2** As a retired accountant, I want to download statements that reconcile line by line, so that my spreadsheet agrees. *Done when* exported statements are deterministic and their totals equal the ledger.
- **US-P10-3** As a customer of fifty years, I want my old policy honoured exactly as written, so that migration costs me nothing. *Done when* the migrated cover is bound to its legacy wording and terms and nothing is re-underwritten.

### P11 Josh Harding
- **US-P11-1** As someone with lumpy income, I want to pay ahead when a big invoice lands, so that lean months are covered. *Done when* the excess sits as a credit on the policy, is consumed by later billings and can be refunded on request.
- **US-P11-2** As a self-employed claimant, I want each month's benefit calculated on that month's income, so that part-time work does not end my claim. *Done when* offsets and partial disability are applied per period and a late declaration adjusts afterwards.
- **US-P11-3** As a traveller, I want to claim for something that happened overseas, so that distance is not a barrier. *Done when* the claim is registered at once, routed to the complex queue and evidenced through the adapters.

### P12 Fatima Al-Sayed
- **US-P12-1** As someone who prefers Arabic for complex documents, I want my schedule and notices in Arabic with the English shown as controlling, so that I understand what I sign. *Done when* documents render in my preferred language from the same data and say which version controls.
- **US-P12-2** As someone who avoids interest, I want to be able to decline interest added to a late payment, so that my values are respected. **(design gap)** *Done when* the design says whether a payee may waive late-payment interest and how that posts.
- **US-P12-3** As an applicant with little local medical history, I want missing records handled without being declined by default, so that my past is not held against me. *Done when* an evidence request that cannot be met routes to an underwriter with reasons and never defaults to decline or to standard.

### P13 Dave Pākura
- **US-P13-1** As a proud man in trouble, I want the insurer to reach me early and offer real alternatives, so that I do not lose cover by staying silent. *Done when* the dunning sequence creates a call task, the alternatives are offered and the outcome is a reason code.
- **US-P13-2** As a customer in hardship, I want reduced premiums for six months and to resume early when I can, so that I recover on my terms. *Done when* the override is approved, dated and evented, and ending it early restores normal billing from that date.
- **US-P13-3** As a long-standing policyholder, I want my old policy's value to keep it alive before it lapses, so that decades of premiums are not lost. *Done when* the automatic premium loan pays the overdue premium and I am told each time.

### P14 Chloe Bennett
- **US-P14-1** As a scheme member, I want to register and see my own cover and nothing else from my employer's scheme, so that it feels like mine. *Done when* my device holds only my slice and the employer sees no health or claim detail.
- **US-P14-2** As a member paying for top-up cover, I want my own bill and my own standing, so that my employer's payments and mine do not mix. *Done when* voluntary cover has its own receivable and my arrears never touch the employer's standing.
- **US-P14-3** As someone who mistyped a reference, I want my payment found and applied to my policy, so that a typo does not cost me cover. *Done when* the payment is matched or held in suspense with an owner, corrected by a new entry and my standing recalculated.

### P15 Rangi Walker
- **US-P15-1** As a farmer with patchy coverage, I want to read everything offline and act when I have signal, so that Sunday night is enough. *Done when* every screen reads from the device and anything that moves money waits for a connection and a signed step.
- **US-P15-2** As a separated father, I want my nomination changed to my children, so that the right people are paid. *Done when* the nomination in force on the date of death is the one read at claim.

### P16 Ruth Abrahams
- **US-P16-1** As someone with no phone or computer, I want everything by post and to be treated as fully served, so that I am not pushed online. *Done when* mandatory notices go by post with proof of delivery and an undeliverable notice stops what depends on it.
- **US-P16-2** As an annuitant, I want to confirm I am alive by signing a form, so that my payments continue. *Done when* a signed declaration satisfies the check and a missed one holds payments without cancelling them.
- **US-P16-3** As a mother, I want my son told nothing without my say, so that my affairs stay mine. *Done when* a request from a person with no recorded authority is refused and nothing is disclosed.

### P17 Ben Kowalski
- **US-P17-1** As a founder who scripts everything, I want every API call to be safe to retry, so that a timeout never double-charges me. *Done when* a repeated command with the same key returns the original result.
- **US-P17-2** As a director, I want the company's key person policy assigned to the lender properly, so that the overdraft condition is met. *Done when* the interest is recorded with its notice date and the restricted commands are blocked.
- **US-P17-3** As a customer who lets a policy lapse, I want to know what reinstating needs, so that I can decide quickly. *Done when* the reinstatement conditions and cost are shown and a reinstatement inside the window restores the policy.

### P18 Hemi and Kiri Parata
- **US-P18-1** As a joint owner in a separation, I want nothing given up without both signatures, so that neither of us can strip the cover. *Done when* a cancellation or reduction signed by one owner is refused.
- **US-P18-2** As separating partners, I want our joint cover split into two single covers without medical questions, so that we each stay insured. *Done when* the split creates linked covers with terms and history carried and the second policy moves to the other person's group.
- **US-P18-3** As a parent keeping the children on my health plan, I want the plan split with waiting periods served kept, so that nobody starts again. *Done when* each adult keeps their terms and the children follow the chosen parent.

### P19 Oliver Trent
- **US-P19-1** As a high earner, I want a large application decided within the promised time, so that I am not left waiting on my banker's patience. *Done when* service timers warn and escalate and the case reaches someone with authority.
- **US-P19-2** As a person insured through my trust and personally, I want the insurer to know my total cover, so that the offer made is one it can honour. *Done when* the capacity step reads my accumulation across partitions before terms are offered.

### P20 Sarah Mitchell
- **US-P20-1** As a parent with irregular child support, I want a missed premium to be recoverable without penalty beyond the rules, so that one late payment is not a crisis. *Done when* I can pay at any step and standing moves at once.
- **US-P20-2** As a scheme member, I want to add my teenagers at my own cost, so that they are covered through my work. *Done when* dependants are voluntary cover billed to me and the employer sees only my tier.

### P21 Kevin Zhang
- **US-P21-1** As an investor, I want a pricing error put right even on money I have already withdrawn, so that I am not out of pocket. *Done when* replay finds my orders and I am paid cash for the difference as a case.
- **US-P21-2** As a borrower with several loans, I want each lender's interest recorded in order, so that proceeds go to the right bank first. *Done when* interests are ordered by notice date and a claim pays them in that order.
- **US-P21-3** As a household, I want my wife to receive documents in Mandarin while I receive English, so that we both understand. *Done when* presentation preference is per party and both render from the same data.

### P22 Jess Morgan
- **US-P22-1** As someone who buys on impulse, I want a full refund if I cancel in the cooling-off period, so that a mistake costs nothing. *Done when* all premium is refunded, the receivable reversed and commission reversed.
- **US-P22-2** As a customer with ADHD, I want to tell the insurer what helps me and have it applied everywhere, so that I do not have to ask each time. *Done when* my adjustments are recorded with my knowledge and applied to notices and routing without a label on any screen.
- **US-P22-3** As an applicant with a history, I want terms applied only to the component they concern, so that one answer does not load everything. *Done when* one decision writes different terms to different covers.

### P23 Tane Rikihana
- **US-P23-1** As a fisherman away for days, I want my partner able to pay my premiums and nothing else, so that she can help without holding everything. *Done when* her authority allows payment and refuses changes to cover, beneficiaries or bank details.
- **US-P23-2** As a family, we want a claim to be registered even when the facts are unclear, so that we are not turned away. *Done when* a claim with no death certificate is opened, reserved and worked with the coroner's evidence request.

### P24 Emily Foster
- **US-P24-1** As a parent on leave, I want a premium holiday offered before I think of cancelling, so that I keep cover through a tight year. *Done when* a cancellation request shows what is given up and the alternatives first.
- **US-P24-2** As a new parent, I want to raise my cover after the birth without medical questions, so that my family is protected. *Done when* the option is checked against the event evidence and the count used, and adds a layer without underwriting.

### P25 Patrick Doyle
- **US-P25-1** As a farmer, I want to pay once a year after the lamb cheque, so that premiums follow income. *Done when* yearly billing posts one receivable earned daily over the year.
- **US-P25-2** As a partner bringing my son in, I want him added as an owner and a life, so that the farm's cover follows the succession. *Done when* the partnership change is a relationship change with due diligence and his cover is underwritten as a new layer.

### P26 Nikau Pene
- **US-P26-1** As a casual worker, I want to be covered in the weeks I qualify and told when I am not, so that I know where I stand. *Done when* eligibility is evaluated at each effective date from the employer's file.
- **US-P26-2** As someone who received money by mistake, I want a fair way to repay it, so that an insurer's error does not sink me. *Done when* the overpayment is a receivable with a repayment arrangement as a case.

### P27 Dr Anjali Desai
- **US-P27-1** As a customer who appoints a new adviser, I want her to see my policies at once, so that advice is informed. *Done when* servicing access follows my authority immediately and commission follows the configured setting.
- **US-P27-2** As a customer replacing old cover, I want no gap and no new waiting period on what I already had, so that I lose nothing by upgrading. *Done when* replacement layers carry my terms and only the increase is underwritten.
- **US-P27-3** As a company owner, I want my accountant to see the company's policy and nothing personal, so that the books are right. *Done when* the accountant's relationship shows premiums and documents for the company's policy only.

### P28 Wiremu Tipene
- **US-P28-1** As a kaumātua helped by my mokopuna, I want her recorded as my support person, so that she can help without being me. *Done when* she can be present and speak, cannot decide, and nothing requires sharing my sign-in.
- **US-P28-2** As someone bounced around call centres, I want one named person on my matter, so that I tell my story once. *Done when* my case has one owner at a time, shown to me by name, with handover recorded.

### P29 Hannah Lee
- **US-P29-1** As someone getting married, I want to increase my cover on the strength of the wedding, so that I need no new medical. *Done when* the increase is a new layer in my adviser's current book with its own commission and clawback.
- **US-P29-2** As a joint investor with my fiancé, I want withdrawals to need both of us, so that our savings are safe. *Done when* a withdrawal signed by one owner is refused under a both-to-sign rule.

### P30 Frank Russo
- **US-P30-1** As a claimant with a partial trauma event, I want the partial amount paid and to know what cover remains, so that I can plan. *Done when* the partial payment and the linked life cover reduction post in one entry and a new schedule issues.
- **US-P30-2** As a grower after a cyclone, I want relief applied without having to plead my case, so that I can get on with recovery. *Done when* a regional relief override applies to my policy in bulk and I am told.

### P31 Grace Okafor
- **US-P31-1** As a wife insuring a husband overseas, I want his application assessed with overseas evidence, so that distance does not mean decline. *Done when* the non-resident life is a party with due diligence, evidence adapters handle overseas sources and the decision records its reasons.
- **US-P31-2** As a daughter, I want my mother overseas paid safely if I die, so that she is looked after. *Done when* the overseas payee is verified and screened and paid over the cross-border rail.

### P32 Toby Nash
- **US-P32-1** As a sixteen-year-old, I want my health claims private from my parents once I reach the set age, so that I can see a doctor in confidence. *Done when* from the configured age my parents see the counter move without the person or service.

### P33 Lorraine Brady
- **US-P33-1** As a welfare guardian, I want to manage my son's policy within the court order and get a record of everything I did, so that I can account for it. *Done when* my actions are limited to the order's scope and a report of them can be produced.
- **US-P33-2** As a seasonal worker, I want my scheme cover to follow the season correctly, so that I am covered when I am working. *Done when* seasonal joins and leaves are effective-dated and the continuation window opens on leaving.

### P34 Arjun Mehta
- **US-P34-1** As an applicant for a very large sum, I want to know quickly whether the insurer can take it, so that I do not wait for nothing. *Done when* the capacity step refers to facultative before any offer and the timers are visible.
- **US-P34-2** As an investor in a USD fund, I want orders priced on the fund's own calendar, so that a New Zealand holiday does not misprice me. *Done when* the pricing date follows the fund's calendar and cut-off.
- **US-P34-3** As a customer adding a large lump sum, I want source-of-funds checks done once and well, so that I am not asked the same thing repeatedly. *Done when* the rule raises one case per event with prior answers visible to the investigator.

### P35 Moana Kahu
- **US-P35-1** As a sole parent on a benefit, I want my billing day moved to benefit day, so that the money is there. *Done when* a billing day change applies without altering what has been billed.
- **US-P35-2** As a customer in hardship, I want no second attempt on my account without a conversation, so that I am not hit with bank fees. *Done when* my marker suppresses re-presentment until a person has spoken to me.
- **US-P35-3** As a customer hit by someone else's outage, I want any fee and any harm to my standing put right without asking, so that I do not pay for their failure. *Done when* a remediation case finds me, waives the fee and corrects standing.

### P36 Stuart Campbell
- **US-P36-1** As a retiree, I want the annuity income I was quoted to be the income I get, so that my plan holds. *Done when* the quote is a dry run of the rating function and the issued income matches.
- **US-P36-2** As someone turning 65, I want notice that my income protection ends and what my options are, so that it is not a surprise. *Done when* the expiry event gives advance notice with any conversion option.
- **US-P36-3** As a vendor owed money, I want my interest in the buyer's policy recorded, so that the debt is protected. *Done when* a non-lender interest is recorded, enforced and released only on my discharge.

### P37 Yuki Tanaka
- **US-P37-1** As a visitor leaving the country, I want to cancel and receive any refund in Japan, so that nothing is left behind. *Done when* unearned premium is refunded by value date to a verified account and a returned payment goes back to payable with a task.
- **US-P37-2** As someone who reads English slowly, I want plain words and a translation, so that I understand. *Done when* templates meet the plain-language check and offer a translation with the primary version controlling.

### P38 Pita and Lagi Fuimaono
- **US-P38-1** As elders with obligations, we want to decide for ourselves after a respectful conversation, so that help does not become control. *Done when* a large surrender or loan is paused for a conversation and our decision stands afterwards.
- **US-P38-2** As parents, we want our daughter to see our policy without being able to change it, so that the family worries less. *Done when* a view-only relationship shows what we allow and permits no commands.
- **US-P38-3** As a couple on a last-survivor policy, we want the first death recorded properly, so that the policy does what it says. *Done when* the first death changes the cover's state and pays nothing.

### P39 Rebecca Sinclair
- **US-P39-1** As the earner, I want my husband to have equal authority, so that he is never treated as a secondary. *Done when* either owner can act alone under an either-to-sign rule and correspondence goes to both.

### P40 Hone Waititi
- **US-P40-1** As a person with three hats, I want each relationship to use the contact details I choose for it, so that work mail goes to work and family mail comes home. *Done when* notices follow the relationship's contact point.
- **US-P40-2** As a director and a member, I want one app showing each role with its own slice, so that I am never confused about which hat I wear. *Done when* my device holds the scheme cover as a life and my own policies as owner, and the employer portal shows me no health detail.

### P41 Claire Dubois
- **US-P41-1** As someone bringing an inheritance in tranches, I want each deposit handled without starting from zero, so that due diligence is thorough but not repetitive. *Done when* each single premium raises a case that links to the earlier ones.
- **US-P41-2** As a dual tax resident, I want my tax details applied correctly, so that reporting is right the first time. *Done when* my policy appears in the international reporting file with the right residencies.

### P42 Mike Thompson
- **US-P42-1** As a stepfather, I want my wife and her children covered on my plan with their privacy intact, so that the family is one policy without being one file. *Done when* a year of family claims pays straight through and I see only shared counters for Donna.
- **US-P42-2** As a patient facing surgery, I want pre-approval so that I know what will be paid before I go in. *Done when* the approval holds an amount against my limits with an expiry and the invoice releases it and posts the real amount.
- **US-P42-3** As a father, I want my adult daughter offered her own policy when she ages out, so that she is not dropped. *Done when* her cover ends at the anniversary after the limit and a continuation offer has already started.

### P43 Sophie Williams
- **US-P43-1** As a freelancer paid late, I want to clear arrears over a few months, so that a slow client does not lapse my policy. *Done when* an arrangement holds standing while kept and the lapse clock resumes where it was if broken.
- **US-P43-2** As a claimant, I want other income offset exactly as the policy says, so that I can check the maths. *Done when* each benefit line shows the offset as its own figure.

### P44 Raj and Sunita Patel
- **US-P44-1** As card payers after a skimming incident, we want our premiums to keep working when cards are reissued, so that fraud does not lapse our cover. *Done when* failures tied to the compromise do not charge fees or advance lapse unfairly and we are asked for new details.
- **US-P44-2** As trustees, we want the trust's policy changes signed by the right trustees, so that the accountant is not bypassed. *Done when* signing rules on the ownership relationship are enforced.

### P45 Isaac Brown
- **US-P45-1** As a soldier deploying, I want to know whether I am covered overseas and my mother able to keep things running, so that I can leave with a clear head. *Done when* the product's posting rule is shown, an exclusion is a code the assessor judges, and my mother's authority works within its scope.

### P46 Beverley Simmons
- **US-P46-1** As someone who sometimes pays twice, I want the extra noticed and offered back, so that I am not out of pocket. *Done when* a duplicate becomes a credit on the policy, a prompt is raised and a refund is available.
- **US-P46-2** As a mother and daughter, we want the power of attorney to start only when it must and leave me my dignity, so that I keep what I can still do. *Done when* activation needs the medical certificate, the attorney acts within scope and I keep my own sign-in for viewing.
- **US-P46-3** As a customer who may be pressured, I want a change of beneficiary to a stranger checked, so that I am protected. *Done when* the instruction matches an abuse indicator and waits for specialist review.

### P47 Dion Reihana
- **US-P47-1** As a tradie with lumpy cash flow, I want to be asked about my payment method after repeated bounces, so that the problem is fixed and not just retried. *Done when* repeated dishonours raise a task and suspend the authority after the set number.

### P48 Helen Doyle
- **US-P48-1** As a partner in my own right, I want my own policies, my own contact details and letters addressed to me, so that I am not filed under my husband. *Done when* my relationships choose my contact points and no template assumes a primary owner.

### P49 Alex Rivera
- **US-P49-1** As someone who changed their legal name, I want one update that flows to every policy and document, so that I do it once. *Done when* the party change updates every relationship and a new schedule issues with the earlier one kept.
- **US-P49-2** As a non-binary customer, I want to be rated without being forced into a category that is not mine, so that I am treated with respect. **(design gap)** *Done when* the design says how sex as a rating factor is captured for a non-binary life.

### P50 Barry Hughes
- **US-P50-1** As a customer being scammed, I want the insurer to slow down and talk to me without taking my choices away, so that I have a chance to see it. *Done when* the surrender to a new overseas payee triggers a conversation and a cooling-off, and my decision stands after.
- **US-P50-2** As a father, I want my daughter's worry noted but nothing shared with her, so that my privacy holds. *Done when* her contact is recorded as a third-party concern that prompts a person to consider a marker and discloses nothing.

### P51 Mei Lin Wong
- **US-P51-1** As a mother of newborn twins, I want both added in one step, so that paperwork does not eat the first month. *Done when* two backdated joins apply by value date within the window.
- **US-P51-2** As a customer with several policies, I want one consolidated bill while each policy keeps its own record, so that I pay once. *Done when* the consolidated view is a read model and each policy's receivable is separate in the ledger.
- **US-P51-3** As trustees of a new trust, we want the trust accepted as a policyholder properly, so that the education fund is in the right name. *Done when* due diligence on the trust and trustees completes before issue.

### P52 Caleb Stone
- **US-P52-1** As someone struggling, I want to be able to put friction on my own refunds, so that I cannot raid my premiums on a bad night. **(design gap)** *Done when* the design says whether a customer-set hold with a cooling-off on removal exists for refunds and surrenders.
- **US-P52-2** As a customer, I want my pattern noticed without being accused, so that I am offered help. *Done when* the overpay-then-refund rule raises a restricted case and front-line staff see only a support prompt.

### P53 Ngaire Thompson
- **US-P53-1** As someone on satellite internet, I want a dropped connection never to leave a change half done, so that I can trust what I see. *Done when* a request shows as pending until its event lands and a retry does not duplicate it.

### P54 David Park
- **US-P54-1** As an owner who works past midnight, I want to know which day my action counts for, so that a payment at 11.59pm is not a day late. *Done when* commands either side of the cutoff carry the right business date and the screen says so.
- **US-P54-2** As an employer opening a third site, I want to add its staff in one go as their own billing group, so that the invoice splits by site. *Done when* a bulk join creates the billing group and the invoice schedule shows it.

### P55 Lucy Hartley
- **US-P55-1** As a student with my own policy, I want my parents to pay without seeing anything, so that I keep my independence. *Done when* a payer with no ownership has no view and the payer rule is cleared by relationship.
- **US-P55-2** As someone who lost her phone, I want to get back in safely, so that I am not locked out for days. *Done when* recovery through a second device or fresh verification rebinds a device and purges the old one.

### P56 Ahmed Hassan
- **US-P56-1** As the one who pays for many relatives, I want my payments accepted without constant suspicion, so that helping family is not a red flag every month. *Done when* the payer-not-owner rule learns the recorded relationships and stops re-raising cleared cases.
- **US-P56-2** As a head of a large household, I want to know who can be on my health plan, so that I do not apply for people who cannot be covered. *Done when* an ineligible dependant is refused with the reason.

### P57 Vanessa Clarke
- **US-P57-1** As a new widow, I want one person to deal with and to be told what happens next, so that I am not chasing. *Done when* the claim has a named case manager, proactive updates and a bereavement prompt for a marker.
- **US-P57-2** As a surviving joint owner, I want the lender paid and the balance paid to me, so that the mortgage is dealt with. *Done when* proceeds go to the interested party first up to its interest and the balance to me.
- **US-P57-3** As the surviving adult on the health plan, I want to become the policyholder without a gap, so that the children stay covered. *Done when* ownership passes through the ownership workflow and I am asked to set up payment.
- **US-P57-4** As family of the deceased, we want his own policy paid to the estate with an executor overseas, so that probate distance is not a barrier. *Done when* the executor is verified remotely and payment waits on proof of authority.

### P58 Jordan Lee
- **US-P58-1** As an applicant declined for income protection, I want the reasons I can be told and a way to ask for review, so that I am not left with a bare no. *Done when* the decline has two signatures, states disclosable reasons and offers a review.
- **US-P58-2** As a customer who felt insulted, I want my complaint recorded even though I only said it in passing, so that it counts. *Done when* a remark to staff is captured in one step and acknowledged within the timer.

### P59 Elsie Marsh
- **US-P59-1** As an annuitant who cannot use the phone, I want letters and my niece overseas able to act, so that my payments are safe. *Done when* the overseas attorney is verified remotely and proof of life is satisfied by letter.

### P60 Marcus Fonoti
- **US-P60-1** As a public figure, I want staff who know me unable to browse my file, so that my health stays private. *Done when* access needs a path and a purpose, unusual lookups are flagged and a related staff member is never routed my claim.
- **US-P60-2** As a returning player, I want my cover from Australia taken into account, so that I am not treated as a blank page. *Done when* previous insurer evidence is requested through an adapter and the decision records what it relied on.

## Businesses

### B1 Kaiti Corner Dairy
- **US-B01-1** As a shop owner who pays by bank transfer, I want my payment matched even when the reference is wrong, so that I am not chased for money I paid. *Done when* matching falls through its keys in order and a low-confidence match becomes an exception item.

### B2 Bright Sparks Early Learning
- **US-B02-1** As a director, I want large membership changes to need both of us, so that one mistake cannot remove staff cover. *Done when* changes above our setting wait for a second administrator.
- **US-B02-2** As an employer, I want to see tiers and premiums and never who my staff's dependants are, so that I stay out of their private lives. *Done when* the portal shows the tier and no dependant identities.

### B3 Tidewave Software Ltd
- **US-B03-1** As an API-first employer, I want payroll changes to flow in with a preview I can approve by API, so that nobody uploads a spreadsheet. *Done when* API intake produces the same preview and commands as a file.
- **US-B03-2** As an employer, I want to pay for the core and let staff buy options, so that everyone gets a base. *Done when* cores bill to the scheme and options bill to each member as voluntary cover.

### B4 Waititi Construction Ltd
- **US-B04-1** As a small employer, I want my directors covered as named lives with the right occupational terms, so that site work is properly priced. *Done when* each member's category and class drive the invoice line.

### B5 Campbell Plumbing Ltd
- **US-B05-1** As a business changing hands over three years, I want administrators and the key person policy's ownership to change in step, so that authority follows ownership. *Done when* each stage is a dated relationship change and an ownership transfer where the owner changes.

### B6 Reihana Electrical Ltd
- **US-B06-1** As an employer who sometimes pays short, I want the shortfall carried to the next invoice and my staff left alone, so that my cash flow problem is mine. *Done when* the short payment is allocated by the scheme's terms and no member is individually in arrears.

### B7 Desai Anaesthesia Ltd
- **US-B07-1** As a company policyholder, I want a claim on my director paid to the company and handled as her private matter, so that the roles are kept apart. *Done when* the payee is the policyholder and claim detail is visible only to the life insured.

### B8 Sophie Williams Design
- **US-B08-1** As a sole trader paying a personal premium from the business account, I want that accepted once explained, so that I am not asked every month. *Done when* the payer relationship is recorded and the rule stops firing.

### B9 Dubois Translations
- **US-B09-1** As a customer paying from a euro account, I want the payment converted and applied in full view, so that I know what was credited. *Done when* currencies meet only through position accounts and the receipt shows the rate.

### B10 Patel Superette
- **US-B10-1** As an owner of two companies, I want two schemes kept separate with one login, so that the books stay apart. *Done when* each scheme is its own policy and my administrator relationships cover both.

### B11 Patel Liquor
- **US-B11-1** As a merchant whose terminal was compromised, I want to be treated as a victim, so that my own cover is not penalised. *Done when* my scheme's failed card collection is tied to the incident and remediated.

### B12 Doyle Farming Partnership
- **US-B12-1** As a partnership with a lender, I want changes that need the lender's consent to be clear up front, so that we do not start what we cannot finish. *Done when* a restricted change is quoted with the consent requirement shown and refused without it.

### B13 Russo Vineyard
- **US-B13-1** As an owner selling the business, I want the key person cover ended or transferred cleanly, so that the sale is tidy. *Done when* cancellation refunds unearned premium and the lender's interest is released by discharge.

### B14 Westcoast Hideaway B&B
- **US-B14-1** As a business with a premium holiday from 2020, I want it carried over exactly, so that migration does not restart or drop it. *Done when* the override loads with its original dates.

### B15 Park Hospitality Group Ltd
- **US-B15-1** As a multi-site employer, I want one invoice with a schedule per site, so that each restaurant carries its cost. *Done when* billing groups split the invoice and the total equals the sum of member premiums.
- **US-B15-2** As an employer with payroll software, I want weekly changes backdated correctly, so that a late leaver is credited. *Done when* a backdated leaver produces an adjustment line on the next invoice by value date.

### B16 Whangārei Community Foodbank
- **US-B16-1** As a volunteer treasurer, I want to declare our headcount once a year and settle up, so that cover is simple. *Done when* the deposit premium earns daily and the declaration posts an adjustment either way.
- **US-B16-2** As an outgoing treasurer, I want to hand over cleanly, so that the next person can act the same day. *Done when* administrator relationships end and start on the date and sessions close at once.

### B17 Ngāti Kahu ki Whaingaroa Trust
- **US-B17-1** As an iwi entity offering cover to members, I want our administrator's decisions accepted within agreed limits, so that members are covered quickly. *Done when* a delegated decision inside the authority binds and one outside routes to the insurer's queue.

### B18 Taranaki Netball Club
- **US-B18-1** As a club that did not proceed, I want the quote to expire tidily, so that nothing is left hanging. *Done when* the proposal lapses with notice and no money or state remains.

### B19 Ridgeline Property Developments Ltd
- **US-B19-1** As a lender-driven borrower, I want the assignment recorded the day notice arrives, so that drawdown is not delayed. *Done when* the interest carries the notice date and priority follows it.

### B20 Trent Family Trust
- **US-B20-1** As trustees, we want any two of three to be able to act, so that the independent trustee is not a bottleneck or bypassed. *Done when* two valid trustee signatures succeed and one fails.

### B21 Waititi Whānau Trust
- **US-B21-1** As a trust changing trustees, we want authority to move on the resolution date and the Sydney trustee verified remotely, so that the trust can keep acting. *Done when* the relationship change is dated, nothing moves partition, and requests signed by a former trustee are refused.

### B22 Patel Property Trust
- **US-B22-1** As a trust repaying its loan, we want the lender's interest released, so that the policy is ours alone again. *Done when* a signed discharge releases the restriction.

### B23 Wong Family Trust
- **US-B23-1** As a new trust, we want to invest for the twins with both parents required to withdraw, so that the fund is protected. *Done when* contributions buy units and a withdrawal needs the trust's signing rule.

### B24 Southern Lakes Adventure Tours Ltd
- **US-B24-1** As a seasonal employer, I want a bad file caught before it removes my staff, so that a spreadsheet slip is not a disaster. *Done when* a file removing more than the set share is held for explicit confirmation.
- **US-B24-2** As an employer of pilots, I want cover above the automatic limit arranged without my seeing why, so that their health stays private. *Done when* I see whether the excess is in place and never the terms.

### B25 Pacific Freight Logistics Ltd
- **US-B25-1** As a finance team, we want a file with a few bad rows to apply the good ones, so that three errors do not hold up four hundred changes. *Done when* rejected rows are listed with reasons and the rest apply after approval.
- **US-B25-2** As a company with turnover in finance, I want a leaver's access gone and a joiner's access live the same day, so that control is continuous. *Done when* the relationship change revokes sessions through the priority channel.
- **US-B25-3** As an audited company, I want a year of who approved what, so that auditors are answered from the record. *Done when* a saved query returns every command with its administrator and approver.

## Distribution

### D1 Tessa Maddox
- **US-D01-1** As an adviser who moved, I want to be paid exactly the agreed shares from the move date, so that I do not have to audit my own statement. *Done when* the split is 25%, 56.25% and 18.75% until the tail ends and 75% and 25% after, with no new rule.
- **US-D01-2** As an adviser, I want disclosures visible only where my client has consented, so that I never see what I should not. *Done when* consent is per life per application and ends at the decision.
- **US-D01-3** As an adviser in a dispute, I want the disputed share held and the rest paid, so that a disagreement does not stop my income. *Done when* the held account equals the open dispute and release follows a signed agreement.

### D2 Viliami Fifita
- **US-D02-1** As a joint writer, I want my 40% tracked on my own book, so that my colleague's move does not touch my pay. *Done when* only the moving adviser's book changes rule.

### D3 Colin Baird
- **US-D03-1** As a retiring adviser, I want my book split among successors with my tail honoured, so that my clients are looked after and I am paid. *Done when* layers move to three new books each carrying the tail and the successors' own books pay none.

### D4 Anika Sharma
- **US-D04-1** As a new adviser replacing old cover, I want to earn on the increase and be told plainly that I earn nothing on the replaced amount, so that the advice is clean. *Done when* replacement layers earn trail only on the original lines and the top-up earns full commission on my book.
- **US-D04-2** As an adviser with a financed book, I want my lender paid first from each statement automatically, so that I stay in good standing. *Done when* the payment instruction pays the lender first and cannot be removed without its consent.

### D5 Joe Tane
- **US-D05-1** As an adviser under investigation, I want my commission to keep accruing while payment is held, so that nothing has to be recalculated if I am cleared. *Done when* statements issue marked held and the balance pays on release.

### D6 Harbourline Advice Ltd
- **US-D06-1** As a FAP, I want clawback to fall where the agreement put it when commission was paid, so that liability is predictable. *Done when* a lapse after the move debits the party named on the exposure item.

### D7 Kōwhai Financial Ltd
- **US-D07-1** As a FAP changing aggregator, I want one change to move every book, so that I am not re-papering each adviser. *Done when* one fragment activation changes override lines for new books and keeps the old group's tail on existing ones.

### D8 Tussock Partners Ltd
- **US-D08-1** As a FAP administrator, I want to see every adviser under me and nothing above, so that I can run the practice. *Done when* the portal scope follows the dated structure.

### D9 Clearwater Advice Ltd
- **US-D09-1** As customers of a FAP that lost its licence, we want to keep being serviced, so that we are not stranded. *Done when* new business is refused, lines are held and servicing is reassigned with customers told.

### D10 Northlight Dealer Group
- **US-D10-1** As a former aggregator, I want override on business written while the FAP was a member, so that I am paid what was agreed. *Done when* the dated fragment still applies to books existing at the change date.

### D11 Longbay Dealer Group
- **US-D11-1** As the new aggregator, I want override on new books only, so that I am paid for what I support. *Done when* new books reference my fragment.

### D12 Ridgeway Employee Benefits Ltd
- **US-D12-1** As a broker replaced mid-year, I want brokerage to the appointment date, so that I am paid for my part. *Done when* the scheme's book changes from the appointment date with any agreed split as dated lines.

### D13 Summit Workplace Benefits Ltd
- **US-D13-1** As the incoming broker, I want access to the scheme from my appointment and not before, so that the handover is clean. *Done when* portal scope starts on the relationship date.

### D14 Keystone Home Loans Ltd
- **US-D14-1** As a referral partner, I want my fixed fee when the policy issues, so that referrals are worth making. *Done when* a fixed fee component pays on issue with its own clawback terms and no customer access.

### D15 Anchor Business Finance Ltd
- **US-D15-1** As a financier, I want my assignment honoured until I release it, so that my security is real. *Done when* removal of the instruction without my recorded consent is refused.

## Reinsurers, employers, providers and delegates

### R1 Kestrel Re
- **US-R01-1** As a reinsurer, I want a statement and listing that agree with each other and carry only the data the treaty needs, so that I can settle without queries. *Done when* cession records sum to the statement and the listing holds no names or contact details.

### R2 Halyard Re
- **US-R02-1** As a facultative reinsurer, I want referrals with controlled access to evidence and my terms stored exactly, so that I am bound to what I offered. *Done when* evidence is released from the vault under a logged purpose and the cession carries my reference.
- **US-R02-2** As an overseas reinsurer, I want net settlement in the treaty currency, so that I am paid what the statement says. *Done when* one net payment goes over the cross-border rail and the clearing accounts return to zero.

### R3 Stormline Re
- **US-R03-1** As a catastrophe reinsurer, I want the event evaluated across the portfolio, so that the recovery is right. *Done when* the aggregate treaty responds from events and the recovery posts to internal accounts by command.

### E1 Kai Fresh Supermarkets Ltd
- **US-E01-1** As a large employer, I want a year of monthly files to run without manual fixes, so that payroll and insurance stay in step. *Done when* twelve files apply with previews, backdated changes adjust the next invoice and every reconciliation is clean.
- **US-E01-2** As an employer at renewal, I want the offer early and old rates to continue if we have not agreed, so that cover never stops by accident. *Done when* the renewal workflow starts on time and the scheme's terms decide the fallback.

### E2 Te Ara Public Service Scheme
- **US-E02-1** As a very large scheme, I want our size never to slow anyone else down, so that we are a good neighbour. *Done when* billing and files run in chunks under the apply budget.

### E3 Southline Labour Hire Ltd
- **US-E03-1** As an employer who shares little data, I want members covered on attributes alone, so that we need not hand over identities. *Done when* unnamed member records rate and bill correctly and identity is created only at claim.

### E4 Heretaunga Packhouse Ltd
- **US-E04-1** As an employer whose payroll export changes yearly, I want the mapping updated and proven before it is used, so that a new format does not corrupt membership. *Done when* a new mapping version passes its sample-file tests and a successful preview confirms it.

### TPA1 Benefit Admin Partners Ltd
- **US-TPA1-1** As a delegate, I want clear rejections when I exceed my authority, so that I know what to send to the insurer. *Done when* each rejected decision returns a reason and routes to the insurer's queue.

### PR1 Riverside Surgical Centre
- **US-PR1-1** As a provider, I want pre-approval answered quickly and to be paid directly with a remittance advice, so that patients are not out of pocket. *Done when* the response meets the timer and the provider run pays with an advice per provider.

### PR2 BrightSmile Dental
- **US-PR2-1** As the insurer's fraud team, we want repeated billing patterns from a provider caught quietly, so that we can investigate without tipping off. *Done when* the rule raises a restricted case invisible to front-line staff and recoveries offset the next provider run.

### FA1 Custodia Fund Services
- **US-FA1-1** As a fund administrator, I want units on issue to agree before a price is struck, so that we never price on a mismatch. *Done when* a difference blocks the price.

### BK1 Primary banking partner
- **US-BK1-1** As the banking partner, I want every file and item uniquely identified, so that a resubmission is never paid twice. *Done when* a resubmitted file is rejected by hash and a repeated API call returns the original result.

### BK2 Second banking partner
- **US-BK2-1** As the continuity bank, I want urgent payments redirected to me by configuration, so that an outage does not stop claims. *Done when* urgent runs switch and customers see the expected date.

## Gap personas

### N1 Wendy Aperahama
- **US-N01-1** As a scheme member the insurer has never heard of by name, I want my claim accepted and my identity established then, so that anonymity before does not block me now. *Done when* identification creates a party, links the member record and carries history.

### N3 Chidi Okafor
- **US-N03-1** As a life insured living overseas, I want my own disclosure, consent and privacy, so that my wife does not see my answers. *Done when* I sign my own disclosure and consent separately.

### N4 Daniel Kereama
- **US-N04-1** As a wheelchair user, I want a fair route to the wellness discount, so that I am not priced out by a step count. *Done when* a manual award or alternative ends in the same entitlement command and the programme's terms record the alternative.
- **US-N04-2** As a screen reader user, I want every journey usable without sight, so that I can self-serve. *Done when* app and portal journeys pass assistive technology checks and documents have accessible formats.

### N5 Rob Sutherland
- **US-N05-1** As a bankrupt policyholder, I want to know who controls my policy's value, so that I do not act unlawfully. **(design gap)** *Done when* the design says how an insolvency official's claim is recorded and enforced.

### N6 Shane Poutama
- **US-N06-1** As a prisoner, I want my policy kept in order by post and through a representative, so that I do not lose cover inside. **(design gap)** *Done when* post-only service and a representative work, and the design says how imprisonment affects income protection.

### N7 Leilani Fonoti
- **US-N07-1** As a staff member with my own policies, I want my file handled by people who do not know me, so that my privacy is equal to any customer's. *Done when* my colleagues have no path to my records and my own lookups of relatives are refused and flagged.

### N9 Petra Novak
- **US-N09-1** As someone who left my employer, I want to keep my cover without medical questions and claim on it later, so that changing jobs does not uninsure me. *Done when* the continuation policy is issued inside the window, linked to the scheme history, and a claim is paid on it.

### N10 Dorothy Chen
- **US-N10-1** As a person in care, I want my sister to keep my old policy and annuity in order, so that I am looked after. *Done when* the representative satisfies proof of life and receives my notices.

### N11 Michael Brady
- **US-N11-1** As an adult under guardianship, I want my money kept separate and my cover managed for me, so that I am protected. *Done when* my policy sits in my own group and my guardian's actions are scoped and reported.

## Staff

### ST1 Junior underwriter
- **US-ST01-1** As a junior underwriter, I want only cases I can decide offered to me, so that I never exceed my authority by accident. *Done when* an over-limit decision is not offered and the case routes up.

### ST2 Senior underwriter
- **US-ST02-1** As a senior underwriter, I want to countersign declines and large sums with the file in front of me, so that the second signature means something. *Done when* I record my own reason codes and both identities are on the decision.

### ST3 Health claims assessor
- **US-ST03-1** As an assessor, I want the cover check to give me facts and not a verdict, so that I make the decision. *Done when* I see in-force status, definitions and exclusion codes and record what I relied on.

### ST4 Senior claims assessor
- **US-ST04-1** As a case manager, I want reviews to come to me as tasks on their dates, so that nobody has to remember. *Done when* review plans generate timers and a missed review flags the book without stopping a payment.

### ST5 Collections officer
- **US-ST05-1** As a collections officer, I want my day ordered by what is about to lapse and worth most, so that I spend time where it matters. *Done when* the queue sorts by days to lapse then annual premium and I see no health information.

### ST6 Hardship specialist
- **US-ST06-1** As a specialist, I want to see the adjustments and the recommended approach and not a label, so that I treat the person and not the flag. *Done when* routed actions show adjustments in effect and the kind only where my role needs it.

### ST7 Group administrator
- **US-ST07-1** As a group administrator, I want to act only on my assigned schemes and cover a colleague's by grant, so that access matches work. *Done when* a cover grant inherits paths and not authority.

### ST8 Unit pricing maker
- **US-ST08-1** As a pricing maker, I want checks to stop a bad price before anyone sees it, so that errors are caught at the source. *Done when* a failed check holds the price for review.

### ST9 Unit pricing checker
- **US-ST09-1** As a checker, I want to be unable to approve my own price, so that the control is real. *Done when* maker and checker must differ by policy.

### ST10 Finance operator
- **US-ST10-1** As a finance operator, I want payment runs that reconcile themselves to the bank, so that breaks are exceptions and not searches. *Done when* submitted equals acknowledged plus rejected and settled plus returned.

### ST11 Finance lead
- **US-ST11-1** As the finance lead, I want to be unable to sign off while a blocking check is red unless I record a waiver, so that the close means something. *Done when* sign-off is refused or a waiver event with owner and date exists.

### ST12 Appointed actuary
- **US-ST12-1** As the appointed actuary, I want to approve the effect of a table and not just the table, so that I sign what customers will feel. *Done when* my approval is on the dry-run report and activation carries it.
- **US-ST12-2** As the actuary, I want extracts that are frozen and reconcile, so that the valuation stands on firm ground. *Done when* opening plus movements equals closing and the extract hash is anchored.

### ST13 Compliance investigator
- **US-ST13-1** As an investigator, I want restricted cases nobody else can see exist, so that an investigation is not tipped off. *Done when* front-line staff and the claimant see no trace of the referral.

### ST14 Complaints reviewer
- **US-ST14-1** As a reviewer, I want every complaint to reach deadlock notice automatically if we run out of time, so that none ages quietly. *Done when* the timer sends the notice with the scheme's details.

### ST15 Product owner
- **US-ST15-1** As a product owner, I want to see who a change affects before I activate it, so that no customer is surprised. *Done when* the change record carries a dry run and activation is refused without approvers.

### ST16 Platform engineer and incident commander
- **US-ST16-1** As an incident commander, I want every incident to end in a remediation case that replays affected policies, so that fixes are complete and provable. *Done when* every affected policy is adjusted or excluded with a reason.
- **US-ST16-2** As a platform engineer, I want the system to stop the line on an integrity break and to replay identically, so that correctness is never a matter of opinion. *Done when* a recorded log replays to the same state hash and an invariant break halts posting.

### ST17 Treasury and payment run approver
- **US-ST17-1** As a run approver, I want to be unable to approve what I prepared or to edit payees, so that fraud needs more than one person. *Done when* segregation is enforced by policy.
