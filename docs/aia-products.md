# AIA New Zealand products

This system now carries the AIA New Zealand product range. The products were built from
AIA's public material as at 14 November 2026: its product pages, brochures and the policy
wordings it publishes at documents.aia.co.nz.

The structures follow the published material. That covers benefits, entry and expiry ages,
cover limits, option menus, built-in benefits, discounts, grace periods and cooling off.
Premium rates and premium factors are best-estimate placeholders, because AIA does not
publish its rates. Every placeholder is marked `PH` in the product file and listed below.

Every AIA premium is also checked, to the cent, against an integer recalculation (`crates/product/tests/recalculation.rs`): each option choice's factor, every frequency, the policy fee, GST, each premium type's rating age, and the Multi-Benefit Discount. That check found a fault: a premium that is exactly half a cent per period was rounded up. Dividing by the number of periods last fixed it.

Each definition in `products/definitions/aia.star` summarises a claim event and names the AIA appendix that governs it. Since 14 Nov 2026 the published wordings themselves are loaded too (see "The full wordings" below), and each cover is bound to both, so a claim is assessed against AIA's own words.

## The full wordings

- **Where they are:** `products/wordings/aia/<code>.txt`, one file per published document (34). Each starts with a header (title, document code and version, source URL, date retrieved), then the text in full.
- **How they were read:** AIA's online editions at documents.aia.co.nz, page by page, with `[page k of N]` markers. Text laid out side by side appears in the order the edition holds it. Interim Cover and the Starter Plan came from the adviser PDFs. AIA's own published document governs; the copy is what claims reads beside it.
- **How they reach the system:** `python3 tools/aia_wordings.py` turns the files into `products/definitions/aia_wordings.star`: one definition `aia-wording-<code>` each, titled with the document code and version. The sim config activates it after `aia.star`. Each AIA product file has a `WORDINGS` map that binds every cover to the umbrella wording, its own appendix and any wording that applies to it (waiver of premium, redundancy, business income support). A cover then holds the hash of that text, so a claim is assessed against the exact words in force when the cover started.
- **When AIA changes a wording:** replace the file, set the new version in its header, rerun the tool and add the definition version; existing covers keep the hash they were bound to.
- **Gaps:**
  - **Essentials Life:** AIA's online edition has no text layer, so it is not loaded. Essentials covers stay on their summaries until AIA supplies the text.
  - **Assessor screens:** the core keeps only the hash of a definition, so claims and documents have no route yet that serves the wording text.
  - **Corporate Solutions:** each scheme has its own policy document, so none is loaded.
- **Test:** `crates/product/tests/aia.rs::every_aia_cover_is_bound_to_the_wordings_that_govern_it`.

## The range

| Product file | Code | What it is |
|---|---|---|
| `aia_living.star` | `aia-living` | AIA Living (personal), the modular adviser range. It covers life cover, Family Protection and accidental death. Critical Conditions, Progressive Care and Severe Trauma come stand-alone or accelerated, and TPD comes own or any occupation, stand-alone or accelerated. Income comes as agreed value, indemnity or loss of earnings, plus Mortgage, Income or Rent Cover and Retirement Protection. The add-ons are Specified Accidental Injury and Specialist and Testing Support. |
| `aia_living_business.star` | `aia-living-business` | AIA Living Business. It has the business versions of life, trauma and TPD, each with the Business Safeguard facility, plus Business Continuity (agreed value and indemnity), Rural Continuity (with Business Income Support) and business Specified Accidental Injury. |
| `aia_start_up_income.star` | `aia-start-up-income` | Start-Up Income Protection, for business owners trading three years or less. |
| `aia_cover_for_life.star` | `aia-cover-for-life` | Cover for Life: whole of life cover, with premiums paid to a chosen age from 65 to 85 and the cover then continuing. |
| `aia_essentials_life.star` | `aia-essentials-life` | Essentials Life Cover: simplified-question life cover from $50,000 to $1m. |
| `aia_starter.star` | `aia-starter` | AIA Starter Plan: life cover, rent or mortgage cover and specialist cover for first-time buyers. |
| `aia_interim.star` | `aia-interim` | AIA Living Interim Cover: free cover while an application is underwritten. It pays life cover on death by accident or illness up to $500,000. It also pays accidental TPD and accidental trauma up to $150,000 each, and accidental disablement income of $2,000 a month for up to 24 months. |
| `aia_private_health.star` | `aia-private-health` | AIA Private Health, with the Private Health Plus upgrade. |
| `aia_cancer_care.star` | `aia-cancer-care` | AIA Cancer Care. |
| `aia_corporate.star` | `aia-corporate` | AIA Corporate Solutions group schemes. It covers group life, optional group TPD, group critical illness, Severe Trauma for Corporate Solutions (from 5 May 2026) and group income protection, each by benefit category. |

All of these are effective from 14 November 2026 and sit in statutory fund SF1-life (health
in SF2-health). Closed and past ranges are not built: TCM, AIA Real, Sovereign, Absolute
Health and MajorCare. Bank-distributed variants of the health wordings (the ASB health
wordings 1302 and 1305) are also not built.

## Sources

Every AIA wording is at `https://documents.aia.co.nz/<slug>/`. Versions current on 14 Nov 2026:

- **Umbrella:** 1100 AL-UMB v8 (12 May 2026).
- **Life and death:** 1101 AL-LIFE v5 (10 Oct 2023), 1102 AL-AD v3, 1103 AL-FP v6.
- **Trauma:** 1105 AL-CC v9 (22 Sep 2026), 1106 AL-PC v7, 1119 AL-ST v2.
- **TPD:** 1104 AL-TPD v4.
- **Income:** 1108 AL-IP v5, 1115 AL-IND v5, 1109 AL-LOE v5, 1117 AL-MIRC v2, 1111 AL-RP v2 (23 Jul 2021), 1113 AL-RED v1 (5 Aug 2019).
- **Waiver of premium:** 1114 ALP-WOP v1.
- **Add-ons:** 1116 AL-SAIC v1 (1 Nov 2024), 1118 AL-STS v2 (1 Nov 2026).
- **Business:** 1151 ALB-LIFE v4, 1155 ALB-CC v8, 1156 ALB-PC v7, 1166 ALB-ST v2, 1154 ALB-TPD v2 (all 12 May 2026), 1158 ALB-BC v3, 1159 ALB-RC v4, 1160 ALB-BI v3, 1164 ALB-WOP v1, 1165 ALB-SAIC v1, 1180 A-SU IP v3.
- **Other life:** 1400 A-CFL v1 (20 Mar 2026), Essentials Life Cover wording (online edition has no text layer), AIA Starter Plan v1, and Interim Cover X00066 002a 2512.
- **Health:** 1300 APH-UMB v8 (1 Nov 2026; renewals from 23 Oct 2026), 1301 APH-WOP v1, 1304 AIA Cancer Care-UMB v5 (1 Nov 2026).
- **Group:** the Corporate Solutions brochures for life and TPD, critical illness and income protection. AIA does not publish scheme policies.
- **Product pages and brochures** on aia.co.nz for life, trauma, TPD, income protection, business, rural, health, and the Multi-Benefit Discount and AIA Vitality.

## How the products map onto the system

- **Option menus (option axes).** A cover's menu is a list of axes, such as waiting period, benefit period, premium type, waiver of premium or an optional upgrade. A life chooses one choice on each axis. The layer stores the selection as one code, `axis=choice,...` in axis order, in the field `excess_option`. The premium factor is the product of the chosen factors. The terms (waiting days, benefit months or age, expiry age or term, premium basis, features) are the union of what each choice sets, and two choices that set the same term are refused. A choice can be offered only with named choices on other axes: for example, the mental health limit is only offered with a benefit period to 65 or 70. See `crates/product/src/options.rs` and docs/decisions.md.
- **Premium types.** These are rate for age (stepped), level to an age (65 or 80), level for 10 years (fixed for blocks of years, each block at the age it began), and Cover for Life's limited pay. Under limited pay, the premium is fixed at the entry age and stops at the anniversary on or after the pay-to age, and the cover continues. The core prices and bills on these.
- **Entry ages** per cover (`min_entry_age`, `max_entry_age`) are checked at issue and when a cover is added, using the life's age on the day the cover starts. The reject code is `entry_age`.
- **Built-in benefits** are listed on each cover with their kind (early payment, additional, reimbursement, monthly addition, facility, premium waiver), their limits (a cap, a share of the cover, a multiple of the monthly benefit; the lowest applies) and, for optional ones, the feature that switches them on. Claims reads these. The core does not pay them on its own.
- **Accelerated benefits** reduce life cover (`reduces = ["life"]`) and need life cover on the same life (composition `requires`).
- **The Multi-Benefit Discount** is a discount condition, `benefit_categories`, judged per life. The life needs a required category at its minimum: life or accidental death at $100,000, or Family Protection at $1,000 a month. It then gets 10%, 12.5% or 15% for one, two or three other categories at their minimums: trauma $75,000, TPD $75,000, income $2,000 a month. Only the best tier applies.
- **AIA Vitality** is a tiered discount (0% to 20%) from the entitlement `aia-vitality`, which a wellness module sets for each life.
- **Cover rules.** Every product needs a main benefit. Stand-alone and accelerated versions of one benefit are exclusive, and only one income basis may be held. Retirement Protection needs one of the income covers (`requires_any`).
- **Group schemes** use benefit categories (staff and executive) with salary multiples, category maximums and automatic acceptance limits, and they are rated on scheme unit-rate tables. Group income is a monthly share of salary: 0.0625 of annual salary is 75% a year.
- **New cover types:** `tpd` and `accidental_injury`. Underwriting accepts them, and its exclusions and rules name them where they apply.

## Placeholders (best estimates, to be replaced)

| What | Where | Placeholder |
|---|---|---|
| Every premium rate | `products/rates/aia_*.star`, from `tools/aia_placeholder_rates.py` | A smooth curve per table: a rate at 30 for a female non-smoker in class 1, rising a fixed factor every five years, scaled for sex, smoking and occupation class. Group tables are two flat category rates. |
| Premium type factors | personal and business | Level 10 years 1.18; level to 65 1.62; level to 80 2.05 (on the rate for age at entry). |
| Optional benefit factors | personal and business | Waiver 1.06; Life Future Insurability 1.02; Early Cancer Upgrade 1.04; TPD condition 1.12; Children's and Maternity 1.03; trauma buy-back 1.07; life buy-back 1.05; TPD to 70 1.09; Business Safeguard 1.03. |
| Income factors | personal | Waiting 2, 4, 8, 13, 26, 52, 104 weeks: 1.38, 1.00, 0.74, 0.57, 0.43, 0.34, 0.29. Benefit period 1y, 2y, 5y, to 65, to 70: 0.46, 0.61, 0.79, 1.00, 1.12. Premier 1.11. Mental health limit 0.90. Claim indexation 1.10. Redundancy 1.04. |
| Business, rural and start-up factors | business, start-up | Waiting 4/8/13 weeks 1.00/0.78/0.62; period 6/12/24 months 0.62/1.00/1.45; partial disablement 1.12; ACC offset 0.80; peak season 1.08; Business Income Support 1.15. |
| Cover for Life pay-to factors | `aia_cover_for_life.star` | To 65: 2.10. To 70: 1.85. To 75: 1.70. To 80: 1.60. To 85: 1.55. |
| Essentials, Starter | their files | Simplified underwriting 1.15; Starter online 1.12; Starter rent cover 6/12 months 0.50/0.75; Starter entry 18 to 55, expiry 70. |
| Health excess factors | private health, cancer care, specialist testing | Private Health excesses $0 to $10,000: 1.55 down to 0.47. Plus nil 1.22. Cancer Care and specialist testing nil 1.25 to 1.30. Health entry to 74. Cancer screening allowance $500. |
| Minimum cover amounts AIA does not publish | personal, business | Lump sums $10,000; monthly benefits $500; SAIC $100 to $5,000 a month. Income maximums are $30,000 a month and MIRC $15,000. Start-up is $500 to $10,000 a month. |
| Family Protection in the Multi-Benefit Discount | `aia_living.star` | $1,000 a month. AIA converts the monthly amount to a lump sum over the term, which this system does not. |
| Policy fee and modal loadings | each product | $75 a year (none on Cover for Life and Essentials). Fortnightly and monthly 6%, quarterly 4%, half-yearly 2%. |
| Reinstatement | each product | 180 days, evidence-free within 60 days (life), or 90 and 30 (health and simplified). |
| Corporate scheme terms | `aia_corporate.star` | Multiples: life 3x (staff) and 5x (executive), TPD the same, CI 1x, income 75%. Maximums and automatic acceptance limits are as in the file. The income waiting period is 90 days and the benefit period 24 months. In practice each is set per scheme. |

## Each product end to end

`crates/underwriting/tests/aia_journeys.rs` takes each product through the modules as they run in production. The core quotes the application. The underwriting module takes it from proposal to decision. The driver carries it to reinsurance for capacity and to the core to issue on the quoted product version. The core bills three months, each bill equal to the quote, and takes the payments. The claims module on the partition admits a claim, and the payments module settles it.

| Product | Journey | Claim paid |
| --- | --- | --- |
| aia-living | life, Critical Conditions and agreed value income on one life; AIA Interim Cover written while underwritten and ended at issue | Critical Conditions at the sum insured |
| aia-living-business | key person life cover | death, ending the policy |
| aia-start-up-income | income for occupation class 2 | first month, after the waiting period, by a benefit run |
| aia-cover-for-life | whole of life cover | death |
| aia-essentials-life | life cover | death |
| aia-starter | life cover | death |
| aia-interim | written by underwriting when an AIA Living proposal opens | death during underwriting |
| aia-private-health | Private Health at its first excess | surgery invoice, under the product's own limits, paid to the provider by a run |
| aia-cancer-care | Cancer Care at its first excess | cancer treatment invoice, paid to the provider by a run |
| aia-corporate | scheme issued on the core with three staff members and billed to the employer | a member's death, with the scheme staying in force |

Two limits on what these journeys show:

- **Corporate Solutions** is issued straight to the core with its members. In production, group administration (`ic-group`) sends membership from the employer's file. That path is tested for group life by the group thread, but not with `aia-corporate`.
- **Claims rules and authority** are the insurer's own to set and are not shipped (docs/claims.md). The journeys activate a minimal set. A health invoice is priced on the AIA product's own limits for its category.

## Not built yet, and who owns it

- **Claims (`ic-claims`):** built on 14 November (docs/claims.md, "What the chosen option sets"): the benefit period and to-age, built-in benefits within their limits and only where their feature is chosen, product lifetime limits, a per-event cap, the mental health limit in all and claim indexation. What is left is the core's part below.
- **The core, for claims:** done (14 Nov). `cover_facts` fills `benefit_months`, `benefit_to` (the first day the life is the option's `benefit_to_age`), `mental_health_months` (24 where the option has `mental-health-limit`; the period is a constant in `crates/core/src/claims.rs`, since the products name the feature and not the months), `claim_indexation`, `health_limits`, `event_cap_minor` (a product now declares `event_cap`, and `aia-interim` declares $500,000) and `built_in`. Test: `crates/sim/tests/aia.rs::the_claim_facts_carry_the_terms_of_the_option_chosen_and_the_products_limits`. Where the option ends the benefit neither by months nor by age, or the cover has no options, `benefit_months` is the cover's own monthly benefit period (24 on `aia-interim` accidental income, 60 on `income-protection`).
- **Underwriting (`ic-underwriting`):** done on 14 November (9f9394e; `src/eligibility.rs`, docs/underwriting.md). An AIA application outside these conditions is refused `eligibility`: occupation class required for TPD, income and accidental injury; TPD entry to 55, or 60 in class 1; income to 70 only in classes 1 and 2; Family Protection to an age at least 15 years from entry; trauma at most $250,000 under 16; Essentials at most $500,000 from 61 to 70. Proposals carry the option code to the core, and AIA Living proposals get `aia-interim` interim cover. The class mapping is a placeholder until AIA's occupation guide is mapped (class 1 clerical, managerial and professional).
- **Wellness:** built on 14 November (`crates/wellness`, docs/wellness.md). It gives the 10% Initial Discount on joining, applies Premium Flex two weeks after each policy anniversary from the anniversary, and removes the discount from the next premium due on leaving. Still missing: the intake from AIA's Vitality programme (its format is AIA's to give) and the notice to the policy owner before a change.
- **The core:**
  - Paid-up alteration for Cover for Life after half the premium term: built by the core for the owner's request (`MakePaidUp`, edb26cf, 14 Nov); the reduction factor 0.95 is a placeholder. The automatic conversion when a premium is overdue after the paid-up date is not built.
  - Premium suspension (up to 12 months).
  - Multi-Benefit Discount counting of cover a life holds on another policy: built on 14 November by the AIA thread. A category's `elsewhere` names `product/cover` entries, and the core counts what the same person (by party) holds in force on their other retail policies in the same group (`Policy.sums_elsewhere`, recounted each time the policy is worked on). A change shows on the next bill. Not counted: policies in another group, and so on another partition, and AIA's closed or Sovereign policies, which this system does not hold. Test: `crates/sim/tests/aia.rs::cover_the_life_holds_on_another_policy_counts_towards_the_multi_benefit_discount`.
  - Special events increases capped in money in total, which is approximated by two uses.
  - The Life Future Insurability anniversary option, which is declared only as a facility.
- **Reinsurance:** done by the reinsurance thread (14 Nov). The shipped synthetic treaties name the AIA range: life, trauma and TPD on the retail products go to the retail surplus treaty, and aia-corporate group life goes to the group treaty. Income, health and accidental injury are retained. Real AIA treaty terms would replace these.

## Questions for Mitchell

1. **Rate tables.** Real premium rates are needed for every `aia-*` table, and real premium factors for each `PH` above. AIA does not publish them, so they must come from AIA, from a reinsurer, or from your appointed actuary.
2. **Use of AIA's names:** settled. Mitchell confirmed on 14 Nov 2026 that the system is being built for AIA, so AIA's product names, marks and published material may be used.
