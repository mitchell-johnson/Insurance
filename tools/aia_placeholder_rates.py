#!/usr/bin/env python3
"""Write the placeholder rate tables for the AIA products.

AIA New Zealand does not publish its premium rates, so these tables are
best estimates built from a smooth curve: a rate at age 30 for a female
non-smoker in the lightest occupation class, growing by a fixed factor every
five years, scaled for sex, smoking and occupation class. They pass the
finance schema checks in config/finance/tables.star (rising with age, at
most 2.5 times the band before, smokers never below non-smokers) and are
listed in docs/aia-products.md for the appointed actuary to replace.

Run from the repository root:  python3 tools/aia_placeholder_rates.py
It rewrites products/rates/aia_*.star; the output is committed, so a review
sees every rate.
"""

from decimal import ROUND_HALF_EVEN, Decimal
from pathlib import Path

OUT = Path("products/rates")
CLASSES = ["1", "2", "3", "4", "5"]
CLASS_FACTOR = {"1": Decimal("1.00"), "2": Decimal("1.15"), "3": Decimal("1.35"), "4": Decimal("1.70"), "5": Decimal("2.20")}

# id: (file, unit, first age, last age, rate at 30, five-year growth, male factor, smoker factor, by class, cap)
TABLES = {
    "aia-life": ("aia_life", "sum insured", 10, 105, "0.55", "1.42", "1.25", "2.05", False, "999"),
    "aia-family-protection": ("aia_family_protection", "monthly benefit", 10, 85, "48.00", "1.38", "1.25", "2.05", False, "9999"),
    "aia-accidental-death": ("aia_accidental_death", "sum insured", 16, 66, "0.30", "1.06", "1.60", "1.10", False, "99"),
    "aia-trauma": ("aia_trauma", "sum insured", 0, 105, "1.10", "1.40", "1.05", "1.95", False, "999"),
    "aia-progressive-care": ("aia_progressive_care", "sum insured", 0, 105, "0.95", "1.40", "1.05", "1.95", False, "999"),
    "aia-severe-trauma": ("aia_severe_trauma", "sum insured", 0, 105, "0.70", "1.40", "1.05", "1.95", False, "999"),
    "aia-tpd": ("aia_tpd", "sum insured", 16, 71, "0.45", "1.45", "1.10", "1.90", True, "999"),
    "aia-income": ("aia_income", "monthly benefit", 16, 71, "260.00", "1.30", "0.85", "1.45", True, "99999"),
    "aia-accidental-injury": ("aia_accidental_injury", "monthly amount", 16, 66, "55.00", "1.04", "1.30", "1.05", True, "9999"),
    "aia-business-continuity": ("aia_business_continuity", "monthly benefit", 16, 66, "210.00", "1.28", "0.90", "1.45", True, "99999"),
}

# Annual premium per life, by age band only (no sex, smoker or class).
PER_LIFE = {
    "aia-specialist-testing": ("aia_specialist_testing", "95.00", "1.30"),
    "aia-private-health": ("aia_private_health", "420.00", "1.32"),
    "aia-private-health-plus": ("aia_private_health_plus", "160.00", "1.30"),
    "aia-cancer-care": ("aia_cancer_care", "110.00", "1.38"),
}


def bands(first: int, last: int):
    """Five-year bands, the first starting at `first` and ending on the next age ending in 4 or 9."""
    start = first
    while start <= last:
        end = min(last, start + (4 - start % 5) if start % 5 != 4 else start)
        if end < start:
            end = start
        yield start, end
        start = end + 1


def q(value: Decimal, places: int) -> str:
    return str(value.quantize(Decimal(1).scaleb(-places), rounding=ROUND_HALF_EVEN))


def curve(at30: Decimal, growth: Decimal, mid: int) -> Decimal:
    # Ages below 30 fall gently, so children's rates stay above zero.
    steps = Decimal(mid - 32) / Decimal(5)
    if steps < 0:
        return at30 / (Decimal("1.08") ** int(-steps))
    return at30 * (growth ** int(steps))


def write_table(table_id: str, spec) -> None:
    file, unit, first, last, at30, growth, male, smoker, by_class, cap = spec
    at30, growth, male, smoker, cap = (Decimal(x) for x in (at30, growth, male, smoker, cap))
    rows = []
    for lo, hi in bands(first, last):
        mid = (lo + hi) // 2
        base = min(curve(at30, growth, mid), cap / (male * smoker * CLASS_FACTOR["5"]))
        for sex, sf in (("female", Decimal(1)), ("male", male)):
            for smokes, smf in ((False, Decimal(1)), (True, smoker)):
                for cls in CLASSES if by_class else [None]:
                    cf = CLASS_FACTOR[cls] if cls else Decimal(1)
                    rate = max(base * sf * smf * cf, Decimal("0.0001"))
                    extra = f', occupation_class = "{cls}"' if cls else ""
                    rows.append(
                        f'        rate("{q(rate, 4)}", age_from = {lo}, age_to = {hi}, sex = "{sex}", smoker = {smokes}{extra}),'
                    )
    body = "\n".join(rows)
    (OUT / f"{file}.star").write_text(
        f"""# Placeholder rates for the AIA products (tools/aia_placeholder_rates.py).
# Not AIA's rates and not an actuarial basis: best estimates until the
# appointed actuary supplies the real table (docs/aia-products.md).
# Annual rate per thousand of {unit}.

RATE_TABLE = rate_table(
    id = "{table_id}",
    version = 1,
    effective_from = "2026-11-14",
    rows = [
{body}
    ],
)
"""
    )


def write_per_life(table_id: str, spec) -> None:
    file, at30, growth = spec
    at30, growth = Decimal(at30), Decimal(growth)
    rows = []
    for lo, hi in bands(0, 150):
        mid = min((lo + hi) // 2, 100)
        rows.append(f'        rate("{q(curve(at30, growth, mid), 2)}", age_from = {lo}, age_to = {hi}),')
    body = "\n".join(rows)
    (OUT / f"{file}.star").write_text(
        f"""# Placeholder rates for the AIA products (tools/aia_placeholder_rates.py).
# Not AIA's rates and not an actuarial basis: best estimates until the
# appointed actuary supplies the real table (docs/aia-products.md).
# Annual premium per life, before GST.

RATE_TABLE = rate_table(
    id = "{table_id}",
    version = 1,
    effective_from = "2026-11-14",
    rows = [
{body}
    ],
)
"""
    )


if __name__ == "__main__":
    for table_id, spec in TABLES.items():
        write_table(table_id, spec)
    for table_id, spec in PER_LIFE.items():
        write_per_life(table_id, spec)
