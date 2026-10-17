package codexplugin

import (
	"context"

	"github.com/modelcontextprotocol/go-sdk/mcp"
)

const dashboardResourceURI = "ui://usagi/dashboard-v1"
const dashboardResourceMIMEType = "text/html;profile=mcp-app"

func addDashboardResource(server *mcp.Server, appHTML string) {
	server.AddResource(&mcp.Resource{
		URI:      dashboardResourceURI,
		Name:     "Usagi Dashboard",
		MIMEType: dashboardResourceMIMEType,
	}, func(_ context.Context, request *mcp.ReadResourceRequest) (*mcp.ReadResourceResult, error) {
		if request.Params.URI != dashboardResourceURI {
			return nil, mcp.ResourceNotFoundError(request.Params.URI)
		}
		return &mcp.ReadResourceResult{Contents: []*mcp.ResourceContents{{
			URI:      dashboardResourceURI,
			MIMEType: dashboardResourceMIMEType,
			Text:     appHTML,
			Meta: mcp.Meta{"ui": map[string]any{
				"prefersBorder": false,
			}},
		}}}, nil
	})
}
