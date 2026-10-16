//go:build production

package app

func RuntimePortRange() PortRange {
	return PortRange{First: productionPortFirst, Count: runtimePortCount}
}

func runtimeMode() string {
	return "production"
}
