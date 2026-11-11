//! Commission on real premiums: the core in a box issues and collects, the
//! module allocates, and the core's own ledgers are what the tests read.

mod common;

use common::*;

/// Purpose: the first premium on an advised policy pays initial commission on the annual
/// premium and renewal commission on the premium received, split 75/25 by the book's rule,
/// and the amounts land in each payee's payable account on the core's ledger.
/// Why it matters: this is the whole forward path in one test: event in, allocation, posting
/// to the core, confirmation back. If any link breaks, nobody is paid or the ledger and the
/// module disagree, and every reconciliation in the design depends on them agreeing.
#[test]
fn a_first_premium_pays_initial_and_renewal_commission_by_the_rule() {
    let mut w = worked_example();
    let (policy, _) = w.issue(1, 500_000_00, None);
    w.write(&policy, "life", 1, "eng-a-one", "a-at-fap-one", 1_200_00).unwrap();
    let paid = w.pay(&policy, "r1");
    assert!(paid > 0);
    let net = w.dist.movements.iter().map(|m| m.amount_minor).sum::<i64>();
    // Initial: 100% of 1,200.00. Renewal: 10% of the cover premium received.
    let renewal = net - 1_200_00;
    assert!(renewal > 0 && renewal < paid / 5, "renewal {renewal} on {paid}");
    assert_eq!(w.payable("a-co") + w.payable("fap-one"), net);
    assert_eq!(w.payable("a-co"), net - w.payable("fap-one"));
    // 25% of each component, rounded half-even; A's company absorbs the odd cent.
    assert_eq!(w.payable("fap-one"), 300_00 + ic_distribution::rules::portion(renewal, pct(25.0)));
    w.assert_reconciled();
}
