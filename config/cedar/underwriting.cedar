// Underwriting authority: who may do what on an underwriting case.
//
// The principal is an underwriter with the attributes of their authority
// profile. The resource is one cover's terms inside a decision, or the case
// itself for work that decides nothing. The context carries the facts the
// module looked up: the profile's limit for this cover type, whether this
// person has already touched the case, and whether they have declared a
// relationship with anyone on the application.
//
// The profile is enforced, not advisory: with no permit, the action is not
// offered and cannot be recorded.

// Work that decides nothing (gathering evidence, booking and holding an
// interview, asking for an opinion) is open to anyone accredited for the
// product.
permit (principal, action == Action::"work", resource)
when { principal.products.contains(resource.product) };

// Deciding a cover: accredited for the product, and the sum insured within
// the profile's limit for the line.
permit (principal, action == Action::"decide", resource)
when
{
  principal.products.contains(resource.product) &&
  context.has_limit &&
  resource.sum_insured <= context.limit
};

// The second signature: a senior, within their own limit, who is not the
// first signer.
permit (principal, action == Action::"countersign", resource)
when
{
  principal.senior &&
  principal.products.contains(resource.product) &&
  context.has_limit &&
  resource.sum_insured <= context.limit &&
  !context.already_signed
};

// Loadings only up to the profile's level.
forbid (principal, action in [Action::"decide", Action::"countersign"], resource)
when { resource.loading_percent > principal.max_loading_percent };

forbid (principal, action in [Action::"decide", Action::"countersign"], resource)
when { resource.has_exclusions && !principal.may_exclude };

forbid (principal, action in [Action::"decide", Action::"countersign"], resource)
when { resource.outcome == "decline" && !principal.may_decline };

forbid (principal, action in [Action::"decide", Action::"countersign"], resource)
when { resource.outcome == "defer" && !principal.may_defer };

// Peer review and non-disclosure review: a senior who never touched the case.
permit (principal, action in [Action::"peer_review", Action::"review_non_disclosure"], resource)
when { principal.senior && !context.touched };

// Queue leads and managers hand work over and see workload.
permit (principal, action == Action::"reassign", resource)
when { principal.role == "queue_lead" || principal.role == "manager" };

// Nobody works, decides or reviews a case on someone they have declared a
// relationship with.
forbid (principal, action, resource)
when { context.conflict };
