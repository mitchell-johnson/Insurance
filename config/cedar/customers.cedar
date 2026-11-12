// Who may do what in the customer master (crates/customers, docs/customers.md).
//
// Principal: Service::"<app role>", the role in the caller's Entra ID token.
// Action:    Action::"read" on Field::"<field>" (attributes: name, class),
//            Action::"<command>" on Command::"<command>",
//            Action::"match" | "search" | "relationships" | "export" | "routing"
//            | "merge" | "unmerge" | "reviews" | "audit" | "operate" on Party::"party".
// Context:   purpose (declared by the caller and logged), self (the person the
//            caller acts for is this party), actor_roles (the kinds of
//            relationship that person holds to a policy or scheme this party is
//            also related to), classes (field classes a record command touches).
//
// Default deny. Changes are made by pull request with two approvals.

// ---- Finding people ------------------------------------------------------

permit(principal, action == Action::"match", resource)
when {
    [Service::"group-admin", Service::"claims", Service::"underwriting", Service::"new-business",
     Service::"distribution", Service::"complaints", Service::"migration", Service::"staff"].contains(principal)
};

permit(principal, action == Action::"search", resource)
when {
    [Service::"staff", Service::"claims", Service::"compliance", Service::"distribution", Service::"complaints"].contains(principal)
};

// Relationships carry IDs and kinds only. The gateway builds its
// authorisation read model from them; the modules use them to find who to
// write to and who may act.
permit(principal, action == Action::"relationships", resource)
when {
    [Service::"portal", Service::"staff", Service::"claims", Service::"documents", Service::"group-admin",
     Service::"distribution", Service::"policy-servicing", Service::"compliance", Service::"payments",
     Service::"underwriting", Service::"new-business", Service::"complaints", Service::"reinsurance"].contains(principal)
};

// Whether a request should go to a person first (a vulnerability marker).
// Answers yes or no; the marker itself is never shown.
permit(principal, action == Action::"routing", resource)
when {
    [Service::"portal", Service::"staff", Service::"claims", Service::"policy-servicing", Service::"complaints"].contains(principal)
};

// ---- Reading fields, by purpose ------------------------------------------

// Notices and letters: who to address, where, and how.
permit(principal == Service::"documents", action == Action::"read", resource)
when {
    context.purpose == "notices" &&
    (resource.class == "contact" ||
     ["given_name", "middle_names", "family_name", "preferred_name", "organisation_name"].contains(resource.name))
};

// Claims: who the life and the claimant are, how to reach them, tax on payments.
permit(principal == Service::"claims", action == Action::"read", resource)
when {
    context.purpose == "claims" &&
    ["identity", "contact", "tax", "organisation"].contains(resource.class)
};

permit(principal, action == Action::"read", resource)
when {
    [Service::"underwriting", Service::"new-business"].contains(principal) &&
    context.purpose == "underwriting" &&
    ["identity", "contact"].contains(resource.class)
};

// Payee checks compare the account holder's name.
permit(principal == Service::"payments", action == Action::"read", resource)
when {
    context.purpose == "payee_verification" &&
    ["given_name", "middle_names", "family_name", "organisation_name"].contains(resource.name)
};

// Commission payees: names, organisation, tax details for withholding.
permit(principal == Service::"distribution", action == Action::"read", resource)
when {
    context.purpose == "commission" &&
    (["organisation", "tax"].contains(resource.class) ||
     ["given_name", "middle_names", "family_name", "preferred_name", "email", "postal_address"].contains(resource.name))
};

permit(principal == Service::"compliance", action == Action::"read", resource)
when {
    ["due_diligence", "tax_reporting"].contains(context.purpose) &&
    ["identity", "contact", "tax", "organisation"].contains(resource.class)
};

permit(principal == Service::"complaints", action == Action::"read", resource)
when {
    context.purpose == "complaints" &&
    ["identity", "contact", "organisation"].contains(resource.class)
};

permit(principal == Service::"staff", action == Action::"read", resource)
when {
    context.purpose == "servicing" &&
    ["identity", "contact", "tax", "organisation"].contains(resource.class)
};

// A person reading their own record through the app.
permit(principal == Service::"portal", action == Action::"read", resource)
when {
    context.purpose == "self_service" && context.self &&
    ["identity", "contact", "tax", "organisation"].contains(resource.class)
};

// An adviser reading a client they advise on a policy.
permit(principal == Service::"portal", action == Action::"read", resource)
when {
    context.purpose == "advice" && context.actor_roles.contains("adviser") &&
    ["identity", "contact"].contains(resource.class)
};

// An employer's administrator reading a scheme member's name.
permit(principal == Service::"portal", action == Action::"read", resource)
when {
    context.purpose == "scheme_administration" && context.actor_roles.contains("employer_administrator") &&
    ["given_name", "family_name", "preferred_name", "date_of_birth"].contains(resource.name)
};

// Break glass: everything but the restricted class, alerted and reviewed.
permit(principal == Service::"break-glass", action == Action::"read", resource)
when { context.purpose == "break_glass" && resource.class != "restricted" };

// ---- Changing records ----------------------------------------------------

permit(principal == Service::"staff", action, resource is Command)
when {
    [Action::"create", Action::"record", Action::"decline_correction", Action::"add_relationship",
     Action::"end_relationship", Action::"set_consent", Action::"activate", Action::"mark_deceased",
     Action::"retract_death", Action::"exit", Action::"assign_group"].contains(action)
};

permit(principal == Service::"migration", action, resource is Command)
when { action != Action::"place_hold" && action != Action::"release_hold" };

permit(principal == Service::"claims", action, resource is Command)
when {
    [Action::"create", Action::"record", Action::"add_relationship", Action::"end_relationship",
     Action::"mark_deceased", Action::"retract_death"].contains(action)
};

permit(principal, action, resource is Command)
when {
    [Service::"underwriting", Service::"new-business", Service::"distribution"].contains(principal) &&
    [Action::"create", Action::"record", Action::"add_relationship", Action::"end_relationship",
     Action::"set_consent", Action::"activate"].contains(action)
};

permit(principal, action, resource is Command)
when {
    [Service::"group-admin", Service::"policy-servicing"].contains(principal) &&
    [Action::"create", Action::"add_relationship", Action::"end_relationship", Action::"assign_group"].contains(action)
};

permit(principal == Service::"complaints", action == Action::"create", resource is Command);

permit(principal == Service::"compliance", action, resource is Command)
when {
    [Action::"record_verification", Action::"set_risk_rating", Action::"record"].contains(action) &&
    (action != Action::"record" || ["tax", "restricted"].containsAll(context.classes))
};

permit(principal == Service::"identity", action == Action::"record_verification", resource is Command);

permit(principal == Service::"legal", action, resource is Command)
when { [Action::"place_hold", Action::"release_hold"].contains(action) };

// A person changing their own contact details and consents in the app.
permit(principal == Service::"portal", action, resource is Command)
when {
    context.self &&
    (action == Action::"set_consent" ||
     (action == Action::"record" && ["contact"].containsAll(context.classes)))
};

// The vulnerability marker is set by staff trained to do it, never by the person.
forbid(principal, action == Action::"record", resource is Command)
when { context.classes.contains("restricted") && !([Service::"staff", Service::"compliance"].contains(principal)) };

// ---- Duplicates, access requests, audit and operations -------------------

permit(principal, action, resource)
when {
    [Service::"staff", Service::"migration"].contains(principal) &&
    [Action::"merge", Action::"unmerge", Action::"reviews"].contains(action)
};

permit(principal == Service::"staff", action == Action::"export", resource)
when { context.purpose == "subject_access" };

permit(principal == Service::"portal", action == Action::"export", resource)
when { context.purpose == "subject_access" && context.self };

permit(principal == Service::"privacy", action == Action::"audit", resource);

permit(principal == Service::"operations", action == Action::"operate", resource);
