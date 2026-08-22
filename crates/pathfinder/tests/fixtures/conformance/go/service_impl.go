package conformance

import "fmt"

// GreeterService implements Greeter in a separate file.
type GreeterService struct {
	GreetingPrefix string
}

func (g *GreeterService) Greet(name string) string {
	return fmt.Sprintf("%s, %s!", g.GreetingPrefix, name)
}
