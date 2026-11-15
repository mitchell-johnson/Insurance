// The cast: who signs in during the suite, as which persona from
// `docs/testing/personas/personas-insurance.md`, and what the insurer has
// let each of them do. `stack/seed.js` sets all of it up through the
// system's own routes before the tests run; nothing here is a test double.
//
// A staff member's `party` is their workforce directory ID, which is also
// the ID the claims authority book, the underwriting profiles and the
// complaints records know them by.
"use strict";

// Role templates: the actions and data classes a role carries.
const O = "ordinary", P = "personal", F = "financial", S = "insurance_sensitive";
const templates = [
  ["administrator", ["staff.manage", "staff.approve_grant", "service.approve_change"], [O]],
  ["service", ["policy.view", "service.enter_change", "customers.find"], [O, P]],
  ["service-lead", ["policy.view", "service.enter_change", "service.approve_change", "customers.find"], [O, P]],
  ["collections", ["policy.view", "service.enter_change", "payments.collections", "payments.view", "customers.find"], [O, P, F]],
  ["hardship", ["policy.view", "service.enter_change", "service.vulnerability_kind", "customers.find"], [O, P]],
  ["held-balance", ["held_balance.view", "held_balance.prepare", "held_balance.approve"], [O, F]],
  ["held-balance-legal", ["held_balance.view", "held_balance.approve", "held_balance.approve_legal"], [O, F]],
  ["conduct-officer", ["held_balance.view", "held_balance.approve_conduct", "complaints.conduct"], [O, F]],
  ["unit-pricing-maker", ["unit_pricing.view", "unit_pricing.prepare"], [O]],
  ["unit-pricing-checker", ["unit_pricing.view", "unit_pricing.approve"], [O]],
  ["unit-pricing-lead", ["unit_pricing.view", "unit_pricing.lead", "unit_pricing.approve"], [O]],
  ["frontline", ["complaints.record"], [O]],
  ["complaints-team", ["complaints.handle", "complaints.vulnerability_trained"], [O, P]],
  ["complaints-junior", ["complaints.handle"], [O, P], "junior"],
  ["complaints-legal", ["complaints.legal"], [O]],
  ["ledger-operator", ["finance.operate"], [F]],
  ["ledger-approver", ["finance.approve"], [F]],
  ["ledger-lead", ["finance.lead"], [F]],
  ["actuary", ["finance.view", "finance.actuary"], [F]],
  ["ledger-reader", ["finance.view"], [F]],
  ["claims", ["claims.work", "policy.view", "customers.find"], [O, P, S]],
  ["ri-ops", ["reinsurance.operate"], [F]],
  ["ri-lead", ["reinsurance.operate", "reinsurance.approve"], [F]],
  ["ri-reader", ["reinsurance.view"], [F]],
  ["records", ["documents.download", "policy.view"], [O]],
  ["distribution-admin", ["commission.view", "commission.structure"], [O]],
  ["commission-ops", ["commission.view", "commission.ops"], [O, F]],
  ["commission-approver", ["commission.view", "commission.approve"], [O, F]],
  ["commission-compliance", ["commission.view", "commission.compliance"], [O]],
  ["commission-reader", ["commission.view"], [O]],
  ["underwriter", ["underwriting.work"], [S]],
  ["senior-underwriter", ["underwriting.work", "underwriting.senior"], [S]],
  ["payments-maker", ["payments.maker"], [F]],
  ["payments-approver", ["payments.approver", "payments.treasury"], [F]],
  ["payee-maintainer", ["payments.payees"], [F]],
  ["payments-conflicted", ["payments.approver", "payments.payees"], [F]],
  ["payments-legal", ["payments.legal"], [F]],
  ["payments-reader", ["payments.view"], [F]],
  ["payments-compliance", ["payments.compliance", "payments.view"], [F]],
  ["payments-reconciler", ["payments.reconciler", "payments.view"], [F]],
  ["group-admin", ["group.view", "group.admin"], [O]],
  ["group-reader", ["group.view"], [O]],
  ["product-owner", ["staff.configure"], [O]],
].map(([id, actions, classes, profile]) => ({ id, actions, classes, default_profile: profile || "standard", ceilings: {}, break_glass: false }));

const profiles = [
  { id: "standard", version: 1, limits: { held_balance: 100_000_00, complaints_authority: 3, assisted_change: 500_00 } },
  { id: "junior", version: 1, limits: { complaints_authority: 1, assisted_change: 100_00 } },
];

// Staff: [key, persona, what they are, templates]. The key is how tests name them.
const staff = [
  ["admin", "Staff administrator", "Staff administrator", ["administrator"]],
  ["st1", "ST1", "Junior underwriter", ["underwriter"]],
  ["st1b", "ST1", "Junior underwriter (second)", ["underwriter"]],
  ["st2", "ST2", "Senior underwriter", ["senior-underwriter"]],
  ["st2b", "ST2", "Senior underwriter (second)", ["senior-underwriter"]],
  ["st3", "ST3", "Health claims assessor", ["claims"]],
  ["st4", "ST4", "Senior claims assessor and case manager", ["claims"]],
  ["st4b", "ST4", "Senior claims assessor (second signature)", ["claims"]],
  ["n7", "N7", "Leilani Fonoti, claims assessor who is also a customer", ["claims"]],
  ["st5", "ST5", "Collections officer", ["collections"]],
  ["st5b", "ST5", "Service officer", ["service"]],
  ["st5l", "ST5", "Service team lead", ["service-lead"]],
  ["st6", "ST6", "Hardship and vulnerable customer specialist", ["hardship"]],
  ["st7", "ST7", "Group administrator", ["group-admin"]],
  ["st7r", "ST7", "Group administrator (read only)", ["group-reader"]],
  ["st8", "ST8", "Unit pricing maker", ["unit-pricing-maker"]],
  ["st9", "ST9", "Unit pricing checker", ["unit-pricing-checker"]],
  ["st9l", "ST9", "Unit pricing lead", ["unit-pricing-lead"]],
  ["st10", "ST10", "Finance operator", ["ledger-operator", "payments-maker", "held-balance"]],
  ["st10b", "ST10", "Finance approver", ["ledger-approver", "held-balance"]],
  ["st11", "ST11", "Finance lead", ["ledger-lead", "held-balance"]],
  ["st12", "ST12", "Appointed actuary", ["actuary"]],
  ["st13", "ST13", "Compliance and fraud investigator", ["payments-compliance", "commission-compliance", "claims"]],
  ["st14", "ST14", "Complaints reviewer", ["complaints-team"]],
  ["st14j", "ST14", "Complaints handler (junior)", ["complaints-junior"]],
  ["st15", "ST15", "Product owner", ["product-owner"]],
  ["st17", "ST17", "Treasury and payment run approver", ["payments-approver"]],
  ["st17b", "ST17", "Treasury and payment run approver (second)", ["payments-approver"]],
  ["front", "Frontline", "Frontline staff", ["frontline"]],
  ["legal", "Legal", "Legal counsel", ["complaints-legal", "held-balance-legal", "payments-legal"]],
  ["conduct", "Conduct", "Conduct officer", ["conduct-officer"]],
  ["auditor", "Auditor", "Internal auditor (read only)", ["ledger-reader", "payments-reader", "ri-reader", "commission-reader"]],
  ["records", "Records", "Records officer", ["records"]],
  ["dist", "Distribution", "Distribution administrator", ["distribution-admin"]],
  ["comops", "Commission", "Commission operations", ["commission-ops"]],
  ["comapp", "Commission", "Commission approver", ["commission-approver"]],
  ["ri1", "Reinsurance", "Reinsurance operations", ["ri-ops"]],
  ["ri2", "Reinsurance", "Reinsurance lead", ["ri-lead"]],
  ["payee", "Payments", "Payee maintainer", ["payee-maintainer"]],
  ["payee2", "Payments", "Payee maintainer (second pair of eyes)", ["payee-maintainer"]],
  ["paybad", "Payments", "Approver who also maintains payees", ["payments-conflicted"]],
  ["recon", "Payments", "Bank reconciler", ["payments-reconciler"]],
];

// Partners and customers: [key, persona, who they are, party].
const partners = [
  ["d1", "D1", "Tessa Maddox, adviser", "adv-d1"],
  ["d2", "D2", "Viliami Fifita, adviser", "adv-d2"],
  ["d4", "D4", "Anika Sharma, adviser at Kōwhai Financial", "adv-d4"],
  ["d5", "D5", "Joe Tane, adviser under investigation", "adv-d5"],
  ["d6", "D6", "Harbourline Advice Ltd, advice provider", "fap-d6"],
  ["d9", "D9", "Clearwater Advice Ltd, the provider that loses its licence", "fap-d9"],
  ["d9a", "D9", "An adviser engaged with Clearwater Advice", "adv-d9a"],
  ["d10", "D10", "Northlight Dealer Group", "grp-d10"],
  ["e1", "E1", "Kai Fresh Supermarkets Ltd, scheme administrator", "hr-e1"],
  ["e1b", "E1", "Kai Fresh Supermarkets Ltd, second administrator", "hr-e1b"],
  ["e3", "E3", "Southline Labour Hire Ltd, scheme administrator", "hr-e3"],
  ["p18", "P18", "Kiri Parata, customer", "cust-p18"],
  ["p1", "P1", "A retail customer", "cust-p1"],
];

const cast = {};
for (const [key, persona, about, held] of staff) cast[key] = { key, subject: `sub-${key}`, party: key, persona, about, templates: held, staff: true };
for (const [key, persona, about, party] of partners) cast[key] = { key, subject: `sub-${key}`, party, persona, about, staff: false };
// The platform engineer is known only by the operations role on the token, and is nobody's staff record.
cast.st16 = { key: "st16", subject: "sub-st16", roles: "operations", party: null, persona: "ST16", about: "Platform engineer and incident commander", staff: false };
// Someone the insurer has never heard of.
cast.stranger = { key: "stranger", subject: "sub-nobody", party: null, persona: "Anyone", about: "A sign-in the insurer does not know", staff: false };

module.exports = { cast, templates, profiles, staff: staff.map(([key]) => cast[key]), partners: partners.map(([key]) => cast[key]) };
