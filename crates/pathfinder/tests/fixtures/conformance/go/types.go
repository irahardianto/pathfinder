package conformance

// AppConfig declared in types.go.
type AppConfig struct {
	TimeoutSeconds int
	MaxRetries     int
}

// UseConfig contains a non-call reference (type annotation on param).
func UseConfig(cfg *AppConfig) int {
	return cfg.TimeoutSeconds
}

// ZeroCallerSymbol is a symbol with genuinely zero callers.
func ZeroCallerSymbol() bool {
	return true
}

// DecoratedFunction has compiler directives / comments.
//go:noinline
func DecoratedFunction() int {
	return 100
}

// NonAsciiHello has non-ASCII characters before the symbol name, on its own
// declaration line (TS-A2).
func /* 🦀 café 🌍 */ NonAsciiHello() string {
	return "hello"
}
