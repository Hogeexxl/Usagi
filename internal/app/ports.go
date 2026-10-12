package app

const (
	productionPortFirst  uint16 = 37891
	developmentPortFirst uint16 = 37991
	runtimePortCount     uint16 = 32
)

type PortRange struct {
	First uint16
	Count uint16
}
