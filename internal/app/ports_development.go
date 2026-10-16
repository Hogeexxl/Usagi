//go:build !production

package app

func RuntimePortRange() PortRange {
	return PortRange{First: developmentPortFirst, Count: runtimePortCount}
}

func runtimeMode() string {
	return "development"
}
