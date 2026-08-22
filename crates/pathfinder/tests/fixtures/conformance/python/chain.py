def chain_leaf() -> int:
    """Leaf function."""
    return 42

def chain_middle() -> int:
    """Middle function calling chain_leaf."""
    return chain_leaf() + 1

def chain_root() -> int:
    """Root function calling chain_middle."""
    return chain_middle() * 2
