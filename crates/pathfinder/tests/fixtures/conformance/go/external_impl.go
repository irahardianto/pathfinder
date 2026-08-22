package conformance

import "fmt"

// String method on AppConfig declared in types.go (method on type declared elsewhere).
func (c *AppConfig) String() string {
	return fmt.Sprintf("AppConfig(timeout=%d)", c.TimeoutSeconds)
}
