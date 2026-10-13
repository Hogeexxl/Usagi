package pricing

import (
	"regexp"
	"slices"
	"strconv"
	"strings"
	"time"
)

type lookupKey struct {
	versionID int64
	provider  string
	model     string
	served    string
}

type lookupResult struct {
	price  Price
	priced bool
}

func (c *Catalog) PriceAt(at time.Time, provider, model, served string) (Price, bool) {
	if len(c.versions) == 0 {
		return Price{}, false
	}
	version := &c.versions[0]
	for i := range c.versions {
		if c.versions[i].effectiveAtNS > at.UnixNano() {
			break
		}
		version = &c.versions[i]
	}
	key := lookupKey{versionID: version.id, provider: provider, model: model, served: served}
	if result, ok := c.cache[key]; ok {
		return result.price, result.priced
	}

	price, priced := priceModel(version, provider, model)
	if !priced && served != "" && served != model {
		price, priced = priceModel(version, provider, served)
	}
	c.cache[key] = lookupResult{price: price, priced: priced}
	return price, priced
}

func priceModel(version *priceVersion, provider, model string) (Price, bool) {
	catalogs, known := version.providerCatalogs[provider]
	member := hasMembershipCatalog(catalogs)
	if known {
		catalogs = withoutMembershipCatalogs(catalogs)
	}
	names := pricedNames(model)
	if member {
		if apiModel, ok := kimiCodeAPI[strings.ToLower(strings.TrimSpace(model))]; ok {
			names = append(names, apiModel)
		}
	}
	for _, name := range names {
		if known {
			if price, ok := pricedBy(version, catalogs, name); ok {
				return price, true
			}
		}
		if price, ok := pricedBy(version, version.makerCatalogs, name); ok {
			return price, true
		}
	}
	return Price{}, false
}

var (
	grokEffort = regexp.MustCompile(`^((?:.*/)?grok-[0-9][^/]*?)-(?:minimal|low|medium|high|xhigh|extra-high)$`)
	grokWindow = regexp.MustCompile(`^((?:.*/)?grok-[0-9][^/@]*)@[0-9]+k$`)
)

func pricedNames(model string) []string {
	out := []string{model}
	if model == "codex-auto-review" {
		out = append(out, "gpt-5.6-luna")
	}
	if match := grokWindow.FindStringSubmatch(strings.ToLower(strings.TrimSpace(model))); match != nil {
		out = append(out, match[1])
	}
	if match := grokEffort.FindStringSubmatch(strings.ToLower(strings.TrimSpace(model))); match != nil {
		out = append(out, match[1])
	}
	return out
}

func pricedBy(version *priceVersion, catalogs []string, id string) (Price, bool) {
	for _, catalogID := range catalogs {
		if model, ok := version.models[catalogID][id]; ok && model.price != nil {
			return *model.price, true
		}
	}
	b := bareID(id)
	if unprofiledID, ok := unprofiled(b); ok {
		b = unprofiledID
	}
	for _, want := range []string{b, cutAt(b, '('), cutAt(b, ':')} {
		for _, catalogID := range catalogs {
			keys := matchingKeys(version.models[catalogID], func(key string) bool { return bareID(key) == want })
			if len(keys) > 0 {
				return *version.models[catalogID][keys[0]].price, true
			}
		}
	}
	want := dashed(b)
	for _, catalogID := range catalogs {
		keys := matchingKeys(version.models[catalogID], func(key string) bool { return dashed(bareID(key)) == want })
		if len(keys) > 0 {
			return *version.models[catalogID][keys[0]].price, true
		}
	}
	if price, ok := canonicallyPriced(version, catalogs, b); ok {
		return price, true
	}
	if renamedID, _, ok := renamed(version, b); ok {
		return pricedBy(version, catalogs, renamedID)
	}
	return Price{}, false
}

func matchingKeys(models map[string]catalogModel, matches func(string) bool) []string {
	keys := make([]string, 0, len(models))
	for key, model := range models {
		if model.price != nil && matches(key) {
			keys = append(keys, key)
		}
	}
	slices.Sort(keys)
	return keys
}

func canonicallyPriced(version *priceVersion, catalogs []string, id string) (Price, bool) {
	for _, same := range []func(string) bool{
		func(canonical string) bool { return bareID(canonical) == id },
		func(canonical string) bool { return dashed(bareID(canonical)) == dashed(id) },
	} {
		for _, catalogID := range catalogs {
			var price *Price
			agree := true
			for _, model := range version.models[catalogID] {
				maker, _, ok := strings.Cut(model.canonicalModelID, "/")
				if !ok || !strings.EqualFold(maker, catalogID) || model.price == nil || model.price.isZero() || !same(model.canonicalModelID) {
					continue
				}
				if price != nil && *price != *model.price {
					agree = false
					break
				}
				copy := *model.price
				price = &copy
			}
			if price != nil && agree {
				return *price, true
			}
		}
	}
	return Price{}, false
}

func (p Price) isZero() bool {
	return p == (Price{})
}

func renamed(version *priceVersion, id string) (string, int, bool) {
	if found(id, version.listed) || version.canonical[id] {
		return "", 0, false
	}
	base, window := unmarked(id)
	if base != id && knownModel(version, base) {
		return base, window, true
	}
	for _, prefix := range relayPrefixes {
		rest, ok := strings.CutPrefix(base, prefix)
		if !ok || !strings.ContainsAny(rest, "0123456789") {
			continue
		}
		if knownModel(version, rest) {
			return rest, window, true
		}
	}
	return "", 0, false
}

func knownModel(version *priceVersion, id string) bool {
	if found(id, version.listed) {
		return true
	}
	return version.canonical[id]
}

func found(id string, models map[string]bool) bool {
	if models[id] {
		return true
	}
	if unprofiledID, ok := unprofiled(id); ok {
		return found(unprofiledID, models)
	}
	if index := strings.IndexByte(id, '('); index > 0 && strings.HasSuffix(id, ")") {
		if models[id[:index]] {
			return true
		}
		id = id[:index]
	}
	if index := strings.IndexByte(id, ':'); index > 0 {
		return models[id[:index]]
	}
	return false
}

var relayPrefixes = []string{"claude-"}

func unmarked(id string) (string, int) {
	if !strings.HasSuffix(id, "]") {
		return id, 0
	}
	index := strings.LastIndexByte(id, '[')
	if index <= 0 {
		return id, 0
	}
	mark := id[index+1 : len(id)-1]
	unit := 0
	switch {
	case strings.HasSuffix(mark, "k"):
		unit = 1_000
	case strings.HasSuffix(mark, "m"):
		unit = 1_000_000
	default:
		return id, 0
	}
	number := mark[:len(mark)-1]
	if strings.Trim(number, "0123456789.") != "" {
		return id, 0
	}
	value, err := strconv.ParseFloat(number, 64)
	if err != nil || value <= 0 {
		return id, 0
	}
	return id[:index], int(value * float64(unit))
}

func cutAt(value string, separator byte) string {
	if index := strings.IndexByte(value, separator); index > 0 {
		return value[:index]
	}
	return value
}

func bareID(id string) string {
	id = strings.ToLower(id)
	if index := strings.LastIndexByte(id, '/'); index >= 0 {
		id = id[index+1:]
	}
	return id
}

var bedrockGeos = []string{"global.", "us.", "us-gov.", "eu.", "apac.", "jp.", "au.", "ca.", "in."}

func unprofiled(id string) (string, bool) {
	for _, geography := range bedrockGeos {
		if rest, ok := strings.CutPrefix(id, geography); ok && strings.Contains(rest, ".") {
			return rest, true
		}
	}
	return "", false
}

func dashed(id string) string {
	return strings.ReplaceAll(id, ".", "-")
}

func hasMembershipCatalog(catalogs []string) bool {
	for _, catalog := range catalogs {
		if catalog == "kimi-code-plan-global" || catalog == "kimi-code-plan-cn" {
			return true
		}
	}
	return false
}

func withoutMembershipCatalogs(catalogs []string) []string {
	filtered := make([]string, 0, len(catalogs))
	for _, catalog := range catalogs {
		if catalog != "kimi-code-plan-global" && catalog != "kimi-code-plan-cn" {
			filtered = append(filtered, catalog)
		}
	}
	return filtered
}

var kimiCodeAPI = map[string]string{
	"k3":                        "kimi-k3",
	"k3-256k":                   "kimi-k3",
	"kimi-for-coding-highspeed": "kimi-k2.7-code-highspeed",
}
