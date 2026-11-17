// Staff authorisation for distribution and the held balance workbench.
//
// Every staff action answers four questions. What: is the action in the
// allow-list of one of the caller's roles. Whose: does the caller have a
// path to the record. How much: is the amount inside the caller's authority
// profile. Why: is a purpose declared. Default deny.
//
// The caller's roles come from their templates and active grants. The limit
// in the context is the one their authority profile gives for this kind of
// action, capped by the template's ceiling before it reaches here.

// Structure and licence records: the distribution administrator. A licence
// status may also be recorded by compliance, who receive the regulator's notice.
permit (principal, action in [Action::"structure.manage", Action::"payee.manage"], resource)
when { principal.roles.contains("distribution_admin") };

permit (principal, action == Action::"licence.record", resource)
when { principal.roles.contains("distribution_admin") || principal.roles.contains("compliance") };

// A payee's payments are held, and released, by the distribution administrator
// and by compliance, who hold them while an adviser is investigated without
// touching the licence.
permit (principal, action == Action::"payee.hold", resource)
when { principal.roles.contains("distribution_admin") || principal.roles.contains("compliance") };

// Rules, schedules, books and book-level items are proposed by commission
// operations and approved by a commission approver and by compliance.
permit (principal, action in [Action::"config.propose", Action::"statement.cut", Action::"policy.assign"], resource)
when { principal.roles.contains("commission_ops") };

permit (principal, action == Action::"config.approve", resource)
when { principal.roles.contains("commission_approver") && context.amount <= context.limit };

permit (principal, action == Action::"config.approve_compliance", resource)
when { principal.roles.contains("compliance") };

// The workbench: counterparty finance prepares, an approver signs, and legal
// and the conduct officer sign where the controls call for them.
permit (principal, action == Action::"workbench.prepare", resource)
when { principal.roles.contains("counterparty_finance") && context.amount <= context.limit };

permit (principal, action == Action::"workbench.approve", resource)
when { principal.roles.contains("counterparty_finance_approver") && context.amount <= context.limit };

permit (principal, action == Action::"workbench.approve_legal", resource)
when { principal.roles.contains("legal") };

permit (principal, action == Action::"workbench.approve_conduct", resource)
when { principal.roles.contains("conduct_officer") };

permit (principal, action in [Action::"workbench.view", Action::"workbench.document"], resource)
when
{
  principal.roles.contains("counterparty_finance") ||
  principal.roles.contains("counterparty_finance_approver") ||
  principal.roles.contains("legal") ||
  principal.roles.contains("conduct_officer")
};

// Instructions with legal priority (court orders, statutory deductions) are
// entered only by legal.
permit (principal, action == Action::"payee.restricted_instruction", resource)
when { principal.roles.contains("legal") };

// Conduct cases belong to the conduct officer; compliance works them too.
permit (principal, action == Action::"case.manage", resource)
when { principal.roles.contains("conduct_officer") || principal.roles.contains("compliance") };

// Why: nothing is decided or read without a declared purpose.
forbid (principal, action, resource)
when { context.purpose == "" };

// Whose: no path to the record, no access, whatever the role.
forbid (principal, action, resource)
when { !context.has_path };

// Maker and checker: whoever prepared an action cannot approve it.
forbid (
  principal,
  action in
    [Action::"config.approve",
     Action::"config.approve_compliance",
     Action::"workbench.approve",
     Action::"workbench.approve_legal",
     Action::"workbench.approve_conduct"],
  resource
)
when { context.is_maker };

// Separation of duties: commission approval and adviser management are a
// declared conflict. Someone who manages the structure does not approve what
// it is paid.
forbid (principal, action == Action::"config.approve", resource)
when { principal.roles.contains("distribution_admin") };

// Separation of duties: payment entry and payment approval. Someone who
// prepares workbench actions does not approve them, even another person's.
forbid (principal, action == Action::"workbench.approve", resource)
when { principal.roles.contains("counterparty_finance") };
