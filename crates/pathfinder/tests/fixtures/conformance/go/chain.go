package conformance

// ChainLeaf is the leaf function in the call chain.
func ChainLeaf() int {
	return 42
}

// ChainMiddle calls ChainLeaf.
func ChainMiddle() int {
	return ChainLeaf() + 1
}

// ChainRoot calls ChainMiddle.
func ChainRoot() int {
	return ChainMiddle() * 2
}
