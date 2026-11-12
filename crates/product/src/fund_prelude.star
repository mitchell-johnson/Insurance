# Authoring kit for fund definitions. Loaded before every fund module, and
# part of what a fund's content hash covers. Kept apart from the product
# prelude so that a change here never changes a product's hash.

def fund(id, version, name, currency, cut_off, price_decimals, unit_rounding,
         buy_spread_bps = 0, sell_spread_bps = 0, non_pricing_days = [],
         open_to_new_money = True, pricing = {}):
    return {
        "id": id,
        "version": version,
        "name": name,
        "currency": currency,
        "cut_off": cut_off,
        "price_decimals": price_decimals,
        "unit_rounding": unit_rounding,
        "buy_spread_bps": buy_spread_bps,
        "sell_spread_bps": sell_spread_bps,
        "non_pricing_days": non_pricing_days,
        "open_to_new_money": open_to_new_money,
        "pricing": pricing,
    }
