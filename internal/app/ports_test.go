package app

import "testing"

func TestRuntimePortRangeAndDescriptorBuildMode(t *testing.T) {
	ports := RuntimePortRange()
	if ports.Count != 32 {
		t.Fatalf("RuntimePortRange count = %d, want 32", ports.Count)
	}
	if RuntimeDescriptorEnabled() {
		if ports.First != productionPortFirst {
			t.Fatalf("production RuntimePortRange first = %d, want %d", ports.First, productionPortFirst)
		}
		return
	}
	if ports.First != developmentPortFirst {
		t.Fatalf("development RuntimePortRange first = %d, want %d", ports.First, developmentPortFirst)
	}
}
