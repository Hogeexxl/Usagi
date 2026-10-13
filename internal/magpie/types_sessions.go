package magpie

import "time"

type SessionTokens struct {
	Input        int `json:"input"`
	Output       int `json:"output"`
	CacheRead    int `json:"cache_read"`
	CacheWrite   int `json:"cache_write"`
	CacheWrite1h int `json:"cache_write_1h,omitempty"`
}

type SessionModel struct {
	Model  string  `json:"model"`
	Cost   float64 `json:"cost"`
	Priced bool    `json:"priced"`
	SessionTokens
}

type SessionVia struct {
	Provider string    `json:"provider"`
	Model    string    `json:"model"`
	Effort   string    `json:"effort,omitempty"`
	Calls    int       `json:"calls"`
	Tokens   int       `json:"tokens"`
	Last     time.Time `json:"last"`
}

type Session struct {
	ReadOnly bool           `json:"read_only,omitempty"`
	Gateway  bool           `json:"gateway,omitempty"`
	Agent    string         `json:"agent"`
	ID       string         `json:"id"`
	Cwd      string         `json:"cwd"`
	Title    string         `json:"title"`
	Start    time.Time      `json:"start"`
	Last     time.Time      `json:"last"`
	Models   []SessionModel `json:"models"`
	Cost     float64        `json:"cost"`
	Unpriced int            `json:"unpriced"`
	Name     string         `json:"name"`
	Icon     string         `json:"icon"`
	Path     string         `json:"path,omitempty"`
	Via      []SessionVia   `json:"via,omitempty"`
	SessionTokens
}

type SessionsResponse struct {
	Sessions []Session `json:"sessions"`
	Terminal bool      `json:"terminal"`
	Dirs     []string  `json:"dirs"`
}

type SessionDayUsage struct {
	Agent  string  `json:"agent"`
	Cwd    string  `json:"cwd"`
	Model  string  `json:"model"`
	Cost   float64 `json:"cost"`
	Priced bool    `json:"priced"`
	SessionTokens
}

type SessionActive struct {
	Agent   string  `json:"agent"`
	Cwd     string  `json:"cwd"`
	Seconds int64   `json:"seconds"`
	Hours   []int64 `json:"hours,omitempty"`
}

type SessionDay struct {
	Date   string            `json:"date"`
	Usage  []SessionDayUsage `json:"usage"`
	Active []SessionActive   `json:"active"`
}

type SessionStats struct {
	From   string            `json:"from"`
	To     string            `json:"to"`
	Days   []SessionDay      `json:"days"`
	Agents map[string]string `json:"agents"`
}

type SessionSummary struct {
	Key       string    `json:"key"`
	Agent     string    `json:"agent"`
	ID        string    `json:"id"`
	Cwd       string    `json:"cwd"`
	Title     string    `json:"title"`
	Last      time.Time `json:"last"`
	Cost      float64   `json:"cost"`
	Priced    bool      `json:"priced"`
	Active    int64     `json:"active"`
	Models    []string  `json:"models"`
	Days      []int     `json:"days"`
	Prompts   int       `json:"prompts"`
	Replies   int       `json:"replies"`
	ToolCalls int       `json:"tool_calls"`
	SessionTokens
}

type SessionShape struct {
	Edges  []int `json:"edges"`
	Counts []int `json:"counts"`
	Total  int   `json:"total"`
}

type ToolStat struct {
	Name     string `json:"name"`
	Category string `json:"category,omitempty"`
	Calls    int    `json:"calls"`
	Sessions int    `json:"sessions"`
}

type ToolWeek struct {
	Start string         `json:"start"`
	Calls map[string]int `json:"calls"`
}

type ToolUse struct {
	Calls      int        `json:"calls"`
	Sessions   int        `json:"sessions"`
	Top        []ToolStat `json:"top"`
	Categories []ToolStat `json:"categories"`
	Weeks      []ToolWeek `json:"weeks"`
}

type Skill struct {
	Name     string         `json:"name"`
	Calls    int            `json:"calls"`
	Sessions int            `json:"sessions"`
	Last     string         `json:"last"`
	Agents   map[string]int `json:"agents"`
	Projects []ToolStat     `json:"projects"`
}

type SkillUse struct {
	Calls int     `json:"calls"`
	Top   []Skill `json:"top"`
	Count int     `json:"count"`
}

type SessionOverview struct {
	Count    int                         `json:"count"`
	Median   int                         `json:"median"`
	P90      int                         `json:"p90"`
	Days     []int                       `json:"days"`
	Top      map[string][]SessionSummary `json:"top"`
	Messages []int                       `json:"messages"`
	Output   []int                       `json:"output"`
	Shape    map[string]SessionShape     `json:"shape"`
	Tools    ToolUse                     `json:"tools"`
	Skills   SkillUse                    `json:"skills"`
}

type SessionProgress struct {
	Indexing bool  `json:"indexing"`
	Files    int64 `json:"files"`
	Done     int64 `json:"done"`
	Bytes    int64 `json:"bytes"`
	Read     int64 `json:"read"`
}

type SessionOverviewQuery struct {
	Days  int
	Agent string
	Model string
	Cwd   string
}
