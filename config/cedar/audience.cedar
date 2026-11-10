// Audience scoping: who sees what when the viewer is not the policyholder.
//
// Every read model row records the policy, cover and life it concerns. A row
// is `sensitive` when it carries an insurance fact that implies health: a
// loading, an exclusion code, the existence or type of a claim, a waiver of
// premium. The viewer's relationship to the policy arrives in the context,
// resolved from the customer master.

// The policyholder sees the whole policy, except sensitive rows about
// another person. Family privacy: an adult's health-implying facts are
// theirs, even when someone else owns and pays for the policy.
permit (principal, action == Action::"view", resource)
when
{
  context.relationships.contains("policyholder") &&
  (!resource.sensitive || resource.subject == principal)
};

// A life insured or a scheme member sees their own cover and nothing else
// from the policy or the employer's scheme.
permit (principal, action == Action::"view", resource)
when
{
  (context.relationships.contains("life_insured") ||
   context.relationships.contains("member")) &&
  resource.subject == principal
};

// The servicing adviser sees the policy but never sensitive rows. Consent to
// see health disclosures is given per application in the underwriting
// module, not here.
permit (principal, action == Action::"view", resource)
when { context.relationships.contains("adviser") && !resource.sensitive };

// An employer administrator sees the scheme and its billing, not any
// member's sensitive rows.
permit (principal, action == Action::"view", resource)
when
{
  context.relationships.contains("employer_admin") &&
  !resource.sensitive &&
  ["scheme", "billing", "member"].contains(resource.kind)
};

// A lender with an assignment sees that the policy exists and its standing.
permit (principal, action == Action::"view", resource)
when { context.relationships.contains("assignee") && resource.kind == "policy" };

// Stale relationship data is never good enough for sensitive rows.
forbid (principal, action == Action::"view", resource)
when { resource.sensitive && !context.fresh };
