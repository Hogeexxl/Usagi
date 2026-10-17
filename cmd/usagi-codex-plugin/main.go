package main

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"os"

	"github.com/Hogeexxl/Usagi/internal/codexplugin"
	"github.com/modelcontextprotocol/go-sdk/mcp"
)

func main() {
	if err := run(); err != nil {
		_, _ = fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run() error {
	appHTML, err := os.ReadFile("./app/usagi-dashboard.html")
	if err != nil {
		return fmt.Errorf("read Dashboard HTML: %w", err)
	}
	if len(appHTML) == 0 {
		return errors.New("Dashboard HTML is empty")
	}
	sidebarSVG, err := os.ReadFile("./assets/sidebar.svg")
	if err != nil {
		return fmt.Errorf("read sidebar icon: %w", err)
	}
	toolIconURI := "data:image/svg+xml;base64," + base64.StdEncoding.EncodeToString(sidebarSVG)
	runtime, err := codexplugin.NewProductionRuntimeResolver()
	if err != nil {
		return fmt.Errorf("create production runtime resolver: %w", err)
	}
	client := codexplugin.NewHTTPClient()
	server := codexplugin.NewServer(codexplugin.Dependencies{
		Runtime:     runtime,
		AppHTML:     string(appHTML),
		ToolIconURI: toolIconURI,
		HTTPClient:  client,
	})
	if err := server.Run(context.Background(), &mcp.StdioTransport{}); err != nil {
		return fmt.Errorf("run MCP stdio server: %w", err)
	}
	return nil
}
