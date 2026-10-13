package magpie

type ProviderCatalog struct {
	ID      string `json:"id"`
	Catalog string `json:"catalog"`
}

type ProviderCatalogsResponse struct {
	Providers []ProviderCatalog `json:"providers"`
	FileError string            `json:"fileError"`
}
