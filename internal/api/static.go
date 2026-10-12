package api

import (
	"bytes"
	"io/fs"
	"mime"
	"net/http"
	"path"
	"strings"
	"time"
)

func newStaticHandler(assets fs.FS) http.Handler {
	return staticHandler{assets: assets}
}

type staticHandler struct {
	assets fs.FS
}

func (h staticHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet && r.Method != http.MethodHead {
		http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
		return
	}
	name := strings.TrimPrefix(path.Clean("/"+r.URL.Path), "/")
	if name == "." || name == "" {
		name = "index.html"
	}
	if !fs.ValidPath(name) {
		http.NotFound(w, r)
		return
	}
	if data, err := fs.ReadFile(h.assets, name); err == nil {
		serveAsset(w, r, name, data)
		return
	}
	index, err := fs.ReadFile(h.assets, "index.html")
	if err != nil {
		http.NotFound(w, r)
		return
	}
	serveAsset(w, r, "index.html", index)
}

func serveAsset(w http.ResponseWriter, r *http.Request, name string, data []byte) {
	contentType := mime.TypeByExtension(path.Ext(name))
	if contentType == "" {
		contentType = "application/octet-stream"
	}
	w.Header().Set("Content-Type", contentType)
	http.ServeContent(w, r, name, time.Time{}, bytes.NewReader(data))
}
