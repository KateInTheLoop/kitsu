from decimal import Decimal, ROUND_HALF_UP


def compute_total(subtotal: Decimal, tax: Decimal) -> Decimal:
    """Add tax and round the payable total to cents."""
    return (subtotal + tax).quantize(Decimal("0.01"), rounding=ROUND_HALF_UP)
