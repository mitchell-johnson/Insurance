# The reason code list. Every decision, routing and closure carries codes
# from here; free text is allowed only in an underwriter's notes.
#
# `disclosable` says whether the customer may be told the reason, and `text`
# is then the plain words they are told. A reason that may not be disclosed
# has no customer text and is held under the stricter access class. Which
# reasons may be withheld needs legal review; the split below is a default.

REASONS = reason_library(
    id = "reasons",
    version = 1,
    effective_from = "2026-01-01",
    entries = [
        reason("RC-0001", "assessment", True, "Your answers met our standard terms."),
        # Assessment
        reason("RC-0101", "assessment", True, "Your medical history."),
        reason("RC-0102", "assessment", True, "Your height and weight."),
        reason("RC-0103", "assessment", True, "Your smoking or vaping."),
        reason("RC-0104", "assessment", True, "Your alcohol use."),
        reason("RC-0105", "assessment", True, "Your family medical history."),
        reason("RC-0106", "assessment", True, "Your mental health history."),
        reason("RC-0107", "assessment", True, "A medical test result."),
        reason("RC-0108", "assessment", True, "Treatment or investigation that is not yet complete."),
        reason("RC-0120", "assessment", True, "Your occupation."),
        reason("RC-0121", "assessment", True, "A sport or pastime you take part in."),
        reason("RC-0122", "assessment", True, "Your travel or residence plans."),
        reason("RC-0130", "assessment", True, "The amount of cover compared with your financial position."),
        reason("RC-0131", "assessment", True, "Cover you already hold."),
        reason("RC-0140", "assessment", True, "Your age and the amount of cover."),
        reason("RC-0190", "assessment", False, ""),
        reason("RC-0191", "assessment", False, ""),
        # Routing
        reason("RC-0201", "routing", True, "An underwriter needs to review your application."),
        reason("RC-0202", "routing", True, "We need to talk through some of your answers with you."),
        reason("RC-0203", "routing", True, "The amount of cover needs a senior underwriter."),
        # Evidence
        reason("RC-0301", "evidence", True, "We need a report from your doctor."),
        reason("RC-0302", "evidence", True, "We need a blood test or medical examination."),
        reason("RC-0303", "evidence", True, "We need evidence of your income or finances."),
        reason("RC-0304", "evidence", True, "We need your history from a previous insurer."),
        reason("RC-0305", "evidence", True, "We do not have medical records for part of your history."),
        reason("RC-0901", "evidence", True, "Evidence we asked for could not be obtained."),
        reason("RC-0902", "evidence", True, "All the evidence we asked for has arrived."),
        # Closure
        reason("RC-0910", "closure", True, "The application ran out of time before it could be completed."),
        reason("RC-0911", "closure", True, "Our offer was not accepted before it expired."),
        reason("RC-0912", "closure", True, "You declined our offer."),
        reason("RC-0913", "closure", True, "The application was withdrawn."),
        reason("RC-0920", "routing", True, "You asked us to look at our offer again."),
        reason("RC-0930", "routing", True, "The amount of cover needs our reinsurer's agreement."),
        reason("RC-0931", "assessment", True, "Our reinsurer could not offer terms for this amount of cover."),
        reason("RC-0932", "assessment", True, "Terms agreed with our reinsurer."),
        reason("RC-0933", "assessment", True, "This risk is outside what we can insure."),
        reason("RC-0940", "routing", True, "A second underwriter asked for the decision to be looked at again."),
        reason("RC-0950", "routing", True, "You confirmed the answers from your interview."),
        reason("RC-0990", "closure", True, "Our prices or product changed before your policy could start."),
        # Decisions that arrive as data
        reason("RC-0960", "source", True, "Terms carried over from your earlier policy."),
        reason("RC-0961", "source", False, ""),
        reason("RC-0970", "source", True, "Decision made under your scheme's underwriting arrangement."),
        reason("RC-0971", "source", False, ""),
        reason("RC-0976", "source", True, "Cover you held under your employer's scheme, continued on the terms it was accepted on."),
        reason("RC-0977", "source", True, "More cover under your policy's option to increase it after a life event, with no health questions."),
        reason("RC-0978", "closure", True, "We could not accept the proof of the life event for your option to increase your cover."),
        reason("RC-0979", "source", True, "Cover under a conversion facility of an existing policy, with no health questions."),
        reason("RC-0982", "source", True, "A shorter waiting period on your income cover, now your business cover has been cancelled, with no health questions."),
        reason("RC-0981", "closure", True, "We could not accept the request to use the conversion facility of your existing policy."),
        reason("RC-0975", "source", True, "Cover your previous insurer accepted, carried over under your scheme's takeover terms."),
        reason("RC-0980", "routing", False, ""),
    ],
)
