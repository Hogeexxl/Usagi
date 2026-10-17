package app

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"time"
)

const healthProbeTimeout = 300 * time.Millisecond

var ErrNoAvailablePort = errors.New("no available loopback port")

type BindOutcome struct {
	Listener net.Listener
	Existing *url.URL
}

func BindLoopback(ports PortRange) (BindOutcome, error) {
	return bindLoopback(ports, net.Listen, probeUsagiHealth)
}

func bindLoopback(
	ports PortRange,
	listen func(string, string) (net.Listener, error),
	probe func(uint16) bool,
) (BindOutcome, error) {
	var firstAvailable net.Listener

	for offset := uint16(0); offset < ports.Count; offset++ {
		port := ports.First + offset
		address := net.JoinHostPort("127.0.0.1", strconv.Itoa(int(port)))
		listener, err := listen("tcp", address)
		if err == nil {
			if firstAvailable == nil {
				firstAvailable = listener
				continue
			}
			if err := listener.Close(); err != nil {
				_ = firstAvailable.Close()
				return BindOutcome{}, fmt.Errorf("close extra listener on port %d: %w", port, err)
			}
			continue
		}

		if errors.Is(err, addressInUseError) {
			if probe(port) {
				if firstAvailable != nil {
					if closeErr := firstAvailable.Close(); closeErr != nil {
						return BindOutcome{}, fmt.Errorf("close reserved listener after finding existing Usagi on port %d: %w", port, closeErr)
					}
				}
				existing, _ := url.Parse("http://" + address)
				return BindOutcome{Existing: existing}, nil
			}
			continue
		}

		if firstAvailable != nil {
			_ = firstAvailable.Close()
		}
		return BindOutcome{}, fmt.Errorf("bind loopback port %d: %w", port, err)
	}

	if firstAvailable != nil {
		return BindOutcome{Listener: firstAvailable}, nil
	}
	return BindOutcome{}, ErrNoAvailablePort
}

func probeUsagiHealth(port uint16) bool {
	transport := &http.Transport{Proxy: nil}
	client := &http.Client{
		Transport: transport,
		Timeout:   healthProbeTimeout,
		CheckRedirect: func(*http.Request, []*http.Request) error {
			return http.ErrUseLastResponse
		},
	}
	defer transport.CloseIdleConnections()

	address := net.JoinHostPort("127.0.0.1", strconv.Itoa(int(port)))
	response, err := client.Get("http://" + address + "/api/health")
	if err != nil {
		return false
	}
	values := response.Header.Values("X-Usagi-App")
	healthOK := response.StatusCode >= http.StatusOK && response.StatusCode < http.StatusMultipleChoices && len(values) == 1 && values[0] == "Usagi"
	_ = response.Body.Close()
	if !healthOK {
		return false
	}

	response, err = client.Get("http://" + address + "/api/v1/info")
	if err != nil {
		return false
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return false
	}
	var info struct {
		RuntimeMode string `json:"runtime_mode"`
	}
	if err := json.NewDecoder(io.LimitReader(response.Body, 4096)).Decode(&info); err != nil {
		return false
	}
	return info.RuntimeMode == runtimeMode()
}
