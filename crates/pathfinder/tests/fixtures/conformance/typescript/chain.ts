/**
 * Leaf function in caller chain >= 2 deep.
 */
export function chainLeaf(): number {
  return 42;
}

/**
 * Middle function calling chainLeaf.
 */
export function chainMiddle(): number {
  return chainLeaf() + 1;
}

/**
 * Root function calling chainMiddle.
 */
export function chainRoot(): number {
  return chainMiddle() * 2;
}
