package api

import (
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
)

func allowRequest(w http.ResponseWriter, r *http.Request) bool {
	if !allowedHost(r.Host) {
		writeAPIError(w, http.StatusForbidden, ErrorForbiddenHost)
		return false
	}
	if origin := r.Header.Get("Origin"); origin != "" && !allowedOrigin(origin) {
		writeAPIError(w, http.StatusForbidden, ErrorForbiddenOrigin)
		return false
	}
	return true
}

func allowedHost(authority string) bool {
	host := authority
	if strings.HasPrefix(authority, "[") {
		parsedHost, port, err := net.SplitHostPort(authority)
		if err == nil {
			if !validPort(port) {
				return false
			}
			host = parsedHost
		} else if strings.HasSuffix(authority, "]") {
			host = strings.TrimSuffix(strings.TrimPrefix(authority, "["), "]")
		} else {
			return false
		}
	} else if strings.Contains(authority, ":") {
		parsedHost, port, err := net.SplitHostPort(authority)
		if err != nil || !validPort(port) {
			return false
		}
		host = parsedHost
	}
	return loopbackHostname(host)
}

func allowedOrigin(origin string) bool {
	parsed, err := url.Parse(origin)
	if err != nil || parsed.User != nil || parsed.Host == "" || parsed.Path != "" || parsed.RawQuery != "" || parsed.ForceQuery || parsed.Fragment != "" || parsed.RawFragment != "" {
		return false
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return false
	}
	if parsed.Port() != "" && !validPort(parsed.Port()) {
		return false
	}
	return allowedHost(parsed.Host)
}

func loopbackHostname(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func validPort(port string) bool {
	value, err := strconv.Atoi(port)
	return err == nil && value >= 0 && value <= 65535
}
