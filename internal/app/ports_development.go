//go:build !production

package app

func RuntimePortRange() PortRange {
	return PortRange{First: developmentPortFirst, Count: runtimePortCount}
}

func RuntimeDescriptorEnabled() bool {
	return false
}
