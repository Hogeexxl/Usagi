CREATE TABLE app_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
INSERT INTO app_meta VALUES ('schema_version', '1');
INSERT INTO app_meta VALUES ('data_revision', '0');
INSERT INTO app_meta VALUES ('initial_sync_complete', '0');

CREATE TABLE sync_days (
  day TEXT PRIMARY KEY,
  fingerprint BLOB,
  row_count INTEGER NOT NULL DEFAULT 0,
  last_sync_ns INTEGER,
  frozen INTEGER NOT NULL DEFAULT 0,
  difference TEXT NOT NULL DEFAULT ''
);

CREATE TABLE requests (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  t_ns INTEGER NOT NULL,
  local_day TEXT NOT NULL,
  agent TEXT NOT NULL DEFAULT '',
  agent_name TEXT NOT NULL DEFAULT '',
  provider TEXT NOT NULL DEFAULT '',
  provider_name TEXT NOT NULL DEFAULT '',
  model TEXT NOT NULL DEFAULT '',
  requested TEXT NOT NULL DEFAULT '',
  served TEXT NOT NULL DEFAULT '',
  effort TEXT NOT NULL DEFAULT '',
  kind TEXT NOT NULL DEFAULT '',
  session TEXT NOT NULL DEFAULT '',
  native_session TEXT NOT NULL DEFAULT '',
  subagent TEXT NOT NULL DEFAULT '',
  parent_agent TEXT NOT NULL DEFAULT '',
  source TEXT NOT NULL DEFAULT '',
  route_id INTEGER NOT NULL DEFAULT 0,
  rid TEXT NOT NULL DEFAULT '',
  response_id TEXT NOT NULL DEFAULT '',
  status INTEGER NOT NULL DEFAULT 0,
  rejected INTEGER NOT NULL DEFAULT 0,
  error TEXT NOT NULL DEFAULT '',
  tries INTEGER NOT NULL DEFAULT 0,
  input INTEGER NOT NULL DEFAULT 0,
  output INTEGER NOT NULL DEFAULT 0,
  cache_read INTEGER NOT NULL DEFAULT 0,
  cache_write INTEGER NOT NULL DEFAULT 0,
  cache_write_1h INTEGER NOT NULL DEFAULT 0,
  reasoning INTEGER NOT NULL DEFAULT 0,
  ms INTEGER NOT NULL DEFAULT 0,
  ttft_ms INTEGER NOT NULL DEFAULT 0,
  first_text_ms INTEGER NOT NULL DEFAULT 0,
  flow_ms INTEGER NOT NULL DEFAULT 0,
  magpie_cost REAL NOT NULL DEFAULT 0,
  magpie_priced INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX requests_day_time ON requests(local_day, t_ns);
CREATE INDEX requests_session ON requests(
  (CASE WHEN agent IN ('claude', 'claude-desktop') THEN 'claude' ELSE agent END),
  (CASE WHEN native_session <> '' THEN native_session ELSE session END),
  local_day
);

CREATE TABLE sessions (
  agent TEXT NOT NULL,
  id TEXT NOT NULL,
  cwd TEXT NOT NULL DEFAULT '',
  title TEXT NOT NULL DEFAULT '',
  start_ns INTEGER,
  last_ns INTEGER,
  gateway INTEGER NOT NULL DEFAULT 0,
  read_only INTEGER NOT NULL DEFAULT 0,
  has_path INTEGER NOT NULL DEFAULT 0,
  agent_name TEXT NOT NULL DEFAULT '',
  first_seen_ns INTEGER NOT NULL,
  last_seen_ns INTEGER NOT NULL,
  disappeared INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(agent, id)
);

CREATE TABLE session_links (
  child_agent TEXT NOT NULL,
  child_id TEXT NOT NULL,
  parent_agent TEXT NOT NULL,
  parent_id TEXT NOT NULL,
  relation_type TEXT NOT NULL,
  source TEXT NOT NULL,
  PRIMARY KEY(child_agent, child_id, parent_agent, parent_id, relation_type, source)
);

CREATE TABLE price_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source TEXT NOT NULL,
  fetched_at_ns INTEGER NOT NULL,
  effective_at_ns INTEGER NOT NULL,
  provider_catalogs_json TEXT NOT NULL,
  maker_catalogs_json TEXT NOT NULL
);

CREATE TABLE prices (
  version_id INTEGER NOT NULL,
  catalog_id TEXT NOT NULL,
  model_key TEXT NOT NULL,
  canonical_model_id TEXT NOT NULL DEFAULT '',
  input REAL,
  output REAL,
  cache_read REAL,
  cache_write REAL,
  cache_write_1h REAL,
  PRIMARY KEY(version_id, catalog_id, model_key)
);
