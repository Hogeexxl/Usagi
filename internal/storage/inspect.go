package storage

import (
	"context"
	"database/sql"
	"fmt"
	"reflect"
	"sort"
	"strings"
	"unicode"
)

type schemaQueryer interface {
	QueryContext(context.Context, string, ...any) (*sql.Rows, error)
	QueryRowContext(context.Context, string, ...any) *sql.Row
}

type schemaSnapshot struct {
	tables      []tableSnapshot
	indexes     []indexSnapshot
	foreignKeys []foreignKeySnapshot
	triggers    []triggerSnapshot
}

type tableSnapshot struct {
	name         string
	columns      []columnSnapshot
	checks       [][]schemaToken
	withoutRowID bool
	strict       bool
}

type columnSnapshot struct {
	sequence        int
	name            string
	declaredType    string
	notNull         bool
	defaultValue    []schemaToken
	primaryKeyOrder int
	hidden          int
}

type indexSnapshot struct {
	tableName string
	name      string
	origin    string
	unique    bool
	partial   bool
	keys      []indexKeySnapshot
	predicate []schemaToken
}

type indexKeySnapshot struct {
	sequence   int
	column     string
	expression []schemaToken
	descending bool
	collation  string
}

type foreignKeySnapshot struct {
	tableName   string
	parent      string
	columns     []foreignKeyColumnSnapshot
	onUpdate    string
	onDelete    string
	match       string
	deferrable  bool
	initialMode string
}

type foreignKeyColumnSnapshot struct {
	from string
	to   *string
}

type foreignKeyColumnEntry struct {
	sequence int
	column   foreignKeyColumnSnapshot
}

type foreignKeyGroup struct {
	snapshot foreignKeySnapshot
	columns  []foreignKeyColumnEntry
}

type triggerSnapshot struct {
	name      string
	tableName string
	sqlTokens []schemaToken
}

type schemaObject struct {
	kind      string
	name      string
	tableName string
	sql       string
}

type schemaTokenKind uint8

const (
	schemaIdentifier schemaTokenKind = iota
	schemaString
	schemaNumber
	schemaSymbol
)

type schemaToken struct {
	kind   schemaTokenKind
	text   string
	quoted bool
}

func inspectSchema(ctx context.Context, queryer schemaQueryer) (schemaSnapshot, error) {
	objects, err := readSchemaObjects(ctx, queryer)
	if err != nil {
		return schemaSnapshot{}, err
	}
	var snapshot schemaSnapshot
	indexSQL := make(map[string]string)
	for _, object := range objects {
		if object.kind == "index" {
			indexSQL[object.name] = object.sql
		}
	}
	for _, object := range objects {
		switch object.kind {
		case "table":
			tokens, err := tokenizeSQL(object.sql)
			if err != nil {
				return schemaSnapshot{}, schemaMismatch("table", object.name, err)
			}
			withoutRowID, strict := tableOptions(tokens)
			checks, err := tableChecks(tokens)
			if err != nil {
				return schemaSnapshot{}, schemaMismatch("table", object.name, err)
			}
			columns, err := inspectColumns(ctx, queryer, object.name)
			if err != nil {
				return schemaSnapshot{}, err
			}
			snapshot.tables = append(snapshot.tables, tableSnapshot{
				name:         object.name,
				columns:      columns,
				checks:       checks,
				withoutRowID: withoutRowID,
				strict:       strict,
			})
			indexes, err := inspectIndexes(ctx, queryer, object.name, indexSQL)
			if err != nil {
				return schemaSnapshot{}, err
			}
			snapshot.indexes = append(snapshot.indexes, indexes...)
			foreignKeys, err := inspectForeignKeys(ctx, queryer, object.name, tokens)
			if err != nil {
				return schemaSnapshot{}, err
			}
			snapshot.foreignKeys = append(snapshot.foreignKeys, foreignKeys...)
		case "trigger":
			tokens, err := tokenizeSQL(object.sql)
			if err != nil {
				return schemaSnapshot{}, schemaMismatch("trigger", object.name, err)
			}
			snapshot.triggers = append(snapshot.triggers, triggerSnapshot{
				name:      object.name,
				tableName: object.tableName,
				sqlTokens: canonicalSchemaTokens(tokens),
			})
		}
	}
	sortSchemaSnapshot(&snapshot)
	return snapshot, nil
}

func readSchemaObjects(ctx context.Context, queryer schemaQueryer) ([]schemaObject, error) {
	rows, err := queryer.QueryContext(ctx, `
		SELECT type, name, tbl_name, sql
		FROM sqlite_schema
		WHERE type IN ('table', 'index', 'trigger')
		  AND name NOT GLOB 'sqlite_*'
		ORDER BY type, name COLLATE BINARY
	`)
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	defer rows.Close()
	var objects []schemaObject
	for rows.Next() {
		var object schemaObject
		var sqlText sql.NullString
		if err := rows.Scan(&object.kind, &object.name, &object.tableName, &sqlText); err != nil {
			return nil, mapSQLiteError(err)
		}
		if sqlText.Valid {
			object.sql = sqlText.String
		}
		objects = append(objects, object)
	}
	if err := rows.Err(); err != nil {
		return nil, mapSQLiteError(err)
	}
	return objects, nil
}

func inspectColumns(ctx context.Context, queryer schemaQueryer, tableName string) ([]columnSnapshot, error) {
	rows, err := queryer.QueryContext(ctx, "PRAGMA table_xinfo("+quoteIdentifier(tableName)+")")
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	defer rows.Close()
	var columns []columnSnapshot
	for rows.Next() {
		var cid, notNull, primaryKeyOrder, hidden int
		var name, declaredType string
		var defaultValue sql.NullString
		if err := rows.Scan(&cid, &name, &declaredType, &notNull, &defaultValue, &primaryKeyOrder, &hidden); err != nil {
			return nil, mapSQLiteError(err)
		}
		column := columnSnapshot{
			sequence:        cid,
			name:            name,
			declaredType:    strings.ToUpper(strings.TrimSpace(declaredType)),
			notNull:         notNull != 0,
			primaryKeyOrder: primaryKeyOrder,
			hidden:          hidden,
		}
		if defaultValue.Valid {
			column.defaultValue, err = tokenizeSQL(defaultValue.String)
			if err != nil {
				return nil, schemaMismatch("column", tableName+"."+name, err)
			}
			column.defaultValue = canonicalSchemaTokens(column.defaultValue)
		}
		columns = append(columns, column)
	}
	if err := rows.Err(); err != nil {
		return nil, mapSQLiteError(err)
	}
	sort.Slice(columns, func(i, j int) bool { return columns[i].sequence < columns[j].sequence })
	return columns, nil
}

func inspectIndexes(ctx context.Context, queryer schemaQueryer, tableName string, indexSQL map[string]string) ([]indexSnapshot, error) {
	rows, err := queryer.QueryContext(ctx, "PRAGMA index_list("+quoteIdentifier(tableName)+")")
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	defer rows.Close()
	type indexListEntry struct {
		name    string
		origin  string
		unique  bool
		partial bool
	}
	var entries []indexListEntry
	for rows.Next() {
		var sequence, unique, partial int
		var name, origin string
		if err := rows.Scan(&sequence, &name, &unique, &origin, &partial); err != nil {
			return nil, mapSQLiteError(err)
		}
		entries = append(entries, indexListEntry{name: name, origin: origin, unique: unique != 0, partial: partial != 0})
	}
	if err := rows.Err(); err != nil {
		return nil, mapSQLiteError(err)
	}
	if err := rows.Close(); err != nil {
		return nil, mapSQLiteError(err)
	}
	var indexes []indexSnapshot
	for _, entry := range entries {
		index := indexSnapshot{
			tableName: tableName,
			origin:    entry.origin,
			unique:    entry.unique,
			partial:   entry.partial,
		}
		if entry.origin == "c" {
			index.name = entry.name
		}
		var expressions [][]schemaToken
		indexText := indexSQL[entry.name]
		if indexText != "" {
			expressions, err = indexExpressions(indexText)
			if err != nil {
				return nil, schemaMismatch("index", indexNameForError(index), err)
			}
			index.predicate, err = indexPredicate(indexText)
			if err != nil {
				return nil, schemaMismatch("index", indexNameForError(index), err)
			}
		}
		keys, err := inspectIndexKeys(ctx, queryer, entry.name, expressions)
		if err != nil {
			return nil, err
		}
		index.keys = keys
		indexes = append(indexes, index)
	}
	return indexes, nil
}

func inspectIndexKeys(ctx context.Context, queryer schemaQueryer, indexName string, expressions [][]schemaToken) ([]indexKeySnapshot, error) {
	rows, err := queryer.QueryContext(ctx, "PRAGMA index_xinfo("+quoteIdentifier(indexName)+")")
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	defer rows.Close()
	var keys []indexKeySnapshot
	for rows.Next() {
		var sequence, columnID, descending, key int
		var name, collation sql.NullString
		if err := rows.Scan(&sequence, &columnID, &name, &descending, &collation, &key); err != nil {
			return nil, mapSQLiteError(err)
		}
		if key == 0 {
			continue
		}
		item := indexKeySnapshot{sequence: sequence, descending: descending != 0}
		if collation.Valid {
			item.collation = strings.ToLower(collation.String)
		}
		if columnID >= 0 {
			if name.Valid {
				item.column = name.String
			}
		} else if columnID == -2 {
			if sequence < 0 || sequence >= len(expressions) {
				return nil, schemaMismatch("index", indexName, fmt.Errorf("missing expression for key %d", sequence))
			}
			item.expression = expressions[sequence]
		} else {
			return nil, schemaMismatch("index", indexName, fmt.Errorf("unsupported key column id %d", columnID))
		}
		keys = append(keys, item)
	}
	if err := rows.Err(); err != nil {
		return nil, mapSQLiteError(err)
	}
	sort.Slice(keys, func(i, j int) bool { return keys[i].sequence < keys[j].sequence })
	return keys, nil
}

func inspectForeignKeys(ctx context.Context, queryer schemaQueryer, tableName string, tableTokens []schemaToken) ([]foreignKeySnapshot, error) {
	definitions, err := foreignKeyDefinitions(tableTokens)
	if err != nil {
		return nil, schemaMismatch("foreign key", tableName, err)
	}
	rows, err := queryer.QueryContext(ctx, "PRAGMA foreign_key_list("+quoteIdentifier(tableName)+")")
	if err != nil {
		return nil, mapSQLiteError(err)
	}
	defer rows.Close()
	groups := make(map[int]*foreignKeyGroup)
	for rows.Next() {
		var id, sequence int
		var parent, from, onUpdate, onDelete, match string
		var to sql.NullString
		if err := rows.Scan(&id, &sequence, &parent, &from, &to, &onUpdate, &onDelete, &match); err != nil {
			return nil, mapSQLiteError(err)
		}
		group := groups[id]
		if group == nil {
			group = &foreignKeyGroup{snapshot: foreignKeySnapshot{
				tableName:   tableName,
				parent:      parent,
				onUpdate:    strings.ToLower(onUpdate),
				onDelete:    strings.ToLower(onDelete),
				match:       strings.ToLower(match),
				initialMode: "immediate",
			}}
			groups[id] = group
		}
		column := foreignKeyColumnSnapshot{from: from}
		if to.Valid {
			value := to.String
			column.to = &value
		}
		group.columns = append(group.columns, foreignKeyColumnEntry{sequence: sequence, column: column})
	}
	if err := rows.Err(); err != nil {
		return nil, mapSQLiteError(err)
	}
	result := make([]foreignKeySnapshot, 0, len(groups))
	if len(groups) != len(definitions) {
		return nil, schemaMismatch("foreign key", tableName, fmt.Errorf("SQL definition count differs from foreign_key_list"))
	}
	for id := 0; id < len(groups); id++ {
		group := groups[id]
		if group == nil {
			return nil, schemaMismatch("foreign key", tableName, fmt.Errorf("missing FK id %d", id))
		}
		sort.Slice(group.columns, func(i, j int) bool { return group.columns[i].sequence < group.columns[j].sequence })
		foreignKey := group.snapshot
		for _, column := range group.columns {
			foreignKey.columns = append(foreignKey.columns, column.column)
		}
		// SQLite prepends each declared FK to its list; PRAGMA assigns IDs while
		// walking that list. ID therefore identifies the reverse declaration order,
		// including duplicate mappings with different actions or deferrability.
		definition := definitions[len(definitions)-1-id]
		if !matchesForeignKeyDefinition(foreignKey, definition) {
			return nil, schemaMismatch("foreign key", tableName, fmt.Errorf("FK id %d does not match its declaration", id))
		}
		foreignKey.deferrable = definition.deferrable
		foreignKey.initialMode = definition.initialMode
		result = append(result, foreignKey)
	}
	return result, nil
}

type foreignKeyDefinition struct {
	parent      string
	from        []string
	to          []string
	deferrable  bool
	initialMode string
	onUpdate    string
	onDelete    string
}

func foreignKeyDefinitions(tokens []schemaToken) ([]foreignKeyDefinition, error) {
	body, ok := tableDefinition(tokens)
	if !ok {
		return nil, nil
	}
	var definitions []foreignKeyDefinition
	for _, segment := range splitTopLevel(body, ",") {
		references := findTopLevelKeyword(segment, "references", 0)
		if references < 0 {
			continue
		}
		definition := foreignKeyDefinition{initialMode: "immediate", onUpdate: "no action", onDelete: "no action"}
		foreignKey := findTopLevelKeyword(segment, "foreign", 0)
		if foreignKey >= 0 && foreignKey+1 < references && isKeyword(segment[foreignKey+1], "key") {
			open := findSymbol(segment, "(", foreignKey+2)
			close := matchingParen(segment, open)
			if open < 0 || close < 0 {
				return nil, fmt.Errorf("malformed FOREIGN KEY column list")
			}
			definition.from = identifierList(segment[open+1 : close])
		} else if len(segment) > 0 && segment[0].kind == schemaIdentifier {
			definition.from = []string{segment[0].text}
		}
		parent := references + 1
		if parent >= len(segment) || segment[parent].kind != schemaIdentifier {
			return nil, fmt.Errorf("missing REFERENCES table")
		}
		definition.parent = segment[parent].text
		if parent+2 < len(segment) && isSymbol(segment[parent+1], ".") && segment[parent+2].kind == schemaIdentifier {
			definition.parent = segment[parent+2].text
			parent += 2
		}
		end := parent + 1
		if end < len(segment) && isSymbol(segment[end], "(") {
			close := matchingParen(segment, end)
			if close < 0 {
				return nil, fmt.Errorf("malformed REFERENCES column list")
			}
			definition.to = identifierList(segment[end+1 : close])
			end = close + 1
		}
		for i := end; i < len(segment); i++ {
			if isKeyword(segment[i], "on") && i+2 < len(segment) {
				action := segment[i+2].text
				if (action == "no" || action == "set") && i+3 < len(segment) {
					action += " " + segment[i+3].text
				}
				if isKeyword(segment[i+1], "update") {
					definition.onUpdate = action
				}
				if isKeyword(segment[i+1], "delete") {
					definition.onDelete = action
				}
			}
			if isKeyword(segment[i], "deferrable") && (i == 0 || !isKeyword(segment[i-1], "not")) {
				definition.deferrable = true
			}
			if isKeyword(segment[i], "initially") && i+1 < len(segment) {
				if isKeyword(segment[i+1], "deferred") {
					definition.initialMode = "deferred"
				} else if isKeyword(segment[i+1], "immediate") {
					definition.initialMode = "immediate"
				}
			}
		}
		definitions = append(definitions, definition)
	}
	return definitions, nil
}

func matchesForeignKeyDefinition(foreignKey foreignKeySnapshot, definition foreignKeyDefinition) bool {
	if !strings.EqualFold(foreignKey.parent, definition.parent) || len(foreignKey.columns) != len(definition.from) || foreignKey.onUpdate != definition.onUpdate || foreignKey.onDelete != definition.onDelete {
		return false
	}
	for i, column := range foreignKey.columns {
		if !strings.EqualFold(column.from, definition.from[i]) {
			return false
		}
		if len(definition.to) != 0 && (column.to == nil || !strings.EqualFold(*column.to, definition.to[i])) {
			return false
		}
	}
	return true
}

func tableOptions(tokens []schemaToken) (withoutRowID, strict bool) {
	_, close, ok := tableDefinitionBounds(tokens)
	if !ok {
		return false, false
	}
	for i := close + 1; i < len(tokens); i++ {
		if isKeyword(tokens[i], "without") && i+1 < len(tokens) && isKeyword(tokens[i+1], "rowid") {
			withoutRowID = true
		}
		if isKeyword(tokens[i], "strict") {
			strict = true
		}
	}
	return withoutRowID, strict
}

func tableChecks(tokens []schemaToken) ([][]schemaToken, error) {
	body, ok := tableDefinition(tokens)
	if !ok {
		return nil, nil
	}
	var checks [][]schemaToken
	for i := 0; i < len(body); i++ {
		if !isKeyword(body[i], "check") || i+1 >= len(body) || !isSymbol(body[i+1], "(") {
			continue
		}
		close := matchingParen(body, i+1)
		if close < 0 {
			return nil, fmt.Errorf("malformed CHECK expression")
		}
		checks = append(checks, canonicalSchemaTokens(body[i+2:close]))
	}
	sort.Slice(checks, func(i, j int) bool { return tokenSequenceKey(checks[i]) < tokenSequenceKey(checks[j]) })
	return checks, nil
}

func tableDefinition(tokens []schemaToken) ([]schemaToken, bool) {
	body, _, ok := tableDefinitionBounds(tokens)
	return body, ok
}

func tableDefinitionBounds(tokens []schemaToken) ([]schemaToken, int, bool) {
	for i := 0; i+1 < len(tokens); i++ {
		if !isKeyword(tokens[i], "table") {
			continue
		}
		open := findSymbol(tokens, "(", i+1)
		if open < 0 {
			return nil, -1, false
		}
		close := matchingParen(tokens, open)
		if close < 0 {
			return nil, -1, false
		}
		return tokens[open+1 : close], close, true
	}
	return nil, -1, false
}

func indexExpressions(indexSQL string) ([][]schemaToken, error) {
	tokens, err := tokenizeSQL(indexSQL)
	if err != nil {
		return nil, err
	}
	on := findTopLevelKeyword(tokens, "on", 0)
	if on < 0 {
		return nil, fmt.Errorf("missing ON clause")
	}
	open := findSymbol(tokens, "(", on+1)
	if open < 0 {
		return nil, fmt.Errorf("missing index key list")
	}
	close := matchingParen(tokens, open)
	if close < 0 {
		return nil, fmt.Errorf("malformed index key list")
	}
	parts := splitTopLevel(tokens[open+1:close], ",")
	for i := range parts {
		parts[i] = canonicalSchemaTokens(stripIndexModifiers(parts[i]))
	}
	return parts, nil
}

func indexPredicate(indexSQL string) ([]schemaToken, error) {
	tokens, err := tokenizeSQL(indexSQL)
	if err != nil {
		return nil, err
	}
	on := findTopLevelKeyword(tokens, "on", 0)
	if on < 0 {
		return nil, fmt.Errorf("missing ON clause")
	}
	open := findSymbol(tokens, "(", on+1)
	if open < 0 {
		return nil, fmt.Errorf("missing index key list")
	}
	close := matchingParen(tokens, open)
	if close < 0 {
		return nil, fmt.Errorf("malformed index key list")
	}
	where := findTopLevelKeyword(tokens, "where", close+1)
	if where < 0 {
		return nil, nil
	}
	return canonicalSchemaTokens(tokens[where+1:]), nil
}

func stripIndexModifiers(tokens []schemaToken) []schemaToken {
	end := len(tokens)
	if end > 0 && (isKeyword(tokens[end-1], "asc") || isKeyword(tokens[end-1], "desc")) {
		end--
	}
	depth := 0
	collate := -1
	for i := 0; i < end; i++ {
		if isSymbol(tokens[i], "(") {
			depth++
		} else if isSymbol(tokens[i], ")") {
			depth--
		} else if depth == 0 && isKeyword(tokens[i], "collate") {
			collate = i
		}
	}
	if collate >= 0 && collate+2 == end {
		end = collate
	}
	return append([]schemaToken(nil), tokens[:end]...)
}

func splitTopLevel(tokens []schemaToken, separator string) [][]schemaToken {
	var parts [][]schemaToken
	start, depth := 0, 0
	for i, token := range tokens {
		if isSymbol(token, "(") {
			depth++
		} else if isSymbol(token, ")") {
			depth--
		} else if depth == 0 && isSymbol(token, separator) {
			parts = append(parts, tokens[start:i])
			start = i + 1
		}
	}
	return append(parts, tokens[start:])
}

func identifierList(tokens []schemaToken) []string {
	var identifiers []string
	for _, part := range splitTopLevel(tokens, ",") {
		for _, token := range part {
			if token.kind == schemaIdentifier {
				identifiers = append(identifiers, token.text)
				break
			}
		}
	}
	return identifiers
}

func findTopLevelKeyword(tokens []schemaToken, keyword string, start int) int {
	depth := 0
	for i := start; i < len(tokens); i++ {
		if isSymbol(tokens[i], "(") {
			depth++
		} else if isSymbol(tokens[i], ")") {
			depth--
		} else if depth == 0 && isKeyword(tokens[i], keyword) {
			return i
		}
	}
	return -1
}

func findSymbol(tokens []schemaToken, symbol string, start int) int {
	for i := start; i < len(tokens); i++ {
		if isSymbol(tokens[i], symbol) {
			return i
		}
	}
	return -1
}

func matchingParen(tokens []schemaToken, open int) int {
	if open < 0 || open >= len(tokens) || !isSymbol(tokens[open], "(") {
		return -1
	}
	depth := 0
	for i := open; i < len(tokens); i++ {
		if isSymbol(tokens[i], "(") {
			depth++
		} else if isSymbol(tokens[i], ")") {
			depth--
			if depth == 0 {
				return i
			}
		}
	}
	return -1
}

func quoteIdentifier(identifier string) string {
	return `"` + strings.ReplaceAll(identifier, `"`, `""`) + `"`
}

func isKeyword(token schemaToken, keyword string) bool {
	return token.kind == schemaIdentifier && !token.quoted && token.text == keyword
}

func isSymbol(token schemaToken, symbol string) bool {
	return token.kind == schemaSymbol && token.text == symbol
}

func schemaMismatch(category, name string, err error) error {
	return newStorageError(ErrorSchemaMismatch, fmt.Errorf("%s %q: %w", category, name, err))
}

func indexNameForError(index indexSnapshot) string {
	if index.name != "" {
		return index.name
	}
	return index.tableName + " (" + index.origin + " index)"
}

func sortSchemaSnapshot(snapshot *schemaSnapshot) {
	sort.Slice(snapshot.tables, func(i, j int) bool { return snapshot.tables[i].name < snapshot.tables[j].name })
	sort.Slice(snapshot.indexes, func(i, j int) bool {
		return indexSnapshotSortKey(snapshot.indexes[i]) < indexSnapshotSortKey(snapshot.indexes[j])
	})
	sort.Slice(snapshot.foreignKeys, func(i, j int) bool {
		return foreignKeySnapshotSortKey(snapshot.foreignKeys[i]) < foreignKeySnapshotSortKey(snapshot.foreignKeys[j])
	})
	sort.Slice(snapshot.triggers, func(i, j int) bool { return snapshot.triggers[i].name < snapshot.triggers[j].name })
}

func compareSchemaSnapshots(expected, actual schemaSnapshot) error {
	if err := compareSnapshotList("table", expected.tables, actual.tables, func(value tableSnapshot) string { return value.name }); err != nil {
		return err
	}
	if err := compareSnapshotList("index", expected.indexes, actual.indexes, indexNameForError); err != nil {
		return err
	}
	if err := compareSnapshotList("foreign key", expected.foreignKeys, actual.foreignKeys, func(value foreignKeySnapshot) string {
		return value.tableName + " -> " + value.parent
	}); err != nil {
		return err
	}
	return compareSnapshotList("trigger", expected.triggers, actual.triggers, func(value triggerSnapshot) string { return value.name })
}

func compareSnapshotList[T any](category string, expected, actual []T, name func(T) string) error {
	limit := len(expected)
	if len(actual) < limit {
		limit = len(actual)
	}
	for i := 0; i < limit; i++ {
		if !reflect.DeepEqual(expected[i], actual[i]) {
			return newStorageError(ErrorSchemaMismatch, fmt.Errorf("%s %q differs", category, name(expected[i])))
		}
	}
	if len(expected) > limit {
		return newStorageError(ErrorSchemaMismatch, fmt.Errorf("%s %q is missing", category, name(expected[limit])))
	}
	if len(actual) > limit {
		return newStorageError(ErrorSchemaMismatch, fmt.Errorf("%s %q is unexpected", category, name(actual[limit])))
	}
	return nil
}

func indexSnapshotSortKey(index indexSnapshot) string {
	return index.tableName + "\x00" + index.name + "\x00" + fmt.Sprintf("%#v", index)
}

func foreignKeySnapshotSortKey(key foreignKeySnapshot) string {
	var builder strings.Builder
	fmt.Fprintf(&builder, "%q\x00%q\x00%q\x00%q\x00%q\x00%t\x00%q\x00", key.tableName, key.parent, key.onUpdate, key.onDelete, key.match, key.deferrable, key.initialMode)
	for _, column := range key.columns {
		fmt.Fprintf(&builder, "%q=", column.from)
		if column.to == nil {
			builder.WriteByte('N')
		} else {
			fmt.Fprintf(&builder, "V%q", *column.to)
		}
		builder.WriteByte('\x00')
	}
	return builder.String()
}

func tokenSequenceKey(tokens []schemaToken) string {
	var builder strings.Builder
	for _, token := range tokens {
		fmt.Fprintf(&builder, "%d:%d:%s;", token.kind, len(token.text), token.text)
	}
	return builder.String()
}

func canonicalSchemaTokens(tokens []schemaToken) []schemaToken {
	canonical := append([]schemaToken(nil), tokens...)
	for i := range canonical {
		canonical[i].quoted = false
	}
	return canonical
}

func tokenizeSQL(sqlText string) ([]schemaToken, error) {
	var tokens []schemaToken
	for i := 0; i < len(sqlText); {
		if unicode.IsSpace(rune(sqlText[i])) {
			i++
			continue
		}
		if i+1 < len(sqlText) && sqlText[i:i+2] == "--" {
			i += 2
			for i < len(sqlText) && sqlText[i] != '\n' && sqlText[i] != '\r' {
				i++
			}
			continue
		}
		if i+1 < len(sqlText) && sqlText[i:i+2] == "/*" {
			end := strings.Index(sqlText[i+2:], "*/")
			if end < 0 {
				return nil, fmt.Errorf("unterminated block comment")
			}
			i += end + 4
			continue
		}
		start := i
		switch sqlText[i] {
		case '\'':
			end, err := scanQuotedSQL(sqlText, i, '\'', '\'')
			if err != nil {
				return nil, err
			}
			tokens = append(tokens, schemaToken{kind: schemaString, text: sqlText[start:end]})
			i = end
		case '"', '`':
			end, value, err := scanQuotedIdentifier(sqlText, i, sqlText[i])
			if err != nil {
				return nil, err
			}
			tokens = append(tokens, schemaToken{kind: schemaIdentifier, text: strings.ToLower(value), quoted: true})
			i = end
		case '[':
			end := strings.IndexByte(sqlText[i+1:], ']')
			if end < 0 {
				return nil, fmt.Errorf("unterminated bracket identifier")
			}
			value := sqlText[i+1 : i+1+end]
			tokens = append(tokens, schemaToken{kind: schemaIdentifier, text: strings.ToLower(value), quoted: true})
			i += end + 2
		default:
			if isIdentifierStart(sqlText[i]) {
				i++
				for i < len(sqlText) && isIdentifierPart(sqlText[i]) {
					i++
				}
				tokens = append(tokens, schemaToken{kind: schemaIdentifier, text: strings.ToLower(sqlText[start:i])})
				continue
			}
			if isNumberStart(sqlText, i) {
				i = scanNumber(sqlText, i)
				tokens = append(tokens, schemaToken{kind: schemaNumber, text: sqlText[start:i]})
				continue
			}
			if i+1 < len(sqlText) && isTwoCharacterOperator(sqlText[i:i+2]) {
				i += 2
			} else {
				i++
			}
			tokens = append(tokens, schemaToken{kind: schemaSymbol, text: sqlText[start:i]})
		}
	}
	return tokens, nil
}

func scanQuotedSQL(text string, start int, quote, escapeQuote byte) (int, error) {
	for i := start + 1; i < len(text); i++ {
		if text[i] != quote {
			continue
		}
		if i+1 < len(text) && text[i+1] == escapeQuote {
			i++
			continue
		}
		return i + 1, nil
	}
	return 0, fmt.Errorf("unterminated SQL string")
}

func scanQuotedIdentifier(text string, start int, quote byte) (int, string, error) {
	var value strings.Builder
	for i := start + 1; i < len(text); i++ {
		if text[i] != quote {
			value.WriteByte(text[i])
			continue
		}
		if i+1 < len(text) && text[i+1] == quote {
			value.WriteByte(quote)
			i++
			continue
		}
		return i + 1, value.String(), nil
	}
	return 0, "", fmt.Errorf("unterminated quoted identifier")
}

func isIdentifierStart(char byte) bool {
	return char == '_' || char == '$' || char >= 0x80 ||
		(char >= 'a' && char <= 'z') || (char >= 'A' && char <= 'Z')
}

func isIdentifierPart(char byte) bool {
	return isIdentifierStart(char) || (char >= '0' && char <= '9')
}

func isNumberStart(text string, index int) bool {
	return text[index] >= '0' && text[index] <= '9' ||
		text[index] == '.' && index+1 < len(text) && text[index+1] >= '0' && text[index+1] <= '9'
}

func scanNumber(text string, start int) int {
	for i := start + 1; i < len(text); i++ {
		char := text[i]
		if (char >= '0' && char <= '9') || (char >= 'a' && char <= 'z') || (char >= 'A' && char <= 'Z') || char == '_' || char == '.' {
			continue
		}
		if (char == '+' || char == '-') && i > start && strings.ContainsRune("eEpP", rune(text[i-1])) {
			continue
		}
		return i
	}
	return len(text)
}

func isTwoCharacterOperator(operator string) bool {
	switch operator {
	case "<=", ">=", "<>", "!=", "==", "||", "<<", ">>", "->":
		return true
	default:
		return false
	}
}
