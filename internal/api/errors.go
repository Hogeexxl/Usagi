package api

import (
	"encoding/json"
	"errors"
	"net/http"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
	"github.com/Hogeexxl/Usagi/internal/query"
)

const (
	ErrorInvalidRange      = "INVALID_RANGE"
	ErrorInvalidFilter     = "INVALID_FILTER"
	ErrorThreadNotFound    = "THREAD_NOT_FOUND"
	ErrorMagpieUnavailable = "MAGPIE_UNAVAILABLE"
	ErrorUpstreamFailed    = "UPSTREAM_FAILED"
	ErrorInternal          = "INTERNAL_ERROR"
	ErrorDataLoading       = "DATA_LOADING"
	ErrorNotFound          = "NOT_FOUND"
	ErrorForbiddenHost     = "FORBIDDEN_HOST"
	ErrorForbiddenOrigin   = "FORBIDDEN_ORIGIN"
)

type errorResponseDTO struct {
	Error string `json:"error"`
}

func writeAPIError(w http.ResponseWriter, status int, code string) {
	writeJSON(w, status, errorResponseDTO{Error: code})
}

func writeProjectionError(w http.ResponseWriter, err error) {
	switch {
	case errors.Is(err, query.ErrInvalidRange):
		writeAPIError(w, http.StatusBadRequest, ErrorInvalidRange)
	case errors.Is(err, query.ErrInvalidFilter):
		writeAPIError(w, http.StatusBadRequest, ErrorInvalidFilter)
	case errors.Is(err, projection.ErrThreadNotFound):
		writeAPIError(w, http.StatusNotFound, ErrorThreadNotFound)
	case errors.Is(err, query.ErrUpstreamUnavailable):
		writeAPIError(w, http.StatusServiceUnavailable, ErrorMagpieUnavailable)
	case isUpstreamFailure(err):
		writeAPIError(w, http.StatusBadGateway, ErrorUpstreamFailed)
	default:
		writeAPIError(w, http.StatusInternalServerError, ErrorInternal)
	}
}

func isUpstreamFailure(err error) bool {
	var webErr *magpie.WebError
	var httpErr *magpie.HTTPError
	return errors.As(err, &webErr) || errors.As(err, &httpErr)
}

func writeJSON(w http.ResponseWriter, status int, value any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(value)
}
