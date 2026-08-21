from invoice import compute_total


def render_total(subtotal, tax):
    return f"Total: {compute_total(subtotal, tax)}"
