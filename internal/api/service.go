package api

import "net/http"

type serviceStateDTO struct {
	State string `json:"state"`
}

func (s *Server) handleService(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	writeJSON(w, http.StatusOK, serviceStateDTO{State: "running"})
}

func (s *Server) handleServiceStop(w http.ResponseWriter, r *http.Request) {
	if !requestHasNoQuery(w, r) {
		return
	}
	writeJSON(w, http.StatusOK, serviceStateDTO{State: "stopped"})
	if flusher, ok := w.(http.Flusher); ok {
		flusher.Flush()
	}
	s.shutdownOnce.Do(s.shutdownRequester)
}
