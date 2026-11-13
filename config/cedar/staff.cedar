// Staff authorisation: the four questions on every action.
//
// The platform works out the facts (which templates and grants the person
// holds right now, whether they have a path to the record, what their
// authority profile allows) and puts them in the context. This policy
// decides. Default deny: nothing is allowed unless the permit below applies
// and no forbid does.
//
// A change that widens access here needs a security approver.

// What, whose, how much, why, and the data class the action needs.
permit (principal, action, resource)
when
{
  context.what &&
  context.whose &&
  context.how_much &&
  context.why &&
  context.class_held
};

// Clinical information is reached only through a held or review task on a
// specific case. Never by an ad hoc lookup, never through a grant.
forbid (principal, action, resource)
when { context.class == "clinical" && context.purpose_kind != "task" };

// Clinical information is never reached under break-glass, nor through
// any other grant (a one-off grant, covering a colleague, an uplift): the
// task is the person's own.
forbid (principal, action, resource)
when { context.class == "clinical" && (context.break_glass || context.granted) };

// Clinical information is seen one record at a time: never by a search
// across records or in a bulk export.
forbid (principal, action, resource)
when { context.class == "clinical" && !context.one_record };

// Separation of duties: declared conflicts are refused whatever the role.
forbid (principal, action, resource)
when { context.conflict };

// Maker and checker: the person who prepared an action cannot approve it.
forbid (principal, action, resource)
when { context.is_maker };

// Decisions and health-implying data need current entitlements. Stale
// relationship data is good enough only for ordinary reads.
forbid (principal, action, resource)
when
{
  !context.fresh &&
  (context.decides ||
   context.class == "clinical" ||
   context.class == "insurance_sensitive")
};
