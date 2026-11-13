# An illustrative quarterly insurer return: financial position, premium and
# claims conduct. The lines and their account mappings are a proposal for
# finance and compliance to replace with the regulator's own template.
#
# A balance or movement line sums the named finance accounts. A conduct
# line counts facts the claims and complaints modules report. Declines and
# overturned declines are separate lines and are never netted.

def balance(code, label, accounts, credit_positive = False, fund = None):
    return {"code": code, "label": label, "source": "balance", "accounts": accounts, "credit_positive": credit_positive, "fund": fund}

def movement(code, label, accounts, credit_positive = False, fund = None):
    return {"code": code, "label": label, "source": "movement", "accounts": accounts, "credit_positive": credit_positive, "fund": fund}

def conduct(code, label, measure):
    return {"code": code, "label": label, "source": "conduct", "measure": measure}

RETURN = {
    "id": "insurer-quarterly",
    "version": 1,
    "title": "Quarterly insurer return",
    "currency": "NZD",
    "lines": [
        balance("A1", "Cash and settlement balances", ["1000"]),
        balance("A2", "Premiums receivable", ["1100"]),
        balance("L1", "Unearned premium", ["2000"], credit_positive = True),
        balance("L2", "Policy liabilities, risk adjustment and margin", ["2400", "2410", "2420", "2430"], credit_positive = True),
        balance("L3", "Policyholder money held in suspense", ["2050"], credit_positive = True),
        movement("P1", "Premium earned in the period", ["4000"], credit_positive = True),
        movement("P1L", "Premium earned in the period, life fund", ["4000"], credit_positive = True, fund = "SF1-life"),
        movement("P2", "Change in policy liabilities", ["6000", "6020", "6030"]),
        conduct("C1", "Claims accepted", "claim_accepted"),
        conduct("C2", "Claims declined", "claim_declined"),
        conduct("C3", "Declines overturned on review or by a dispute scheme", "decline_overturned"),
        conduct("C4", "Admissions reversed", "admission_reversed"),
        conduct("C5", "Complaints received", "complaint_received"),
        conduct("C6", "Complaints upheld", "complaint_upheld"),
        conduct("C7", "Policies lapsed", "policy_lapsed"),
        conduct("C8", "Lapsed policies reinstated", "policy_reinstated"),
        conduct("C9", "Replacement cover written", "replacement_written"),
    ],
}
